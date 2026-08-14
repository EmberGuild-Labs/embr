//! Compression codecs. Each block in an archive names exactly one of these.
//!
//! The codec set is deliberately small: a fast general-purpose codec (zstd), a
//! high-ratio one (xz/LZMA2), and `Store` for data that is already compressed
//! and would only waste CPU. Codec IDs are part of the on-disk format, so
//! existing values must never be reused for something else.

use anyhow::{anyhow, Result};
use std::io::{self, Read, Write};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    Store = 0,
    Zstd = 1,
    Xz = 2,
}

impl Codec {
    pub fn as_u8(self) -> u8 {
        self as u8
    }

    pub fn from_u8(v: u8) -> Result<Codec> {
        match v {
            0 => Ok(Codec::Store),
            1 => Ok(Codec::Zstd),
            2 => Ok(Codec::Xz),
            other => Err(anyhow!(
                "unknown codec id {other}; archive was written by a newer EMBR"
            )),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Codec::Store => "store",
            Codec::Zstd => "zstd",
            Codec::Xz => "xz",
        }
    }

    pub fn parse(s: &str) -> Result<Codec> {
        match s.to_ascii_lowercase().as_str() {
            "store" | "none" => Ok(Codec::Store),
            "zstd" => Ok(Codec::Zstd),
            "xz" | "lzma" | "lzma2" => Ok(Codec::Xz),
            other => Err(anyhow!("unknown codec '{other}' (want store, zstd or xz)")),
        }
    }
}

/// Window log to request from zstd. A larger window is what lets the codec
/// match against data far earlier in the same solid block, which is most of
/// EMBR's advantage over DEFLATE's fixed 32 KB window.
fn window_log_for(block_size: usize) -> u32 {
    let mut log = 10u32;
    while (1usize << log) < block_size && log < MAX_WINDOW_LOG {
        log += 1;
    }
    log
}

/// Upper bound on window log we will ever request when writing. The reader
/// allows exactly this much, so raising it is a format-breaking change.
pub const MAX_WINDOW_LOG: u32 = 27; // 128 MB

/// Compress `src` into `out` as one complete block, returning the number of
/// *uncompressed* bytes consumed. The caller measures the compressed length
/// from the output stream position, since only it knows where the block began.
///
/// Used for both buffered blocks (pass a `&[u8]`) and oversized single files
/// (pass the open `File`), so large inputs never have to be held in memory.
pub fn encode_block<R: Read, W: Write>(
    codec: Codec,
    level: i32,
    block_size: usize,
    mut src: R,
    out: &mut W,
) -> Result<u64> {
    let raw = match codec {
        Codec::Store => io::copy(&mut src, out)?,
        Codec::Zstd => {
            let mut enc = zstd::stream::write::Encoder::new(&mut *out, level)?;
            enc.set_parameter(zstd::zstd_safe::CParameter::WindowLog(window_log_for(
                block_size,
            )))?;
            // zstd's own worker threads; cheap parallelism without restructuring
            // the writer around a job queue.
            enc.set_parameter(zstd::zstd_safe::CParameter::NbWorkers(
                num_cpus::get().min(16) as u32,
            ))?;
            let n = io::copy(&mut src, &mut enc)?;
            enc.finish()?;
            n
        }
        Codec::Xz => {
            let preset = level.clamp(0, 9) as u32;
            let mut enc = xz2::write::XzEncoder::new(&mut *out, preset);
            let n = io::copy(&mut src, &mut enc)?;
            enc.finish()?;
            n
        }
    };
    Ok(raw)
}

/// Decompress one block. `raw_len` is taken from the index and used to size the
/// output buffer up front and to reject blocks that decode to the wrong size.
pub fn decode_block<R: Read>(codec: Codec, src: R, raw_len: u64) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(raw_len as usize);
    match codec {
        Codec::Store => {
            src.take(raw_len).read_to_end(&mut out)?;
        }
        Codec::Zstd => {
            let mut dec = zstd::stream::read::Decoder::new(src)?;
            dec.window_log_max(MAX_WINDOW_LOG)?;
            dec.read_to_end(&mut out)?;
        }
        Codec::Xz => {
            let mut dec = xz2::read::XzDecoder::new(src);
            dec.read_to_end(&mut out)?;
        }
    }
    if out.len() as u64 != raw_len {
        return Err(anyhow!(
            "block decoded to {} bytes but index says {raw_len}; archive is corrupt",
            out.len()
        ));
    }
    Ok(out)
}
