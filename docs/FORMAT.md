# The EMBR container format, version 1

This document specifies the on-disk layout of a `.embr` archive. It is the
normative reference; the implementation in `crates/embr-format` follows it.

Status: **v1, unstable.** The format may change without a compatibility path
until it is declared frozen. Archives record their version, and a reader
refuses anything newer than it understands.

## Design goals, in priority order

1. **Correctness.** A round-trip is byte-identical, or it fails loudly. Every
   file carries a BLAKE3 hash that is checked on extraction by default.
2. **Smaller than ZIP on real folders**, without a novel compression
   algorithm. The wins come from the container: solid blocks, large windows,
   sorted layout, deduplication.
3. **Seekable.** Extracting one file must not require decompressing the whole
   archive. This rules out plain `tar.gz`/`tar.zst`, which are the usual
   answer to "just use a better codec".
4. **Streaming-friendly to write.** Blocks are emitted as they are produced;
   the writer never needs the whole archive in memory or a second pass over
   the data it has already written.

## Layout

```
 offset 0
+---------------------------+
| Header            16 B    |  magic, version
+---------------------------+
| Block 0                   |  solid, independently decodable
| Block 1                   |
| ...                       |
| Block N-1                 |
+---------------------------+
| Index          (variable) |  zstd-compressed; paths, metadata, block map
+---------------------------+
| Footer            64 B    |  where the index is, and its hash
+---------------------------+
 end of file
```

The index is at the tail because block sizes are not known until their data has
been compressed. The footer is last and fixed-size so a reader finds the index
in two seeks and never scans the archive.

All integers are little-endian.

### Header (16 bytes, offset 0)

| Offset | Size | Field     | Value                        |
|--------|------|-----------|------------------------------|
| 0      | 4    | `magic`   | ASCII `EMBR`                 |
| 4      | 2    | `version` | `1`                          |
| 6      | 10   | reserved  | zero                         |

The magic sits at offset 0 so `file(1)` and macOS Uniform Type Identifiers can
sniff an archive by content, not just by extension.

### Footer (64 bytes, at `EOF - 64`)

| Offset | Size | Field          | Meaning                                     |
|--------|------|----------------|---------------------------------------------|
| 0      | 8    | `index_offset` | Absolute offset of the index                |
| 8      | 8    | `index_clen`   | Index length as stored (compressed)         |
| 16     | 8    | `index_rlen`   | Index length before compression             |
| 24     | 32   | `index_hash`   | BLAKE3 of the *uncompressed* index          |
| 56     | 2    | `version`      | `1`                                         |
| 58     | 2    | reserved       | zero                                        |
| 60     | 4    | `magic`        | ASCII `EMBR`                                |

Trailing magic makes truncation detectable immediately: a cut-off archive fails
here rather than producing a confusing error deep inside a decoder.

### Blocks

A block is a single complete compressed stream — a whole zstd or xz frame, or
raw bytes for `Store`. Blocks are **independently decodable**: no block depends
on the state of another. That is what makes single-file extraction cheap, and
it is the property a future recovery-record feature would need.

Files are packed *into* blocks. A block typically holds many small files, which
is the "solid" part: they share one compression dictionary instead of each
paying to establish its own.

The default block size is 64 MiB. It is a direct trade:

- larger blocks compress better (more shared context)
- smaller blocks make single-file extraction cheaper (less to decode to reach
  one file)

A file at or above the block size is streamed into a block of its own, so the
writer's peak memory stays near the block size regardless of input size.

### Index

The index is a [bincode](https://github.com/bincode-org/bincode)-serialized
`Index` struct, compressed with zstd level 19, and covered by the BLAKE3 in the
footer. Path lists are extremely compressible, so this is cheap; on a 2,500-file
source tree the index is ~343 KB raw and ~131 KB stored.

```rust
struct Index {
    blocks:     Vec<BlockInfo>,
    entries:    Vec<Entry>,
    created_by: String,        // provenance, not interpreted on read
}

struct BlockInfo {
    offset: u64,   // absolute offset in the archive
    clen:   u64,   // bytes on disk
    rlen:   u64,   // bytes after decoding
    codec:  u8,    // 0 = Store, 1 = Zstd, 2 = Xz
}

struct Entry {
    path:        String,          // '/'-separated, relative, no '..'
    kind:        Kind,            // File | Dir | Symlink
    mode:        u32,             // Unix mode; only permission bits applied
    mtime:       i64,             // seconds since the Unix epoch
    mtime_nanos: u32,
    size:        u64,
    hash:        Option<[u8; 32]>, // BLAKE3; None for dirs and symlinks
    link_target: Option<String>,
    segments:    Vec<Segment>,
}

struct Segment {
    block:  u32,   // index into Index::blocks
    offset: u64,   // offset within the *decoded* block
    len:    u64,
}
```

A file's bytes are the concatenation of its segments, in order. Zero segments
means an empty file.

The v1 writer emits **at most one segment per file**: small files are buffered
into a shared block, and a file at or above the block size gets a block to
itself. The list form is in the format anyway because content-defined chunking
(see non-goals) will split files across blocks, and readers that handle the
general case today will not need changing then. A reader must therefore accept
any number of segments.

Because segments are just references, **deduplication is free**: two identical
files simply carry the same segment list. There is no separate dedup table.

Codec IDs are permanent. A value once assigned is never reused for a different
codec, so an old reader either decodes a block correctly or refuses it by name.

## Packing pipeline

Creation runs in four phases:

1. **Walk.** Collect directories, symlinks and file candidates. Directories and
   symlinks go straight into the index; they have no data.
2. **Hash.** BLAKE3 every file. This is also where unreadable files surface,
   before anything has been written to the output.
3. **Sort.** Order files by `(lane, extension, directory, filename)`.
4. **Pack.** Fill blocks in that order, deduplicating by hash as we go.

### Why the sort matters

Sorting is free and is worth a real percentage. Grouping by extension puts files
that share a vocabulary next to each other inside the same solid block — all the
`.py` together, all the `.h` together — so the compressor's window is full of
relevant context rather than whatever happened to be adjacent in the directory
tree.

### Lanes

Files are split into two lanes, and lanes are packed into separate blocks.

- **Compress** — the archive's chosen codec and level.
- **Fast** — already-compressed payloads (JPEG, H.264, ZIP containers, …),
  compressed with zstd at level 1 regardless of the archive's codec.

The fast lane exists because measurement says high-effort compression of
already-compressed data is waste, but *not* that it should be skipped entirely.
On a real folder of 80 photos:

| Treatment                     | Size    | Time  |
|-------------------------------|---------|-------|
| Stored, no compression        | 100.0%  | 0.49s |
| Solid blocks, zstd level 1    |  99.3%  | 0.55s |
| Solid blocks, zstd level 12   |  99.1%  | 1.12s |

The ~0.9% is cross-file redundancy — a camera roll shares EXIF blocks, colour
profiles and embedded thumbnails — and it is only visible because the photos are
in one solid block. Level 1 captures nearly all of it at a cost that disappears
into I/O. Level 12 costs double the time for a further 0.2%, and xz would cost
far more for nothing, which is why the fast lane pins its own codec instead of
honouring `--codec`.

Lane assignment is by file extension. It is a heuristic: a `.zip` holding stored
entries really is compressible and would be better served by the full-effort
lane. `--fast-lane false` turns the split off.

## Path safety

Archive paths are validated when written **and again when read**, because an
archive is untrusted input regardless of where it came from. Rejected:

- absolute paths (`/etc/passwd`)
- any `..` component
- Windows drive prefixes (`C:/…`)
- empty or `.` components, and backslashes

After joining onto the destination, the result is re-walked component by
component and must never rise above the destination. The destination itself is
canonicalized first, so a symlinked destination cannot widen the check.

## Integrity

- Every file entry carries a BLAKE3 hash of its contents.
- The index carries a BLAKE3 of itself in the footer.
- `embr extract` verifies each file as it writes it, by default.
- `embr verify` decodes every block and re-hashes every file without writing.

BLAKE3 is used rather than a CRC because it does double duty as the dedup key,
and because it is fast enough that verification is not a reason to turn
verification off.

## Deliberate non-goals in v1

These are absent because they are not yet implemented, not because they are
unwanted. Each is a planned extension the layout already accommodates.

- **Encryption.** When added, filenames must be encrypted too — ZIP's habit of
  leaking every path in an "encrypted" archive is a real flaw worth not
  repeating.
- **Recovery records.** Independently decodable blocks make Reed–Solomon parity
  straightforward to append.
- **Content-defined chunking.** Whole-file dedup catches exact duplicates;
  CDC would catch near-duplicates and enable incremental re-archiving.
- **Executable filters (BCJ).** Worth 10–20% on binaries.
- **Lossless media recompression.** The largest remaining win on real user
  data, and the reason lanes exist as a concept rather than a boolean.
- **Parallel block compression.** Compression currently uses zstd's internal
  worker threads; compressing independent blocks concurrently would scale
  further, and the format already allows it since blocks are independent.
