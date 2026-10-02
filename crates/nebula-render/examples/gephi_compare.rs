//! Layout-only throughput on the exact same binary fixture used by GephiBench.
use nebula_core::{Graph, Pos};
use nebula_render::{
    layout_gpu::{LayoutGpu, LayoutSettings},
    scene::GpuGraph,
};
use std::{
    fs::File,
    io::{BufReader, Read},
    time::Instant,
};
fn u32le(r: &mut impl Read) -> anyhow::Result<u32> {
    let mut b = [0; 4];
    r.read_exact(&mut b)?;
    Ok(u32::from_le_bytes(b))
}
fn main() -> anyhow::Result<()> {
    let a: Vec<_> = std::env::args().collect();
    anyhow::ensure!(
        a.len() == 4 || a.len() == 5,
        "gephi_compare FIXTURE WARMUP STEPS [POSITIONS]"
    );
    let warmup: u32 = a[2].parse()?;
    let steps: u32 = a[3].parse()?;
    anyhow::ensure!(steps > 0, "steps must be positive");
    let start = Instant::now();
    let mut r = BufReader::new(File::open(&a[1])?);
    let mut magic = [0; 8];
    r.read_exact(&mut magic)?;
    anyhow::ensure!(
        u64::from_le_bytes(magic) == 0x3148434e45424e,
        "Invalid fixture"
    );
    let n = u32le(&mut r)?;
    let mut b = [0; 8];
    r.read_exact(&mut b)?;
    let m = usize::try_from(u64::from_le_bytes(b))?;
    let mut positions: Vec<Pos> = Vec::with_capacity(n as usize);
    for _ in 0..n {
        positions.push([
            f32::from_bits(u32le(&mut r)?),
            f32::from_bits(u32le(&mut r)?),
        ]);
    }
    let mut edges = Vec::with_capacity(m);
    for _ in 0..m {
        let e = [u32le(&mut r)?, u32le(&mut r)?];
        anyhow::ensure!(e[0] < n && e[1] < n, "Invalid edge");
        edges.push(e);
    }
    anyhow::ensure!(r.read(&mut [0u8; 1])? == 0, "Trailing fixture data");
    let graph = Graph::new(n as u64, edges);
    graph.ensure_csr();
    let load_ms = start.elapsed().as_secs_f64() * 1000.;
    pollster::block_on(async {
        let instance = wgpu::Instance::default();
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions::default())
            .await?;
        let info = adapter.get_info();
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("comparison"),
                required_features: wgpu::Features::empty(),
                required_limits: adapter.limits(),
                memory_hints: Default::default(),
                trace: wgpu::Trace::Off,
                experimental_features: wgpu::ExperimentalFeatures::disabled(),
            })
            .await?;
        GpuGraph::validate(&graph, &positions, 30., &device.limits())?;
        let gpu = GpuGraph::upload(&device, &queue, &graph, &positions, 30.);
        let settings = LayoutSettings::default();
        let layout = LayoutGpu::new(&device, &gpu, &settings);
        for _ in 0..warmup {
            let mut encoder = device.create_command_encoder(&Default::default());
            layout.step(&mut encoder);
            queue.submit([encoder.finish()]);
        }
        device.poll(wgpu::PollType::wait_indefinitely())?;
        let start = Instant::now();
        for _ in 0..steps {
            let mut encoder = device.create_command_encoder(&Default::default());
            layout.step(&mut encoder);
            queue.submit([encoder.finish()]);
        }
        device.poll(wgpu::PollType::wait_indefinitely())?;
        let step_ms = start.elapsed().as_secs_f64() * 1000. / steps as f64;
        // Validate the resulting layout after timing, so failed computation cannot
        // be reported as a speedup. Readback is excluded from the timed region.
        let staging = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("validate positions"),
            size: (n as u64 * 8).max(8),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        encoder.copy_buffer_to_buffer(&gpu.positions, 0, &staging, 0, n as u64 * 8);
        queue.submit([encoder.finish()]);
        let (tx, rx) = std::sync::mpsc::channel();
        staging
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = tx.send(result);
            });
        device.poll(wgpu::PollType::wait_indefinitely())?;
        rx.recv()??;
        let mapped = staging.slice(..).get_mapped_range();
        anyhow::ensure!(
            mapped
                .chunks_exact(4)
                .all(|b| f32::from_le_bytes(b.try_into().unwrap()).is_finite()),
            "Nonfinite layout"
        );
        if let Some(path) = a.get(4) {
            std::fs::write(path, &mapped)?;
        }
        drop(mapped);
        staging.unmap();
        println!(
            "{}",
            serde_json::json!({"engine":"nebula-gpu", "adapter":info.name, "backend":format!("{:?}",info.backend), "nodes":n,"edges":m,"warmup":warmup,"steps":steps,"load_ms":load_ms,"step_ms":step_ms})
        );
        Ok::<_, anyhow::Error>(())
    })
}
