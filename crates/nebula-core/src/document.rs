//! Lossless interchange and versioned project documents. Internal indices never
//! replace source IDs in exported data. Layout uses a weak projection; traversal
//! and PageRank use original edge directions and weights.
use crate::{formats, Graph, Pos};
use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::{
    collections::{BTreeMap, HashMap},
    fs,
    io::{BufReader, BufWriter, Write},
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

pub type Attributes = BTreeMap<String, Value>;
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Node {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub attributes: Attributes,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Edge {
    pub source: u32,
    pub target: u32,
    #[serde(default)]
    pub directed: bool,
    #[serde(default = "unit")]
    pub weight: f32,
    #[serde(default = "edge_type")]
    pub kind: String,
    #[serde(default)]
    pub attributes: Attributes,
}
fn unit() -> f32 {
    1.0
}
fn edge_type() -> String {
    "edges".into()
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Document {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    #[serde(default)]
    pub type_colors: BTreeMap<String, u32>,
    #[serde(default)]
    pub warnings: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Project {
    pub version: u32,
    pub document: Arc<Document>,
    pub positions: Vec<Pos>,
    #[serde(default)]
    pub view: Value,
}
impl Project {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.version == 1,
            "Unsupported project version {}",
            self.version
        );
        self.document.validate()?;
        ensure!(
            self.positions.len() == self.document.nodes.len(),
            "Project position count does not match nodes"
        );
        ensure!(
            self.positions.iter().flatten().all(|v| v.is_finite()),
            "Project has invalid positions"
        );
        Ok(())
    }
}
#[derive(Clone, Default)]
pub struct Progress {
    pub cancelled: Arc<AtomicBool>,
    pub stage: Arc<std::sync::Mutex<String>>,
}
impl Progress {
    pub fn set(&self, stage: impl Into<String>) -> Result<()> {
        self.check()?;
        *self.stage.lock().unwrap() = stage.into();
        Ok(())
    }
    pub fn check(&self) -> Result<()> {
        ensure!(!self.cancelled.load(Ordering::Relaxed), "Cancelled");
        Ok(())
    }
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }
}
impl Document {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.nodes.len() < u32::MAX as usize,
            "Too many nodes for the viewer's 32-bit IDs"
        );
        let mut ids = std::collections::HashSet::new();
        for node in &self.nodes {
            ensure!(
                !node.id.is_empty() && ids.insert(&node.id),
                "Empty or duplicate node ID: {}",
                node.id
            );
        }
        for (i, edge) in self.edges.iter().enumerate() {
            ensure!(
                (edge.source as usize) < self.nodes.len()
                    && (edge.target as usize) < self.nodes.len(),
                "Edge {} refers to a missing node",
                i + 1
            );
            ensure!(
                edge.weight.is_finite() && edge.weight >= 0.0,
                "Edge {} has an invalid weight",
                i + 1
            );
        }
        Ok(())
    }
    pub fn graph(&self) -> Graph {
        Graph::with_metadata(
            self.nodes.len() as u64,
            self.edges.iter().map(|e| [e.source, e.target]).collect(),
            self.edges.iter().map(|e| e.directed).collect(),
            self.edges.iter().map(|e| e.weight).collect(),
        )
    }
    pub fn graph_for_kind(&self, kind: &str) -> Graph {
        let selected: Vec<_> = self.edges.iter().filter(|e| e.kind == kind).collect();
        Graph::with_metadata(
            self.nodes.len() as u64,
            selected.iter().map(|e| [e.source, e.target]).collect(),
            selected.iter().map(|e| e.directed).collect(),
            selected.iter().map(|e| e.weight).collect(),
        )
    }
    pub fn subset(&self, visible: &[bool]) -> Self {
        let mut result = Self {
            type_colors: self.type_colors.clone(),
            ..Self::default()
        };
        let mut map = vec![None; self.nodes.len()];
        for (i, n) in self.nodes.iter().enumerate() {
            if visible.get(i).copied().unwrap_or(false) {
                map[i] = Some(result.nodes.len() as u32);
                result.nodes.push(n.clone());
            }
        }
        for e in &self.edges {
            if let (Some(a), Some(b)) = (map[e.source as usize], map[e.target as usize]) {
                let mut e = e.clone();
                e.source = a;
                e.target = b;
                result.edges.push(e);
            }
        }
        result
    }
    pub fn export_json(&self) -> Value {
        let nodes: Vec<Value> = self
            .nodes
            .iter()
            .map(|n| serde_json::json!({"id":n.id,"label":n.label,"attributes":n.attributes}))
            .collect();
        let edges: Vec<Value> = self.edges.iter().map(|e| serde_json::json!({"source":self.nodes[e.source as usize].id,"target":self.nodes[e.target as usize].id,"directed":e.directed,"weight":e.weight,"type":e.kind,"attributes":e.attributes})).collect();
        serde_json::json!({"schema_version":1,"nodes":nodes,"edges":edges,"type_colors":self.type_colors})
    }
    pub fn from_legacy(loaded: formats::Loaded) -> Self {
        let mut kinds: HashMap<[u32; 2], String> = HashMap::new();
        let mut colors = BTreeMap::new();
        for t in loaded.edge_types {
            if let Some(c) = t.color {
                colors.insert(t.name.clone(), c);
            }
            for e in t.edges {
                kinds.insert(e, t.name.clone());
            }
        }
        let nodes = loaded
            .labels
            .into_iter()
            .enumerate()
            .map(|(i, id)| {
                let attrs: Attributes = loaded
                    .attrs
                    .get(i)
                    .into_iter()
                    .flatten()
                    .map(|(k, v)| (k.clone(), Value::String(v.clone())))
                    .collect();
                let label = attrs
                    .get("label")
                    .and_then(Value::as_str)
                    .unwrap_or(&id)
                    .to_string();
                Node {
                    id,
                    label,
                    attributes: attrs,
                }
            })
            .collect();
        let edges = loaded
            .graph
            .edges()
            .iter()
            .map(|e| Edge {
                source: e[0],
                target: e[1],
                directed: loaded.directed,
                weight: 1.0,
                kind: kinds.get(e).cloned().unwrap_or_else(edge_type),
                attributes: Attributes::new(),
            })
            .collect();
        Self { nodes,edges,type_colors:colors,warnings:vec!["This legacy importer retains topology; format-specific styling and unsupported metadata may be omitted. Use JSON, CSV, GraphML or GEXF for metadata-preserving import.".into()] }
    }
}

/// Compact incidence index for virtualized relationship inspection. Metadata
/// remains in the document; this only stores references into its edge array.
pub struct EdgeIndex {
    offsets: Vec<usize>,
    indices: Vec<usize>,
}
impl EdgeIndex {
    pub fn new(doc: &Document) -> Self {
        let n = doc.nodes.len();
        let mut offsets = vec![0usize; n + 1];
        for e in &doc.edges {
            offsets[e.source as usize + 1] += 1;
            if e.target != e.source {
                offsets[e.target as usize + 1] += 1;
            }
        }
        for i in 0..n {
            offsets[i + 1] += offsets[i];
        }
        let mut indices = vec![0; offsets[n]];
        let mut cursor = offsets[..n].to_vec();
        for (i, e) in doc.edges.iter().enumerate() {
            let a = e.source as usize;
            indices[cursor[a]] = i;
            cursor[a] += 1;
            if e.target != e.source {
                let b = e.target as usize;
                indices[cursor[b]] = i;
                cursor[b] += 1;
            }
        }
        Self { offsets, indices }
    }
    pub fn at(&self, id: u32) -> &[usize] {
        let i = id as usize;
        &self.indices[self.offsets[i]..self.offsets[i + 1]]
    }
}

struct Builder {
    doc: Document,
    ids: HashMap<String, u32>,
}
impl Builder {
    fn new() -> Self {
        Self {
            doc: Document::default(),
            ids: HashMap::new(),
        }
    }
    fn intern(&mut self, id: &str) -> Result<u32> {
        ensure!(!id.trim().is_empty(), "Empty node ID");
        if let Some(i) = self.ids.get(id) {
            return Ok(*i);
        }
        let i = u32::try_from(self.doc.nodes.len()).context("Too many nodes")?;
        self.ids.insert(id.into(), i);
        self.doc.nodes.push(Node {
            id: id.into(),
            label: id.into(),
            attributes: Attributes::new(),
        });
        Ok(i)
    }
    fn edge(
        &mut self,
        a: &str,
        b: &str,
        directed: bool,
        weight: f32,
        kind: String,
        attributes: Attributes,
    ) -> Result<()> {
        ensure!(
            weight.is_finite() && weight >= 0.0,
            "Weight must be finite and nonnegative"
        );
        let (a, b) = (self.intern(a)?, self.intern(b)?);
        self.doc.edges.push(Edge {
            source: a,
            target: b,
            directed,
            weight,
            kind,
            attributes,
        });
        Ok(())
    }
}
fn id(v: &Value) -> Result<String> {
    match v {
        Value::String(s) => Ok(s.clone()),
        Value::Number(n) => Ok(n.to_string()),
        _ => bail!("Node IDs must be strings or numbers"),
    }
}
pub fn display(v: &Value) -> String {
    v.as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| v.to_string())
}
fn attributes(obj: &Map<String, Value>, reserved: &[&str]) -> Attributes {
    let mut a: Attributes = obj
        .get("attributes")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    for (k, v) in obj {
        if !reserved.contains(&k.as_str()) && k != "attributes" {
            a.insert(k.clone(), v.clone());
        }
    }
    a
}
pub fn parse_json(v: Value, progress: &Progress) -> Result<Document> {
    if let Some(version) = v.get("schema_version") {
        ensure!(
            version.as_u64() == Some(1),
            "Unsupported JSON schema version"
        );
    }
    let mut b = Builder::new();
    let mut explicit = std::collections::HashSet::new();
    if let Some(nodes) = v.get("nodes").and_then(Value::as_array) {
        for (i, n) in nodes.iter().enumerate() {
            if i % 4096 == 0 {
                progress.check()?;
            }
            let key = id(n.get("id").unwrap_or(n)).with_context(|| format!("Node {}", i + 1))?;
            ensure!(explicit.insert(key.clone()), "Duplicate node ID {key}");
            let idx = b.intern(&key)? as usize;
            if let Some(obj) = n.as_object() {
                b.doc.nodes[idx].label = n.get("label").map(display).unwrap_or(key);
                b.doc.nodes[idx].attributes = attributes(obj, &["id", "label"]);
            }
        }
    }
    let default_directed = v.get("directed").and_then(Value::as_bool).unwrap_or(false);
    let mut read_edges = |arr: &[Value], kind: &str| -> Result<()> {
        for (i, e) in arr.iter().enumerate() {
            if i % 4096 == 0 {
                progress.check()?;
            }
            let a = e
                .get("source")
                .or_else(|| e.get(0))
                .context("Edge missing source")?;
            let c = e
                .get("target")
                .or_else(|| e.get(1))
                .context("Edge missing target")?;
            let weight = e
                .get("weight")
                .map(|w| w.as_f64().context("Edge weight must be a number"))
                .transpose()?
                .unwrap_or(1.0) as f32;
            let attrs = e
                .as_object()
                .map(|o| attributes(o, &["source", "target", "weight", "directed", "type"]))
                .unwrap_or_default();
            b.edge(
                &id(a)?,
                &id(c)?,
                e.get("directed")
                    .and_then(Value::as_bool)
                    .unwrap_or(default_directed),
                weight,
                e.get("type").and_then(Value::as_str).unwrap_or(kind).into(),
                attrs,
            )
            .with_context(|| format!("Edge {} in {kind}", i + 1))?;
        }
        Ok(())
    };
    if let Some(types) = v.get("edge_types").and_then(Value::as_array) {
        for t in types {
            read_edges(
                t.get("edges")
                    .and_then(Value::as_array)
                    .context("Edge type missing edges")?,
                t.get("name").and_then(Value::as_str).unwrap_or("edges"),
            )?;
        }
    } else {
        for key in ["edges", "links"] {
            if let Some(arr) = v.get(key).and_then(Value::as_array) {
                read_edges(arr, "edges")?;
            }
        }
    }
    if let Some(types) = v.get("edge_types").and_then(Value::as_array) {
        for t in types {
            if let Some(c) = t.get("color").and_then(Value::as_array) {
                let ch = |i: usize, d: u64| {
                    c.get(i).and_then(Value::as_u64).unwrap_or(d).min(255) as u32
                };
                b.doc.type_colors.insert(
                    t.get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("edges")
                        .into(),
                    ch(0, 120) | (ch(1, 120) << 8) | (ch(2, 120) << 16) | (ch(3, 255) << 24),
                );
            }
        }
    }
    if let Some(colors) = v.get("type_colors") {
        b.doc
            .type_colors
            .extend(serde_json::from_value::<BTreeMap<String, u32>>(
                colors.clone(),
            )?);
    }
    ensure!(
        v.get("nodes").is_some()
            || v.get("edges").is_some()
            || v.get("links").is_some()
            || v.get("edge_types").is_some(),
        "JSON has no nodes or edges"
    );
    b.doc.validate()?;
    Ok(b.doc)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CsvOptions {
    pub delimiter: u8,
    pub headers: bool,
    pub source: usize,
    pub target: usize,
    pub directed: bool,
    pub weight: Option<usize>,
    pub kind: Option<usize>,
    pub node_file: Option<std::path::PathBuf>,
    pub node_id: usize,
    pub node_label: Option<usize>,
}
impl Default for CsvOptions {
    fn default() -> Self {
        Self {
            delimiter: b',',
            headers: true,
            source: 0,
            target: 1,
            directed: false,
            weight: None,
            kind: None,
            node_file: None,
            node_id: 0,
            node_label: None,
        }
    }
}
#[derive(Clone)]
pub struct CsvPreview {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
    pub options: CsvOptions,
}
pub fn preview_csv(path: &Path, options: CsvOptions) -> Result<CsvPreview> {
    let mut r = csv::ReaderBuilder::new()
        .delimiter(options.delimiter)
        .has_headers(options.headers)
        .from_path(path)?;
    let h = r.headers()?.clone();
    let columns = if options.headers {
        h.iter().map(str::to_owned).collect()
    } else {
        (0..h.len()).map(|i| format!("Column {}", i + 1)).collect()
    };
    let rows = r
        .records()
        .take(10)
        .map(|r| r.map(|r| r.iter().map(str::to_owned).collect()))
        .collect::<std::result::Result<_, _>>()?;
    Ok(CsvPreview {
        columns,
        rows,
        options,
    })
}
pub fn csv_defaults(path: &Path) -> Result<CsvPreview> {
    let delimiter = if path.extension().is_some_and(|e| e == "tsv") {
        b'\t'
    } else {
        b','
    };
    let mut r = csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .has_headers(false)
        .from_path(path)?;
    let first = r.records().next().transpose()?.context("CSV is empty")?;
    let normalized: Vec<String> = first.iter().map(|s| s.to_ascii_lowercase()).collect();
    let find = |names: &[&str]| normalized.iter().position(|s| names.contains(&s.as_str()));
    let source = find(&["source", "src", "from"]);
    let target = find(&["target", "dst", "to"]);
    let o = CsvOptions {
        delimiter,
        headers: source.is_some() && target.is_some(),
        source: source.unwrap_or(0),
        target: target.unwrap_or(1),
        weight: find(&["weight"]),
        kind: find(&["type", "kind", "relationship"]),
        ..CsvOptions::default()
    };
    preview_csv(path, o)
}
fn cell<'a>(row: &'a csv::StringRecord, i: usize) -> Result<&'a str> {
    row.get(i)
        .filter(|v| !v.trim().is_empty())
        .context("Missing required column value")
}
fn csv_attributes(
    row: &csv::StringRecord,
    header: &csv::StringRecord,
    skip: &[usize],
) -> Attributes {
    row.iter()
        .enumerate()
        .filter(|(i, _)| !skip.contains(i))
        .map(|(i, s)| {
            let key = header
                .get(i)
                .map(str::to_owned)
                .unwrap_or_else(|| format!("column_{}", i + 1));
            let value = serde_json::from_str::<Value>(s)
                .ok()
                .filter(|v| !v.is_array() && !v.is_object())
                .unwrap_or_else(|| Value::String(s.into()));
            (key, value)
        })
        .collect()
}
pub fn import_csv(path: &Path, o: &CsvOptions, p: &Progress) -> Result<Document> {
    let mut b = Builder::new();
    if let Some(nodes) = &o.node_file {
        p.set("Reading node table")?;
        let mut r = csv::ReaderBuilder::new()
            .delimiter(o.delimiter)
            .has_headers(o.headers)
            .from_path(nodes)?;
        let header = if o.headers {
            r.headers()?.clone()
        } else {
            csv::StringRecord::new()
        };
        let mut ids = std::collections::HashSet::new();
        for row in r.records() {
            p.check()?;
            let row = row?;
            let key = cell(&row, o.node_id)?;
            ensure!(
                ids.insert(key.to_string()),
                "Duplicate node ID in node table: {key}"
            );
            let idx = b.intern(key)? as usize;
            let label = o
                .node_label
                .map(|i| cell(&row, i))
                .transpose()?
                .unwrap_or(key);
            b.doc.nodes[idx].label = label.into();
            b.doc.nodes[idx].attributes = csv_attributes(
                &row,
                &header,
                &[o.node_id, o.node_label.unwrap_or(usize::MAX)],
            );
        }
    }
    p.set("Reading edge table")?;
    let mut r = csv::ReaderBuilder::new()
        .delimiter(o.delimiter)
        .has_headers(o.headers)
        .from_path(path)?;
    let header = if o.headers {
        r.headers()?.clone()
    } else {
        csv::StringRecord::new()
    };
    for (i, row) in r.records().enumerate() {
        if i % 4096 == 0 {
            p.set(format!("Reading edge row {}", i + 1))?;
        }
        let row = row.with_context(|| format!("Edge row {}", i + 1))?;
        let parse = || -> Result<(&str, &str, f32, String, Attributes)> {
            let (a, c) = (cell(&row, o.source)?, cell(&row, o.target)?);
            if o.node_file.is_some() {
                ensure!(
                    b.ids.contains_key(a) && b.ids.contains_key(c),
                    "Endpoint absent from node table: {a} → {c}"
                );
            }
            let weight = o
                .weight
                .map(|j| cell(&row, j)?.parse::<f32>().context("Invalid weight"))
                .transpose()?
                .unwrap_or(1.0);
            let kind = o
                .kind
                .map(|j| cell(&row, j).map(str::to_owned))
                .transpose()?
                .unwrap_or_else(edge_type);
            Ok((
                a,
                c,
                weight,
                kind,
                csv_attributes(
                    &row,
                    &header,
                    &[
                        o.source,
                        o.target,
                        o.weight.unwrap_or(usize::MAX),
                        o.kind.unwrap_or(usize::MAX),
                    ],
                ),
            ))
        };
        let (a, c, w, k, attrs) = parse().with_context(|| format!("Edge row {}", i + 1))?;
        b.edge(a, c, o.directed, w, k, attrs)
            .with_context(|| format!("Edge row {}", i + 1))?;
    }
    b.doc.validate()?;
    Ok(b.doc)
}

pub fn parse_xml(text: &str, ext: &str, p: &Progress) -> Result<Document> {
    let xml = roxmltree::Document::parse(text).context("Parsing graph XML")?;
    let mut b = Builder::new();
    if ext == "graphml" {
        let graphs: Vec<_> = xml
            .descendants()
            .filter(|n| n.has_tag_name("graph"))
            .collect();
        ensure!(
            graphs.len() == 1,
            "GraphML requires one flat graph; nested graphs are not supported"
        );
        ensure!(
            !xml.descendants().any(|n| n.has_tag_name("hyperedge")),
            "GraphML hyperedges are not supported"
        );
        let g = graphs[0];
        let directed = g.attribute("edgedefault") == Some("directed");
        let keys: HashMap<_, _> = xml
            .descendants()
            .filter(|n| n.has_tag_name("key"))
            .filter_map(|n| {
                Some((
                    n.attribute("id")?,
                    (
                        n.attribute("attr.name").unwrap_or(n.attribute("id")?),
                        n.attribute("attr.type").unwrap_or("string"),
                        n.attribute("for").unwrap_or("all"),
                        n.children()
                            .find(|x| x.has_tag_name("default"))
                            .and_then(|x| x.text()),
                    ),
                ))
            })
            .collect();
        let attrs = |n: roxmltree::Node<'_, '_>, scope: &str| -> Result<Attributes> {
            let mut a = Attributes::new();
            for (name, ty, applies, default) in keys.values() {
                if (*applies == scope || *applies == "all") && default.is_some() {
                    a.insert((*name).into(), xml_value(default.unwrap(), ty)?);
                }
            }
            for d in n.children().filter(|n| n.has_tag_name("data")) {
                let key = d.attribute("key").context("GraphML data missing key")?;
                let (name, ty, _, _) = keys
                    .get(key)
                    .copied()
                    .unwrap_or((key, "string", "all", None));
                a.insert(name.into(), xml_value(d.text().unwrap_or(""), ty)?);
            }
            Ok(a)
        };
        for n in g.children().filter(|n| n.has_tag_name("node")) {
            p.check()?;
            let key = n.attribute("id").context("Node missing ID")?;
            ensure!(!b.ids.contains_key(key), "Duplicate node ID {key}");
            let i = b.intern(key)? as usize;
            let a = attrs(n, "node")?;
            b.doc.nodes[i].label = a.get("label").map(display).unwrap_or_else(|| key.into());
            b.doc.nodes[i].attributes = a;
        }
        for e in g.children().filter(|n| n.has_tag_name("edge")) {
            p.check()?;
            let a = e.attribute("source").context("Edge missing source")?;
            let c = e.attribute("target").context("Edge missing target")?;
            ensure!(
                b.ids.contains_key(a) && b.ids.contains_key(c),
                "Edge references undeclared node"
            );
            let mut at = attrs(e, "edge")?;
            if let Some(id) = e.attribute("id") {
                at.insert("id".into(), Value::String(id.into()));
            }
            let w = at.get("weight").and_then(Value::as_f64).unwrap_or(1.0) as f32;
            let k = at.get("type").map(display).unwrap_or_else(edge_type);
            let d = e
                .attribute("directed")
                .map(|v| v == "true" || v == "1")
                .unwrap_or(directed);
            b.edge(a, c, d, w, k, at)?;
        }
    } else {
        let g = xml
            .descendants()
            .find(|n| n.has_tag_name("graph"))
            .context("GEXF missing graph")?;
        ensure!(
            g.attribute("mode").unwrap_or("static") == "static",
            "Dynamic GEXF requires a static snapshot"
        );
        let default_directed = g.attribute("defaultedgetype") == Some("directed");
        let mut keys = HashMap::new();
        for group in g.children().filter(|n| n.has_tag_name("attributes")) {
            let scope = group.attribute("class").unwrap_or("node");
            for a in group.children().filter(|n| n.has_tag_name("attribute")) {
                let key = a.attribute("id").context("Attribute missing ID")?;
                keys.insert(
                    (scope, key),
                    (
                        a.attribute("title").unwrap_or(key),
                        a.attribute("type").unwrap_or("string"),
                    ),
                );
            }
        }
        let attrs = |n: roxmltree::Node<'_, '_>, scope: &str| -> Result<Attributes> {
            let mut a = Attributes::new();
            for v in n.descendants().filter(|n| n.has_tag_name("attvalue")) {
                let key = v.attribute("for").context("Attribute missing key")?;
                let (name, ty) = keys.get(&(scope, key)).copied().unwrap_or((key, "string"));
                a.insert(
                    name.into(),
                    xml_value(v.attribute("value").unwrap_or(""), ty)?,
                );
            }
            Ok(a)
        };
        for n in g.descendants().filter(|n| n.has_tag_name("node")) {
            p.check()?;
            ensure!(
                !n.descendants().skip(1).any(|n| n.has_tag_name("node")),
                "Nested GEXF nodes are unsupported"
            );
            let key = n.attribute("id").context("Node missing ID")?;
            ensure!(!b.ids.contains_key(key), "Duplicate node ID {key}");
            let i = b.intern(key)? as usize;
            b.doc.nodes[i].label = n.attribute("label").unwrap_or(key).into();
            b.doc.nodes[i].attributes = attrs(n, "node")?;
        }
        for e in g.descendants().filter(|n| n.has_tag_name("edge")) {
            p.check()?;
            let a = e.attribute("source").context("Edge missing source")?;
            let c = e.attribute("target").context("Edge missing target")?;
            ensure!(
                b.ids.contains_key(a) && b.ids.contains_key(c),
                "Edge references undeclared node"
            );
            let mut at = attrs(e, "edge")?;
            for key in ["id", "label"] {
                if let Some(v) = e.attribute(key) {
                    at.insert(key.into(), Value::String(v.into()));
                }
            }
            let ty = e.attribute("type").unwrap_or(if default_directed {
                "directed"
            } else {
                "undirected"
            });
            ensure!(
                ty != "mutual",
                "GEXF mutual edges require two directed edges"
            );
            let w = e
                .attribute("weight")
                .map(str::parse::<f32>)
                .transpose()?
                .unwrap_or(1.0);
            b.edge(a, c, ty == "directed", w, edge_type(), at)?;
        }
    }
    b.doc.warnings.push("Imported static graph topology and attributes; source visualization styling is not applied.".into());
    b.doc.validate()?;
    Ok(b.doc)
}
fn xml_value(s: &str, ty: &str) -> Result<Value> {
    Ok(match ty {
        "boolean" => Value::Bool(match s {
            "true" | "1" => true,
            "false" | "0" => false,
            _ => bail!("Invalid boolean {s}"),
        }),
        "int" | "integer" | "long" => Value::from(s.parse::<i64>()?),
        "float" | "double" => {
            let n = s.parse::<f64>()?;
            ensure!(n.is_finite(), "Non-finite attribute");
            Value::from(n)
        }
        _ => Value::String(s.into()),
    })
}

pub fn load(
    path: &Path,
    csv: Option<&CsvOptions>,
    p: &Progress,
) -> Result<(Arc<Document>, Option<Project>)> {
    p.set("Reading file")?;
    if path == Path::new("-") {
        return Ok((
            Arc::new(parse_json(
                serde_json::from_reader(std::io::stdin().lock())?,
                p,
            )?),
            None,
        ));
    }
    let mut memory = sysinfo::System::new();
    memory.refresh_memory();
    let bytes = fs::metadata(path)?.len();
    ensure!(bytes.saturating_mul(4) < memory.available_memory(),"File is too large for available memory. Import a smaller subgraph or close other applications.");
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if ext == "nebula" {
        let project: Project = serde_json::from_reader(BufReader::new(fs::File::open(path)?))?;
        project.validate()?;
        return Ok((project.document.clone(), Some(project)));
    }
    let doc = match ext.as_str() {
        "csv" | "tsv" => import_csv(
            path,
            &csv.cloned().unwrap_or(csv_defaults(path)?.options),
            p,
        )?,
        "json" => parse_json(
            serde_json::from_reader(BufReader::new(fs::File::open(path)?))?,
            p,
        )?,
        "graphml" | "gexf" => parse_xml(&fs::read_to_string(path)?, &ext, p)?,
        _ => Document::from_legacy(formats::load(path)?),
    };
    p.check()?;
    doc.validate()?;
    Ok((Arc::new(doc), None))
}

/// Write in the destination directory, sync, then atomically replace. A failed
/// write never destroys the existing project. Unique temporary names allow jobs
/// in different windows to save safely without sharing scratch files.
pub fn atomic_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut temp = None;
    for i in 0..100 {
        let candidate = parent.join(format!(
            ".nebula-{}-{}-{i}.tmp",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos()
        ));
        if let Ok(f) = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            temp = Some((candidate, f));
            break;
        }
    }
    let (tmp, file) = temp.context("Cannot create save file")?;
    let result = (|| {
        let mut writer = BufWriter::new(file);
        serde_json::to_writer(&mut writer, value)?;
        writer.flush()?;
        writer.get_ref().sync_all()?;
        fs::rename(&tmp, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    fn file(name: &str, content: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("nebula-test-{}-{name}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        fs::write(&path, content).unwrap();
        path
    }
    use std::path::PathBuf;
    #[test]
    fn json_roundtrip_preserves_semantics_and_isolated_nodes() {
        let doc=parse_json(serde_json::json!({"nodes":[{"id":"a","label":"Alpha","attributes":{"flag":true,"n":2,"nested":{"x":1}}},{"id":"b"},{"id":"alone"}],"edges":[{"source":"a","target":"b","directed":true,"weight":2.5,"type":"depends","attributes":{"note":"x"}},{"source":"a","target":"b","directed":false},{"source":"a","target":"a"}]}),&Progress::default()).unwrap();
        let again = parse_json(doc.export_json(), &Progress::default()).unwrap();
        assert_eq!(
            serde_json::to_value(&doc).unwrap(),
            serde_json::to_value(&again).unwrap()
        );
        assert_eq!(again.nodes.len(), 3);
        assert_eq!(again.edges.len(), 3);
        assert_eq!(again.nodes[0].attributes["flag"], true);
        assert_eq!(again.graph().outgoing().neighbors(1), &[0]);
        assert_eq!(again.graph().incoming().neighbors(1), &[0, 0]);
    }
    #[test]
    fn csv_headers_quoted_values_and_node_table() {
        let path = file(
            "quoted.csv",
            "from,to,weight,type,note\nA,B,2,depends,\"one, two\"\nA,A,1,self,\"line\nsecond\"\n",
        );
        let nodes = file(
            "nodes.csv",
            "id,label,flag\nA,Alpha,true\nB,Beta,false\nC,孤立,true\n",
        );
        let mut preview = csv_defaults(&path).unwrap();
        assert!(preview.options.headers);
        assert_eq!(preview.rows[0][4], "one, two");
        preview.options.node_file = Some(nodes.clone());
        preview.options.node_label = Some(1);
        let doc = import_csv(&path, &preview.options, &Progress::default()).unwrap();
        assert_eq!(doc.nodes.len(), 3);
        assert_eq!(doc.nodes[2].label, "孤立");
        assert_eq!(doc.nodes[0].attributes["flag"], true);
        assert_eq!(doc.edges[0].weight, 2.0);
        assert_eq!(doc.edges[1].source, doc.edges[1].target);
        assert_eq!(doc.edges[1].attributes["note"], "line\nsecond");
        fs::remove_file(path).unwrap();
        fs::remove_file(nodes).unwrap();
    }
    #[test]
    fn csv_invalid_row_does_not_silently_disappear() {
        let path = file("invalid.csv", "source,target,weight\na,b,wrong\n");
        let options = csv_defaults(&path).unwrap().options;
        let error = import_csv(&path, &options, &Progress::default()).unwrap_err();
        assert!(format!("{error:#}").contains("Edge row 1"));
        fs::remove_file(path).unwrap();
    }
    #[test]
    fn graphml_attributes_defaults_and_direction() {
        let xml = r#"<graphml xmlns="http://graphml.graphdrawing.org/xmlns"><key id="label" for="node" attr.name="label" attr.type="string"/><key id="score" for="node" attr.name="score" attr.type="double"><default>2.5</default></key><graph edgedefault="directed"><node id="a"><data key="label">A &amp; B</data></node><node id="b"/><node id="isolated"/><edge id="e1" source="a" target="b"/></graph></graphml>"#;
        let doc = parse_xml(xml, "graphml", &Progress::default()).unwrap();
        assert_eq!(doc.nodes[0].label, "A & B");
        assert_eq!(doc.nodes[1].attributes["score"], 2.5);
        assert!(doc.edges[0].directed);
        assert_eq!(doc.edges[0].attributes["id"], "e1");
        assert!(parse_xml(
            "<graphml><graph><graph/></graph></graphml>",
            "graphml",
            &Progress::default()
        )
        .is_err());
    }
    #[test]
    fn gexf_static_metadata() {
        let xml = r#"<gexf><graph defaultedgetype="directed" mode="static"><attributes class="node"><attribute id="0" title="count" type="integer"/></attributes><nodes><node id="a" label="Alpha"><attvalues><attvalue for="0" value="3"/></attvalues></node><node id="b"/></nodes><edges><edge id="edge1" source="a" target="b" weight="4"/></edges></graph></gexf>"#;
        let doc = parse_xml(xml, "gexf", &Progress::default()).unwrap();
        assert_eq!(doc.nodes[0].attributes["count"], 3);
        assert_eq!(doc.edges[0].weight, 4.0);
        assert!(doc.edges[0].directed);
    }
    #[test]
    fn project_atomic_save_validate_and_subset() {
        let doc = Arc::new(
            parse_json(
                serde_json::json!({"nodes":["a","b","c"],"edges":[["a","b"],["b","c"]]}),
                &Progress::default(),
            )
            .unwrap(),
        );
        let subset = doc.subset(&[false, true, true]);
        assert_eq!(subset.nodes[0].id, "b");
        assert_eq!(subset.edges[0].source, 0);
        let path = file("project.nebula", "previous content");
        let project = Project {
            version: 1,
            document: doc,
            positions: vec![[1.0, 2.0]; 3],
            view: serde_json::json!({"zoom":1}),
        };
        atomic_json(&path, &project).unwrap();
        let (_, loaded) = load(&path, None, &Progress::default()).unwrap();
        assert_eq!(loaded.unwrap().positions, project.positions);
        let bad = Project {
            version: 2,
            ..project
        };
        assert!(bad.validate().is_err());
        fs::remove_file(path).unwrap();
    }
    #[test]
    fn cancelled_and_bad_ids_are_rejected() {
        let p = Progress::default();
        p.cancel();
        assert!(parse_json(serde_json::json!({"nodes":["a"]}), &p).is_err());
        assert!(parse_json(serde_json::json!({"nodes":["a","a"]}), &Progress::default()).is_err());
        assert!(parse_json(
            serde_json::json!({"edges":[{"source":"a"}]}),
            &Progress::default()
        )
        .is_err());
    }
}
