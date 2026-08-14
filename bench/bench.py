#!/usr/bin/env python3
"""Compare EMBR against the archivers it needs to beat.

Every design decision in EMBR is supposed to be justified by a measurement, and
this is where the measurements come from. Point it at one or more directories:

    ./bench/bench.py ~/Pictures/2024 ./some-source-tree

It reports compressed size, compression time and *decompression* time for each
tool, because a format that wins on size and loses badly on extraction speed is
not actually better.

Only tools present on the machine are run; missing ones are skipped with a note
rather than failing the whole comparison.
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass, asdict
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
EMBR = REPO / "target" / "release" / "embr"


@dataclass
class Result:
    corpus: str
    tool: str
    raw_bytes: int
    packed_bytes: int
    compress_s: float
    decompress_s: float

    @property
    def ratio(self) -> float:
        return self.packed_bytes / self.raw_bytes if self.raw_bytes else 0.0


def raw_size(path: Path) -> int:
    """Sum of file sizes, not disk usage — disk usage inflates small files by
    the block size and would make every tool look better than it is."""
    total = 0
    for root, _dirs, files in os.walk(path):
        for f in files:
            fp = Path(root) / f
            if fp.is_symlink():
                continue
            try:
                total += fp.stat().st_size
            except OSError:
                pass
    return total


def have(tool: str) -> bool:
    return shutil.which(tool) is not None


def run(cmd: list[str] | str, cwd: Path | None = None) -> float:
    """Run a command, returning wall-clock seconds. Raises on failure."""
    shell = isinstance(cmd, str)
    start = time.perf_counter()
    proc = subprocess.run(
        cmd,
        shell=shell,
        cwd=cwd,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.PIPE,
    )
    elapsed = time.perf_counter() - start
    if proc.returncode != 0:
        raise RuntimeError(
            f"command failed ({proc.returncode}): {cmd}\n"
            f"{proc.stderr.decode(errors='replace')[:2000]}"
        )
    return elapsed


def drop_caches_hint() -> None:
    """macOS will not let us drop the page cache without sudo, so every tool is
    measured warm. That is consistent across tools, which is what matters for a
    comparison, but absolute decompression numbers here are optimistic."""


# Each entry: (name, needs_tool, compress_fn, decompress_fn, archive_suffix)
def build_tools(threads: int) -> list[tuple]:
    tools: list[tuple] = []

    def embr_preset(preset: str):
        def compress(src: Path, out: Path) -> float:
            return run([str(EMBR), "create", str(out), str(src),
                        "--preset", preset, "-q"])

        def decompress(out: Path, dest: Path) -> float:
            # --no-verify so we measure extraction, not BLAKE3, matching the
            # other tools which do not checksum on the way out.
            return run([str(EMBR), "extract", str(out), "-C", str(dest),
                        "--no-verify", "-q"])

        return compress, decompress

    if EMBR.exists():
        for preset in ("fast", "balanced", "max"):
            c, d = embr_preset(preset)
            tools.append((f"embr --preset {preset}", None, c, d, ".embr"))
    else:
        print(f"note: {EMBR} not built; run `cargo build --release` first",
              file=sys.stderr)

    if have("zip"):
        tools.append((
            "zip -9", "zip",
            lambda s, o: run(["zip", "-9", "-r", "-q", str(o), s.name],
                             cwd=s.parent),
            lambda o, d: run(["unzip", "-o", "-q", str(o), "-d", str(d)]),
            ".zip",
        ))

    if have("tar") and have("zstd"):
        tools.append((
            "tar+zstd -19 --long", "zstd",
            lambda s, o: run(
                f"tar cf - -C {sh(s.parent)} {sh(s.name)} | "
                f"zstd -19 --long=27 -T{threads} -q -o {sh(o)} -f"),
            lambda o, d: run(f"zstd -dc --long=27 {sh(o)} | tar xf - -C {sh(d)}"),
            ".tar.zst",
        ))

    if have("tar") and have("xz"):
        tools.append((
            "tar+xz -9e", "xz",
            lambda s, o: run(
                f"tar cf - -C {sh(s.parent)} {sh(s.name)} | "
                f"xz -9e -T{threads} > {sh(o)}"),
            lambda o, d: run(f"xz -dc {sh(o)} | tar xf - -C {sh(d)}"),
            ".tar.xz",
        ))

    for sevenzip in ("7zz", "7z"):
        if have(sevenzip):
            tools.append((
                f"{sevenzip} -mx=9", sevenzip,
                lambda s, o, b=sevenzip: run([b, "a", "-t7z", "-mx=9", "-bso0",
                                              "-bsp0", str(o), str(s)]),
                lambda o, d, b=sevenzip: run([b, "x", "-bso0", "-bsp0",
                                              f"-o{d}", "-y", str(o)]),
                ".7z",
            ))
            break

    return tools


def sh(p) -> str:
    """Quote a path for the shell pipelines above."""
    return "'" + str(p).replace("'", "'\\''") + "'"


def bench_corpus(corpus: Path, tools: list[tuple], keep: bool) -> list[Result]:
    raw = raw_size(corpus)
    results: list[Result] = []
    print(f"\n=== {corpus.name} — {human(raw)}, "
          f"{sum(len(f) for _, _, f in os.walk(corpus))} files ===")

    with tempfile.TemporaryDirectory(prefix="embr-bench-") as tmp:
        tmpd = Path(tmp)
        for name, _need, compress, decompress, suffix in tools:
            out = tmpd / (corpus.name + suffix)
            dest = tmpd / ("out-" + name.replace(" ", "_").replace("/", "_"))
            dest.mkdir(parents=True, exist_ok=True)
            try:
                ct = compress(corpus, out)
                packed = out.stat().st_size
                dt = decompress(out, dest)
            except Exception as e:  # a missing or broken tool must not stop the run
                print(f"  {name:<24} skipped: {e}".split("\n")[0])
                continue
            finally:
                shutil.rmtree(dest, ignore_errors=True)
                if not keep and out.exists():
                    out.unlink()

            r = Result(corpus.name, name, raw, packed, ct, dt)
            results.append(r)
            print(f"  {name:<24} {human(packed):>10}  "
                  f"{r.ratio * 100:5.1f}%  "
                  f"c {ct:6.2f}s  x {dt:6.2f}s")

    # Restate everything relative to zip, which is the format EMBR has to beat.
    baseline = next((r for r in results if r.tool == "zip -9"), None)
    if baseline:
        print(f"  {'-' * 60}")
        for r in results:
            if r.tool == "zip -9":
                continue
            size_delta = (1 - r.packed_bytes / baseline.packed_bytes) * 100
            speed = baseline.decompress_s / r.decompress_s if r.decompress_s else 0
            print(f"  {r.tool:<24} {size_delta:+6.1f}% vs zip, "
                  f"{speed:.1f}x zip's extract speed")
    return results


def human(n: int) -> str:
    v = float(n)
    for unit in ("B", "KB", "MB", "GB", "TB"):
        if v < 1024 or unit == "TB":
            return f"{v:.1f} {unit}" if unit != "B" else f"{int(v)} B"
        v /= 1024
    return f"{v:.1f} TB"


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("corpora", nargs="+", type=Path,
                    help="directories to benchmark")
    ap.add_argument("--threads", type=int, default=0,
                    help="threads for zstd/xz (0 = all cores)")
    ap.add_argument("--json", type=Path,
                    help="also write raw results here")
    ap.add_argument("--keep", action="store_true",
                    help="keep the archives produced")
    args = ap.parse_args()

    drop_caches_hint()
    tools = build_tools(args.threads)
    if not tools:
        print("no archivers available to benchmark", file=sys.stderr)
        return 1

    all_results: list[Result] = []
    for corpus in args.corpora:
        if not corpus.is_dir():
            print(f"skipping {corpus}: not a directory", file=sys.stderr)
            continue
        all_results += bench_corpus(corpus.resolve(), tools, args.keep)

    if args.json:
        args.json.write_text(json.dumps([asdict(r) for r in all_results], indent=2))
        print(f"\nwrote {args.json}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
