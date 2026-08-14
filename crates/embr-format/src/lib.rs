//! EMBR — a solid, seekable, deduplicating archive container.
//!
//! The format's advantage over ZIP comes from the container, not from a novel
//! compression algorithm:
//!
//! * **Solid blocks.** ZIP compresses each file independently, so a folder of
//!   2,500 Python files re-learns the same vocabulary 2,500 times. EMBR packs
//!   many files into one large block with a shared dictionary.
//! * **Large windows.** DEFLATE looks back 32 KB; EMBR asks zstd for up to
//!   128 MB, so near-identical files far apart in the archive still match.
//! * **Sorted layout.** Files are grouped by type before packing, which is free
//!   and materially improves what the solid block can find.
//! * **Whole-file dedup.** Byte-identical files are stored once.
//! * **A fast lane.** Already-compressed media is still packed solid, but at
//!   a cheap level, since high effort there provably buys nothing.
//!
//! Everything here is seekable: the index at the tail records each block's
//! offset, so extracting one file decodes only the blocks it touches.

pub mod classify;
pub mod codec;
pub mod index;
pub mod reader;
pub mod writer;

pub use codec::Codec;
pub use index::{Entry, Index, Kind, Segment};
pub use reader::{Archive, ExtractOptions, ExtractStats};
pub use writer::{create, WriteOptions, WriteStats, DEFAULT_BLOCK_SIZE};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod roundtrip_tests {
    use super::*;
    use std::fs;
    use std::path::{Path, PathBuf};

    /// Build a small tree with duplicates, symlinks, nested dirs and a media
    /// file, archive it, extract it, and compare.
    fn fixture(root: &Path) {
        fs::create_dir_all(root.join("src/deep")).unwrap();
        fs::create_dir_all(root.join("assets")).unwrap();
        let text = "fn main() { println!(\"hello\"); }\n".repeat(200);
        fs::write(root.join("src/main.rs"), &text).unwrap();
        fs::write(root.join("src/deep/copy.rs"), &text).unwrap(); // exact duplicate
        fs::write(root.join("src/lib.rs"), "pub mod a;\n".repeat(50)).unwrap();
        fs::write(root.join("README"), "readme\n").unwrap();
        fs::write(root.join("empty.txt"), "").unwrap();
        // Fast-lane file: extension says already-compressed.
        fs::write(root.join("assets/photo.jpg"), vec![0xABu8; 40_000]).unwrap();
        std::os::unix::fs::symlink("src/main.rs", root.join("link.rs")).unwrap();
    }

    /// Tests run in parallel in one process, so the scratch directory has to be
    /// unique per test, not per process.
    fn scratch(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("embr-test-{}-{name}", std::process::id()))
    }

    fn roundtrip(name: &str, opts: WriteOptions) {
        let tmp = scratch(name);
        let _ = fs::remove_dir_all(&tmp);
        let src = tmp.join("tree");
        fixture(&src);

        let archive = tmp.join("out.embr");
        let stats = create(&archive, std::slice::from_ref(&src), &opts, |_| {}).unwrap();
        assert_eq!(stats.files, 6);
        assert_eq!(stats.symlinks, 1);

        let dest = tmp.join("out");
        let mut a = Archive::open(&archive).unwrap();
        a.extract(&dest, None, &ExtractOptions::default(), |_| {})
            .unwrap();

        let out = dest.join("tree");
        assert_eq!(
            fs::read(src.join("src/main.rs")).unwrap(),
            fs::read(out.join("src/main.rs")).unwrap()
        );
        assert_eq!(
            fs::read(src.join("src/deep/copy.rs")).unwrap(),
            fs::read(out.join("src/deep/copy.rs")).unwrap()
        );
        assert_eq!(
            fs::read(src.join("assets/photo.jpg")).unwrap(),
            fs::read(out.join("assets/photo.jpg")).unwrap()
        );
        assert_eq!(fs::read(out.join("empty.txt")).unwrap().len(), 0);
        assert_eq!(
            fs::read_link(out.join("link.rs")).unwrap().to_str().unwrap(),
            "src/main.rs"
        );

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn roundtrip_zstd() {
        roundtrip("zstd", WriteOptions::default());
    }

    #[test]
    fn roundtrip_xz() {
        roundtrip("xz", WriteOptions {
            codec: Codec::Xz,
            level: 6,
            ..Default::default()
        });
    }

    #[test]
    fn roundtrip_tiny_blocks() {
        // Forces multiple blocks and the oversized-file path.
        roundtrip("tiny", WriteOptions {
            block_size: 4096,
            ..Default::default()
        });
    }

    #[test]
    fn roundtrip_without_dedup_or_fast_lane() {
        roundtrip("plain", WriteOptions {
            dedup: false,
            fast_lane: false,
            ..Default::default()
        });
    }

    #[test]
    fn dedup_collapses_identical_files() {
        let tmp = scratch("dedup");
        let _ = fs::remove_dir_all(&tmp);
        let src = tmp.join("tree");
        fs::create_dir_all(&src).unwrap();
        let blob = vec![7u8; 200_000];
        for i in 0..5 {
            fs::write(src.join(format!("copy{i}.bin")), &blob).unwrap();
        }
        let archive = tmp.join("out.embr");
        let stats = create(&archive, &[src], &WriteOptions::default(), |_| {}).unwrap();
        assert_eq!(stats.deduped_files, 4);
        assert_eq!(stats.deduped_bytes, 800_000);
        let _ = fs::remove_dir_all(&tmp);
    }
}
