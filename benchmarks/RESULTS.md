# Measured layout throughput

Measured 2026-09-30 on Apple M2 Max / Metal, 64 GiB unified memory, 12 CPU cores; macOS 26.5.2 (25F84). Nebula release build with rustc 1.96.0; Gephi Toolkit 0.11.3 / OpenJDK 21.0.1, 11 ForceAtlas2 threads, 16 GiB heap. Fifty warmup steps and 100 timed steps per process, three runs per engine/size.

| Nodes | Edges | Nebula ms/step, median (range) | Gephi ms/step, median (range) | Median ratio |
|---:|---:|---:|---:|---:|
| 1,000 | 6,000 | 0.751 (0.537–1.085) | 2.034 (1.959–2.041) | 2.7× |
| 10,000 | 60,000 | 1.181 (0.700–1.962) | 29.703 (27.859–29.925) | 25.2× |
| 100,000 | 600,000 | 1.496 (1.431–2.534) | 213.807 (209.172–232.302) | 142.9× |

This is **iteration throughput for two different force algorithms**, not matched numerical work or time to equal-quality output. It excludes loading, rendering and UI interaction. It establishes neither an overall application speedup nor a maximum-capacity advantage. Nebula’s scheduling variation is substantial; ranges are included rather than hiding it. These are synthetic clustered graphs, not a representative corpus of user data.

Both engines receive the exact same graph and f32 initial coordinates, confirmed by fixture hashes and node/edge counts. Both validate finite final positions. GPU completion is synchronized around timing. No concurrent benchmark engines. The 1,000-node measurements were rerun after build/test activity finished; the final CSV contains those replacement runs. No results selected by timing.

See [method and reproduction commands](README.md) and [all raw repetitions](results.csv). Gephi provides its layout engine through the [official Toolkit](https://gephi.org/toolkit/).

Nebula source baseline: `0636a89cf76b12daaa9a082c0dd1b084870279c0` plus the uncommitted application work in this workspace; no clean release tag existed for this measurement. Graph and benchmark sources are included for reproduction.
