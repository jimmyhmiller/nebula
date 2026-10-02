//! Occasional GPU->CPU readback of positions (for exact camera fitting and,
//! later, picking). Deliberately gated to manageable sizes by callers — copying
//! and mapping a multi-gigabyte buffer every fit would defeat the point.

use crate::gpu::Gpu;
use crate::scene::GpuGraph;
use nebula_core::Pos;

/// Read back a single node's position (8 bytes). Cheap enough to call per frame
/// while a node is selected, so the selection marker tracks it live.
pub fn read_one_position(gpu: &Gpu, graph: &GpuGraph, index: u32) -> Option<Pos> {
    if index as u64 >= graph.num_nodes {
        return None;
    }
    let offset = index as u64 * 8;
    let staging = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("one_pos_readback"),
        size: 8,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut enc = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("one_pos"),
        });
    enc.copy_buffer_to_buffer(&graph.positions, offset, &staging, 0, 8);
    gpu.queue.submit(Some(enc.finish()));

    let slice = staging.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    let _ = gpu.device.poll(wgpu::PollType::wait_indefinitely());
    rx.recv().ok()?.ok()?;
    let data = slice.get_mapped_range();
    let p: Pos = *bytemuck::from_bytes(&data[..8]);
    drop(data);
    staging.unmap();
    Some(p)
}

/// Read back the positions of a small set of nodes (selection + neighbors) via
/// per-index buffer copies into one staging buffer. Unlike `read_positions`,
/// cost scales with the set size, not the graph, so it is fine to call per frame
/// while a node is selected — even on multi-million-node graphs.
pub fn read_positions_at(gpu: &Gpu, graph: &GpuGraph, indices: &[u32]) -> Option<Vec<Pos>> {
    if indices.is_empty() {
        return Some(Vec::new());
    }
    let bytes = (indices.len() * std::mem::size_of::<Pos>()) as u64;
    let staging = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("pos_gather_readback"),
        size: bytes,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut enc = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("pos_gather"),
        });
    for (slot, &i) in indices.iter().enumerate() {
        if (i as u64) < graph.num_nodes {
            enc.copy_buffer_to_buffer(&graph.positions, i as u64 * 8, &staging, slot as u64 * 8, 8);
        }
    }
    gpu.queue.submit(Some(enc.finish()));

    let slice = staging.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    let _ = gpu.device.poll(wgpu::PollType::wait_indefinitely());
    rx.recv().ok()?.ok()?;
    let data = slice.get_mapped_range();
    let out: Vec<Pos> = bytemuck::cast_slice(&data).to_vec();
    drop(data);
    staging.unmap();
    Some(out)
}

pub fn read_positions(gpu: &Gpu, graph: &GpuGraph) -> Option<Vec<Pos>> {
    let n = graph.num_nodes as usize;
    if n == 0 {
        return Some(Vec::new());
    }
    let bytes = (n * std::mem::size_of::<Pos>()) as u64;

    let staging = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("pos_readback"),
        size: bytes,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });

    let mut enc = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("readback"),
        });
    enc.copy_buffer_to_buffer(&graph.positions, 0, &staging, 0, bytes);
    gpu.queue.submit(Some(enc.finish()));

    let slice = staging.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    // Block until the copy completes and the mapping is ready.
    let _ = gpu.device.poll(wgpu::PollType::wait_indefinitely());
    match rx.recv() {
        Ok(Ok(())) => {}
        _ => return None,
    }

    let data = slice.get_mapped_range();
    let out: Vec<Pos> = bytemuck::cast_slice(&data).to_vec();
    drop(data);
    staging.unmap();
    Some(out)
}

/// Nonblocking whole-layout capture for save, recovery and undo. Submission
/// captures one consistent frame; mapping is polled during subsequent redraws.
pub struct PositionCapture {
    buffer: wgpu::Buffer,
    receiver: std::sync::mpsc::Receiver<Result<(), wgpu::BufferAsyncError>>,
    count: usize,
}
impl PositionCapture {
    pub fn new(gpu: &Gpu, graph: &GpuGraph) -> Self {
        let bytes = (graph.num_nodes * 8).max(8);
        let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("document_capture"),
            size: bytes,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("document_capture"),
            });
        encoder.copy_buffer_to_buffer(&graph.positions, 0, &buffer, 0, bytes);
        gpu.queue.submit(Some(encoder.finish()));
        let (sender, receiver) = std::sync::mpsc::channel();
        buffer.slice(..).map_async(wgpu::MapMode::Read, move |r| {
            let _ = sender.send(r);
        });
        Self {
            buffer,
            receiver,
            count: graph.num_nodes as usize,
        }
    }
    pub fn gather(gpu: &Gpu, graph: &GpuGraph, indices: &[u32]) -> Self {
        let bytes = (indices.len() as u64 * 8).max(8);
        let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("selection_capture"),
            size: bytes,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("selection_capture"),
            });
        for (slot, &id) in indices.iter().enumerate() {
            if (id as u64) < graph.num_nodes {
                encoder.copy_buffer_to_buffer(
                    &graph.positions,
                    id as u64 * 8,
                    &buffer,
                    slot as u64 * 8,
                    8,
                );
            }
        }
        gpu.queue.submit(Some(encoder.finish()));
        let (sender, receiver) = std::sync::mpsc::channel();
        buffer.slice(..).map_async(wgpu::MapMode::Read, move |r| {
            let _ = sender.send(r);
        });
        Self {
            buffer,
            receiver,
            count: indices.len(),
        }
    }
    pub fn poll(&self, gpu: &Gpu) -> Option<anyhow::Result<Vec<Pos>>> {
        let _ = gpu.device.poll(wgpu::PollType::Poll);
        match self.receiver.try_recv() {
            Ok(Ok(())) => {
                let data = self.buffer.slice(..).get_mapped_range();
                let positions = bytemuck::cast_slice::<u8, Pos>(&data)[..self.count].to_vec();
                drop(data);
                self.buffer.unmap();
                Some(Ok(positions))
            }
            Ok(Err(e)) => Some(Err(anyhow::anyhow!("Could not capture layout: {e}"))),
            Err(std::sync::mpsc::TryRecvError::Empty) => None,
            Err(e) => Some(Err(anyhow::anyhow!(e))),
        }
    }
}
