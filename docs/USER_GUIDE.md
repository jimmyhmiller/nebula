# Using Nebula

Start `cargo run --release -p nebula-app` to open the welcome screen. Choose **File → Open**, drop a graph onto the window, or try the built-in sample. You can also pass a file directly:

```sh
cargo run --release -p nebula-app -- examples/service-map.json
```

## Render from the command line

Build once with `cargo build --release -p nebula-app`, then run:

```sh
target/release/nebula graph.json -o graph.png
target/release/nebula graph.graphml --layout hierarchical --labels -o graph.png
cat graph.json | target/release/nebula --file - --color degree -o graph.png
target/release/nebula saved.nebula --frames 1 -o saved-view.png
```

`-o` / `--output` renders a clean PNG and exits automatically after 300 frames. Use `--frames N` to choose a different frame budget; `--screenshot` remains an alias. Force layouts may need a larger budget on large graphs. `--paused` captures the starting arrangement; native projects restore their saved positions/view. Coloring, attribute filters and relationship selection work in batch mode too. Logs go to stderr and failures return a nonzero exit code. Output uses the GPU and the windowing backend, so a supported GPU and desktop session are required even though the render window is hidden.

## Bring in your own data

JSON, CSV/TSV, static GraphML, and static GEXF preserve graph metadata. GraphML/GEXF are the simplest route from existing graph applications such as Gephi. Nested graphs, hyperedges, dynamic GEXF, and GEXF mutual edges are rejected with an explanation rather than flattened silently. Source visualization styling is not applied.

For CSV, choose the delimiter and header setting, then map source, target, optional weight, and relationship type. Quoted delimiters and multiline fields work. Remaining columns become edge attributes. Optionally add a node table and map its ID and label columns; its other columns become node attributes. The node table uses the same delimiter/header setting. It preserves isolated nodes, and every edge endpoint must be present in that table. See [edges.csv](../examples/edges.csv) and [nodes.csv](../examples/nodes.csv).

Input is UTF-8. Import stops on invalid rows with a row-specific error. IDs and labels remain distinct. Self-loops and parallel edges are retained by these importers. Weights must be finite and nonnegative. Review the parsed node/edge counts and import notes before opening the graph. A cancelled or failed import leaves the current document in place.

The legacy DOT, GML, Matrix Market, DIMACS, adjacency-list and edge-list importers remain available. These are topology-oriented parsers with format-specific limitations; their import notes explain that unsupported metadata or styling may be omitted. Use JSON, CSV, GraphML or GEXF when metadata fidelity matters.

For generated data or integrations, use the [JSON format](JSON_FORMAT.md). You can pipe JSON into `nebula --file -`. The existing Turbopack converter is available:

```sh
python3 scripts/tp-to-nebula.py tasks.json graph.json
cargo run --release -p nebula-app -- graph.json
```

## Explore

Drag the canvas to pan, scroll to zoom, and use **Fit view** to fit visible nodes. The GPU calculates the bounds without reading the full layout back to the CPU. Pause the simulation for precise inspection.

Search by original ID, label, or attribute. Results and neighbor lists are virtualized. Select a result to focus its neighborhood, inspect its attributes, and copy its ID or metadata. Incoming/outgoing lists respect direction; undirected edges appear in both. The Relationships list shows type, direction, weight, and attributes on hover. Back/Forward returns to earlier selections and camera views.

**Only this node**, **Hide node**, and one/two-hop neighborhood buttons narrow the view. Neighborhood expansion follows the weak (undirected) projection, so it includes dependencies in either direction. Additional filters combine with the main attribute filter using AND. Minimum degree is total undirected degree. **Restore all nodes** clears restrictions and filters. Hidden nodes are excluded from density aggregation, edge rendering, picking, and label sampling.

Color by an algorithm or attribute. PageRank uses original direction and weights. Components, community detection, degree, and greedy coloring use the weak projection. Proper coloring is impossible for self-loops; choose another mode for those graphs. Force attraction also uses the undirected, unweighted projection. The relationship visibility checkboxes control drawing; **compute over** controls the edge set used by algorithms and subsequent layouts. Click a color mode to recompute after changing that selection.

Labels use a bounded GPU screen-tile sampler at every graph size, with overlap avoidance. They are an overview rather than an exhaustive list; use the inspector/search for the full label and metadata. Layouts and color computations run as background jobs. Cancellation discards their result; a computation already inside an algorithm may finish its current work before the worker becomes available again.

## Save and share

**Save project** writes a `.nebula` file containing the original graph and metadata, positions, camera, selection, appearance, relationship visibility, algorithm edge selection, and filters. Writes replace the destination atomically after syncing the new file. Reopening restores the saved view. Saving a running layout captures a consistent point in time; the reopened simulation resumes if it was running when saved. Pause first to save a fixed arrangement.

**Undo/Redo** restores layout changes and control-panel view changes. Layout history retains at most 12 entries and approximately 128 MiB of position data, with at least one layout retained. Selection navigation has its own Back/Forward history.

Unsaved sessions get a recovery snapshot approximately once a minute when the worker is available. File → Recover unsaved session offers recovery after restarting. Recovery and recent-file state live in `~/Library/Application Support/nebula` on macOS, `%APPDATA%/nebula` on Windows, and `$XDG_STATE_HOME/nebula` (or `~/.local/state/nebula`) on Linux. Save explicitly before relying on a snapshot of the latest changes.

**Export image** saves PNG and can omit controls while retaining graph labels/selection markers. **Export entire graph** writes metadata-preserving JSON. **Export visible subgraph** exports nodes matching the current filters/restrictions and only visible relationship types, preserving original IDs.

## Shortcuts and errors

- Cmd/Ctrl O: Open; Cmd/Ctrl S: Save; Shift Cmd/Ctrl S: Save As.
- Cmd/Ctrl Z: Undo; Shift Cmd/Ctrl Z: Redo.
- Cmd/Ctrl [ / ]: selection navigation.
- Space: pause/resume; F: fit; L: labels; P: left controls; H: help; Esc: close.

The app checks GPU features, per-buffer limits, adjacency index limits, positions, and estimated upload memory before replacing a document. Estimates cannot guarantee allocations under concurrent system memory pressure; a failure is reported and leaves the existing document available where the GPU remains usable. A lost GPU device requires restarting the application. Copy diagnostics from Help when reporting a problem. The desktop workflow has been exercised on Apple M2 Max / Metal; other platforms still need native UI and device coverage.
