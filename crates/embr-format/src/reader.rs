//! Archive reading, listing and extraction.
//!
//! Extraction walks entries in block order and keeps one decoded block cached.
//! Because the writer laid files down sorted, files that share a block are
//! adjacent here too, so a full extract decodes each block exactly once while
//! extracting a single file decodes exactly the blocks that file touches.

use crate::codec::{decode_block, Codec};
use crate::index::{sanitize_path, Entry, Footer, Index, Kind};
use anyhow::{bail, Context, Result};
use std::fs::File;
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};

pub struct Archive {
    file: File,
    pub index: Index,
    pub footer: Footer,
    /// Most recently decoded block, kept because consecutive entries usually
    /// share one.
    cache: Option<(u32, Vec<u8>)>,
}

#[derive(Debug, Default, Clone)]
pub struct ExtractStats {
    pub files: u64,
    pub dirs: u64,
    pub symlinks: u64,
    pub bytes: u64,
}

#[derive(Debug, Clone)]
pub struct ExtractOptions {
    /// Restore permission bits from the archive.
    pub restore_mode: bool,
    /// Restore modification times.
    pub restore_mtime: bool,
    /// Verify each file's BLAKE3 against the index after writing.
    pub verify: bool,
    /// Overwrite files that already exist at the destination.
    pub overwrite: bool,
}

impl Default for ExtractOptions {
    fn default() -> Self {
        ExtractOptions {
            restore_mode: true,
            restore_mtime: true,
            verify: true,
            overwrite: true,
        }
    }
}

impl Archive {
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Archive> {
        let mut file = File::open(path.as_ref())
            .with_context(|| format!("opening {}", path.as_ref().display()))?;
        crate::index::read_header(&mut file)?;
        let footer = Footer::read(&mut file)?;
        let index = Index::read(&mut file, &footer)?;
        Ok(Archive {
            file,
            index,
            footer,
            cache: None,
        })
    }

    pub fn entries(&self) -> &[Entry] {
        &self.index.entries
    }

    /// Total uncompressed size of all file entries, counting deduplicated
    /// files at full size — this is what the user gets back on disk.
    pub fn total_size(&self) -> u64 {
        self.index
            .entries
            .iter()
            .filter(|e| e.kind == Kind::File)
            .map(|e| e.size)
            .sum()
    }

    /// Decode a block, reusing the cache when possible.
    fn block(&mut self, idx: u32) -> Result<&[u8]> {
        if !matches!(&self.cache, Some((cached, _)) if *cached == idx) {
            let info = self
                .index
                .blocks
                .get(idx as usize)
                .with_context(|| format!("index references missing block {idx}"))?
                .clone();
            let codec = Codec::from_u8(info.codec)?;
            self.file.seek(SeekFrom::Start(info.offset))?;
            let mut raw = (&self.file).take(info.clen);
            let data = decode_block(codec, &mut raw, info.rlen)
                .with_context(|| format!("decoding block {idx}"))?;
            self.cache = Some((idx, data));
        }
        Ok(&self.cache.as_ref().unwrap().1)
    }

    /// Read one file entry's full contents into memory.
    pub fn read_file(&mut self, entry: &Entry) -> Result<Vec<u8>> {
        let mut out = Vec::with_capacity(entry.size as usize);
        self.write_file_to(entry, &mut out)?;
        Ok(out)
    }

    /// Stream one file entry's contents into `w`.
    pub fn write_file_to<W: Write>(&mut self, entry: &Entry, w: &mut W) -> Result<()> {
        for seg in &entry.segments {
            let block = self.block(seg.block)?;
            let start = seg.offset as usize;
            let end = start
                .checked_add(seg.len as usize)
                .filter(|e| *e <= block.len())
                .with_context(|| {
                    format!("segment of {} runs past the end of its block", entry.path)
                })?;
            w.write_all(&block[start..end])?;
        }
        Ok(())
    }

    /// Extract entries whose path matches `filter` (all of them if `None`).
    pub fn extract<P: AsRef<Path>>(
        &mut self,
        dest: P,
        filter: Option<&[String]>,
        opts: &ExtractOptions,
        mut progress: impl FnMut(&str),
    ) -> Result<ExtractStats> {
        let dest = dest.as_ref();
        std::fs::create_dir_all(dest)
            .with_context(|| format!("creating {}", dest.display()))?;
        // Resolve the destination so the containment check below compares real
        // paths and cannot be fooled by a symlinked destination.
        let dest = dest
            .canonicalize()
            .with_context(|| format!("resolving {}", dest.display()))?;

        let mut stats = ExtractStats::default();
        let selected: Vec<Entry> = self
            .index
            .entries
            .iter()
            .filter(|e| match filter {
                None => true,
                Some(pats) => pats.iter().any(|p| matches_path(&e.path, p)),
            })
            .cloned()
            .collect();

        if selected.is_empty() {
            if filter.is_some() {
                bail!("no entries in the archive matched");
            }
            return Ok(stats);
        }

        // Directories first (they are sorted to the front), then files in block
        // order so each block is decoded once, then symlinks last so their
        // targets already exist.
        let mut dirs: Vec<&Entry> = Vec::new();
        let mut files: Vec<&Entry> = Vec::new();
        let mut links: Vec<&Entry> = Vec::new();
        for e in &selected {
            match e.kind {
                Kind::Dir => dirs.push(e),
                Kind::File => files.push(e),
                Kind::Symlink => links.push(e),
            }
        }
        files.sort_by_key(|e| {
            e.segments
                .first()
                .map(|s| (s.block, s.offset))
                .unwrap_or((u32::MAX, 0))
        });

        for e in &dirs {
            let path = safe_join(&dest, &e.path)?;
            std::fs::create_dir_all(&path)
                .with_context(|| format!("creating {}", path.display()))?;
            stats.dirs += 1;
        }

        for e in &files {
            let path = safe_join(&dest, &e.path)?;
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("creating {}", parent.display()))?;
            }
            if !opts.overwrite && path.exists() {
                bail!("{} already exists (pass --overwrite to replace)", path.display());
            }
            progress(&e.path);

            let f = File::create(&path)
                .with_context(|| format!("writing {}", path.display()))?;
            let mut w = BufWriter::with_capacity(1 << 20, f);
            if opts.verify {
                let mut hw = HashingWriter {
                    inner: &mut w,
                    hasher: blake3::Hasher::new(),
                };
                self.write_file_to(e, &mut hw)?;
                let got = *hw.hasher.finalize().as_bytes();
                if let Some(expected) = e.hash {
                    if got != expected {
                        bail!("{} failed its checksum; archive is corrupt", e.path);
                    }
                }
            } else {
                self.write_file_to(e, &mut w)?;
            }
            w.flush()?;
            drop(w);

            apply_metadata(&path, e, opts)?;
            stats.files += 1;
            stats.bytes += e.size;
        }

        for e in &links {
            let path = safe_join(&dest, &e.path)?;
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let target = e
                .link_target
                .as_deref()
                .with_context(|| format!("symlink {} has no target", e.path))?;
            if path.symlink_metadata().is_ok() {
                if !opts.overwrite {
                    bail!("{} already exists", path.display());
                }
                std::fs::remove_file(&path).ok();
            }
            std::os::unix::fs::symlink(target, &path)
                .with_context(|| format!("creating symlink {}", path.display()))?;
            stats.symlinks += 1;
        }

        // Directory times are restored last: writing files into a directory
        // updates its mtime, so doing this earlier would be undone.
        if opts.restore_mtime {
            for e in &dirs {
                let path = safe_join(&dest, &e.path)?;
                let _ = filetime::set_file_mtime(
                    &path,
                    filetime::FileTime::from_unix_time(e.mtime, e.mtime_nanos),
                );
            }
        }

        Ok(stats)
    }

    /// Decode every block and re-hash every file, without writing anything.
    pub fn verify(&mut self, mut progress: impl FnMut(&str)) -> Result<u64> {
        let mut checked = 0u64;
        let mut files: Vec<Entry> = self
            .index
            .entries
            .iter()
            .filter(|e| e.kind == Kind::File)
            .cloned()
            .collect();
        files.sort_by_key(|e| {
            e.segments
                .first()
                .map(|s| (s.block, s.offset))
                .unwrap_or((u32::MAX, 0))
        });
        let mut sink = std::io::sink();
        for e in &files {
            progress(&e.path);
            let mut hw = HashingWriter {
                inner: &mut sink,
                hasher: blake3::Hasher::new(),
            };
            self.write_file_to(e, &mut hw)?;
            let got = *hw.hasher.finalize().as_bytes();
            if let Some(expected) = e.hash {
                if got != expected {
                    bail!("{} failed its checksum", e.path);
                }
            }
            checked += 1;
        }
        Ok(checked)
    }
}

struct HashingWriter<'a, W: Write> {
    inner: &'a mut W,
    hasher: blake3::Hasher,
}

impl<W: Write> Write for HashingWriter<'_, W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.hasher.update(&buf[..n]);
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

fn apply_metadata(path: &Path, e: &Entry, opts: &ExtractOptions) -> Result<()> {
    if opts.restore_mode {
        use std::os::unix::fs::PermissionsExt;
        // Only permission bits; the file type bits are not ours to set.
        let perm = std::fs::Permissions::from_mode(e.mode & 0o7777);
        std::fs::set_permissions(path, perm)
            .with_context(|| format!("setting permissions on {}", path.display()))?;
    }
    if opts.restore_mtime {
        filetime::set_file_mtime(
            path,
            filetime::FileTime::from_unix_time(e.mtime, e.mtime_nanos),
        )
        .with_context(|| format!("setting mtime on {}", path.display()))?;
    }
    Ok(())
}

/// Join an archive path onto `dest`, refusing anything that would land outside.
///
/// The path was sanitized on write, but an archive is untrusted input, so the
/// check is repeated here and then confirmed structurally against `dest`.
fn safe_join(dest: &Path, rel: &str) -> Result<PathBuf> {
    let rel = sanitize_path(rel)?;
    let joined = dest.join(&rel);
    // Re-walk the joined path; nothing should have introduced a `..` or a root.
    let mut depth = 0i32;
    for comp in joined.strip_prefix(dest).unwrap_or(&joined).components() {
        match comp {
            Component::Normal(_) => depth += 1,
            Component::CurDir => {}
            Component::ParentDir => depth -= 1,
            Component::RootDir | Component::Prefix(_) => {
                bail!("refusing to extract outside the destination: {rel}")
            }
        }
        if depth < 0 {
            bail!("refusing to extract outside the destination: {rel}");
        }
    }
    Ok(joined)
}

/// Match an entry path against a user-supplied selector: exact match, or a
/// directory prefix so `embr extract a.embr src` pulls all of `src/`.
fn matches_path(path: &str, pattern: &str) -> bool {
    let pattern = pattern.trim_end_matches('/');
    path == pattern || path.starts_with(&format!("{pattern}/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_join_blocks_escapes() {
        let dest = Path::new("/tmp/dest");
        assert!(safe_join(dest, "a/b.txt").is_ok());
        assert!(safe_join(dest, "../evil").is_err());
        assert!(safe_join(dest, "/etc/passwd").is_err());
    }

    #[test]
    fn path_selectors_match_directories() {
        assert!(matches_path("src/main.rs", "src"));
        assert!(matches_path("src/main.rs", "src/"));
        assert!(matches_path("src/main.rs", "src/main.rs"));
        assert!(!matches_path("srcx/main.rs", "src"));
    }
}
