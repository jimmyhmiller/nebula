#!/usr/bin/env python3
"""Reproduce layout throughput comparison with identical graphs and f32 seeds."""
import argparse
import csv
import hashlib
import json
import math
import os
from pathlib import Path
import random
import struct
import subprocess
import time

ROOT = Path(__file__).resolve().parents[1]


def fixture(path, n):
    """Connected, simple undirected graph: ring plus 90% within-block edges."""
    if n < 100 or n % 8:
        raise ValueError("sizes must be multiples of eight and at least 100")
    rng = random.Random(42)
    block = n // 8
    edges = {(i, i + 1) for i in range(n - 1)} | {(0, n - 1)}
    while len(edges) < n * 6:
        a = rng.randrange(n)
        b = (a // block) * block + rng.randrange(block) if rng.random() < 0.9 else rng.randrange(n)
        if a != b:
            edges.add((min(a, b), max(a, b)))
    with path.open("wb") as out:
        out.write(struct.pack("<QIQ", 0x3148434E45424E, n, len(edges)))
        # Explicit f32 positions: both engines receive exactly these bits.
        extent = 30 * math.sqrt(n)
        for _ in range(n):
            out.write(struct.pack("<ff", rng.uniform(-extent, extent), rng.uniform(-extent, extent)))
        for a, b in sorted(edges):
            out.write(struct.pack("<II", a, b))
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--sizes", type=int, nargs="+", default=[1000, 10000, 100000])
    parser.add_argument("--warmup", type=int, default=10)
    parser.add_argument("--steps", type=int, default=30)
    parser.add_argument("--repeats", type=int, default=3)
    parser.add_argument("--threads", type=int, default=max(1, (os.cpu_count() or 2) - 1))
    parser.add_argument("--output", type=Path, default=ROOT / "benchmarks/results.csv")
    args = parser.parse_args()
    if args.warmup < 0 or min(args.steps, args.repeats, args.threads) < 1:
        parser.error("steps, repeats and threads must be positive; warmup must be nonnegative")
    scratch = ROOT / "target/gephi-comparison"
    scratch.mkdir(parents=True, exist_ok=True)
    java_project = ROOT / "benchmarks/gephi"
    cp = str(java_project / "target/classes") + os.pathsep + (java_project / "target/classpath.txt").read_text().strip()
    fields = ["engine", "nodes", "edges", "repeat", "warmup", "steps", "threads", "load_ms", "step_ms", "wall_seconds", "fixture_sha256", "adapter", "backend"]
    args.output.parent.mkdir(parents=True, exist_ok=True)
    with args.output.open("w", newline="") as output:
        writer = csv.DictWriter(output, fieldnames=fields)
        writer.writeheader()
        output.flush()
        for n in args.sizes:
            path = scratch / f"blocks-{n}.bin"
            sha = fixture(path, n)
            commands = {
                "nebula": [str(ROOT / "target/release/examples/gephi_compare"), str(path), str(args.warmup), str(args.steps)],
                "gephi": ["java", "-Djava.awt.headless=true", "-Xmx16g", "-cp", cp, "GephiBench", str(path), str(args.warmup), str(args.steps), str(args.threads)],
            }
            for repeat in range(args.repeats):
                # Alternate order; never benchmark both engines concurrently.
                for engine in (["nebula", "gephi"] if repeat % 2 == 0 else ["gephi", "nebula"]):
                    print(f"{n:,} nodes · {engine} · repeat {repeat + 1}", flush=True)
                    start = time.monotonic()
                    result = subprocess.run(commands[engine], cwd=ROOT, capture_output=True, text=True, timeout=600)
                    wall = time.monotonic() - start
                    (scratch / f"{n}-{engine}-{repeat}.log").write_text(result.stdout + result.stderr)
                    result.check_returncode()
                    measurements = [json.loads(line) for line in result.stdout.splitlines() if line.startswith('{"')]
                    if len(measurements) != 1:
                        raise RuntimeError(f"Expected one measurement; see {scratch}")
                    row = measurements[0]
                    if row["nodes"] != n or row["edges"] != n * 6 or not math.isfinite(row["step_ms"]) or row["step_ms"] <= 0:
                        raise RuntimeError("Invalid benchmark result")
                    row.update(repeat=repeat + 1, fixture_sha256=sha, wall_seconds=wall)
                    writer.writerow(row)
                    output.flush()
                    print(f"  {row['step_ms']:.3f} ms/step", flush=True)


if __name__ == "__main__":
    main()
