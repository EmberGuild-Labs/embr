//! On-disk layout: the fixed header, the index, and the trailing footer.
//!
//! ```text
//! +----------------+  offset 0
//! | Header (16 B)  |  magic + version, so `file(1)` can identify an archive
//! +----------------+
//! | Block 0        |  solid, independently decodable
//! | Block 1        |
//! | ...            |
//! +----------------+
//! | Index          |  zstd-compressed; paths, metadata, block map
//! +----------------+
//! | Footer (64 B)  |  where the index is, and its hash
//! +----------------+  end of file
//! ```
//!
//! The index lives at the end because block sizes are not known until the data
//! is written, and the footer is last and fixed-size so a reader can find the
//! index with two seeks and never scan the archive. Every block records its own
//! offset, which is what makes single-file extraction possible without
//! decompressing everything before it.

use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use std::io::{Read, Seek, SeekFrom, Write};

pub const MAGIC: [u8; 4] = *b"EMBR";
pub const VERSION: u16 = 1;
pub const HEADER_LEN: u64 = 16;
pub const FOOTER_LEN: u64 = 64;

/// Written at offset 0. Intentionally tiny; everything variable lives in the
/// index so the header never needs to change shape.
pub fn write_header<W: Write>(w: &mut W) -> Result<()> {
    let mut buf = [0u8; HEADER_LEN as usize];
    buf[0..4].copy_from_slice(&MAGIC);
    buf[4..6].copy_from_slice(&VERSION.to_le_bytes());
    // bytes 6..16 reserved, must be zero
    w.write_all(&buf)?;
    Ok(())
}

pub fn read_header<R: Read>(r: &mut R) -> Result<u16> {
    let mut buf = [0u8; HEADER_LEN as usize];
    r.read_exact(&mut buf)
        .map_err(|_| anyhow!("file is too short to be an EMBR archive"))?;
    if buf[0..4] != MAGIC {
        bail!("not an EMBR archive (bad magic)");
    }
    let version = u16::from_le_bytes([buf[4], buf[5]]);
    if version > VERSION {
        bail!("archive is format v{version}, this build understands up to v{VERSION}");
    }
    Ok(version)
}

/// Fixed-size trailer. Read by seeking to `len - FOOTER_LEN`.
#[derive(Debug, Clone)]
pub struct Footer {
    pub index_offset: u64,
    /// Length of the index as stored (zstd-compressed).
    pub index_clen: u64,
    /// Length of the serialized index before compression.
    pub index_rlen: u64,
    /// BLAKE3 of the *uncompressed* serialized index.
    pub index_hash: [u8; 32],
}

impl Footer {
    pub fn write<W: Write>(&self, w: &mut W) -> Result<()> {
        let mut buf = [0u8; FOOTER_LEN as usize];
        buf[0..8].copy_from_slice(&self.index_offset.to_le_bytes());
        buf[8..16].copy_from_slice(&self.index_clen.to_le_bytes());
        buf[16..24].copy_from_slice(&self.index_rlen.to_le_bytes());
        buf[24..56].copy_from_slice(&self.index_hash);
        buf[56..58].copy_from_slice(&VERSION.to_le_bytes());
        // 58..60 reserved
        buf[60..64].copy_from_slice(&MAGIC);
        w.write_all(&buf)?;
        Ok(())
    }

    pub fn read<R: Read + Seek>(r: &mut R) -> Result<Footer> {
        let len = r.seek(SeekFrom::End(0))?;
        if len < HEADER_LEN + FOOTER_LEN {
            bail!("file is too short to be an EMBR archive");
        }
        r.seek(SeekFrom::End(-(FOOTER_LEN as i64)))?;
        let mut buf = [0u8; FOOTER_LEN as usize];
        r.read_exact(&mut buf)?;
        if buf[60..64] != MAGIC {
            bail!("EMBR footer not found; archive is truncated or corrupt");
        }
        let mut index_hash = [0u8; 32];
        index_hash.copy_from_slice(&buf[24..56]);
        Ok(Footer {
            index_offset: u64::from_le_bytes(buf[0..8].try_into().unwrap()),
            index_clen: u64::from_le_bytes(buf[8..16].try_into().unwrap()),
            index_rlen: u64::from_le_bytes(buf[16..24].try_into().unwrap()),
            index_hash,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockInfo {
    /// Absolute offset of the block's first byte in the archive.
    pub offset: u64,
    /// Bytes occupied on disk.
    pub clen: u64,
    /// Bytes after decoding.
    pub rlen: u64,
    /// [`crate::codec::Codec`] id.
    pub codec: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    File,
    Dir,
    Symlink,
}

/// One contiguous run of a file's bytes inside one block. Files smaller than a
/// block have exactly one; a file larger than the block size is split across
/// several. Deduplicated files simply share another entry's segments.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Segment {
    pub block: u32,
    /// Offset within the *decoded* block.
    pub offset: u64,
    pub len: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    /// Archive-relative path, always `/`-separated, never absolute and never
    /// containing `..`. Enforced on write and re-checked on extract.
    pub path: String,
    pub kind: Kind,
    /// Unix mode bits. Only the permission bits are applied on extract.
    pub mode: u32,
    /// Modification time, seconds since the Unix epoch.
    pub mtime: i64,
    pub mtime_nanos: u32,
    pub size: u64,
    /// BLAKE3 of the file's contents; `None` for directories and symlinks.
    /// Doubles as the dedup key and as an integrity check on extract.
    pub hash: Option<[u8; 32]>,
    pub link_target: Option<String>,
    pub segments: Vec<Segment>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Index {
    pub blocks: Vec<BlockInfo>,
    pub entries: Vec<Entry>,
    /// Free-form provenance, e.g. "embr 0.1.0". Not interpreted on read.
    pub created_by: String,
}

impl Index {
    /// Serialize, compress, and append to `w`, returning the footer that
    /// describes it.
    pub fn write<W: Write + Seek>(&self, w: &mut W) -> Result<Footer> {
        let raw = bincode::serialize(self)?;
        let hash = blake3::hash(&raw);
        let offset = w.stream_position()?;
        // Level 19 on the index is cheap in absolute terms — it is small
        // relative to the payload — and path lists compress extremely well.
        let packed = zstd::stream::encode_all(&raw[..], 19)?;
        w.write_all(&packed)?;
        Ok(Footer {
            index_offset: offset,
            index_clen: packed.len() as u64,
            index_rlen: raw.len() as u64,
            index_hash: *hash.as_bytes(),
        })
    }

    /// Read and verify the index described by `footer`.
    pub fn read<R: Read + Seek>(r: &mut R, footer: &Footer) -> Result<Index> {
        r.seek(SeekFrom::Start(footer.index_offset))?;
        let mut packed = vec![0u8; footer.index_clen as usize];
        r.read_exact(&mut packed)
            .map_err(|_| anyhow!("archive is truncated: index is incomplete"))?;
        let raw = zstd::stream::decode_all(&packed[..])?;
        if raw.len() as u64 != footer.index_rlen {
            bail!("index decoded to the wrong size; archive is corrupt");
        }
        if blake3::hash(&raw).as_bytes() != &footer.index_hash {
            bail!("index failed its checksum; archive is corrupt");
        }
        Ok(bincode::deserialize(&raw)?)
    }
}

/// Reject paths that would let an archive write outside the extraction
/// directory — absolute paths, `..` traversal, Windows drive letters, and
/// empty components. Applied when building the archive *and* again on extract,
/// because an archive can be crafted by anyone.
pub fn sanitize_path(path: &str) -> Result<String> {
    if path.is_empty() {
        bail!("archive contains an entry with an empty path");
    }
    if path.starts_with('/') || path.starts_with('\\') {
        bail!("refusing absolute path in archive: {path}");
    }
    if path.len() >= 2 && path.as_bytes()[1] == b':' {
        bail!("refusing path with a drive letter: {path}");
    }
    for part in path.split('/') {
        if part == ".." {
            bail!("refusing path that escapes the archive root: {path}");
        }
        if part.is_empty() || part == "." {
            bail!("refusing path with an empty or '.' component: {path}");
        }
        if part.contains('\\') {
            bail!("refusing path containing a backslash: {path}");
        }
    }
    Ok(path.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_traversal_and_absolute_paths() {
        assert!(sanitize_path("a/b.txt").is_ok());
        assert!(sanitize_path("/etc/passwd").is_err());
        assert!(sanitize_path("../../etc/passwd").is_err());
        assert!(sanitize_path("a/../../b").is_err());
        assert!(sanitize_path("C:/windows").is_err());
        assert!(sanitize_path("a//b").is_err());
        assert!(sanitize_path("").is_err());
    }

    #[test]
    fn footer_round_trips() {
        let f = Footer {
            index_offset: 1234,
            index_clen: 56,
            index_rlen: 789,
            index_hash: [7u8; 32],
        };
        let mut buf = std::io::Cursor::new(Vec::new());
        // A footer is only ever read from the end of a file.
        buf.write_all(&vec![0u8; HEADER_LEN as usize]).unwrap();
        f.write(&mut buf).unwrap();
        let got = Footer::read(&mut buf).unwrap();
        assert_eq!(got.index_offset, 1234);
        assert_eq!(got.index_clen, 56);
        assert_eq!(got.index_rlen, 789);
        assert_eq!(got.index_hash, [7u8; 32]);
    }
}
