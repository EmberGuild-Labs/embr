//! Lane assignment.
//!
//! Benchmarks on real folders show general-purpose compressors recover
//! essentially nothing *within* data that is already compressed — JPEG, PNG and
//! H.264 all land within a fraction of a percent of their input, no matter how
//! much CPU is spent. Spending level-12 or xz effort on them is waste.
//!
//! But measurements on a real photo folder showed grouping them into solid
//! blocks still recovers ~0.9%, because a camera roll shares EXIF blocks,
//! colour profiles and embedded thumbnails *across* files. So the fast lane
//! still compresses — it just does so at a cheap level where that cross-file
//! redundancy is nearly free to find. Timing on 197 MB of JPEG: 0.55 s
//! compressed vs 0.49 s stored, i.e. I/O dominates and the effort is free.
//!
//! Keeping the lanes separate also stops media from diluting the dictionary of
//! the blocks holding genuinely compressible data.
//!
//! This is where format-aware recompression will hook in later: lossless JPEG
//! transcoding becomes a third lane rather than a special case bolted on here.

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Lane {
    /// Full-effort compression: the archive's chosen codec and level.
    Compress = 0,
    /// Already-compressed payloads: still solid, but at a cheap level.
    Fast = 1,
}

/// Extensions holding the output of a compression codec, where the entropy has
/// genuinely been squeezed out and more effort buys nothing. Lowercase, no dot.
///
/// The line here is **codec output, not container**. A JPEG *is* a compressed
/// stream. A `.zip` or a `.pkg` merely *holds* compressed streams, alongside
/// headers, tables, padding, and sometimes entries stored with no compression
/// at all — and two related containers share structure that a real compression
/// pass can still find. Measured on this distinction:
///
/// | Corpus                        | Fast lane costs |
/// |-------------------------------|-----------------|
/// | `.zip` / `.jar` / `.whl`      | 1.6%            |
/// | `.pkg` installers             | 0.7%            |
/// | `.dmg` disk images            | 0.16%           |
/// | JPEG photos                   | 0.2%            |
///
/// A percent and a half is worth having, so containers were moved out of this
/// list and get full effort. Media stays: there the fast lane is not defending
/// a fraction of a percent, it is defending against `--preset max` spending two
/// minutes of LZMA on a photo library to achieve nothing.
///
/// Formats compressed with a *strong* codec (7z, rar, zst, xz) stay too. They
/// are containers, but LZMA-class compression leaves far less on the table than
/// DEFLATE-era ZIP does, so the trade runs the other way.
const PRECOMPRESSED: &[&str] = &[
    // still images
    "jpg", "jpeg", "jpe", "jxl", "png", "gif", "webp", "heic", "heif", "avif", "jp2",
    // video
    "mp4", "m4v", "mov", "mkv", "webm", "avi", "wmv", "flv", "mpg", "mpeg", "m2ts", "ts", "3gp",
    // audio
    "mp3", "aac", "m4a", "ogg", "oga", "opus", "flac", "wma", "ape",
    // raw compressed streams, and containers using a strong codec
    "gz", "tgz", "bz2", "xz", "zst", "lz4", "lzma", "br", "7z", "rar", "embr",
    // compressed web fonts
    "woff", "woff2",
];

/// Decide which lane a file belongs to, based on its name.
///
/// Extension matching is a heuristic, not a guarantee. It is cheap and right
/// often enough to be worth it; `--fast-lane false` turns it off and compresses
/// everything at full effort.
pub fn lane_for(path: &str) -> Lane {
    match extension_of(path) {
        Some(ext) if PRECOMPRESSED.contains(&ext.as_str()) => Lane::Fast,
        _ => Lane::Compress,
    }
}

/// Lowercased extension of a path, if it has one.
pub fn extension_of(path: &str) -> Option<String> {
    let name = path.rsplit('/').next().unwrap_or(path);
    // A leading dot means a hidden file, not an extension: ".gitignore" has none.
    let (_, ext) = name.trim_start_matches('.').rsplit_once('.')?;
    if ext.is_empty() {
        None
    } else {
        Some(ext.to_ascii_lowercase())
    }
}

/// Sort key controlling the order files are laid down inside the archive.
///
/// Grouping by lane first keeps the compressed blocks free of media. Within a
/// lane, grouping by extension puts files that share a vocabulary next to each
/// other — all the `.py` together, all the `.h` together — which is what makes
/// a solid block compress far better than the same files in directory order.
/// Directory then filename break ties so related files stay adjacent.
pub fn sort_key(path: &str) -> (Lane, String, String, String) {
    let lane = lane_for(path);
    let ext = extension_of(path).unwrap_or_default();
    let (dir, name) = match path.rsplit_once('/') {
        Some((d, n)) => (d.to_string(), n.to_string()),
        None => (String::new(), path.to_string()),
    };
    (lane, ext, dir, name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extensions_are_lowercased() {
        assert_eq!(extension_of("a/B.JPG").as_deref(), Some("jpg"));
        assert_eq!(extension_of("a/b.tar.gz").as_deref(), Some("gz"));
    }

    #[test]
    fn dotfiles_have_no_extension() {
        assert_eq!(extension_of(".gitignore"), None);
        assert_eq!(extension_of("src/.env"), None);
        assert_eq!(extension_of("Makefile"), None);
    }

    #[test]
    fn media_goes_to_the_fast_lane() {
        assert_eq!(lane_for("holiday/IMG_0001.JPEG"), Lane::Fast);
        assert_eq!(lane_for("clip.mp4"), Lane::Fast);
        assert_eq!(lane_for("song.flac"), Lane::Fast);
        assert_eq!(lane_for("bundle.tar.gz"), Lane::Fast);
        assert_eq!(lane_for("src/main.rs"), Lane::Compress);
        assert_eq!(lane_for("README"), Lane::Compress);
    }

    /// Containers hold compressed data but are not themselves codec output, and
    /// measurably lose ratio when given only cheap effort. Regression guard for
    /// the reasoning in PRECOMPRESSED.
    #[test]
    fn containers_get_full_effort() {
        for name in [
            "Installer.pkg",
            "Firefox.dmg",
            "ubuntu.iso",
            "app.deb",
            "lib.rpm",
            "archive.zip",
            "plugin.jar",
            "package.whl",
            "app.apk",
            "report.docx",
            "book.epub",
        ] {
            assert_eq!(lane_for(name), Lane::Compress, "{name} should get full effort");
        }
    }

    #[test]
    fn sorting_groups_by_lane_then_extension() {
        let mut paths = vec!["b/photo.jpg", "z/a.rs", "a/b.py", "c/d.rs"];
        paths.sort_by_key(|p| sort_key(p));
        assert_eq!(paths, vec!["a/b.py", "c/d.rs", "z/a.rs", "b/photo.jpg"]);
    }
}
