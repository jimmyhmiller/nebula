# Gephi comparison

This harness compares **layout iteration throughput**, not equivalent layout quality or desktop rendering speed. Gephi Toolkit runs the same ForceAtlas2 engine used by Gephi; see [official Toolkit documentation](https://gephi.org/toolkit/) and [version/dependency information](https://github.com/gephi/gephi-toolkit).

Prerequisites: Rust, a supported wgpu GPU/backend, Java 17+ and Maven. Gephi Toolkit 0.11.3 and build plugins are pinned in `gephi/pom.xml`. Java and GPU versions must be recorded alongside results.

```sh
cargo build --release -p nebula-render --example gephi_compare
mvn -q -f benchmarks/gephi/pom.xml compile dependency:build-classpath -Dmdep.outputFile=target/classpath.txt
python3 benchmarks/compare.py --warmup 50 --steps 100
```

The default corpus has 1,000 / 10,000 / 100,000 nodes with six edges per node. Each connected, simple undirected graph contains a ring and eight communities, with 90% of sampled edges inside a community. Seed 42 determines graph and positions. Both engines read the same exact f32 starting coordinates and endpoints. Fixture hashes accompany results; binary fixtures and process logs are saved under `target/gephi-comparison/`.

Each measurement uses a fresh process, 50 untimed steps, and 100 timed steps. Three runs per size/engine alternate order and run sequentially. GPU completion is synchronized before/after timing. Both engines validate finite final coordinates after timing. Report median and range rather than choosing the fastest run.

Gephi uses Barnes–Hut optimization, theta 1.2, default ForceAtlas2 behavior and `CPU count - 1` threads (override `--threads`). Java heap limit is 16 GiB. Nebula uses default GPU force settings, spatial grid and COM pyramid with k=30. Different force laws, approximation methods, integration and trajectories mean that one iteration is **not equivalent numerical work**. This does not measure time to a comparable final layout. Warmup moves each layout along its own trajectory; the timing therefore includes different spatial distributions. Other desktop processes, Java JIT/GC and GPU scheduling can affect results.

`results.csv` records raw steps, repetitions, counts, fixture hashes, and timings. `load_ms` is retained for diagnostics only: Gephi includes framework/project initialization, while Nebula includes graph/CSR construction. `wall_seconds` includes each process's complete lifecycle, including setup and validation, without image output. Neither is an import-to-image benchmark. Rendering, picking, UI latency and maximum capacity need separate tests.

Use `--sizes`, `--warmup`, `--steps`, `--repeats`, `--threads` and `--output` to extend the experiment. Sizes must be multiples of eight and at least 100. Keep machine configuration and workload fixed when comparing results.
