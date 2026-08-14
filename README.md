<img src="assets/logo.png" alt="EMBR" width="128" align="right">

# EMBR

A more effective `.zip`.

EMBR is an archive format and command-line tool. On real folders it produces
archives **19–60% smaller than `zip -9`** while being **~2× faster to create and
~2× faster to extract**, and it can still pull a single file out without
decompressing everything before it.

It does this without inventing a compression algorithm. Every win comes from the
container.

**[embr.proxnode.xyz](https://embr.blaise.wtf)** · [Format spec](docs/FORMAT.md) · [Benchmarks](bench/bench.py)

## Why ZIP loses

ZIP was designed in 1989 and its limits are structural, not fixable by turning
the dial up:

1. **No solid compression.** ZIP compresses every file *independently*. A folder
   of 2,500 Python files makes it re-learn `import`, `self` and `def __init__`
   2,500 times. This is the single biggest loss on real folders.
2. **A 32 KB window.** DEFLATE can only look back 32 KB. Two near-identical
   files 40 MB apart in an archive are invisible to it.
3. **DEFLATE is from 1991.** Entropy coding has genuinely improved since. This
   is the *smallest* of the four.
4. **No deduplication.** Copy a folder into your folder and zip it, and it is
   twice as big.

EMBR addresses each one directly:

| ZIP                            | EMBR                                              |
|--------------------------------|---------------------------------------------------|
| Each file compressed alone     | Many files packed into one **solid block**        |
| 32 KB match window             | Up to **128 MB** window                           |
| Directory order                | **Sorted by type**, so similar files sit together |
| Duplicates stored twice        | **Whole-file dedup** via BLAKE3                   |
| Media compressed pointlessly   | **Fast lane** — cheap level where effort is waste |
| CRC-32                         | **BLAKE3**, verified on extract by default        |

## Measured results

Benchmarked on an Apple Silicon Mac against real data, using
[`bench/bench.py`](bench/bench.py). Percentages are size reduction versus
`zip -9`; "extract" is relative extraction speed.

**Python 3.14 standard library — 54.5 MB, 2,498 files**

| Tool                  | Size    | vs zip | Compress | Extract |
|-----------------------|---------|--------|----------|---------|
| `zip -9`              | 14.2 MB | —      | 4.82s    | 1.0×    |
| `embr --preset fast`  | 13.2 MB | −7.1%  | 0.60s    | 1.9×    |
| `embr --preset balanced` | 11.5 MB | **−18.8%** | **2.02s** | **1.8×** |
| `embr --preset max`   | 10.1 MB | **−29.0%** | 21.71s | 0.9×    |
| `tar+xz -9e`          | 9.9 MB  | −30.0% | 30.05s   | 1.3×    |

**Application binaries — 122.6 MB, 8 files** (`git`, `node`, `ffmpeg`, …)

| Tool                     | Size    | vs zip     | Compress | Extract |
|--------------------------|---------|------------|----------|---------|
| `zip -9`                 | 40.9 MB | —          | 15.76s   | 1.0×    |
| `embr --preset balanced` | 33.0 MB | **−19.4%** | **3.94s** | **6.6×** |
| `embr --preset max`      | 26.1 MB | **−36.3%** | 49.09s   | 0.6×    |
| `tar+xz -9e`             | 25.9 MB | −36.7%     | 62.17s   | 0.7×    |

**A folder containing a duplicated subtree — 109 MB, 5,002 files**

| Tool                     | Size    | vs zip     | Compress |
|--------------------------|---------|------------|----------|
| `zip -9`                 | 28.3 MB | —          | 10.23s   |
| `embr --preset balanced` | 11.5 MB | **−59.4%** | 2.42s    |
| `embr --preset max`      | 10.1 MB | **−64.5%** | 23.10s   |

That row is not contrived. It is "I copied the project before editing it",
"`node_modules` in two places", "the photos got exported twice".

### Where EMBR does *not* help

Honesty matters more than a good table. On data that is **already compressed** —
JPEG, PNG, H.264, MP3 — no general-purpose archiver recovers anything, EMBR
included. A 1.7 GB folder of MP4s stays 1.7 GB in every format on the market.

**197 MB of photos, 80 JPEGs**

| Tool                     | Size     | vs zip | Compress    | Extract |
|--------------------------|----------|--------|-------------|---------|
| `zip -9`                 | 195.5 MB | —      | 6.62s       | 1.0×    |
| `embr --preset balanced` | 195.8 MB | −0.1%  | **0.61s**   | 3.5×    |
| `embr --preset max`      | 195.8 MB | −0.1%  | **0.62s**   | **9.9×** |
| `tar+zstd -19 --long`    | 195.1 MB | +0.2%  | 32.05s      | 6.2×    |
| `tar+xz -9e`             | 195.2 MB | +0.2%  | **134.61s** | 0.9×    |

Everything lands within 0.4% of everything else. What separates them is the CPU
they burn to get there: `xz -9e` spent **134 seconds** to end up 0.2% *larger*
than EMBR. Note that `--preset max` costs 0.62s here rather than minutes — the
fast lane pins its own codec, so asking for maximum compression on a photo
folder does not buy you two minutes of pointless LZMA.

EMBR recovers ~0.9% from a camera roll (shared EXIF blocks and embedded
thumbnails, visible only because the photos share a solid block) and otherwise
declines to waste your time.

**Installers and archives are the same story.** A `.pkg`, `.dmg`, `.zip` or
`.whl` has already had its contents compressed, and re-compressing the result
recovers 1–2% at best:

| Corpus | Input | EMBR | Saved |
|---|---|---|---|
| 6 `.dmg` installers | 130.2 MB | 128.3 MB | 1.5% |
| zips, jars and wheels | 31.3 MB | 25.6 MB | 18.3% |
| Two `.pkg` builds of the same app, one file apart | 78.5 MB | 77.7 MB | 1.1% |

That last row is the one worth understanding. The two packages differ by a
single line of text, yet almost nothing can be saved — because `pkgbuild`
gzipped the payload first, and compression destroys the similarity that
deduplication would otherwise have found. The *same content before packaging*
compresses 7.6×, with dedup removing 118 MB of it.

The lesson generalises: **compress last.** Once something has been through a
compressor, its redundancy is gone for good, and no archiver gets it back.

Closing that gap needs *format-aware, lossless recompression* — transcoding a
JPEG into a smaller bit-exact representation. That is the project's flagship
goal and it is not built yet. See [Roadmap](#roadmap).

## Install

Requires [Rust](https://rustup.rs). If you do not have it:

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

Then build and install `embr` so it runs from anywhere:

```sh
git clone https://github.com/EmberGuild-Labs/embr.git
cd embr
cargo install --path crates/embr-cli
```

That puts the binary in `~/.cargo/bin`. If `embr` is not found afterwards, that
directory is not on your `PATH` — add it once:

```sh
echo 'export PATH="$HOME/.cargo/bin:$PATH"' >> ~/.zshrc   # or ~/.bashrc
exec $SHELL
```

Check it worked:

```sh
embr --version     # embr 0.1.0
```

To rebuild after changing the source, re-run `cargo install --path
crates/embr-cli --force`. For development you can skip installing and use
`cargo build --release && ./target/release/embr` directly.

### macOS: Finder integration

```sh
./macos/build-app.sh
```

This builds `EMBR.app` into `/Applications` and installs a Finder Quick Action.
Together they give you:

- `.embr` registered as a real file type (`xyz.embr.archive`), so Finder knows
  what an archive is and shows it with the flame icon
- **double-clicking a `.embr` extracts it** into a folder beside it, the way
  Archive Utility handles a `.zip`
- **right-click → Quick Actions → Compress with EMBR** on any file or folder

The Quick Action mirrors Finder's own Compress: one selected item becomes
`<name>.embr` beside it, several become `Archive.embr`. It never overwrites —
a second run produces `name 2.embr`. Install it on its own with:

```sh
python3 macos/install-quick-action.py
```

Finder's built-in **Compress** item is not extensible; third parties cannot add
entries beside it. Quick Actions are the supported route, which is why the
EMBR entry lives one level down under that submenu rather than next to
Compress itself. If it does not appear, enable it in **System Settings →
General → Login Items & Extensions → Finder**.

The app is not a self-extracting executable and never will be. Those fail on a
recipient's machine under Gatekeeper and notarization, which is precisely when
an archive most needs to work.

Verified working on macOS 26. If the icon does not appear immediately, Finder's
icon cache is the usual culprit — the installer flushes it, but a logout will
force the issue.

A note for anyone debugging this: `NSWorkspace.icon(forFile:)` called from a
headless script reports the generic document icon even when Finder is drawing
the real one correctly. It is not a reliable way to check. Look at Finder.

## Usage

### Create

```sh
embr create photos.embr ~/Pictures/2024
embr create src.embr ./project --preset max
embr create big.embr ./data --codec zstd -l 19 --block-size 256
```

Presets:

| Preset     | Codec     | Use when                                        |
|------------|-----------|-------------------------------------------------|
| `fast`     | zstd 3    | Speed matters; still beats zip on size          |
| `balanced` | zstd 12   | **Default.** Best size-per-second by a wide margin |
| `max`      | xz 9      | Smallest possible; roughly 10× slower           |

### Extract

```sh
embr extract photos.embr                    # into the current directory
embr extract photos.embr -C /tmp/out        # into a chosen directory
embr extract src.embr project/src           # just one subtree
```

Extraction verifies every file's BLAKE3 as it writes. `--no-verify` skips it.

### Inspect

```sh
embr list src.embr              # paths, one per line
embr list src.embr --long       # mode, size, block placement
embr info src.embr              # structure, ratios, per-lane breakdown
embr verify src.embr            # decode everything, check every hash
embr cat src.embr project/README.md
```

`embr info` shows where the bytes went:

```
archive:    src.embr
contents:   2498 files, 203 dirs, 3 symlinks, 54.5 MB logical
blocks:     2 holding 54.5 MB -> 11.4 MB (20.9%)
index:      343.3 KB -> 131.0 KB at offset 11923662
lanes:
  zstd      2 blocks     54.5 MB ->    11.4 MB  (20.9%)
```

### Tuning

| Flag                  | Default | Effect                                              |
|-----------------------|---------|-----------------------------------------------------|
| `--block-size <MiB>`  | 64      | Bigger compresses better; smaller extracts one file cheaper |
| `--dedup <bool>`      | true    | Store byte-identical files once                     |
| `--fast-lane <bool>`  | true    | Cheap level for already-compressed types            |
| `--follow-symlinks`   | off     | Archive link targets instead of the links           |

## Benchmarking

The harness is how design decisions get settled here. Point it at any
directories:

```sh
cargo build --release
./bench/bench.py ~/Pictures/2024 ./some-source-tree --json results.json
```

It runs EMBR at all three presets against `zip -9`, `tar+zstd -19 --long`,
`tar+xz -9e` and `7z -mx=9` — whichever are installed — and reports size,
compression time and extraction time, then restates everything relative to zip.

## Project layout

```
crates/embr-format/     the container: writer, reader, index, codecs
  src/index.rs          on-disk structures, path safety
  src/writer.rs         walk -> hash -> sort -> pack
  src/reader.rs         seekable extraction, verification
  src/classify.rs       lane assignment and sort order
  src/codec.rs          zstd / xz / store
crates/embr-cli/        the `embr` binary
macos/build-app.sh      builds EMBR.app: file type, icon, double-click
macos/*.applescript     the droplet that handles double-clicks
macos/install-quick-action.py   right-click -> Compress with EMBR
assets/                 logo, and the .iconset macOS needs
  logo-candidates/      the four designs, and the script that draws them
bench/bench.py          comparison harness
docs/FORMAT.md          format specification
```

### Logo

The mark is pixel art drawn on a 16x16 grid — the same grid a favicon and a
Finder icon use, so every exported size is a whole-number multiple with no
resampling anywhere. It is defined as plain text in
`assets/logo-candidates/make_logos.py`; edit a row, re-run the script, and
every PNG, SVG and icon size regenerates.

```sh
python3 assets/logo-candidates/make_logos.py
```

Four candidates were drawn and the "ember" design chosen. Change `CHOSEN` in
that file to swap it.

## How it works

Full details in [docs/FORMAT.md](docs/FORMAT.md). The short version:

```
+---------------------------+
| Header            16 B    |  magic "EMBR", version
+---------------------------+
| Block 0                   |  solid, independently decodable
| Block 1                   |
| ...                       |
+---------------------------+
| Index                     |  zstd-compressed paths, metadata, block map
+---------------------------+
| Footer            64 B    |  where the index is, and its BLAKE3
+---------------------------+
```

Creating an archive runs four phases — **walk**, **hash**, **sort**, **pack**.
Hashing before packing is what makes dedup free. Sorting before packing is what
makes the solid blocks work: files are ordered by `(lane, extension, directory,
name)`, so a block is full of files that share a vocabulary.

The index lives at the tail because block sizes are not known until their data
is compressed, and the footer is fixed-size and last so a reader finds the index
in two seeks. Each block records its own offset, which is what keeps
single-file extraction cheap — pulling 6 files out of a 2,498-file archive takes
0.06s.

## Decisions made along the way

**No new compression algorithm.** Beating zstd or LZMA at entropy coding is a
research career with single-digit-percent payoffs. Every meaningful win here
came from the container and the preprocessing pipeline, which is ordinary
engineering. The codecs are commodities we link against.

**The fast lane compresses instead of storing.** The first design routed media
straight through uncompressed. Measurement killed it: solid blocks at zstd
level 1 were 0.9% smaller than storing and, on 197 MB of JPEG, only 0.06s
slower — I/O dominates. Storing was giving away a real gain for nothing. The
lane kept its name and changed its behaviour.

**Blocks are independently decodable.** This costs a little ratio versus one
giant stream, and buys single-file extraction, parallel decompression, and a
place for recovery records to attach later.

**BLAKE3 over CRC-32.** It doubles as the dedup key, so integrity checking is
nearly free rather than a separate cost, and it is fast enough that verifying on
every extract is a sane default.

**Paths are validated twice.** On write and again on read. An archive is
untrusted input no matter where it came from; ZIP-slip is a bug class, not a
one-off.

**Rust.** Best-in-class zstd/xz/BLAKE3 bindings, one static binary, and it
compiles to WebAssembly — which the planned browser-based opener needs.

## Roadmap

Ordered by payoff ÷ effort. The measurements above are what rank these.

**Next**

- [ ] **WASM opener.** A `.embr` you email someone is a brick unless they can
      open it. Same Rust code via `wasm-pack`, a static page that extracts
      locally in the browser. Treated as required, not optional — a format
      nobody can open dies.
- [x] **macOS app.** `.embr` is a registered file type and double-clicking one
      extracts it. See `macos/build-app.sh`.
- [x] **Finder icon.** `.embr` files show the flame, confirmed on macOS 26.
- [x] **Right-click to compress.** A Quick Action, since Finder's own Compress
      menu item cannot be extended by third parties.
- [ ] **QuickLook extension**, so the space bar previews an archive's contents
      without extracting it. This is the part that would genuinely beat `.zip`
      on feel, not just on size.
- [ ] **Parallel block compression.** Blocks are already independent.

**Then**

- [ ] **Lossless JPEG recompression** — the flagship. ~20% off a photo library,
      bit-exact, and nothing mainstream does it. Turns the one row where EMBR
      ties everyone into the row where it wins outright.
- [ ] **Content-defined chunking (FastCDC).** Catches near-duplicates, not just
      exact ones, and unlocks incremental re-archiving.
- [ ] **BCJ / branch filters.** 10–20% on executables.
- [ ] **Encryption**, with filenames encrypted too — ZIP leaks every path even
      in an "encrypted" archive.
- [ ] **Recovery records.** Reed–Solomon parity; an underrated differentiator
      that RAR users swear by.
- [ ] **Trained dictionaries** for archives of many tiny similar files.

## Known limitations

- **Unix only.** Modes, symlinks and mtimes use Unix APIs. Windows needs work.
- **Files are read twice** — once to hash, once to pack. The OS cache makes this
  cheap in practice, but it is not free on cold storage.
- **No incremental update.** Adding a file rewrites the archive.
- **xz is single-threaded here**, so `--preset max` is roughly 10× slower than
  `balanced` for about 12% more compression.
- **The lane heuristic is extension-based**, so it can be fooled by a file whose
  name does not match its contents. `--fast-lane false` opts out.
- **v1 is unstable.** The format may change without a compatibility path until
  it is declared frozen.

## License

MIT
