//! `nebula` — GPU graph viewer CLI.
//!
//! Builds a graph (generated or loaded), seeds positions, and hands it to the
//! GPU viewer. All layout and rendering then happen on the GPU.

use std::time::Instant;

use nebula_core::{formats, generate, Graph, Pos};
use nebula_layout::{Layout, RandomLayout};
use nebula_render::{App, ColorMode, FilterOp, LayoutSettings, RunOptions};

const HELP: &str = "\
nebula — the GPU graph viewer

USAGE:
    nebula [FILE | GENERATOR] [OPTIONS]
    nebula graph.json -o graph.png

GENERATORS (pick one; default: desktop welcome screen):
    --grid <W> <H>            2D lattice, W*H nodes
    --random <N> <M>          Erdos-Renyi: N nodes, M random edges
    --ba <N> <M>              Barabasi-Albert scale-free: N nodes, m=M attachments
    --blocks <N> <K> <M>      Stochastic block model: N nodes, K communities, M edges
    --geo <N> <R>             Random geometric: N nodes, connect within radius R (0..1)
    --file <PATH>             Load a graph or .nebula project (JSON/CSV/TSV/GraphML/GEXF/legacy)

OPTIONS:
    --color-attr <KEY>       Color by a node attribute
    --filter <KEY:OP:VALUE>  Filter by contains|eq|ne|gt|ge|lt|le
    --layout <NAME>         force|hierarchical|radial
    --compute-over <NAME>   all or a relationship type
    --node-size <FLOAT>     Node radius, 0.5..64 pixels
    --aggregate             Start in density view
    --color <MODE>            uniform|components|degree|pagerank|coloring|communities
    --k <FLOAT>               Optimal edge length (default 30)
    --seed <INT>              RNG seed (default 42)
    --dt <FLOAT>              Simulation timestep
    --substeps <INT>          Sim substeps per frame (default 1)
    --paused                  Start with the simulation paused
    --labels                  Show screen-sampled node labels
    --select <INDEX>          Preselect a node
    --no-edges / --no-nodes   Hide edges / nodes
    --compute-edges           Rasterize edges in a compute shader (UI-toggleable)
    --edges-while-simulating  Keep drawing edges while the force layout runs
                              (default: edges are hidden until it settles)
    --nodes-while-simulating  Draw real nodes while the force layout runs
                              (default: the density LOD stands in until it settles)
    --frames <N>              Exit after N frames (headless)
    -o, --output <PATH>       Render PNG and exit (default 300 frames)
    --screenshot <PATH>       Alias for --output
    --help-overlay            Start with the controls overlay visible
    -h, --help                Show this help

CONTROLS (in the window):
    drag pan / scroll zoom / F fit / click a node to inspect / double-click to focus
    1..6           color: uniform / components / degree / pagerank / coloring / communities
    R / G / O      re-seed random / grid / circle
    E / N          toggle edges / nodes      L   labels      S   save screenshot
    + / -          node size                 [ / ]  edge brightness
    space pause / H help / Tab hud / C clear selection / Esc quit
";

struct Args {
    generator: Gen,
    k: f32,
    seed: u64,
    dt: Option<f32>,
    substeps: u32,
    frames: Option<u64>,
    screenshot: Option<String>,
    color: ColorMode,
    /// Start colored by this node attribute (resolved to an index after load).
    color_attr: Option<String>,
    /// Startup "show only" filter, raw "KEY:OP:VALUE" (resolved after load).
    filter: Option<String>,
    /// Start in the hierarchical (layered DAG) layout.
    hierarchical: bool,
    /// Start in the radial (concentric DAG) layout.
    radial: bool,
    /// Headless preview of the legend-hover highlight.
    preview_highlight: bool,
    /// Edge set for edge-aware algorithms ("all" or an edge type name).
    compute_over: Option<String>,
    paused: bool,
    no_edges: bool,
    no_nodes: bool,
    select: Option<u32>,
    help_overlay: bool,
    labels: bool,
    aggregate: bool,
    compute_edges: bool,
    edges_while_simulating: bool,
    nodes_while_simulating: bool,
    node_size: f32,
}

enum Gen {
    Grid(u32, u32),
    Random(u64, u64),
    Ba(u64, u32),
    Blocks(u64, u32, u64),
    Geo(u64, f32),
    File(String),
    Desktop,
}

fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let args = match parse_args() {
        Ok(a) => a,
        Err(msg) => {
            eprint!("{HELP}");
            if !msg.is_empty() {
                eprintln!("\nerror: {msg}");
            }
            std::process::exit(if msg.is_empty() { 0 } else { 2 });
        }
    };

    if args.frames.is_none() {
        let path = match &args.generator {
            Gen::File(path) => Some(std::path::PathBuf::from(path)),
            Gen::Desktop => None,
            _ => None,
        };
        if matches!(args.generator, Gen::Desktop | Gen::File(_)) {
            return App::desktop(run_options(&args), path).run();
        }
    }

    // --- Build the graph ---------------------------------------------------
    let t0 = Instant::now();
    let mut node_labels: Option<Vec<String>> = None;
    let mut node_attrs: Option<Vec<Vec<(String, String)>>> = None;
    let mut edge_types: Vec<formats::LoadedEdgeType> = Vec::new();
    let mut document = None;
    let mut saved_project = None;
    let (graph, what): (Graph, String) = match &args.generator {
        Gen::Grid(w, h) => (generate::grid_2d(*w, *h), format!("grid {w}x{h}")),
        Gen::Random(n, m) => (
            generate::erdos_renyi_m(*n, *m, args.seed),
            format!("erdos-renyi n={n} m={m}"),
        ),
        Gen::Ba(n, m) => (
            generate::barabasi_albert(*n, *m, args.seed),
            format!("barabasi-albert n={n} m={m}"),
        ),
        Gen::Blocks(n, k, m) => (
            generate::stochastic_blocks(*n, *k, *m, 0.03, args.seed),
            format!("blocks n={n} k={k} m={m}"),
        ),
        Gen::Geo(n, r) => (
            generate::random_geometric(*n, *r, args.seed),
            format!("geometric n={n} r={r}"),
        ),
        Gen::Desktop => unreachable!("desktop launched above"),
        Gen::File(path) => {
            let (doc, project) =
                nebula_core::document::load(std::path::Path::new(path), None, &Default::default())?;
            node_labels = Some(doc.nodes.iter().map(|n| n.label.clone()).collect());
            node_attrs = Some(
                doc.nodes
                    .iter()
                    .map(|n| {
                        n.attributes
                            .iter()
                            .map(|(k, v)| (k.clone(), nebula_core::document::display(v)))
                            .collect()
                    })
                    .collect(),
            );
            let mut types = std::collections::BTreeMap::<String, Vec<[u32; 2]>>::new();
            for edge in &doc.edges {
                types
                    .entry(edge.kind.clone())
                    .or_default()
                    .push([edge.source, edge.target]);
            }
            edge_types = types
                .into_iter()
                .map(|(name, edges)| formats::LoadedEdgeType {
                    color: doc.type_colors.get(&name).copied(),
                    name,
                    edges,
                })
                .collect();
            let graph = doc.graph();
            document = Some(doc);
            saved_project = project;
            (graph, path.clone())
        }
    };
    // Attribute keys in first-seen order (matches the viewer's own ordering),
    // used to resolve --color-attr and --filter by name.
    let mut keys: Vec<&str> = Vec::new();
    if let Some(attrs) = node_attrs.as_ref() {
        for node in attrs {
            for (k, _) in node {
                if !keys.iter().any(|s| s == k) {
                    keys.push(k);
                }
            }
        }
    }

    // Resolve --color-attr to an attribute color index.
    let mut color_mode = args.color;
    if let Some(key) = &args.color_attr {
        match keys.iter().position(|k| *k == key) {
            Some(i) => color_mode = ColorMode::Attribute(i),
            None => anyhow::bail!(
                "attribute '{key}' not found; available: {}",
                keys.join(", ")
            ),
        }
    }

    // Resolve --filter "KEY:OP:VALUE" (op = contains/eq/ne/gt/ge/lt/le).
    let mut filter = None;
    if let Some(spec) = &args.filter {
        let mut parts = spec.splitn(3, ':');
        let key = parts.next().unwrap_or("");
        let op_name = parts.next().unwrap_or("");
        let value = parts.next().unwrap_or("").to_string();
        let idx = keys
            .iter()
            .position(|k| *k == key)
            .ok_or_else(|| anyhow::anyhow!("filter attribute '{key}' not found"))?;
        let op = FilterOp::from_name(op_name)
            .ok_or_else(|| anyhow::anyhow!("unknown filter op '{op_name}'"))?;
        filter = Some((idx, op, value));
    }
    log::info!(
        "built {what}: {} nodes, {} edges in {:.2}s",
        graph.num_nodes(),
        graph.num_edges(),
        t0.elapsed().as_secs_f32()
    );

    // Build CSR up front (needed by GPU spring forces + algorithms).
    let t1 = Instant::now();
    graph.ensure_csr();
    log::info!("built CSR in {:.2}s", t1.elapsed().as_secs_f32());

    // --- Seed positions ----------------------------------------------------
    let n = graph.num_nodes() as usize;
    let mut positions: Vec<Pos> = vec![[0.0; 2]; n];
    let seed_extent = args.k * (n.max(1) as f32).sqrt();
    if let Some(project) = &saved_project {
        positions = project.positions.clone();
    } else {
        RandomLayout {
            extent: seed_extent,
        }
        .place(&graph, &mut positions, args.seed);
    }

    // --- Launch viewer -----------------------------------------------------
    let mut settings = LayoutSettings {
        k: args.k,
        substeps: args.substeps,
        running: !args.paused,
        ..Default::default()
    };
    if let Some(dt) = args.dt {
        settings.dt = dt;
    }
    // Give the cooling schedule a step budget that matches the graph's size,
    // instead of freezing every graph after the same ~366 steps.
    settings.set_cooling_for(graph.num_nodes());
    log::info!(
        "cooling: alpha_decay {:.5} -> ~{} steps to settle ({} nodes)",
        settings.alpha_decay,
        settings.steps_to_settle(),
        graph.num_nodes(),
    );

    let opts = RunOptions {
        title: format!("nebula · {what}"),
        k: args.k,
        settings,
        max_frames: args.frames,
        screenshot: args.screenshot,
        color_mode,
        draw_edges: !args.no_edges,
        draw_nodes: !args.no_nodes,
        select: args.select,
        show_help: args.help_overlay,
        show_labels: args.labels,
        aggregate: args.aggregate,
        compute_edges: args.compute_edges,
        hide_edges_while_simulating: !args.edges_while_simulating,
        aggregate_while_simulating: !args.nodes_while_simulating,
        node_size: args.node_size,
        filter,
        start_hierarchical: args.hierarchical,
        start_radial: args.radial,
        preview_highlight: args.preview_highlight,
        compute_over: args.compute_over.clone(),
        start_attribute: args.color_attr.clone(),
        start_filter: args.filter.clone(),
    };

    let mut app = App::with_labels(graph, positions, opts, node_labels, node_attrs, edge_types);
    if let Some(document) = document {
        app.set_document(document);
    }
    if let Some(project) = saved_project {
        app.set_project(project)?;
    }
    app.run()
}

fn run_options(a: &Args) -> RunOptions {
    let mut settings = LayoutSettings {
        k: a.k,
        substeps: a.substeps,
        running: !a.paused,
        ..Default::default()
    };
    if let Some(dt) = a.dt {
        settings.dt = dt;
    }
    RunOptions {
        k: a.k,
        settings,
        color_mode: a.color,
        draw_edges: !a.no_edges,
        draw_nodes: !a.no_nodes,
        select: a.select,
        show_help: a.help_overlay,
        show_labels: a.labels,
        aggregate: a.aggregate,
        compute_edges: a.compute_edges,
        hide_edges_while_simulating: !a.edges_while_simulating,
        aggregate_while_simulating: !a.nodes_while_simulating,
        node_size: a.node_size,
        compute_over: a.compute_over.clone(),
        start_attribute: a.color_attr.clone(),
        start_filter: a.filter.clone(),
        start_hierarchical: a.hierarchical,
        start_radial: a.radial,
        ..Default::default()
    }
}

fn parse_args() -> Result<Args, String> {
    parse_args_from(std::env::args().skip(1))
}

fn parse_args_from(args: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut it = args.peekable();
    let mut generator: Option<Gen> = None;
    let mut k = 30.0f32;
    let mut seed = 42u64;
    let mut dt = None;
    let mut substeps = 1u32;
    let mut frames = None;
    let mut screenshot = None;
    let mut color = ColorMode::Uniform;
    let mut color_attr: Option<String> = None;
    let mut filter: Option<String> = None;
    let mut hierarchical = false;
    let mut radial = false;
    let mut preview_highlight = false;
    let mut compute_over: Option<String> = None;
    let mut paused = false;
    let mut no_edges = false;
    let mut no_nodes = false;
    let mut select = None;
    let mut help_overlay = false;
    let mut labels = false;
    let mut aggregate = false;
    let mut compute_edges = false;
    let mut edges_while_simulating = false;
    let mut nodes_while_simulating = false;
    let mut node_size = 3.0f32;

    fn next_u64(it: &mut impl Iterator<Item = String>, name: &str) -> Result<u64, String> {
        it.next()
            .ok_or_else(|| format!("{name} needs a value"))?
            .parse()
            .map_err(|_| format!("{name}: expected integer"))
    }
    fn next_u32(it: &mut impl Iterator<Item = String>, name: &str) -> Result<u32, String> {
        u32::try_from(next_u64(it, name)?)
            .map_err(|_| format!("{name}: integer exceeds 32-bit range"))
    }
    fn next_f32(it: &mut impl Iterator<Item = String>, name: &str) -> Result<f32, String> {
        it.next()
            .ok_or_else(|| format!("{name} needs a value"))?
            .parse()
            .map_err(|_| format!("{name}: expected number"))
    }

    while let Some(arg) = it.next() {
        if generator.is_some()
            && (matches!(
                arg.as_str(),
                "--grid" | "--random" | "--ba" | "--blocks" | "--geo" | "--file"
            ) || !arg.starts_with('-'))
        {
            return Err("Choose one graph source or generator".into());
        }
        match arg.as_str() {
            "-h" | "--help" => return Err(String::new()),
            "--grid" => {
                generator = Some(Gen::Grid(
                    next_u32(&mut it, "--grid W")?,
                    next_u32(&mut it, "--grid H")?,
                ))
            }
            "--random" => {
                generator = Some(Gen::Random(
                    next_u64(&mut it, "--random N")?,
                    next_u64(&mut it, "--random M")?,
                ))
            }
            "--ba" => {
                generator = Some(Gen::Ba(
                    next_u64(&mut it, "--ba N")?,
                    next_u32(&mut it, "--ba M")?,
                ))
            }
            "--blocks" => {
                generator = Some(Gen::Blocks(
                    next_u64(&mut it, "--blocks N")?,
                    next_u32(&mut it, "--blocks K")?,
                    next_u64(&mut it, "--blocks M")?,
                ))
            }
            "--geo" => {
                generator = Some(Gen::Geo(
                    next_u64(&mut it, "--geo N")?,
                    next_f32(&mut it, "--geo R")?,
                ))
            }
            "--file" => generator = Some(Gen::File(it.next().ok_or("--file needs a path")?)),
            "--k" => k = next_f32(&mut it, "--k")?,
            "--seed" => seed = next_u64(&mut it, "--seed")?,
            "--dt" => dt = Some(next_f32(&mut it, "--dt")?),
            "--substeps" => substeps = next_u32(&mut it, "--substeps")?,
            "--paused" => paused = true,
            "--no-edges" => no_edges = true,
            "--no-nodes" => no_nodes = true,
            "--select" => select = Some(next_u32(&mut it, "--select")?),
            "--help-overlay" => help_overlay = true,
            "--labels" => labels = true,
            "--aggregate" => aggregate = true,
            "--compute-edges" => compute_edges = true,
            "--edges-while-simulating" => edges_while_simulating = true,
            "--nodes-while-simulating" => nodes_while_simulating = true,
            "--node-size" => node_size = next_f32(&mut it, "--node-size")?,
            "--frames" => frames = Some(next_u64(&mut it, "--frames")?),
            "--screenshot" | "--output" | "-o" => {
                if screenshot.is_some() {
                    return Err("Specify only one output path".into());
                }
                screenshot = Some(it.next().ok_or("--output needs a path")?);
            }
            "--color" => {
                let name = it.next().ok_or("--color needs a mode")?;
                color = match name.as_str() {
                    "uniform" => ColorMode::Uniform,
                    "components" => ColorMode::Components,
                    "degree" => ColorMode::Degree,
                    "pagerank" => ColorMode::PageRank,
                    "coloring" => ColorMode::Coloring,
                    "communities" => ColorMode::Communities,
                    other => return Err(format!("unknown color mode: {other}")),
                };
            }
            "--color-attr" => {
                color_attr = Some(it.next().ok_or("--color-attr needs an attribute name")?);
            }
            "--filter" => {
                filter = Some(it.next().ok_or("--filter needs KEY:OP:VALUE")?);
            }
            "--highlight-demo" => preview_highlight = true,
            "--compute-over" => {
                compute_over = Some(it.next().ok_or("--compute-over needs a name")?);
            }
            "--layout" => {
                let name = it.next().ok_or("--layout needs a name")?;
                match name.as_str() {
                    "hierarchical" | "layered" | "dag" => {
                        hierarchical = true;
                        radial = false;
                    }
                    "radial" | "concentric" => {
                        radial = true;
                        hierarchical = false;
                    }
                    "force" | "force-directed" => {
                        hierarchical = false;
                        radial = false;
                    }
                    other => return Err(format!("unknown layout: {other}")),
                }
            }
            other if !other.starts_with('-') => generator = Some(Gen::File(other.into())),
            other => return Err(format!("unknown argument: {other}")),
        }
    }

    if !k.is_finite() || k <= 0.0 {
        return Err("--k must be positive and finite".into());
    }
    if dt.is_some_and(|v: f32| !v.is_finite() || v <= 0.0) {
        return Err("--dt must be positive and finite".into());
    }
    if !(1..=16).contains(&substeps) {
        return Err("--substeps must be between 1 and 16".into());
    }
    if !node_size.is_finite() || !(0.5..=64.0).contains(&node_size) {
        return Err("--node-size must be between 0.5 and 64".into());
    }
    if screenshot.is_some() && frames.is_none() {
        frames = Some(300);
    }
    if frames == Some(0) {
        return Err("--frames must be positive".into());
    }
    if let Some(g) = &generator {
        match g {
            Gen::Grid(w, h) if (*w as u64) * (*h as u64) >= u32::MAX as u64 => {
                return Err("grid exceeds supported node count".into())
            }
            Gen::Random(n, _) | Gen::Ba(n, _) | Gen::Blocks(n, _, _) | Gen::Geo(n, _)
                if *n >= u32::MAX as u64 =>
            {
                return Err("node count exceeds supported IDs".into())
            }
            Gen::Geo(_, r) if !r.is_finite() || *r <= 0.0 || *r >= 1.0 => {
                return Err("--geo radius must be between 0 and 1".into())
            }
            Gen::Ba(n, m) if *m == 0 || *m > 32 || *n <= *m as u64 => {
                return Err(
                    "--ba requires 1..32 attachments and more nodes than attachments".into(),
                )
            }
            Gen::Random(n, _) if *n < 2 => {
                return Err("--random requires at least two nodes".into())
            }
            Gen::Grid(w, h) if *w == 0 || *h == 0 => {
                return Err("Grid dimensions must be positive".into())
            }
            Gen::Blocks(n, k, _) if *k == 0 || (*k as u64) > *n => {
                return Err("block count must be between 1 and node count".into())
            }
            _ => {}
        }
    }
    Ok(Args {
        generator: generator.unwrap_or(if frames.is_some() {
            Gen::Ba(50_000, 3)
        } else {
            Gen::Desktop
        }),
        k,
        seed,
        dt,
        substeps,
        frames,
        screenshot,
        color,
        color_attr,
        filter,
        hierarchical,
        radial,
        preview_highlight,
        compute_over,
        paused,
        no_edges,
        no_nodes,
        select,
        help_overlay,
        labels,
        aggregate,
        compute_edges,
        edges_while_simulating,
        nodes_while_simulating,
        node_size,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn args(values: &[&str]) -> Result<Args, String> {
        parse_args_from(values.iter().map(|v| v.to_string()))
    }
    #[test]
    fn output_implies_batch_render_and_preserves_explicit_frames() {
        let a = args(&["examples/service-map.json", "-o", "graph.png"]).unwrap();
        assert_eq!(a.frames, Some(300));
        assert_eq!(a.screenshot.as_deref(), Some("graph.png"));
        assert_eq!(
            args(&["--output", "x.png", "--frames", "12"])
                .unwrap()
                .frames,
            Some(12)
        );
        assert!(args(&["-o"]).is_err());
        assert!(args(&["-o", "x.png", "--output", "y.png"]).is_err());
    }
    #[test]
    fn defaults_open_desktop_and_accept_positional_files() {
        assert!(matches!(args(&[]).unwrap().generator, Gen::Desktop));
        assert!(matches!(
            args(&["graph.json"]).unwrap().generator,
            Gen::File(_)
        ));
        assert!(matches!(
            args(&["--frames", "10"]).unwrap().generator,
            Gen::Ba(50_000, 3)
        ));
    }
    #[test]
    fn rejects_truncation_invalid_physics_and_bad_generators() {
        for values in [
            &["--grid", "4294967296", "2"][..],
            &["--k", "NaN"],
            &["--dt", "-1"],
            &["--substeps", "0"],
            &["--random", "1", "2"],
            &["--ba", "10", "33"],
            &["--grid", "0", "20"],
            &["one.json", "two.json"],
        ] {
            assert!(args(values).is_err(), "{values:?}");
        }
    }
}
