//! Render a shared comparison graph with supplied force-layout coordinates.
//! Usage: render_fixture GRAPH.bin POSITIONS.bin OUTPUT.png
use nebula_core::{Graph, Pos};
use nebula_render::{App, RunOptions};
use std::{
    fs,
    io::{Cursor, Read},
};
fn u32le(r: &mut impl Read) -> anyhow::Result<u32> {
    let mut b = [0; 4];
    r.read_exact(&mut b)?;
    Ok(u32::from_le_bytes(b))
}
fn main() -> anyhow::Result<()> {
    let a: Vec<_> = std::env::args().collect();
    anyhow::ensure!(
        a.len() == 4,
        "render_fixture GRAPH.bin POSITIONS.bin OUTPUT.png"
    );
    let mut r = Cursor::new(fs::read(&a[1])?);
    let mut b = [0; 8];
    r.read_exact(&mut b)?;
    anyhow::ensure!(u64::from_le_bytes(b) == 0x3148434e45424e, "Invalid fixture");
    let n = u32le(&mut r)?;
    r.read_exact(&mut b)?;
    let m = usize::try_from(u64::from_le_bytes(b))?;
    for _ in 0..n {
        r.read_exact(&mut b)?;
    }
    let mut edges = Vec::with_capacity(m);
    for _ in 0..m {
        let edge = [u32le(&mut r)?, u32le(&mut r)?];
        anyhow::ensure!(edge.iter().all(|v| *v < n), "Invalid endpoint");
        edges.push(edge);
    }
    anyhow::ensure!(
        r.position() == r.get_ref().len() as u64,
        "Trailing fixture data"
    );
    let raw = fs::read(&a[2])?;
    anyhow::ensure!(raw.len() == n as usize * 8, "Position count mismatch");
    let positions: Vec<Pos> = raw
        .chunks_exact(8)
        .map(|p| {
            [
                f32::from_le_bytes(p[..4].try_into().unwrap()),
                f32::from_le_bytes(p[4..].try_into().unwrap()),
            ]
        })
        .collect();
    anyhow::ensure!(
        positions.iter().flatten().all(|v| v.is_finite()),
        "Nonfinite positions"
    );
    let graph = Graph::new(n as u64, edges);
    graph.ensure_csr();
    let mut opts = RunOptions {
        max_frames: Some(1),
        screenshot: Some(a[3].clone()),
        ..Default::default()
    };
    opts.settings.running = false;
    App::new(graph, positions, opts).run()
}
