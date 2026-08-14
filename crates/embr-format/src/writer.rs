//! Archive creation.
//!
//! The pipeline is: walk → hash → sort → pack. Sorting between hashing and
//! packing is the whole point; see [`crate::classify::sort_key`]. Hashing up
//! front is what makes whole-file dedup possible, which on folders containing a
//! duplicated subtree is worth far more than any codec choice.

use crate::classify::{sort_key, Lane};
use crate::codec::{encode_block, Codec};
use crate::index::{sanitize_path, BlockInfo, Entry, Index, Kind, Segment};
use anyhow::{bail, Context, Result};
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufWriter, Read, Seek, Write};
use std::path::{Path, PathBuf};

pub const DEFAULT_BLOCK_SIZE: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct WriteOptions {
    pub codec: Codec,
    pub level: i32,
    pub block_size: usize,
    /// Collapse byte-identical files into a single stored copy.
    pub dedup: bool,
    /// Route already-compressed file types through the cheap lane.
    pub fast_lane: bool,
    /// Follow symlinks instead of archiving them as links.
    pub follow_symlinks: bool,
}

impl Default for WriteOptions {
    fn default() -> Self {
        WriteOptions {
            codec: Codec::Zstd,
            level: 12,
            block_size: DEFAULT_BLOCK_SIZE,
            dedup: true,
            fast_lane: true,
            follow_symlinks: false,
        }
    }
}

/// Codec and level for a lane. The fast lane always uses zstd regardless of the
/// archive's codec: on already-compressed payloads, xz costs many times more
/// CPU for no measurable gain, so honouring `--codec xz` there would be a
/// pessimisation, not a preference.
pub const FAST_LANE_LEVEL: i32 = 1;

impl WriteOptions {
    fn lane_codec(&self, lane: Lane) -> (Codec, i32) {
        match lane {
            Lane::Compress => (self.codec, self.level),
            Lane::Fast => (Codec::Zstd, FAST_LANE_LEVEL),
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct WriteStats {
    pub files: u64,
    pub dirs: u64,
    pub symlinks: u64,
    pub input_bytes: u64,
    /// Bytes that dedup meant we never had to store.
    pub deduped_bytes: u64,
    pub deduped_files: u64,
    pub blocks: u64,
    pub archive_bytes: u64,
}

/// A file discovered during the walk, before it is packed.
struct Candidate {
    abs: PathBuf,
    rel: String,
    mode: u32,
    mtime: i64,
    mtime_nanos: u32,
    size: u64,
    hash: [u8; 32],
}

pub fn create<P: AsRef<Path>>(
    archive: P,
    inputs: &[PathBuf],
    opts: &WriteOptions,
    mut progress: impl FnMut(&str),
) -> Result<WriteStats> {
    if inputs.is_empty() {
        bail!("no input paths given");
    }
    let mut stats = WriteStats::default();
    let mut index = Index {
        created_by: format!("embr {}", env!("CARGO_PKG_VERSION")),
        ..Default::default()
    };

    // --- walk -------------------------------------------------------------
    progress("scanning");
    let mut candidates: Vec<Candidate> = Vec::new();
    for input in inputs {
        walk_input(input, opts, &mut index, &mut candidates, &mut stats)?;
    }

    // --- hash -------------------------------------------------------------
    // Also the point where unreadable files surface, before anything is
    // written to the output.
    progress("hashing");
    for c in candidates.iter_mut() {
        c.hash = hash_file(&c.abs)
            .with_context(|| format!("hashing {}", c.abs.display()))?;
        stats.input_bytes += c.size;
    }

    // --- sort -------------------------------------------------------------
    candidates.sort_by_key(|c| sort_key(&c.rel));

    // --- pack -------------------------------------------------------------
    progress("packing");
    let out = File::create(archive.as_ref())
        .with_context(|| format!("creating {}", archive.as_ref().display()))?;
    let mut out = BufWriter::with_capacity(1 << 20, out);
    crate::index::write_header(&mut out)?;

    let mut packer = Packer {
        opts,
        buf: Vec::with_capacity(opts.block_size.min(8 << 20)),
        buf_lane: Lane::Compress,
        blocks: Vec::new(),
        seen: HashMap::new(),
    };

    for c in &candidates {
        let lane = if opts.fast_lane {
            crate::classify::lane_for(&c.rel)
        } else {
            Lane::Compress
        };

        let segments = if opts.dedup {
            match packer.seen.get(&c.hash) {
                Some(existing) => {
                    stats.deduped_files += 1;
                    stats.deduped_bytes += c.size;
                    existing.clone()
                }
                None => {
                    let segs = packer.add(&mut out, c, lane)?;
                    packer.seen.insert(c.hash, segs.clone());
                    segs
                }
            }
        } else {
            packer.add(&mut out, c, lane)?
        };

        index.entries.push(Entry {
            path: c.rel.clone(),
            kind: Kind::File,
            mode: c.mode,
            mtime: c.mtime,
            mtime_nanos: c.mtime_nanos,
            size: c.size,
            hash: Some(c.hash),
            link_target: None,
            segments,
        });
        stats.files += 1;
    }
    packer.flush(&mut out)?;

    index.blocks = packer.blocks;
    stats.blocks = index.blocks.len() as u64;

    // Entries are stored in extraction-friendly order: directories first, so
    // that creating them is a single forward pass with no mkdir -p races.
    index.entries.sort_by(|a, b| {
        kind_rank(a.kind)
            .cmp(&kind_rank(b.kind))
            .then_with(|| a.path.cmp(&b.path))
    });

    progress("writing index");
    let footer = index.write(&mut out)?;
    footer.write(&mut out)?;
    out.flush()?;
    let mut out = out.into_inner()?;
    stats.archive_bytes = out.stream_position()?;
    out.sync_all()?;

    Ok(stats)
}

fn kind_rank(k: Kind) -> u8 {
    match k {
        Kind::Dir => 0,
        Kind::File => 1,
        Kind::Symlink => 2,
    }
}

/// Accumulates files into solid blocks and writes them out.
struct Packer<'a> {
    opts: &'a WriteOptions,
    buf: Vec<u8>,
    /// Lane the pending buffer belongs to, which decides how it is encoded.
    buf_lane: Lane,
    blocks: Vec<BlockInfo>,
    seen: HashMap<[u8; 32], Vec<Segment>>,
}

impl Packer<'_> {
    /// Append one file, returning the segments describing where it landed.
    fn add<W: Write + Seek>(
        &mut self,
        out: &mut W,
        c: &Candidate,
        lane: Lane,
    ) -> Result<Vec<Segment>> {
        if c.size == 0 {
            return Ok(Vec::new());
        }

        // Sorting makes lanes contiguous, so a lane change means the pending
        // buffer is complete.
        if lane != self.buf_lane {
            self.flush(out)?;
            self.buf_lane = lane;
        }

        // Files at or above the block size stream straight into a block of
        // their own, so peak memory stays near block_size no matter how large
        // the input is.
        if c.size as usize >= self.opts.block_size {
            self.flush(out)?;
            let (codec, level) = self.opts.lane_codec(lane);
            let f = File::open(&c.abs)
                .with_context(|| format!("opening {}", c.abs.display()))?;
            let block = self.write_block(out, codec, level, f, Some(c.size))?;
            return Ok(vec![Segment {
                block,
                offset: 0,
                len: c.size,
            }]);
        }

        if self.buf.len() + c.size as usize > self.opts.block_size {
            self.flush(out)?;
        }
        let offset = self.buf.len() as u64;
        let mut f = File::open(&c.abs)
            .with_context(|| format!("opening {}", c.abs.display()))?;
        let read = std::io::copy(&mut f, &mut self.buf)?;
        if read != c.size {
            bail!(
                "{} changed size while being archived ({} -> {read} bytes)",
                c.abs.display(),
                c.size
            );
        }
        Ok(vec![Segment {
            block: self.blocks.len() as u32,
            offset,
            len: c.size,
        }])
    }

    /// Seal the pending buffer into a block, if there is one.
    fn flush<W: Write + Seek>(&mut self, out: &mut W) -> Result<()> {
        if self.buf.is_empty() {
            return Ok(());
        }
        let buf = std::mem::take(&mut self.buf);
        let expected = buf.len() as u64;
        let (codec, level) = self.opts.lane_codec(self.buf_lane);
        self.write_block(out, codec, level, &buf[..], Some(expected))?;
        // Put the allocation back rather than dropping it; blocks are large and
        // reallocating one per block is pure waste.
        self.buf = buf;
        self.buf.clear();
        Ok(())
    }

    /// Encode `src` as one block and record it. Returns the block index.
    fn write_block<W: Write + Seek, R: Read>(
        &mut self,
        out: &mut W,
        codec: Codec,
        level: i32,
        src: R,
        expected_raw: Option<u64>,
    ) -> Result<u32> {
        let offset = out.stream_position()?;
        let rlen = encode_block(codec, level, self.opts.block_size, src, out)?;
        let clen = out.stream_position()? - offset;
        if let Some(expected) = expected_raw {
            if rlen != expected {
                bail!("short read while writing block: expected {expected} bytes, got {rlen}");
            }
        }
        self.blocks.push(BlockInfo {
            offset,
            clen,
            rlen,
            codec: codec.as_u8(),
        });
        Ok((self.blocks.len() - 1) as u32)
    }
}

fn walk_input(
    input: &Path,
    opts: &WriteOptions,
    index: &mut Index,
    candidates: &mut Vec<Candidate>,
    stats: &mut WriteStats,
) -> Result<()> {
    let meta = std::fs::symlink_metadata(input)
        .with_context(|| format!("reading {}", input.display()))?;

    // Paths inside the archive are relative to the input's parent, so
    // `embr create a.embr some/dir` yields entries under `dir/...`.
    let _ = &meta;
    let base = input.parent().unwrap_or(Path::new("")).to_path_buf();

    let walker = walkdir::WalkDir::new(input)
        .follow_links(opts.follow_symlinks)
        .sort_by_file_name();

    for item in walker {
        let item = item.with_context(|| format!("walking {}", input.display()))?;
        let abs = item.path();
        let rel = match abs.strip_prefix(&base) {
            Ok(r) if r.as_os_str().is_empty() => continue,
            Ok(r) => to_archive_path(r)?,
            Err(_) => continue,
        };
        let meta = item
            .metadata()
            .with_context(|| format!("reading metadata for {}", abs.display()))?;
        let (mtime, mtime_nanos) = mtime_of(&meta);
        let mode = mode_of(&meta);

        if meta.is_dir() {
            index.entries.push(Entry {
                path: rel,
                kind: Kind::Dir,
                mode,
                mtime,
                mtime_nanos,
                size: 0,
                hash: None,
                link_target: None,
                segments: Vec::new(),
            });
            stats.dirs += 1;
        } else if meta.is_symlink() {
            let target = std::fs::read_link(abs)
                .with_context(|| format!("reading symlink {}", abs.display()))?;
            index.entries.push(Entry {
                path: rel,
                kind: Kind::Symlink,
                mode,
                mtime,
                mtime_nanos,
                size: 0,
                hash: None,
                link_target: Some(target.to_string_lossy().into_owned()),
                segments: Vec::new(),
            });
            stats.symlinks += 1;
        } else if meta.is_file() {
            candidates.push(Candidate {
                abs: abs.to_path_buf(),
                rel,
                mode,
                mtime,
                mtime_nanos,
                size: meta.len(),
                hash: [0u8; 32],
            });
        }
        // Sockets, fifos and devices are skipped: they have no portable
        // meaning inside an archive.
    }
    Ok(())
}

fn to_archive_path(rel: &Path) -> Result<String> {
    let mut parts = Vec::new();
    for comp in rel.components() {
        match comp {
            std::path::Component::Normal(s) => parts.push(s.to_string_lossy().into_owned()),
            _ => bail!("unsupported path component in {}", rel.display()),
        }
    }
    sanitize_path(&parts.join("/"))
}

fn mtime_of(meta: &std::fs::Metadata) -> (i64, u32) {
    use std::os::unix::fs::MetadataExt;
    (meta.mtime(), meta.mtime_nsec() as u32)
}

fn mode_of(meta: &std::fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode()
}

fn hash_file(path: &Path) -> Result<[u8; 32]> {
    let mut f = File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(*hasher.finalize().as_bytes())
}
