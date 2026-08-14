//! `embr` — command-line interface.

use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand};
use embr_format::codec::Codec;
use embr_format::index::Kind;
use embr_format::{create, Archive, ExtractOptions, WriteOptions};
use std::io::{IsTerminal, Write};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "embr",
    version,
    about = "Solid, deduplicating archives that beat .zip on size and speed"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,

    /// Suppress progress output.
    #[arg(short, long, global = true)]
    quiet: bool,
}

#[derive(Subcommand)]
enum Command {
    /// Create an archive from files and directories.
    #[command(visible_alias = "c")]
    Create(CreateArgs),
    /// Extract an archive.
    #[command(visible_alias = "x")]
    Extract(ExtractArgs),
    /// List an archive's contents.
    #[command(visible_alias = "l")]
    List(ListArgs),
    /// Show an archive's structure and compression statistics.
    Info(InfoArgs),
    /// Decode every block and check every file against its stored hash.
    Verify(InfoArgs),
    /// Write one file's contents to stdout.
    Cat(CatArgs),
}

#[derive(Args)]
struct CreateArgs {
    /// Archive to write.
    archive: PathBuf,
    /// Files and directories to include.
    #[arg(required = true)]
    inputs: Vec<PathBuf>,

    /// Compression codec.
    #[arg(long, default_value = "zstd", value_parser = parse_codec)]
    codec: Codec,

    /// Compression level. zstd accepts 1-22, xz 0-9.
    #[arg(short, long)]
    level: Option<i32>,

    /// Preset: fast (zstd 3), balanced (zstd 12), max (xz 9).
    #[arg(long, conflicts_with_all = ["codec", "level"])]
    preset: Option<String>,

    /// Solid block size in MiB. Larger blocks compress better; smaller blocks
    /// make single-file extraction cheaper.
    #[arg(long, default_value_t = 64)]
    block_size: usize,

    /// Store every byte-identical file only once.
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
    dedup: bool,

    /// Compress already-compressed file types (jpg, mp4, zip...) at a cheap
    /// level instead of full effort. They are still packed solid, which is
    /// where the small remaining gain on media comes from.
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
    fast_lane: bool,

    /// Follow symlinks and archive their targets instead of the links.
    #[arg(long)]
    follow_symlinks: bool,
}

#[derive(Args)]
struct ExtractArgs {
    archive: PathBuf,
    /// Extract only these paths (files, or directories and everything under
    /// them). Defaults to the whole archive.
    paths: Vec<String>,
    /// Directory to extract into.
    #[arg(short = 'C', long, default_value = ".")]
    dir: PathBuf,
    /// Skip the per-file checksum check.
    #[arg(long)]
    no_verify: bool,
    /// Fail instead of replacing files that already exist.
    #[arg(long)]
    no_overwrite: bool,
    /// Do not restore permissions or modification times.
    #[arg(long)]
    no_metadata: bool,
}

#[derive(Args)]
struct ListArgs {
    archive: PathBuf,
    /// Show size, mode, mtime and block placement for each entry.
    #[arg(short, long)]
    long: bool,
}

#[derive(Args)]
struct InfoArgs {
    archive: PathBuf,
}

#[derive(Args)]
struct CatArgs {
    archive: PathBuf,
    path: String,
}

fn parse_codec(s: &str) -> Result<Codec, String> {
    Codec::parse(s).map_err(|e| e.to_string())
}

fn main() {
    if let Err(e) = run() {
        eprintln!("embr: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Create(a) => cmd_create(a, cli.quiet),
        Command::Extract(a) => cmd_extract(a, cli.quiet),
        Command::List(a) => cmd_list(a),
        Command::Info(a) => cmd_info(a),
        Command::Verify(a) => cmd_verify(a, cli.quiet),
        Command::Cat(a) => cmd_cat(a),
    }
}

fn cmd_create(a: CreateArgs, quiet: bool) -> Result<()> {
    let (codec, level) = match a.preset.as_deref() {
        None => (
            a.codec,
            a.level.unwrap_or(match a.codec {
                Codec::Zstd => 12,
                Codec::Xz => 6,
                Codec::Store => 0,
            }),
        ),
        Some("fast") => (Codec::Zstd, 3),
        Some("balanced") => (Codec::Zstd, 12),
        Some("max") => (Codec::Xz, 9),
        Some(other) => bail!("unknown preset '{other}' (want fast, balanced or max)"),
    };
    if a.block_size == 0 {
        bail!("--block-size must be at least 1 MiB");
    }

    let opts = WriteOptions {
        codec,
        level,
        block_size: a.block_size * 1024 * 1024,
        dedup: a.dedup,
        fast_lane: a.fast_lane,
        follow_symlinks: a.follow_symlinks,
    };

    let start = std::time::Instant::now();
    let stats = create(&a.archive, &a.inputs, &opts, |phase| {
        if !quiet {
            eprint!("\r\x1b[K{phase}...");
            let _ = std::io::stderr().flush();
        }
    })?;
    let elapsed = start.elapsed();
    if !quiet {
        eprint!("\r\x1b[K");
    }

    let ratio = if stats.input_bytes > 0 {
        stats.archive_bytes as f64 / stats.input_bytes as f64
    } else {
        0.0
    };
    println!(
        "{}  {} files, {} dirs, {} symlinks",
        a.archive.display(),
        stats.files,
        stats.dirs,
        stats.symlinks
    );
    println!(
        "  {} -> {}  ({:.1}% of original, {:.2}x smaller)",
        human(stats.input_bytes),
        human(stats.archive_bytes),
        ratio * 100.0,
        if ratio > 0.0 { 1.0 / ratio } else { 0.0 }
    );
    if stats.deduped_files > 0 {
        println!(
            "  dedup: {} duplicate files, {} not stored",
            stats.deduped_files,
            human(stats.deduped_bytes)
        );
    }
    println!(
        "  {} blocks, {} level {}, {:.2}s ({}/s)",
        stats.blocks,
        codec.name(),
        level,
        elapsed.as_secs_f64(),
        human((stats.input_bytes as f64 / elapsed.as_secs_f64().max(0.001)) as u64)
    );
    Ok(())
}

fn cmd_extract(a: ExtractArgs, quiet: bool) -> Result<()> {
    let mut archive = Archive::open(&a.archive)?;
    let opts = ExtractOptions {
        restore_mode: !a.no_metadata,
        restore_mtime: !a.no_metadata,
        verify: !a.no_verify,
        overwrite: !a.no_overwrite,
    };
    let filter = if a.paths.is_empty() {
        None
    } else {
        Some(a.paths.as_slice())
    };

    let start = std::time::Instant::now();
    let show = !quiet && std::io::stderr().is_terminal();
    let stats = archive.extract(&a.dir, filter, &opts, |path| {
        if show {
            eprint!("\r\x1b[K{path}");
            let _ = std::io::stderr().flush();
        }
    })?;
    if show {
        eprint!("\r\x1b[K");
    }
    let elapsed = start.elapsed();
    println!(
        "extracted {} files ({}) to {} in {:.2}s",
        stats.files,
        human(stats.bytes),
        a.dir.display(),
        elapsed.as_secs_f64()
    );
    Ok(())
}

fn cmd_list(a: ListArgs) -> Result<()> {
    let archive = Archive::open(&a.archive)?;
    let mut out = std::io::BufWriter::new(std::io::stdout().lock());
    for e in archive.entries() {
        if a.long {
            let kind = match e.kind {
                Kind::Dir => "d",
                Kind::File => "-",
                Kind::Symlink => "l",
            };
            let block = e
                .segments
                .first()
                .map(|s| s.block.to_string())
                .unwrap_or_else(|| "-".into());
            writeln!(
                out,
                "{kind}{:o} {:>10} blk:{:<5} {}{}",
                e.mode & 0o777,
                human(e.size),
                block,
                e.path,
                match &e.link_target {
                    Some(t) => format!(" -> {t}"),
                    None => String::new(),
                }
            )?;
        } else {
            writeln!(out, "{}", e.path)?;
        }
    }
    Ok(())
}

fn cmd_info(a: InfoArgs) -> Result<()> {
    let archive = Archive::open(&a.archive)?;
    let file_len = std::fs::metadata(&a.archive)?.len();
    let idx = &archive.index;

    let files = idx.entries.iter().filter(|e| e.kind == Kind::File).count();
    let dirs = idx.entries.iter().filter(|e| e.kind == Kind::Dir).count();
    let links = idx
        .entries
        .iter()
        .filter(|e| e.kind == Kind::Symlink)
        .count();
    let logical = archive.total_size();

    // Bytes actually held in blocks, i.e. after dedup.
    let stored: u64 = idx.blocks.iter().map(|b| b.rlen).sum();
    let block_bytes: u64 = idx.blocks.iter().map(|b| b.clen).sum();

    println!("archive:    {}", a.archive.display());
    println!("created by: {}", idx.created_by);
    println!("size:       {}", human(file_len));
    println!(
        "contents:   {files} files, {dirs} dirs, {links} symlinks, {} logical",
        human(logical)
    );
    if logical > stored {
        println!(
            "dedup:      {} saved before compression ({:.1}%)",
            human(logical - stored),
            (logical - stored) as f64 / logical as f64 * 100.0
        );
    }
    println!(
        "blocks:     {} holding {} -> {} ({:.1}%)",
        idx.blocks.len(),
        human(stored),
        human(block_bytes),
        if stored > 0 {
            block_bytes as f64 / stored as f64 * 100.0
        } else {
            0.0
        }
    );
    println!(
        "index:      {} -> {} at offset {}",
        human(archive.footer.index_rlen),
        human(archive.footer.index_clen),
        archive.footer.index_offset
    );

    // Per-codec breakdown makes the store lane's effect visible.
    let mut by_codec: std::collections::BTreeMap<&str, (u64, u64, u64)> = Default::default();
    for b in &idx.blocks {
        let name = Codec::from_u8(b.codec)?.name();
        let e = by_codec.entry(name).or_default();
        e.0 += 1;
        e.1 += b.rlen;
        e.2 += b.clen;
    }
    println!("lanes:");
    for (name, (count, rlen, clen)) in by_codec {
        println!(
            "  {name:<6} {count:>4} blocks  {:>10} -> {:>10}  ({:.1}%)",
            human(rlen),
            human(clen),
            if rlen > 0 {
                clen as f64 / rlen as f64 * 100.0
            } else {
                0.0
            }
        );
    }
    Ok(())
}

fn cmd_verify(a: InfoArgs, quiet: bool) -> Result<()> {
    let mut archive = Archive::open(&a.archive)?;
    let show = !quiet && std::io::stderr().is_terminal();
    let n = archive.verify(|p| {
        if show {
            eprint!("\r\x1b[K{p}");
            let _ = std::io::stderr().flush();
        }
    })?;
    if show {
        eprint!("\r\x1b[K");
    }
    println!("ok: {n} files verified against their stored hashes");
    Ok(())
}

fn cmd_cat(a: CatArgs) -> Result<()> {
    let mut archive = Archive::open(&a.archive)?;
    let entry = archive
        .entries()
        .iter()
        .find(|e| e.path == a.path && e.kind == Kind::File)
        .cloned()
        .with_context(|| format!("no file '{}' in the archive", a.path))?;
    let mut out = std::io::stdout().lock();
    archive.write_file_to(&entry, &mut out)?;
    out.flush()?;
    Ok(())
}

fn human(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{bytes} B")
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}
