//! Document lifecycle, background jobs and desktop exploration controls.
use super::*;
use anyhow::Context;
use nebula_core::document::{self, CsvOptions, CsvPreview, Document, Progress, Project};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver},
};

#[derive(Clone, Serialize, Deserialize)]
struct View {
    center: [f32; 2],
    zoom: f32,
    settings: LayoutSettings,
    params: RenderParams,
    color: ColorMode,
    filter: NodeFilter,
    extra_filters: Vec<NodeFilter>,
    min_degree: u32,
    mask: Option<Vec<u32>>,
    selected: Option<u32>,
    edge_visibility: Vec<bool>,
    algo_edges: Option<usize>,
    spring_edges: Option<usize>,
    density: bool,
    labels: bool,
    draw_edges: bool,
    draw_nodes: bool,
    hide_edges: bool,
    aggregate_sim: bool,
    compute_edges: bool,
}
impl View {
    fn validate(&self, doc: &Document, keys: usize, types: usize) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.center.iter().all(|v| v.is_finite()) && self.zoom.is_finite() && self.zoom > 0.0,
            "Invalid saved camera"
        );
        let s = &self.settings;
        anyhow::ensure!(
            [
                s.k,
                s.repulsion,
                s.attraction,
                s.gravity,
                s.damping,
                s.dt,
                s.max_speed,
                s.alpha,
                s.alpha_decay,
                s.alpha_min,
                s.alpha_reheat,
                s.sim_duty,
                self.params.base_radius_px,
                self.params.edge_alpha
            ]
            .iter()
            .all(|v| v.is_finite()),
            "Invalid saved settings"
        );
        anyhow::ensure!(
            s.k > 0.0
                && s.dt > 0.0
                && s.substeps > 0
                && s.substeps <= 16
                && s.sim_duty > 0.0
                && s.sim_duty <= 1.0
                && s.alpha_decay > 0.0
                && s.alpha_decay < 1.0,
            "Invalid saved physics limits"
        );
        anyhow::ensure!(
            self.selected.is_none_or(|i| (i as usize) < doc.nodes.len()),
            "Invalid saved selection"
        );
        anyhow::ensure!(
            self.mask
                .as_ref()
                .is_none_or(|m| m.iter().all(|i| (*i as usize) < doc.nodes.len())),
            "Invalid saved visibility"
        );
        anyhow::ensure!(
            self.algo_edges.is_none_or(|i| i < types)
                && self.spring_edges.is_none_or(|i| i < types),
            "Invalid saved relationship selection"
        );
        anyhow::ensure!(
            !matches!(self.color,ColorMode::Attribute(i) if i>=keys),
            "Invalid saved color attribute"
        );
        for f in std::iter::once(&self.filter).chain(&self.extra_filters) {
            anyhow::ensure!(!f.enabled || f.key < keys, "Invalid saved filter attribute");
        }
        Ok(())
    }
}
#[derive(Default, Serialize, Deserialize)]
struct Preferences {
    recent: Vec<PathBuf>,
}
struct Job {
    receiver: Receiver<anyhow::Result<JobResult>>,
    progress: Progress,
}
enum JobResult {
    Open {
        doc: Arc<Document>,
        graph: Arc<Graph>,
        index: Arc<document::EdgeIndex>,
        positions: Vec<Pos>,
        project: Option<Project>,
        path: PathBuf,
    },
    Preview(PathBuf, CsvPreview),
    Colors(
        ColorMode,
        Vec<u32>,
        Option<Vec<f32>>,
        Option<Vec<f32>>,
        String,
    ),
    Layout(Vec<Pos>, bool, Arc<Graph>, Option<usize>),
    Springs(Arc<Graph>, Option<usize>, bool),
    Mask(Vec<bool>),
    Saved(PathBuf, bool, u64),
    Exported(PathBuf),
}
enum CapturePurpose {
    Save(PathBuf, bool),
    Layout(Seed),
    Undo,
    Redo,
}
#[derive(Clone)]
struct UndoEntry {
    positions: Option<Vec<Pos>>,
    view: View,
}
struct Capture {
    revision: u64,
    read: crate::readback::PositionCapture,
    view: View,
    purpose: CapturePurpose,
}
struct SearchJob {
    receiver: Receiver<(String, Vec<u32>)>,
    progress: Progress,
}
pub(super) struct Workspace {
    pub(super) document: Arc<Document>,
    index: Arc<document::EdgeIndex>,
    canvas: Option<(glam::Vec2, glam::Vec2)>,
    fit_after_ui: bool,
    selection_capture: Option<(u32, crate::readback::PositionCapture)>,
    selection_at: Instant,
    pending_focus: bool,
    job: Option<Job>,
    capture: Option<Capture>,
    search_job: Option<SearchJob>,
    pending_import: Option<Box<JobResult>>,
    path: Option<PathBuf>,
    preferences: Preferences,
    csv: Option<(PathBuf, CsvPreview)>,
    csv_error: Option<String>,
    query: String,
    search_results: Vec<u32>,
    searched: String,
    search_due: Option<Instant>,
    restrict: Option<Arc<Vec<bool>>>,
    mask: Option<Arc<Vec<bool>>>,
    extra_filters: Vec<NodeFilter>,
    min_degree: u32,
    filter_due: Option<Instant>,
    undo: Vec<UndoEntry>,
    redo: Vec<UndoEntry>,
    history: Vec<(Camera2D, Option<u32>)>,
    future: Vec<(Camera2D, Option<u32>)>,
    pub(super) error: Option<String>,
    pub(super) notice: Option<String>,
    pub(super) clean_export: bool,
    pub(super) close_requested: bool,
    pub(super) modifiers: winit::keyboard::ModifiersState,
    revision: u64,
    pending_png: Option<PathBuf>,
    initial_layout: Option<Seed>,
    applied_color: Option<ColorMode>,
    spring_due: Option<(Option<usize>, bool)>,
    dirty: bool,
    last_autosave: Instant,
    recovery: Option<PathBuf>,
    pending_open: Option<PathBuf>,
    pending_save_close: bool,
    close_confirm: bool,
    start_view: Option<View>,
    pub(super) restored_project: bool,
    startup: Option<PathBuf>,
    pub(super) fatal: Option<String>,
}
fn data_dir() -> PathBuf {
    #[cfg(target_os = "macos")]
    let base = std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|p| p.join("Library/Application Support"));
    #[cfg(target_os = "windows")]
    let base = std::env::var_os("APPDATA").map(PathBuf::from);
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".local/state")));
    base.unwrap_or_else(std::env::temp_dir).join("nebula")
}
impl Workspace {
    pub(super) fn from_graph(
        graph: &Graph,
        labels: Option<&[String]>,
        attrs: Option<&[Vec<(String, String)>]>,
        types: &[EdgeType],
    ) -> Self {
        let nodes = (0..graph.num_nodes() as usize)
            .map(|i| {
                let id = labels
                    .and_then(|l| l.get(i))
                    .cloned()
                    .unwrap_or_else(|| i.to_string());
                let attributes = attrs
                    .and_then(|a| a.get(i))
                    .into_iter()
                    .flatten()
                    .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
                    .collect();
                document::Node {
                    label: id.clone(),
                    id,
                    attributes,
                }
            })
            .collect();
        let edges = types
            .iter()
            .flat_map(|t| {
                t.edges.iter().map(|e| document::Edge {
                    source: e[0],
                    target: e[1],
                    directed: false,
                    weight: 1.0,
                    kind: t.name.clone(),
                    attributes: Default::default(),
                })
            })
            .collect();
        let document = Arc::new(Document {
            nodes,
            edges,
            type_colors: types
                .iter()
                .filter_map(|t| t.color.map(|c| (t.name.clone(), c)))
                .collect(),
            warnings: Vec::new(),
        });
        let dir = data_dir();
        let preferences = std::fs::File::open(dir.join("recent.json"))
            .ok()
            .and_then(|f| serde_json::from_reader(f).ok())
            .unwrap_or_default();
        let recovery = Some(dir.join("recovery.nebula")).filter(|p| p.exists());
        let index = Arc::new(document::EdgeIndex::new(&document));
        Self {
            index,
            document,
            selection_capture: None,
            canvas: None,
            fit_after_ui: false,
            selection_at: Instant::now() - Duration::from_secs(1),
            pending_focus: false,
            job: None,
            capture: None,
            search_job: None,
            pending_import: None,
            path: None,
            preferences,
            csv: None,
            csv_error: None,
            query: String::new(),
            search_results: Vec::new(),
            searched: String::new(),
            search_due: None,
            restrict: None,
            mask: None,
            extra_filters: Vec::new(),
            min_degree: 0,
            filter_due: None,
            undo: Vec::new(),
            redo: Vec::new(),
            history: Vec::new(),
            future: Vec::new(),
            error: None,
            notice: None,
            clean_export: true,
            close_requested: false,
            modifiers: Default::default(),
            revision: 0,
            pending_png: None,
            initial_layout: None,
            applied_color: Some(ColorMode::Uniform),
            spring_due: None,
            dirty: false,
            last_autosave: Instant::now(),
            recovery,
            pending_open: None,
            pending_save_close: false,
            close_confirm: false,
            start_view: None,
            restored_project: false,
            startup: None,
            fatal: None,
        }
    }
    pub(super) fn simulation_changed(&mut self) {
        self.dirty = true;
        self.revision = self.revision.wrapping_add(1);
    }
    fn busy(&self) -> bool {
        self.job.is_some() || self.capture.is_some() || self.pending_import.is_some()
    }
}
impl App {
    pub(super) fn capture_ready(&self) -> bool {
        !self.work.busy()
            && self.work.filter_due.is_none()
            && self.work.spring_due.is_none()
            && self.work.initial_layout.is_none()
    }
    pub(super) fn mark_dirty(&mut self) {
        self.work.dirty = true;
        self.work.revision = self.work.revision.wrapping_add(1);
    }
    pub(super) fn initialize_visibility(&mut self) {
        if self.filter.enabled {
            let key = self
                .attr_keys
                .get(self.filter.key)
                .cloned()
                .unwrap_or_default();
            let mask = self
                .work
                .document
                .nodes
                .iter()
                .map(|n| {
                    self.filter.op.eval(
                        &n.attributes
                            .get(&key)
                            .map(document::display)
                            .unwrap_or_default(),
                        &self.filter.value,
                    )
                })
                .collect();
            self.work.mask = Some(Arc::new(mask));
            self.push_filtered();
        }
    }
    pub fn set_document(&mut self, document: Arc<Document>) {
        self.work.index = Arc::new(document::EdgeIndex::new(&document));
        self.work.document = document;
    }
    pub fn set_project(&mut self, project: Project) -> anyhow::Result<()> {
        let view: View = serde_json::from_value(project.view)?;
        view.validate(
            &self.work.document,
            self.attr_keys.len(),
            self.edge_types.len(),
        )?;
        self.opts.k = view.settings.k;
        self.settings = view.settings;
        self.work.start_view = Some(view);
        self.work.restored_project = true;
        Ok(())
    }
    pub(super) fn initialize_project_view(&mut self) {
        if let Some(view) = self.work.start_view.take() {
            self.apply_view(view);
        }
    }
    pub fn desktop(opts: RunOptions, path: Option<PathBuf>) -> Self {
        let mut app = Self::new(Graph::empty(0), Vec::new(), opts);
        app.settings.running = false;
        app.work.startup = path;
        app
    }
    pub(super) fn fit_canvas_bounds(&mut self, min: glam::Vec2, max: glam::Vec2) {
        if let Some((a, b)) = self.work.canvas {
            self.camera.fit_bounds_in_rect(min, max, a, b);
        } else {
            self.camera.fit_bounds(min, max);
        }
    }
    pub(super) fn update_canvas(&mut self, ctx: &egui::Context) {
        let r = ctx.available_rect();
        let scale = ctx.pixels_per_point();
        self.work.canvas = Some((
            glam::vec2(r.min.x, r.min.y) * scale,
            glam::vec2(r.max.x, r.max.y) * scale,
        ));
        if self.work.fit_after_ui {
            self.work.fit_after_ui = false;
            let (min, max) = bounds(&self.seed_positions);
            self.fit_canvas_bounds(min, max);
        }
    }
    fn remember(&mut self, path: &Path) {
        if !path.is_file() {
            return;
        }
        self.work.preferences.recent.retain(|p| p != path);
        self.work.preferences.recent.insert(0, path.into());
        self.work.preferences.recent.truncate(12);
        if let Err(e) = std::fs::create_dir_all(data_dir()).and_then(|_| {
            document::atomic_json(&data_dir().join("recent.json"), &self.work.preferences)
                .map_err(std::io::Error::other)
        }) {
            self.work.error = Some(format!("Could not save recent files: {e}"));
        }
    }
    fn begin_job(
        &mut self,
        p: Progress,
        run: impl FnOnce(Progress) -> anyhow::Result<JobResult> + Send + 'static,
    ) {
        if self.work.busy() {
            self.work.notice = Some("Wait for the current operation, or cancel it first.".into());
            return;
        }
        let (tx, rx) = mpsc::channel();
        let worker_progress = p.clone();
        std::thread::spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run(worker_progress.clone())
            }))
            .unwrap_or_else(|_| Err(anyhow::anyhow!("Background operation failed unexpectedly")));
            let result = result.and_then(|r| {
                worker_progress.check()?;
                Ok(r)
            });
            let _ = tx.send(result);
        });
        self.work.job = Some(Job {
            receiver: rx,
            progress: p,
        });
    }
    pub(super) fn open_path(&mut self, path: PathBuf) {
        if self.work.dirty {
            self.work.pending_open = Some(path);
            return;
        }
        self.open_now(path);
    }
    fn open_now(&mut self, path: PathBuf) {
        if self.work.busy() {
            self.work.notice =
                Some("Finish or cancel the current operation before opening another file.".into());
            return;
        }
        self.work.error = None;
        if path.extension().is_some_and(|e| e == "csv" || e == "tsv") {
            self.begin_job(Progress::default(), move |p| {
                p.set("Previewing CSV")?;
                Ok(JobResult::Preview(
                    path.clone(),
                    document::csv_defaults(&path)?,
                ))
            });
        } else {
            self.import_path(path, None);
        }
    }
    fn import_path(&mut self, path: PathBuf, options: Option<CsvOptions>) {
        let k = self.opts.k;
        self.begin_job(Progress::default(), move |p| {
            let (doc, project) = document::load(&path, options.as_ref(), &p)?;
            p.set("Building graph adjacency")?;
            let doc = project.as_ref().map(|p| p.document.clone()).unwrap_or(doc);
            let graph = Arc::new(doc.graph());
            graph.ensure_csr();
            graph.outgoing();
            graph.incoming();
            p.check()?;
            let positions = if let Some(project) = &project {
                project.positions.clone()
            } else {
                p.set("Seeding layout")?;
                let mut pos = vec![[0.0; 2]; doc.nodes.len()];
                RandomLayout {
                    extent: k * (doc.nodes.len().max(1) as f32).sqrt(),
                }
                .place(&graph, &mut pos, 42);
                pos
            };
            let index = Arc::new(document::EdgeIndex::new(&doc));
            Ok(JobResult::Open {
                doc,
                graph,
                index,
                positions,
                project,
                path,
            })
        });
    }
    fn view(&self) -> View {
        View {
            center: self.camera.center.into(),
            zoom: self.camera.zoom,
            settings: self.settings,
            params: self.render_params,
            color: self.color_mode,
            filter: self.filter.clone(),
            extra_filters: self.work.extra_filters.clone(),
            min_degree: self.work.min_degree,
            mask: self.work.restrict.as_ref().map(|m| {
                m.iter()
                    .enumerate()
                    .filter_map(|(i, v)| v.then_some(i as u32))
                    .collect()
            }),
            selected: self.selected,
            edge_visibility: self.edge_types.iter().map(|e| e.visible).collect(),
            algo_edges: self.algo_edge_set,
            spring_edges: self.spring_edge_set,
            density: self.show_density,
            labels: self.show_labels,
            draw_edges: self.draw_edges_pref,
            draw_nodes: self.live.as_ref().is_none_or(|l| l.renderer.draw_nodes),
            hide_edges: self.hide_edges_while_simulating,
            aggregate_sim: self.aggregate_while_simulating,
            compute_edges: self.live.as_ref().is_some_and(|l| l.renderer.edge_raster()),
        }
    }
    fn apply_view(&mut self, v: View) {
        self.camera.center = v.center.into();
        self.camera.zoom = v.zoom;
        self.settings = v.settings;
        if self.spring_edge_set != v.spring_edges {
            self.work.spring_due = Some((v.spring_edges, self.settings.running));
            self.settings.running = false;
        }
        self.render_params = v.params;
        self.color_mode = v.color;
        self.filter = v.filter;
        self.work.extra_filters = v.extra_filters;
        self.work.min_degree = v.min_degree;
        self.work.restrict = v.mask.map(|ids| {
            let mut m = vec![false; self.graph.num_nodes() as usize];
            for id in ids {
                m[id as usize] = true;
            }
            Arc::new(m)
        });
        self.selected = v.selected;
        self.selected_pos = None;
        self.algo_edge_set = v.algo_edges;
        self.show_density = v.density;
        self.show_labels = v.labels;
        self.draw_edges_pref = v.draw_edges;
        self.hide_edges_while_simulating = v.hide_edges;
        self.aggregate_while_simulating = v.aggregate_sim;
        if let Some(l) = self.live.as_mut() {
            l.renderer.draw_nodes = v.draw_nodes;
            l.renderer.set_edge_raster(v.compute_edges);
        }
        for (i, t) in self.edge_types.iter_mut().enumerate() {
            t.visible = v.edge_visibility.get(i).copied().unwrap_or(true);
            if let Some(l) = self.live.as_mut() {
                l.renderer.set_edge_visible(i, t.visible);
            }
        }
        self.work.filter_due = Some(Instant::now());
        self.push_params();
    }
    fn capture(&mut self, purpose: CapturePurpose) {
        if self.work.busy() {
            self.work.notice = Some("Wait for the current operation to finish.".into());
            return;
        }
        if let Some(live) = &self.live {
            self.work.capture = Some(Capture {
                revision: self.work.revision,
                read: crate::readback::PositionCapture::new(&live.gpu, &live.graph_gpu),
                view: self.view(),
                purpose,
            });
        }
    }
    fn save_project(&mut self, save_as: bool) {
        let path = if !save_as {
            self.work
                .path
                .clone()
                .filter(|p| p.extension().is_some_and(|e| e == "nebula"))
        } else {
            None
        };
        let path = path.or_else(|| {
            rfd::FileDialog::new()
                .add_filter("Nebula project", &["nebula"])
                .set_file_name("graph.nebula")
                .save_file()
        });
        if let Some(path) = path {
            self.capture(CapturePurpose::Save(path, false));
        } else {
            self.work.pending_save_close = false;
        }
    }
    pub(super) fn record_view_undo(&mut self) {
        if self.work.document.nodes.is_empty() {
            return;
        }
        let view = self.view();
        self.work.undo.push(UndoEntry {
            positions: None,
            view,
        });
        self.trim_undo();
        self.work.redo.clear();
    }
    fn undo_redo(&mut self, redo: bool) {
        if self.work.busy() {
            return;
        }
        let target = if redo {
            self.work.redo.last()
        } else {
            self.work.undo.last()
        };
        if target.is_some_and(|t| t.positions.is_some()) {
            self.capture(if redo {
                CapturePurpose::Redo
            } else {
                CapturePurpose::Undo
            });
            return;
        }
        let view = self.view();
        let target = if redo {
            self.work.redo.pop()
        } else {
            self.work.undo.pop()
        };
        if let Some(target) = target {
            let current = UndoEntry {
                positions: None,
                view,
            };
            if redo {
                self.work.undo.push(current);
            } else {
                self.work.redo.push(current);
            }
            self.apply_view(target.view);
            self.work.applied_color = None;
            self.start_color_job();
            self.mark_dirty();
        }
    }
    fn trim_undo(&mut self) {
        let mut bytes = self
            .work
            .undo
            .iter()
            .map(|p| p.positions.as_ref().map_or(0, |p| p.len() * 8))
            .sum::<usize>();
        while bytes > 128 * 1024 * 1024 && self.work.undo.len() > 1 {
            bytes -= self
                .work
                .undo
                .remove(0)
                .positions
                .as_ref()
                .map_or(0, |p| p.len() * 8);
        }
        while self.work.undo.len() > 12 {
            self.work.undo.remove(0);
        }
    }
    pub(super) fn start_layout_job(&mut self, seed: Seed) {
        self.capture(CapturePurpose::Layout(seed));
    }
    pub(super) fn start_color_job(&mut self) {
        if self.opts.max_frames.is_some() {
            self.apply_color_mode();
            self.work.applied_color = Some(self.color_mode);
            return;
        }
        if self.work.busy() {
            self.work.notice =
                Some("Color will update when the current operation finishes.".into());
            return;
        }
        let graph = self.graph.clone();
        let doc = self.work.document.clone();
        let mode = self.color_mode;
        let subset = self
            .algo_edge_set
            .and_then(|i| self.edge_types.get(i))
            .map(|t| t.name.clone());
        let key = if let ColorMode::Attribute(i) = mode {
            self.attr_keys.get(i).cloned()
        } else {
            None
        };
        self.begin_job(Progress::default(), move |p| {
            p.set("Computing colors")?;
            let graph = if let Some(kind) = subset {
                Arc::new(doc.graph_for_kind(&kind))
            } else {
                graph
            };
            let n = graph.num_nodes() as usize;
            let (colors, sizes, values, name) = match mode {
                ColorMode::Uniform => (
                    vec![pack_rgba(120, 170, 255, 255); n],
                    None,
                    None,
                    String::new(),
                ),
                ColorMode::Degree => {
                    let vals: Vec<f32> = graph.degrees().into_iter().map(|v| v as f32).collect();
                    (
                        coloring::sequential_colors_f32(&vals, true),
                        Some(coloring::sizes_from_scalar(&vals, 6.0)),
                        Some(vals),
                        "degree".into(),
                    )
                }
                ColorMode::Components | ColorMode::Coloring | ColorMode::Communities => {
                    let vals = match mode {
                        ColorMode::Components => algorithms::connected_components(&graph),
                        ColorMode::Coloring => {anyhow::ensure!(!graph.edges().iter().any(|e|e[0]==e[1]),"Proper coloring is impossible with self-loops. Choose another color mode or import a graph without loops.");algorithms::greedy_coloring(&graph)},
                        _ => algorithms::label_propagation(&graph, 20),
                    };
                    (
                        coloring::categorical_colors(&vals),
                        None,
                        Some(vals.into_iter().map(|v| v as f32).collect()),
                        mode.label().into(),
                    )
                }
                ColorMode::PageRank => {
                    let vals = algorithms::pagerank(&graph, 40, 0.85);
                    (
                        coloring::sequential_colors_f32(&vals, true),
                        Some(coloring::sizes_from_scalar(&vals, 8.0)),
                        Some(vals),
                        "pagerank".into(),
                    )
                }
                ColorMode::Attribute(_) => {
                    let key = key.context("Missing color attribute")?;
                    let values: Vec<Option<&serde_json::Value>> =
                        doc.nodes.iter().map(|n| n.attributes.get(&key)).collect();
                    let nums: Option<Vec<f32>> = values
                        .iter()
                        .map(|v| {
                            v.map(|v| document::display(v).parse::<f32>().ok().filter(|n|n.is_finite()))
                                .unwrap_or(Some(0.0))
                        })
                        .collect();
                    if let Some(nums) = nums {
                        (
                            coloring::sequential_colors_f32(&nums, false),
                            None,
                            Some(nums),
                            key,
                        )
                    } else {
                        let mut labels = std::collections::BTreeMap::new();
                        let mut vals = Vec::with_capacity(n);
                        for v in values {
                            let key = v.map(document::display).unwrap_or_default();
                            let next = labels.len() as u32;
                            vals.push(*labels.entry(key).or_insert(next));
                        }
                        (
                            coloring::categorical_colors(&vals),
                            None,
                            Some(vals.into_iter().map(|v| v as f32).collect()),
                            key,
                        )
                    }
                }
            };
            p.check()?;
            Ok(JobResult::Colors(mode, colors, sizes, values, name))
        });
    }
    fn start_filter_job(&mut self) {
        if self.work.busy() {
            return;
        }
        self.work.filter_due = None;
        let doc = self.work.document.clone();
        let graph = self.graph.clone();
        let restrict = self.work.restrict.clone();
        let filters: Vec<_> = std::iter::once(self.filter.clone())
            .chain(self.work.extra_filters.clone())
            .filter(|f| f.enabled)
            .filter_map(|f| self.attr_keys.get(f.key).cloned().map(|k| (k, f)))
            .collect();
        let min_degree = self.work.min_degree;
        self.begin_job(Progress::default(), move |p| {
            p.set("Filtering nodes")?;
            let degrees = if min_degree > 0 {
                Some(graph.degrees())
            } else {
                None
            };
            let mut mask = Vec::with_capacity(doc.nodes.len());
            for (i, n) in doc.nodes.iter().enumerate() {
                if i % 4096 == 0 {
                    p.check()?;
                }
                mask.push(
                    restrict.as_ref().is_none_or(|m| m[i])
                        && degrees.as_ref().is_none_or(|d| d[i] >= min_degree)
                        && filters.iter().all(|(k, f)| {
                            f.op.eval(
                                &n.attributes
                                    .get(k)
                                    .map(document::display)
                                    .unwrap_or_default(),
                                &f.value,
                            )
                        }),
                );
            }
            Ok(JobResult::Mask(mask))
        });
    }
    pub(super) fn schedule_filter(&mut self) {
        self.work.filter_due = Some(Instant::now() + Duration::from_millis(200));
        self.mark_dirty();
    }
    pub(super) fn visibility_mask(&self, _n: usize) -> Option<Vec<bool>> {
        self.work.mask.as_ref().map(|m| (**m).clone())
    }
    pub(super) fn refresh_selection_async(&mut self, sel: u32) {
        if let Some((id, read)) = &self.work.selection_capture {
            if let Some(result) = self.live.as_ref().and_then(|l| read.poll(&l.gpu)) {
                let id = *id;
                self.work.selection_capture = None;
                if id == sel {
                    match result {
                        Ok(pos) => {
                            self.selected_pos = pos.first().map(|p| glam::vec2(p[0], p[1]));
                            self.neighbor_positions =
                                pos.iter().skip(1).map(|p| glam::vec2(p[0], p[1])).collect();
                            if self.work.pending_focus {
                                self.work.pending_focus = false;
                                self.focus_selected();
                            }
                        }
                        Err(e) => self.work.error = Some(e.to_string()),
                    }
                }
            }
        }
        if self.work.selection_capture.is_none()
            && self.work.selection_at.elapsed() > Duration::from_millis(33)
        {
            let mut indices = vec![sel];
            indices.extend(self.graph.csr().neighbors(sel).iter().copied().take(512));
            if let Some(live) = &self.live {
                self.work.selection_capture = Some((
                    sel,
                    crate::readback::PositionCapture::gather(&live.gpu, &live.graph_gpu, &indices),
                ));
                self.work.selection_at = Instant::now();
            }
        }
    }
    pub(super) fn request_focus(&mut self) {
        self.work.pending_focus = true;
    }
    pub(super) fn select_node(&mut self, id: Option<u32>) {
        if id != self.selected {
            self.work.history.push((self.camera, self.selected));
            if self.work.history.len() > 100 {
                self.work.history.remove(0);
            }
            self.work.future.clear();
        }
        self.selected = id;
        self.selected_pos = None;
        self.work.pending_focus = false;
        self.work.selection_at = Instant::now() - Duration::from_secs(1);
        if let Some(id) = id {
            self.refresh_selection(id);
        }
        self.mark_dirty();
    }
    fn navigate(&mut self, forward: bool) {
        let state = (self.camera, self.selected);
        let target = if forward {
            self.work.future.pop()
        } else {
            self.work.history.pop()
        };
        if let Some((camera, id)) = target {
            if forward {
                self.work.history.push(state);
            } else {
                self.work.future.push(state);
            }
            self.camera = camera;
            self.selected = id;
            self.selected_pos = None;
            self.work.pending_focus = false;
            self.work.selection_at = Instant::now() - Duration::from_secs(1);
        }
    }
    fn isolate(&mut self, hops: u32, hide: bool) {
        let Some(id) = self.selected else {
            return;
        };
        self.record_view_undo();
        let mut mask = if hide {
            self.work
                .restrict
                .as_ref()
                .map(|m| (**m).clone())
                .unwrap_or_else(|| vec![true; self.graph.num_nodes() as usize])
        } else {
            vec![false; self.graph.num_nodes() as usize]
        };
        let csr = self.graph.csr();
        let mut frontier = vec![id];
        let mut visited = std::collections::HashSet::new();
        visited.insert(id);
        for _ in 0..hops {
            let mut next = Vec::new();
            for u in frontier {
                for &v in csr.neighbors(u) {
                    if visited.insert(v) {
                        next.push(v);
                    }
                }
            }
            frontier = next;
        }
        for id in visited {
            mask[id as usize] = !hide;
        }
        self.work.restrict = Some(Arc::new(mask));
        self.work.filter_due = Some(Instant::now());
        self.mark_dirty();
    }
    fn export_graph(&mut self, visible: bool) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("Graph JSON", &["json"])
            .set_file_name("graph.json")
            .save_file()
        else {
            return;
        };
        let doc = self.work.document.clone();
        let graph = self.graph.clone();
        let restrict = self.work.restrict.clone();
        let min_degree = self.work.min_degree;
        let filters: Vec<_> = std::iter::once(self.filter.clone())
            .chain(self.work.extra_filters.clone())
            .filter(|f| f.enabled)
            .filter_map(|f| self.attr_keys.get(f.key).cloned().map(|k| (k, f)))
            .collect();
        let types: std::collections::HashSet<_> = self
            .edge_types
            .iter()
            .filter(|t| t.visible)
            .map(|t| t.name.clone())
            .collect();
        self.begin_job(Progress::default(), move |p| {
            p.set("Exporting graph")?;
            let result = if visible {
                let degrees = if min_degree > 0 {
                    Some(graph.degrees())
                } else {
                    None
                };
                let mask: Vec<_> = doc
                    .nodes
                    .iter()
                    .enumerate()
                    .map(|(i, n)| {
                        restrict.as_ref().is_none_or(|m| m[i])
                            && degrees.as_ref().is_none_or(|d| d[i] >= min_degree)
                            && filters.iter().all(|(key, f)| {
                                f.op.eval(
                                    &n.attributes
                                        .get(key)
                                        .map(document::display)
                                        .unwrap_or_default(),
                                    &f.value,
                                )
                            })
                    })
                    .collect();
                let mut doc = doc.subset(&mask);
                doc.edges.retain(|e| types.contains(&e.kind));
                doc
            } else {
                (*doc).clone()
            };
            p.check()?;
            document::atomic_json(&path, &result.export_json())?;
            Ok(JobResult::Exported(path))
        });
    }
    pub(super) fn workspace_shortcut(&mut self, code: KeyCode) -> bool {
        let command = self.work.modifiers.super_key() || self.work.modifiers.control_key();
        if !command {
            return false;
        }
        match code {
            KeyCode::KeyO => {
                if let Some(path) = rfd::FileDialog::new().pick_file() {
                    self.open_path(path);
                }
            }
            KeyCode::KeyS => self.save_project(self.work.modifiers.shift_key()),
            KeyCode::KeyZ => self.undo_redo(self.work.modifiers.shift_key()),
            KeyCode::BracketLeft => self.navigate(false),
            KeyCode::BracketRight => self.navigate(true),
            _ => return false,
        }
        true
    }
    pub(super) fn poll_jobs(&mut self) {
        if let Some(result) = self.live.as_mut().and_then(|l| l.bounds.poll(&l.gpu)) {
            match result {
                Ok((min, max)) => {
                    self.fit_canvas_bounds(min, max);
                    self.mark_dirty();
                }
                Err(e) => self.work.notice = Some(e.to_string()),
            }
        }
        if let Some(live) = &self.live {
            if let Ok(error) = live.gpu.errors.try_recv() {
                if self.opts.max_frames.is_some() {
                    self.work.fatal = Some(error.clone());
                    self.should_exit = true;
                }
                self.work.error = Some(error);
                self.settings.running = false;
            }
        }
        if let Some(path) = self.work.pending_png.take() {
            self.save_frame(&path.to_string_lossy());
        }
        if let Some(path) = self.work.startup.take() {
            self.open_now(path);
        }
        if let Some(job) = self.work.job.as_ref() {
            match job.receiver.try_recv() {
                Ok(result) => {
                    let cancelled = job
                        .progress
                        .cancelled
                        .load(std::sync::atomic::Ordering::Relaxed);
                    self.work.job = None;
                    if !cancelled {
                        match result {
                            Ok(result) => self.finish_job(result),
                            Err(e) => {
                                if self.work.applied_color != Some(self.color_mode) {
                                    self.color_mode =
                                        self.work.applied_color.unwrap_or(ColorMode::Uniform);
                                    self.update_title();
                                }
                                self.work.error = Some(format!("{e:#}"));
                            }
                        }
                    } else {
                        self.work.notice =
                            Some("Operation cancelled; your current graph is unchanged.".into());
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.work.job = None;
                    self.work.error = Some("Background worker stopped unexpectedly".into());
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        if let (Some(c), Some(l)) = (&self.work.capture, &self.live) {
            if let Some(result) = c.read.poll(&l.gpu) {
                let c = self.work.capture.take().unwrap();
                match result {
                    Err(e) => self.work.error = Some(format!("{e:#}")),
                    Ok(positions) => {
                        let project = Project {
                            version: 1,
                            document: self.work.document.clone(),
                            positions,
                            view: serde_json::to_value(&c.view).unwrap(),
                        };
                        match c.purpose {
                            CapturePurpose::Save(path, auto) => {
                                self.begin_job(Progress::default(), move |p| {
                                    p.set(if auto {
                                        "Saving recovery snapshot"
                                    } else {
                                        "Saving project"
                                    })?;
                                    document::atomic_json(&path, &project)?;
                                    Ok(JobResult::Saved(path, auto, c.revision))
                                });
                            }
                            CapturePurpose::Layout(seed) => {
                                self.work.undo.push(UndoEntry {
                                    positions: Some(project.positions),
                                    view: c.view,
                                });
                                self.trim_undo();
                                self.work.redo.clear();
                                let graph = self.graph.clone();
                                let doc = self.work.document.clone();
                                let kind = self
                                    .algo_edge_set
                                    .and_then(|i| self.edge_types.get(i))
                                    .map(|t| t.name.clone());
                                let k = self.opts.k;
                                let spring_set = self.algo_edge_set;
                                self.settings.running = false;
                                self.begin_job(Progress::default(), move |p| {
                                    p.set("Computing layout")?;
                                    let graph = if let Some(kind) = kind {
                                        Arc::new(doc.graph_for_kind(&kind))
                                    } else {
                                        graph
                                    };
                                    let n = graph.num_nodes() as usize;
                                    let mut pos = vec![[0.0; 2]; n];
                                    let layout: Box<dyn Layout> = match seed {
                                        Seed::Hierarchical => Box::new(LayeredLayout {
                                            target_height: k * (n.max(1) as f32).sqrt(),
                                            aspect: 1.8,
                                            sweeps: 4,
                                        }),
                                        Seed::Radial => Box::new(RadialLayout {
                                            radius: k * (n.max(1) as f32).sqrt() * 0.6,
                                            sweeps: 4,
                                        }),
                                        Seed::Random => Box::new(RandomLayout {
                                            extent: k * (n.max(1) as f32).sqrt(),
                                        }),
                                        Seed::Grid => Box::new(GridLayout { spacing: k }),
                                        Seed::Circle => Box::new(CircleLayout {
                                            radius: k * (n.max(1) as f32).sqrt() * 0.5,
                                        }),
                                    };
                                    layout.place(&graph, &mut pos, 42);
                                    p.check()?;
                                    graph.ensure_csr();
                                    Ok(JobResult::Layout(
                                        pos,
                                        matches!(seed, Seed::Random | Seed::Grid | Seed::Circle),
                                        graph,
                                        spring_set,
                                    ))
                                });
                            }
                            CapturePurpose::Undo | CapturePurpose::Redo => {
                                let redo = matches!(c.purpose, CapturePurpose::Redo);
                                let target = if redo {
                                    self.work.redo.pop()
                                } else {
                                    self.work.undo.pop()
                                };
                                if let Some(target) = target {
                                    let current = UndoEntry {
                                        positions: target
                                            .positions
                                            .as_ref()
                                            .map(|_| project.positions),
                                        view: c.view,
                                    };
                                    if redo {
                                        self.work.undo.push(current);
                                        self.trim_undo();
                                    } else {
                                        self.work.redo.push(current);
                                    }
                                    if let (Some(l), Some(positions)) =
                                        (self.live.as_mut(), target.positions)
                                    {
                                        l.graph_gpu.set_positions(&l.gpu.queue, &positions);
                                        l.renderer.mark_scene_dirty();
                                    }
                                    self.apply_view(target.view);
                                    self.start_color_job();
                                    self.mark_dirty();
                                }
                            }
                        }
                    }
                }
            }
        }
        if let Some(job) = &self.work.search_job {
            if let Ok((query, results)) = job.receiver.try_recv() {
                self.work.search_job = None;
                if query == self.work.query {
                    self.work.searched = query;
                    self.work.search_results = results;
                }
            }
        }
        if self.work.search_due.is_some_and(|t| Instant::now() >= t)
            && self.work.search_job.is_none()
        {
            self.work.search_due = None;
            let query = self.work.query.clone();
            let doc = self.work.document.clone();
            let p = Progress::default();
            let wp = p.clone();
            let (tx, rx) = mpsc::channel();
            std::thread::spawn(move || {
                let needle = query.to_lowercase();
                let mut results = Vec::new();
                for (i, n) in doc.nodes.iter().enumerate() {
                    if i % 4096 == 0 && wp.check().is_err() {
                        break;
                    }
                    if needle.is_empty()
                        || n.id.to_lowercase().contains(&needle)
                        || n.label.to_lowercase().contains(&needle)
                        || n.attributes.iter().any(|(k, v)| {
                            k.to_lowercase().contains(&needle)
                                || document::display(v).to_lowercase().contains(&needle)
                        })
                    {
                        results.push(i as u32);
                    }
                }
                let _ = tx.send((query, results));
            });
            self.work.search_job = Some(SearchJob {
                receiver: rx,
                progress: p,
            });
        }
        if !self.work.busy() {
            if let Some((set, running)) = self.work.spring_due.take() {
                let doc = self.work.document.clone();
                let graph = self.graph.clone();
                let kind = set
                    .and_then(|i| self.edge_types.get(i))
                    .map(|t| t.name.clone());
                self.begin_job(Progress::default(), move |p| {
                    p.set("Restoring force relationships")?;
                    let graph = kind
                        .map(|k| Arc::new(doc.graph_for_kind(&k)))
                        .unwrap_or(graph);
                    graph.ensure_csr();
                    Ok(JobResult::Springs(graph, set, running))
                });
            }
        }
        if !self.work.busy() && self.work.applied_color != Some(self.color_mode) {
            self.start_color_job();
        }
        if !self.work.busy() {
            if let Some(seed) = self.work.initial_layout.take() {
                self.start_layout_job(seed);
            }
        }
        if self.work.filter_due.is_some_and(|t| Instant::now() >= t) && !self.work.busy() {
            self.start_filter_job();
        }
        if self.work.dirty
            && !self.work.busy()
            && self.work.last_autosave.elapsed() > Duration::from_secs(60)
        {
            match std::fs::create_dir_all(data_dir()) {
                Ok(()) => {
                    self.capture(CapturePurpose::Save(
                        data_dir().join("recovery.nebula"),
                        true,
                    ));
                    self.work.last_autosave = Instant::now();
                }
                Err(e) => {
                    self.work.error = Some(format!("Recovery directory unavailable: {e}"));
                    self.work.last_autosave = Instant::now();
                }
            }
        }
        if self.work.close_requested {
            self.work.close_requested = false;
            if self.work.dirty || self.work.busy() {
                self.work.close_confirm = true;
            } else {
                self.should_exit = true;
            }
        }
    }
    fn finish_job(&mut self, result: JobResult) {
        match result {
            JobResult::Preview(path, preview) => {
                self.work.csv = Some((path, preview));
            }
            JobResult::Open {
                doc,
                graph,
                index,
                positions,
                project,
                path,
            } => {
                if project.is_none()
                    && path != Path::new("Sample graph")
                    && self.opts.max_frames.is_none()
                {
                    self.work.csv = None;
                    self.work.pending_import = Some(Box::new(JobResult::Open {
                        doc,
                        graph,
                        index,
                        positions,
                        project,
                        path,
                    }));
                    return;
                }
                if let Err(e) = self.install_document(doc, graph, index, positions, project) {
                    self.work.error = Some(format!("Could not open graph: {e:#}"));
                    return;
                }
                self.work.path = Some(path.clone());
                self.remember(&path);
                self.opts.title = format!(
                    "nebula · {}",
                    path.file_name().unwrap_or_default().to_string_lossy()
                );
                self.update_title();
                self.work.search_due = Some(Instant::now());
                self.work.dirty = path.extension().is_none_or(|e| e != "nebula");
            }
            JobResult::Colors(mode, colors, sizes, values, name) => {
                self.work.applied_color = Some(mode);
                if mode != self.color_mode {
                    self.start_color_job();
                    return;
                }
                self.base_colors = colors;
                self.base_sizes =
                    sizes.unwrap_or_else(|| vec![1.0; self.graph.num_nodes() as usize]);
                self.last_values = values;
                self.value_name = name;
                self.legend = match mode {
                    ColorMode::Uniform => Legend::Uniform,
                    ColorMode::Degree | ColorMode::PageRank => {
                        Legend::scalar(&self.last_values, &self.value_name, true)
                    }
                    ColorMode::Attribute(i) => {
                        let key = self.attr_keys.get(i).cloned().unwrap_or_default();
                        let numeric = self
                            .work
                            .document
                            .nodes
                            .iter()
                            .filter_map(|n| n.attributes.get(&key))
                            .all(|v| document::display(v).parse::<f32>().is_ok());
                        if numeric {
                            Legend::scalar(&self.last_values, &key, false)
                        } else {
                            Legend::categorical(&self.last_values, &key)
                        }
                    }
                    _ => Legend::categorical(&self.last_values, &self.value_name),
                };
                self.push_filtered();
            }
            JobResult::Springs(graph, set, running) => {
                if let Some(live) = self.live.as_mut() {
                    live.graph_gpu.set_csr(&live.gpu.device, graph.csr());
                    live.layout.rebind(&live.gpu.device, &live.graph_gpu);
                }
                self.spring_edge_set = set;
                self.settings.running = running;
            }
            JobResult::Layout(pos, running, graph, spring_set) => {
                if running {
                    if let Some(live) = self.live.as_mut() {
                        live.graph_gpu.set_csr(&live.gpu.device, graph.csr());
                        live.layout.rebind(&live.gpu.device, &live.graph_gpu);
                    }
                    self.spring_edge_set = spring_set;
                }
                if let Some(l) = self.live.as_mut() {
                    l.graph_gpu.set_positions(&l.gpu.queue, &pos);
                    l.renderer.mark_scene_dirty();
                }
                let (min, max) = bounds(&pos);
                self.fit_canvas_bounds(min, max);
                self.settings.running = running;
                self.settings.alpha = 1.0;
                self.mark_dirty();
            }
            JobResult::Mask(mask) => {
                self.filter_match_count = Some(mask.iter().filter(|v| **v).count());
                self.work.mask = Some(Arc::new(mask));
                self.push_filtered();
            }
            JobResult::Saved(path, auto, revision) => {
                if !auto {
                    self.work.path = Some(path.clone());
                    self.work.dirty = self.work.revision != revision;
                    self.remember(&path);
                    self.work.notice = Some(format!("Saved {}", path.display()));
                    if !self.work.dirty {
                        let _ = std::fs::remove_file(data_dir().join("recovery.nebula"));
                    }
                    if self.work.pending_save_close {
                        self.work.pending_save_close = false;
                        if self.work.dirty {
                            self.work.close_confirm = true;
                        } else {
                            self.should_exit = true;
                        }
                    }
                    if !self.work.dirty {
                        if let Some(next) = self.work.pending_open.take() {
                            self.open_now(next);
                        }
                    }
                }
            }
            JobResult::Exported(path) => {
                self.work.notice = Some(format!("Exported {}", path.display()))
            }
        }
    }
    fn install_document(
        &mut self,
        doc: Arc<Document>,
        graph: Arc<Graph>,
        index: Arc<document::EdgeIndex>,
        positions: Vec<Pos>,
        project: Option<Project>,
    ) -> anyhow::Result<()> {
        use anyhow::Context;
        let live = self.live.as_ref().context("GPU is unavailable")?;
        let attributes: Vec<Vec<(String, String)>> = doc
            .nodes
            .iter()
            .map(|n| {
                n.attributes
                    .iter()
                    .map(|(k, v)| (k.clone(), document::display(v)))
                    .collect()
            })
            .collect();
        let keys = attribute_keys(Some(&attributes));
        let mut typed = std::collections::BTreeMap::<String, Vec<[u32; 2]>>::new();
        for e in &doc.edges {
            typed
                .entry(e.kind.clone())
                .or_default()
                .push([e.source, e.target]);
        }
        if typed.is_empty() {
            typed.insert("edges".into(), Vec::new());
        }
        let edge_types: Vec<EdgeType> = typed
            .into_iter()
            .map(|(name, edges)| EdgeType {
                color: doc.type_colors.get(&name).copied(),
                name,
                edges,
                visible: true,
            })
            .collect();
        let view = project
            .as_ref()
            .map(|p| serde_json::from_value::<View>(p.view.clone()))
            .transpose()
            .context("Invalid project view")?;
        if let Some(v) = &view {
            v.validate(&doc, keys.len(), edge_types.len())?;
        }
        let k = view.as_ref().map(|v| v.settings.k).unwrap_or(self.opts.k);
        let gpu_bytes = GpuGraph::validate(&graph, &positions, k, &live.gpu.device.limits())?;
        let mut memory = sysinfo::System::new();
        memory.refresh_memory();
        anyhow::ensure!(gpu_bytes.saturating_mul(2)<memory.available_memory(),"Graph needs approximately {} MiB for GPU upload and scratch, but only {} MiB is available. Close other applications or import a smaller subgraph.",gpu_bytes.saturating_mul(2)/1048576,memory.available_memory()/1048576);
        live.gpu
            .device
            .push_error_scope(wgpu::ErrorFilter::OutOfMemory);
        live.gpu
            .device
            .push_error_scope(wgpu::ErrorFilter::Validation);
        let graph_gpu = GpuGraph::upload(&live.gpu.device, &live.gpu.queue, &graph, &positions, k);
        let mut renderer = Renderer::new(&live.gpu.device, live.gpu.config.format, &graph_gpu);
        let mut settings = LayoutSettings {
            k,
            ..self.opts.settings
        };
        settings.set_cooling_for(graph.num_nodes());
        settings.running = graph.num_nodes() > 0;
        let layout = LayoutGpu::new(&live.gpu.device, &graph_gpu, &settings);
        let density = Density::new(&live.gpu.device, live.gpu.config.format, &graph_gpu);
        let specs: Vec<_> = edge_types
            .iter()
            .map(|t| crate::render::EdgeTypeInput {
                name: &t.name,
                color: t.color,
                visible: t.visible,
                edges: &t.edges,
            })
            .collect();
        renderer.set_edge_types(&live.gpu.device, &graph_gpu, &specs);
        let labels = crate::labels::Labels::new(&live.gpu, &graph_gpu);
        let scene_bounds = crate::labels::Bounds::new(&live.gpu, &graph_gpu);
        let validation = pollster::block_on(live.gpu.device.pop_error_scope());
        let allocation = pollster::block_on(live.gpu.device.pop_error_scope());
        if let Some(error) = validation.or(allocation) {
            anyhow::bail!("GPU could not load this graph: {error}. Try a smaller graph.");
        }
        let live = self.live.as_mut().unwrap();
        live.graph_gpu = graph_gpu;
        live.renderer = renderer;
        live.layout = layout;
        live.density = density;
        live.labels = labels;
        live.bounds = scene_bounds;
        self.work.index = index;
        self.work.document = doc;
        self.graph = graph;
        self.seed_positions = positions;
        self.labels = Some(
            self.work
                .document
                .nodes
                .iter()
                .map(|n| n.label.clone())
                .collect(),
        );
        self.node_attrs = Some(attributes);
        self.attr_keys = keys;
        self.edge_types = edge_types;
        self.settings = settings;
        self.opts.k = k;
        self.color_mode = ColorMode::Uniform;
        self.filter = NodeFilter::default();
        self.algo_edge_set = None;
        self.spring_edge_set = None;
        self.work.extra_filters.clear();
        self.work.min_degree = 0;
        self.work.restrict = None;
        self.work.mask = None;
        self.work.undo.clear();
        self.work.redo.clear();
        self.work.history.clear();
        self.work.future.clear();
        self.work.search_results.clear();
        self.work.query.clear();
        self.work.searched.clear();
        self.work.csv = None;
        if let Some(j) = self.work.search_job.take() {
            j.progress.cancel();
        }
        self.work.selection_capture = None;
        self.work.pending_focus = false;
        self.selected = None;
        self.selected_pos = None;
        self.neighbor_positions.clear();
        self.highlight = None;
        self.pinned_highlight = None;
        self.total_steps = 0;
        self.steps_at_fps_mark = 0;
        self.fps_timer = Instant::now();
        self.frame_count = 0;
        self.last_frame = Instant::now();
        self.next_sim_at = Instant::now();
        self.show_density = self.graph.num_nodes() > 2_000_000;
        let (min, max) = bounds(&self.seed_positions);
        self.fit_canvas_bounds(min, max);
        self.base_colors = vec![pack_rgba(120, 170, 255, 255); self.graph.num_nodes() as usize];
        self.base_sizes = vec![1.0; self.graph.num_nodes() as usize];
        if let Some(view) = view {
            self.apply_view(view);
        }
        if project.is_none() {
            self.work.fit_after_ui = true;
            self.color_mode = self.opts.color_mode;
            self.show_labels = self.opts.show_labels;
            self.show_density |= self.opts.aggregate;
            self.selected = self
                .opts
                .select
                .filter(|i| (*i as usize) < self.work.document.nodes.len());
            if let Some(key) = &self.opts.start_attribute {
                if let Some(i) = self.attr_keys.iter().position(|k| k == key) {
                    self.color_mode = ColorMode::Attribute(i);
                } else {
                    self.work.error = Some(format!("Color attribute '{key}' was not found"));
                }
            }
            if let Some(spec) = &self.opts.start_filter {
                let parts: Vec<_> = spec.splitn(3, ':').collect();
                if parts.len() == 3 {
                    if let (Some(key), Some(op)) = (
                        self.attr_keys.iter().position(|k| k == parts[0]),
                        FilterOp::from_name(parts[1]),
                    ) {
                        self.filter = NodeFilter {
                            enabled: true,
                            key,
                            op,
                            value: parts[2].into(),
                        };
                        self.work.filter_due = Some(Instant::now());
                    }
                }
            }
            if !self.opts.settings.running {
                self.settings.running = false;
            }
            self.algo_edge_set = self
                .opts
                .compute_over
                .as_ref()
                .and_then(|name| self.edge_types.iter().position(|t| &t.name == name));
            self.work.initial_layout = if self.opts.start_radial {
                Some(Seed::Radial)
            } else if self.opts.start_hierarchical {
                Some(Seed::Hierarchical)
            } else {
                None
            };
        }
        self.work.applied_color = None;
        self.start_color_job();
        Ok(())
    }
    pub(super) fn workspace_ui(&mut self, ctx: &egui::Context) {
        let busy = self.work.busy();
        let has_graph = !self.work.document.nodes.is_empty();
        let mut open = false;
        let mut recent = None;
        let mut save = false;
        let mut save_as = false;
        let mut png = false;
        let mut export = None;
        let mut undo = false;
        let mut redo = false;
        let mut sample = false;
        egui::TopBottomPanel::top("document_menu").show(ctx,|ui|{
            ui.horizontal(|ui|{
                ui.menu_button("File",|ui|{
                    if ui.add_enabled(!busy,egui::Button::new("Open…  ⌘/Ctrl O")).clicked(){open=true;ui.close();}
                    ui.menu_button("Recent files",|ui|{for path in &self.work.preferences.recent{if ui.add_enabled(!busy,egui::Button::new(path.to_string_lossy())).clicked(){recent=Some(path.clone());ui.close();}}});
                    if let Some(path) = &self.work.recovery {
                        if ui.add_enabled(!busy, egui::Button::new("Recover unsaved session…")).clicked() {
                            recent = Some(path.clone());
                            ui.close();
                        }
                    }
                    if ui.add_enabled(!busy && has_graph,egui::Button::new("Save project  ⌘/Ctrl S")).clicked(){save=true;ui.close();}
                    if ui.add_enabled(!busy && has_graph,egui::Button::new("Save project as…")).clicked(){save_as=true;ui.close();}
                    ui.separator();
                    if ui.add_enabled(!busy && has_graph,egui::Button::new("Export image…")).clicked(){png=true;ui.close();}
                    ui.checkbox(&mut self.work.clean_export,"Image without controls");
                    if ui.add_enabled(!busy && has_graph,egui::Button::new("Export entire graph…")).clicked(){export=Some(false);ui.close();}
                    if ui.add_enabled(!busy && has_graph,egui::Button::new("Export visible subgraph…")).clicked(){export=Some(true);ui.close();}
                });
                ui.menu_button("Edit",|ui|{
                    if ui.add_enabled(!busy&&!self.work.undo.is_empty(),egui::Button::new("Undo  ⌘/Ctrl Z")).clicked(){undo=true;ui.close();}
                    if ui.add_enabled(!busy&&!self.work.redo.is_empty(),egui::Button::new("Redo  ⇧⌘/Ctrl Z")).clicked(){redo=true;ui.close();}
                });
                ui.menu_button("Help",|ui|{
                    ui.label("Open a graph → search a node → explore its neighbors → save your project.");
                    ui.label("Drag the canvas to pan. Scroll to zoom. Click a node to inspect it.");
                    ui.label("Pause before precise inspection. Fit view resets the camera.");
                    ui.label("Color and filters reveal structure. Relationship visibility only affects drawing.");
                    ui.label("Layout and color algorithms use the selected ‘compute over’ relationship set.");
                    ui.label("Force layout uses undirected attraction; PageRank uses edge direction and weight.");
                    ui.label("Undo restores layouts, camera, filters and appearance. History is bounded to 128 MiB / 12 layouts (at least one).");
                    if ui.button("Copy diagnostics").clicked(){let gpu=self.live.as_ref().map(|l|format!("{:?}\n{:?}",l.gpu.adapter.get_info(),l.gpu.device.limits())).unwrap_or_default();ui.ctx().copy_text(format!("Nebula {}\n{} nodes / {} edges\n{}",env!("CARGO_PKG_VERSION"),self.graph.num_nodes(),self.graph.num_edges(),gpu));}
                });
                if self.work.dirty{ui.label("● Unsaved changes");}
                if let Some(job)=&self.work.job{ui.spinner();ui.label(job.progress.stage.lock().unwrap().clone());if ui.button("Cancel").clicked(){job.progress.cancel();}}
                if self.work.capture.is_some(){ui.spinner();ui.label("Capturing layout…");}
            });
        });
        if open {
            if let Some(path) = rfd::FileDialog::new()
                .add_filter(
                    "Graphs",
                    &[
                        "nebula", "json", "csv", "tsv", "graphml", "gexf", "dot", "gv", "gml",
                        "mtx", "gr", "txt", "edges", "el",
                    ],
                )
                .pick_file()
            {
                self.open_path(path);
            }
        }
        if let Some(path) = recent.take() {
            self.open_path(path);
        }
        if save {
            self.save_project(false);
        }
        if save_as {
            self.save_project(true);
        }
        if undo {
            self.undo_redo(false);
        }
        if redo {
            self.undo_redo(true);
        }
        if let Some(visible) = export {
            self.export_graph(visible);
        }
        if png {
            if let Some(path) = rfd::FileDialog::new()
                .add_filter("PNG image", &["png"])
                .set_file_name("graph.png")
                .save_file()
            {
                self.work.notice = None;
                self.work.pending_png = Some(path);
            }
        }
        if !has_graph {
            egui::Window::new("Welcome to Nebula")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.heading("Explore your graph");
                    ui.label("Drop a graph file anywhere, or choose Open from the File menu.");
                    ui.label(
                        "CSV/TSV, JSON, GraphML, GEXF, DOT, GML, Matrix Market, and edge lists.",
                    );
                    ui.add_space(8.0);
                    if ui
                        .add_enabled(!busy, egui::Button::new("Try a sample graph"))
                        .clicked()
                    {
                        sample = true;
                    }

                });
        }
        if let Some(path) = recent.take() {
            self.open_now(path);
        }
        if sample {
            self.begin_job(Progress::default(), move |p| {
                p.set("Creating sample")?;
                let graph = nebula_core::generate::stochastic_blocks(1200, 5, 6000, 0.03, 42);
                let doc = Arc::new(Document {
                    nodes: (0..1200)
                        .map(|i| document::Node {
                            id: format!("node-{i}"),
                            label: format!("Sample node {i}"),
                            attributes: std::collections::BTreeMap::from([(
                                "group".into(),
                                serde_json::json!(i / 240),
                            )]),
                        })
                        .collect(),
                    edges: graph
                        .edges()
                        .iter()
                        .map(|e| document::Edge {
                            source: e[0],
                            target: e[1],
                            directed: false,
                            weight: 1.0,
                            kind: "connections".into(),
                            attributes: Default::default(),
                        })
                        .collect(),
                    ..Document::default()
                });
                let graph = Arc::new(doc.graph());
                graph.ensure_csr();
                let mut positions = vec![[0.0; 2]; 1200];
                RandomLayout { extent: 1200.0 }.place(&graph, &mut positions, 42);
                let index = Arc::new(document::EdgeIndex::new(&doc));
                Ok(JobResult::Open {
                    doc,
                    graph,
                    index,
                    positions,
                    project: None,
                    path: PathBuf::from("Sample graph"),
                })
            });
        }
        self.csv_ui(ctx);
        if self.work.pending_import.is_some() {
            let mut accept = false;
            let mut cancel = false;
            if let Some(JobResult::Open { doc, path, .. }) = self.work.pending_import.as_deref() {
                egui::Window::new("Graph ready to open").collapsible(false).show(ctx,|ui|{
                    ui.label(path.to_string_lossy());ui.label(format!("{} nodes · {} edges",doc.nodes.len(),doc.edges.len()));
                    ui.label("Original IDs, isolated nodes, self-loops, parallel edges, attributes and weights are retained by the metadata-preserving importers.");
                    for warning in &doc.warnings{ui.colored_label(egui::Color32::YELLOW,warning);}
                    ui.horizontal(|ui|{if ui.button("Open graph").clicked(){accept=true;}if ui.button("Cancel").clicked(){cancel=true;}});
                });
            }
            if accept {
                if let Some(result) = self.work.pending_import.take() {
                    if let JobResult::Open {
                        doc,
                        graph,
                        index,
                        positions,
                        project,
                        path,
                    } = *result
                    {
                        match self.install_document(doc, graph, index, positions, project) {
                            Ok(()) => {
                                self.work.path = Some(path.clone());
                                self.remember(&path);
                                self.opts.title = format!(
                                    "nebula · {}",
                                    path.file_name().unwrap_or_default().to_string_lossy()
                                );
                                self.update_title();
                                self.work.search_due = Some(Instant::now());
                                self.mark_dirty();
                            }
                            Err(e) => {
                                self.work.error = Some(format!("Could not open graph: {e:#}"))
                            }
                        }
                    }
                }
            }
            if cancel {
                self.work.pending_import = None;
            }
        }
        if has_graph {
            self.explorer_ui(ctx);
        }
        let mut dismiss = false;
        if let Some(error) = self.work.error.clone() {
            egui::Window::new("Operation could not finish")
                .collapsible(false)
                .show(ctx, |ui| {
                    ui.label(error);
                    ui.label("Your current graph remains available.");
                    if ui.button("Dismiss").clicked() {
                        dismiss = true;
                    }
                });
        }
        if dismiss {
            self.work.error = None;
        }
        if let Some(notice) = &self.work.notice {
            egui::TopBottomPanel::bottom("notice").show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(notice);
                    if ui.small_button("Dismiss").clicked() {
                        dismiss = true;
                    }
                });
            });
        }
        if dismiss {
            self.work.notice = None;
        }
        if self.work.pending_open.is_some() {
            let mut discard = false;
            let mut cancel = false;
            let mut save = false;
            egui::Window::new("Save before opening another graph?")
                .collapsible(false)
                .show(ctx, |ui| {
                    ui.label("The current graph has unsaved changes.");
                    if ui.button("Save current graph").clicked() {
                        save = true;
                    }
                    if ui.button("Discard changes and open").clicked() {
                        discard = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            if save {
                self.save_project(false);
            }
            if discard {
                self.work.dirty = false;
                if let Some(path) = self.work.pending_open.take() {
                    self.open_now(path);
                }
            }
            if cancel {
                self.work.pending_open = None;
            }
        }
        if self.work.close_confirm {
            let mut discard = false;
            let mut cancel = false;
            let mut save = false;
            egui::Window::new("Close Nebula?")
                .collapsible(false)
                .show(ctx, |ui| {
                    ui.label("Save the current graph before closing.");
                    if ui
                        .add_enabled(!busy, egui::Button::new("Save and close"))
                        .clicked()
                    {
                        save = true;
                    }
                    if ui.button("Close without saving").clicked() {
                        discard = true;
                    }
                    if ui.button("Keep working").clicked() {
                        cancel = true;
                    }
                });
            if save {
                self.work.pending_save_close = true;
                self.save_project(false);
            }
            if discard {
                if let Some(j) = &self.work.job {
                    j.progress.cancel();
                }
                self.should_exit = true;
                let _ = std::fs::remove_file(data_dir().join("recovery.nebula"));
            }
            if cancel {
                self.work.close_confirm = false;
            }
        }
    }
    fn csv_ui(&mut self, ctx: &egui::Context) {
        let Some((path, mut preview)) = self.work.csv.clone() else {
            return;
        };
        let old = preview.options.clone();
        let mut import = false;
        let mut cancel = false;
        let mut node_file = false;
        egui::Window::new("Import table").default_width(700.0).collapsible(false).show(ctx,|ui|{
            ui.label(path.to_string_lossy());ui.horizontal(|ui|{ui.checkbox(&mut preview.options.headers,"First row is a header");egui::ComboBox::from_id_salt("delimiter").selected_text(match preview.options.delimiter{b'\t'=>"Tab",b';'=>"Semicolon",_=>"Comma"}).show_ui(ui,|ui|{ui.selectable_value(&mut preview.options.delimiter,b',',"Comma");ui.selectable_value(&mut preview.options.delimiter,b'\t',"Tab");ui.selectable_value(&mut preview.options.delimiter,b';',"Semicolon");});ui.checkbox(&mut preview.options.directed,"Directed edges");});
            ui.horizontal(|ui|{column_choice(ui,"Source",&preview.columns,&mut preview.options.source);column_choice(ui,"Target",&preview.columns,&mut preview.options.target);});
            ui.horizontal(|ui|{optional_column(ui,"Weight",&preview.columns,&mut preview.options.weight);optional_column(ui,"Relationship type",&preview.columns,&mut preview.options.kind);});
            ui.label("Remaining columns become typed edge attributes. Self-loops and duplicate edges are retained.");
            egui::ScrollArea::horizontal().max_height(180.0).show(ui,|ui|{egui::Grid::new("csv_preview").striped(true).show(ui,|ui|{for name in &preview.columns{ui.strong(name);}ui.end_row();for row in &preview.rows{for cell in row{ui.label(cell);}ui.end_row();}});});
            ui.separator();if ui.button("Add separate node table…").clicked(){node_file=true;}
            if let Some(node_path)=preview.options.node_file.clone(){ui.label(node_path.to_string_lossy());
                match document::preview_csv(&node_path,preview.options.clone()){Ok(nodes)=>{ui.horizontal(|ui|{column_choice(ui,"Node ID",&nodes.columns,&mut preview.options.node_id);optional_column(ui,"Node label",&nodes.columns,&mut preview.options.node_label);});},Err(e)=>{ui.colored_label(egui::Color32::RED,e.to_string());}}
                if ui.small_button("Remove node table").clicked(){preview.options.node_file=None;}
                ui.label("Node table uses the same delimiter/header setting. All edge endpoints must exist in it.");
            }
            ui.label("Input must be UTF-8. Invalid rows stop the import with a row error; data is never silently skipped.");
            if let Some(error)=&self.work.csv_error{ui.colored_label(egui::Color32::RED,error);}
            ui.horizontal(|ui|{if ui.add_enabled(!self.work.busy() && preview.options.source!=preview.options.target,egui::Button::new("Import graph")).clicked(){import=true;}if ui.button("Cancel").clicked(){cancel=true;}});
        });
        if node_file {
            preview.options.node_file = rfd::FileDialog::new()
                .add_filter("Node table", &["csv", "tsv"])
                .pick_file();
        }
        if old.delimiter != preview.options.delimiter || old.headers != preview.options.headers {
            match document::preview_csv(&path, preview.options.clone()) {
                Ok(p) => {
                    preview = p;
                    self.work.csv_error = None;
                }
                Err(e) => self.work.csv_error = Some(format!("{e:#}")),
            }
        }
        self.work.csv = Some((path.clone(), preview.clone()));
        if cancel {
            self.work.csv = None;
            self.work.csv_error = None;
        }
        if import {
            self.import_path(path, Some(preview.options));
        }
    }
    fn explorer_ui(&mut self, ctx: &egui::Context) {
        let mut select = None;
        let mut focus = false;
        let mut isolate = None;
        let mut restore = false;
        let mut back = false;
        let mut forward = false;
        let mut filter_changed = false;
        let old_extra_filters = self.work.extra_filters.clone();
        let old_min_degree = self.work.min_degree;
        egui::SidePanel::right("explorer")
            .resizable(true)
            .default_width(310.0)
            .show(ctx, |ui| {
                ui.heading("Explore");
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(!self.work.history.is_empty(), egui::Button::new("← Back"))
                        .clicked()
                    {
                        back = true;
                    }
                    if ui
                        .add_enabled(!self.work.future.is_empty(), egui::Button::new("Forward →"))
                        .clicked()
                    {
                        forward = true;
                    }
                });
                let response = ui.add(
                    egui::TextEdit::singleline(&mut self.work.query)
                        .hint_text("Search ID, label or attributes"),
                );
                if response.changed() {
                    self.work.search_due = Some(Instant::now() + Duration::from_millis(200));
                    if let Some(job) = &self.work.search_job {
                        job.progress.cancel();
                    }
                }
                let pending = self.work.query != self.work.searched;
                if pending {
                    ui.label("Searching…");
                }
                let count = if pending && !self.work.query.is_empty() {
                    0
                } else if self.work.query.is_empty() {
                    self.work.document.nodes.len()
                } else {
                    self.work.search_results.len()
                };
                ui.label(format!("{count} nodes"));
                egui::ScrollArea::vertical()
                    .id_salt("nodes_table")
                    .max_height(200.0)
                    .show_rows(ui, 22.0, count, |ui, rows| {
                        for row in rows {
                            let i = if self.work.query.is_empty() {
                                row as u32
                            } else {
                                self.work.search_results[row]
                            };
                            let node = &self.work.document.nodes[i as usize];
                            let visible = self.work.mask.as_ref().is_none_or(|m| m[i as usize]);
                            let text = if visible {
                                format!("{}  ·  {}", node.label, node.id)
                            } else {
                                format!("{}  ·  {} (hidden)", node.label, node.id)
                            };
                            if ui
                                .selectable_label(self.selected == Some(i), text)
                                .clicked()
                            {
                                select = Some(i);
                                focus = true;
                            }
                        }
                    });
                ui.separator();
                if let Some(id) = self.selected {
                    let node = &self.work.document.nodes[id as usize];
                    ui.strong(&node.label);
                    ui.label(format!("ID: {}", node.id));
                    ui.horizontal(|ui| {
                        if ui.button("Copy ID").clicked() {
                            ui.ctx().copy_text(node.id.clone());
                        }
                        if ui.button("Copy metadata").clicked() {
                            ui.ctx()
                                .copy_text(serde_json::to_string_pretty(node).unwrap_or_default());
                        }
                    });
                    egui::ScrollArea::vertical()
                        .id_salt("node_metadata")
                        .max_height(130.0)
                        .show(ui, |ui| {
                            for (key, value) in &node.attributes {
                                ui.label(format!("{key}: {}", document::display(value)));
                            }
                        });
                    ui.horizontal(|ui| {
                        if ui.button("Focus").clicked() {
                            focus = true;
                        }
                        if ui.button("Only this node").clicked() {
                            isolate = Some((0, false));
                        }
                        if ui.button("Hide node").clicked() {
                            isolate = Some((0, true));
                        }
                    });
                    ui.horizontal(|ui| {
                        if ui.button("1-hop neighborhood").clicked() {
                            isolate = Some((1, false));
                        }
                        if ui.button("2 hops").clicked() {
                            isolate = Some((2, false));
                        }
                    });
                    for (name, csr) in [
                        ("Outgoing", self.graph.outgoing()),
                        ("Incoming", self.graph.incoming()),
                    ] {
                        let neighbors = csr.neighbors(id);
                        ui.collapsing(format!("{name} ({})", neighbors.len()), |ui| {
                            egui::ScrollArea::vertical()
                                .id_salt(name)
                                .max_height(110.0)
                                .show_rows(ui, 20.0, neighbors.len(), |ui, rows| {
                                    for i in rows {
                                        let nb = neighbors[i];
                                        let n = &self.work.document.nodes[nb as usize];
                                        if ui
                                            .selectable_label(
                                                false,
                                                format!("{} · {}", n.label, n.id),
                                            )
                                            .clicked()
                                        {
                                            select = Some(nb);
                                            focus = true;
                                        }
                                    }
                                });
                        });
                    }
                } else {
                    ui.label("Click a node or a search result to inspect its relationships.");
                }
                if let Some(id) = self.selected {
                    let incident = self.work.index.at(id);
                    ui.collapsing(format!("Relationships ({})", incident.len()), |ui| {
                        egui::ScrollArea::vertical()
                            .id_salt("relationship_metadata")
                            .max_height(160.0)
                            .show_rows(ui, 24.0, incident.len(), |ui, rows| {
                                for row in rows {
                                    let edge = &self.work.document.edges[incident[row]];
                                    let other = if edge.source == id {
                                        edge.target
                                    } else {
                                        edge.source
                                    };
                                    let arrow = if !edge.directed {
                                        "↔"
                                    } else if edge.source == id {
                                        "→"
                                    } else {
                                        "←"
                                    };
                                    let neighbor = &self.work.document.nodes[other as usize];
                                    if ui
                                        .selectable_label(
                                            false,
                                            format!(
                                                "{arrow} {} · {} · weight {}",
                                                neighbor.label, edge.kind, edge.weight
                                            ),
                                        )
                                        .on_hover_text(
                                            serde_json::to_string_pretty(&edge.attributes)
                                                .unwrap_or_default(),
                                        )
                                        .clicked()
                                    {
                                        select = Some(other);
                                        focus = true;
                                    }
                                }
                            });
                        ui.label("Hover a relationship to inspect its attributes.");
                    });
                }
                if ui.button("Restore all nodes").clicked() {
                    restore = true;
                }
                ui.separator();
                ui.collapsing("Additional filters (AND)", |ui| {
                    filter_changed |= ui
                        .add(
                            egui::DragValue::new(&mut self.work.min_degree)
                                .prefix("Minimum degree: "),
                        )
                        .changed();
                    let mut remove = None;
                    for (i, f) in self.work.extra_filters.iter_mut().enumerate() {
                        ui.push_id(i, |ui| {
                            ui.horizontal(|ui| {
                                filter_changed |= ui.checkbox(&mut f.enabled, "").changed();
                                egui::ComboBox::from_id_salt("key")
                                    .selected_text(
                                        self.attr_keys
                                            .get(f.key)
                                            .map(String::as_str)
                                            .unwrap_or("Attribute"),
                                    )
                                    .show_ui(ui, |ui| {
                                        for (j, key) in self.attr_keys.iter().enumerate() {
                                            filter_changed |=
                                                ui.selectable_value(&mut f.key, j, key).changed();
                                        }
                                    });
                                egui::ComboBox::from_id_salt("op")
                                    .selected_text(f.op.label())
                                    .show_ui(ui, |ui| {
                                        for op in FilterOp::ALL {
                                            filter_changed |= ui
                                                .selectable_value(&mut f.op, op, op.label())
                                                .changed();
                                        }
                                    });
                                if ui.small_button("×").clicked() {
                                    remove = Some(i);
                                }
                            });
                            filter_changed |= ui.text_edit_singleline(&mut f.value).changed();
                        });
                    }
                    if let Some(i) = remove {
                        self.work.extra_filters.remove(i);
                        filter_changed = true;
                    }
                    if !self.attr_keys.is_empty() && ui.button("Add attribute filter").clicked() {
                        self.work.extra_filters.push(NodeFilter {
                            enabled: true,
                            ..NodeFilter::default()
                        });
                        filter_changed = true;
                    }
                });
                if !self.work.document.warnings.is_empty() {
                    ui.collapsing("Import notes", |ui| {
                        for w in &self.work.document.warnings {
                            ui.label(w);
                        }
                    });
                }
            });
        if back {
            self.navigate(false);
        }
        if forward {
            self.navigate(true);
        }
        if let Some(id) = select {
            self.select_node(Some(id));
        }
        if focus {
            if let Some(id) = self.selected {
                self.refresh_selection(id);
            }
            self.focus_selected();
        }
        if let Some((hops, hide)) = isolate {
            self.isolate(hops, hide);
        }
        if restore {
            self.record_view_undo();
            self.work.restrict = None;
            self.filter.enabled = false;
            self.work.extra_filters.clear();
            self.work.min_degree = 0;
            filter_changed = true;
        }
        if filter_changed {
            if !restore {
                let new_filters =
                    std::mem::replace(&mut self.work.extra_filters, old_extra_filters);
                let new_degree = std::mem::replace(&mut self.work.min_degree, old_min_degree);
                self.record_view_undo();
                self.work.extra_filters = new_filters;
                self.work.min_degree = new_degree;
            }
            self.work.filter_due = Some(Instant::now() + Duration::from_millis(200));
            self.mark_dirty();
        }
    }
}
fn column_choice(ui: &mut egui::Ui, name: &str, columns: &[String], value: &mut usize) {
    egui::ComboBox::from_id_salt(name)
        .selected_text(format!(
            "{name}: {}",
            columns.get(*value).map(String::as_str).unwrap_or("Choose")
        ))
        .show_ui(ui, |ui| {
            for (i, col) in columns.iter().enumerate() {
                ui.selectable_value(value, i, col);
            }
        });
}
fn optional_column(ui: &mut egui::Ui, name: &str, columns: &[String], value: &mut Option<usize>) {
    egui::ComboBox::from_id_salt(name)
        .selected_text(format!(
            "{name}: {}",
            value
                .and_then(|i| columns.get(i))
                .map(String::as_str)
                .unwrap_or("None")
        ))
        .show_ui(ui, |ui| {
            ui.selectable_value(value, None, "None");
            for (i, col) in columns.iter().enumerate() {
                ui.selectable_value(value, Some(i), col);
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn view_roundtrip_and_rejection_of_invalid_settings() {
        let graph = Graph::new(2, vec![[0, 1]]);
        let app = App::new(graph, vec![[0.0, 0.0], [1.0, 1.0]], RunOptions::default());
        let view = app.view();
        let encoded = serde_json::to_value(&view).unwrap();
        let loaded: View = serde_json::from_value(encoded.clone()).unwrap();
        assert_eq!(encoded, serde_json::to_value(&loaded).unwrap());
        loaded.validate(&app.work.document, 0, 1).unwrap();
        let mut bad = loaded;
        bad.selected = Some(20);
        assert!(bad.validate(&app.work.document, 0, 1).is_err());
        bad.selected = None;
        bad.settings.k = 0.0;
        assert!(bad.validate(&app.work.document, 0, 1).is_err());
    }
    #[test]
    fn undo_filter_changes_does_not_need_layout_readback() {
        let mut app = App::new(
            Graph::new(2, vec![[0, 1]]),
            vec![[0.0, 0.0], [1.0, 1.0]],
            RunOptions::default(),
        );
        app.record_view_undo();
        app.work.min_degree = 3;
        app.undo_redo(false);
        assert_eq!(app.work.min_degree, 0);
        assert_eq!(app.work.redo.len(), 1);
        let deadline = Instant::now() + Duration::from_secs(5);
        while app.work.busy() {
            assert!(Instant::now() < deadline, "color worker did not finish");
            std::thread::yield_now();
            app.poll_jobs();
        }
        app.undo_redo(true);
        assert_eq!(app.work.min_degree, 3);
    }
}
