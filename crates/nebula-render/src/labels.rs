//! Bounded, screen-space label sampling. One candidate per screen tile keeps
//! labels useful at any graph size without reading every position to the CPU.
use crate::{camera::CameraUniform, gpu::Gpu, scene::GpuGraph};
use nebula_core::Pos;
use std::sync::mpsc;
use wgpu::util::DeviceExt;
const COUNT: usize = 400;
const SHADER: &str = r#"
struct Camera { center:vec2<f32>,scale:vec2<f32>,viewport:vec2<f32>,zoom:f32,pad:f32 }
struct Entry { id:u32,pad:u32,pos:vec2<f32> }
@group(0) @binding(0) var<uniform> camera:Camera;
@group(0) @binding(1) var<storage,read> positions:array<vec2<f32>>;
@group(0) @binding(2) var<storage,read> sizes:array<f32>;
@group(0) @binding(3) var<storage,read_write> occupied:array<atomic<u32>>;
@group(0) @binding(4) var<storage,read_write> output:array<Entry>;
@compute @workgroup_size(256)
fn sample(@builtin(workgroup_id) wg:vec3<u32>,@builtin(local_invocation_index) li:u32,@builtin(num_workgroups) dims:vec3<u32>){
    let i=(wg.y*dims.x+wg.x)*256u+li;
    if i>=arrayLength(&sizes) || sizes[i]<=0.0 {return;}
    let p=positions[i];let ndc=(p-camera.center)*camera.scale;
    if any(ndc<vec2<f32>(-1.0)) || any(ndc>=vec2<f32>(1.0)){return;}
    let cell=vec2<u32>((ndc+vec2<f32>(1.0))*10.0);
    let slot=cell.y*20u+cell.x;
    // Atomic exchange ensures exactly one writer; every slot is independent.
    if atomicExchange(&occupied[slot],1u)==0u {output[slot]=Entry(i,0u,p);}
}
"#;
pub struct Labels {
    camera: wgpu::Buffer,
    occupied: wgpu::Buffer,
    output: wgpu::Buffer,
    bind: wgpu::BindGroup,
    pipeline: wgpu::ComputePipeline,
    pending: Option<(
        wgpu::Buffer,
        mpsc::Receiver<Result<(), wgpu::BufferAsyncError>>,
    )>,
    pub candidates: Vec<(u32, Pos)>,
    last: std::time::Instant,
}
impl Labels {
    pub fn new(gpu: &Gpu, graph: &GpuGraph) -> Self {
        let d = &gpu.device;
        let camera = d.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("label_camera"),
            contents: bytemuck::bytes_of(
                &crate::camera::Camera2D::new(glam::vec2(1280.0, 800.0)).uniform(),
            ),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let buffer = |name, size| {
            d.create_buffer(&wgpu::BufferDescriptor {
                label: Some(name),
                size,
                usage: wgpu::BufferUsages::STORAGE
                    | wgpu::BufferUsages::COPY_SRC
                    | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        };
        let occupied = buffer("label_tiles", COUNT as u64 * 4);
        let output = buffer("label_output", COUNT as u64 * 16);
        let module = d.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("label_sampler"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let pipeline = d.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("label_sampler"),
            layout: None,
            module: &module,
            entry_point: Some("sample"),
            compilation_options: Default::default(),
            cache: None,
        });
        let bind = d.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("label_sampler"),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: camera.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: graph.positions.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: graph.sizes.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: occupied.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: output.as_entire_binding(),
                },
            ],
        });
        Self {
            camera,
            occupied,
            output,
            bind,
            pipeline,
            pending: None,
            candidates: Vec::new(),
            last: std::time::Instant::now() - std::time::Duration::from_secs(1),
        }
    }
    pub fn update(&mut self, gpu: &Gpu, graph: &GpuGraph, camera: &CameraUniform) {
        let _ = gpu.device.poll(wgpu::PollType::Poll);
        if let Some((buffer, rx)) = &self.pending {
            match rx.try_recv() {
                Ok(Ok(())) => {
                    let data = buffer.slice(..).get_mapped_range();
                    self.candidates.clear();
                    for chunk in data.chunks_exact(16) {
                        let id = u32::from_le_bytes(chunk[..4].try_into().unwrap());
                        if id < u32::MAX {
                            self.candidates.push((
                                id,
                                [
                                    f32::from_le_bytes(chunk[8..12].try_into().unwrap()),
                                    f32::from_le_bytes(chunk[12..16].try_into().unwrap()),
                                ],
                            ));
                        }
                    }
                    drop(data);
                    buffer.unmap();
                    self.pending = None;
                }
                Ok(Err(_)) | Err(mpsc::TryRecvError::Disconnected) => {
                    self.pending = None;
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        if self.pending.is_some()
            || self.last.elapsed() < std::time::Duration::from_millis(250)
            || graph.num_nodes == 0
        {
            return;
        }
        self.last = std::time::Instant::now();
        gpu.queue
            .write_buffer(&self.camera, 0, bytemuck::bytes_of(camera));
        let mut entries = vec![0u8; COUNT * 16];
        for chunk in entries.chunks_exact_mut(16) {
            chunk[..4].copy_from_slice(&u32::MAX.to_le_bytes());
        }
        gpu.queue.write_buffer(&self.output, 0, &entries);
        let mut enc = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("labels"),
            });
        enc.clear_buffer(&self.occupied, 0, None);
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("labels"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind, &[]);
            let groups = graph.num_nodes.div_ceil(256);
            let x = groups.min(65535) as u32;
            pass.dispatch_workgroups(x, groups.div_ceil(x as u64) as u32, 1);
        }
        let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("label_readback"),
            size: COUNT as u64 * 16,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        enc.copy_buffer_to_buffer(&self.output, 0, &buffer, 0, COUNT as u64 * 16);
        gpu.queue.submit(Some(enc.finish()));
        let (tx, rx) = mpsc::channel();
        buffer.slice(..).map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        self.pending = Some((buffer, rx));
    }
}

/// Exact visible-node bounds with a 20-byte asynchronous readback, independent
/// of graph size. Ordered float bits let integer atomics reduce signed f32s.
pub struct Bounds {
    pipeline: wgpu::ComputePipeline,
    bind: wgpu::BindGroup,
    result: wgpu::Buffer,
    count: u64,
    pending: Option<(
        wgpu::Buffer,
        mpsc::Receiver<Result<(), wgpu::BufferAsyncError>>,
    )>,
}
impl Bounds {
    pub fn new(gpu: &Gpu, graph: &GpuGraph) -> Self {
        let source = r#"
@group(0) @binding(0) var<storage,read> positions:array<vec2<f32>>;
@group(0) @binding(1) var<storage,read> sizes:array<f32>;
@group(0) @binding(2) var<storage,read_write> bounds:array<atomic<u32>>;
fn ordered(f:f32)->u32 {let bits=bitcast<u32>(f);return bits ^ select(0x80000000u,0xffffffffu,(bits&0x80000000u)!=0u);}
@compute @workgroup_size(256)
fn reduce(@builtin(workgroup_id) wg:vec3<u32>,@builtin(local_invocation_index) li:u32,@builtin(num_workgroups) dims:vec3<u32>){
    let i=(wg.y*dims.x+wg.x)*256u+li;
    if i>=arrayLength(&sizes) || sizes[i]<=0.0{return;}
    let p=positions[i];let x=ordered(p.x);let y=ordered(p.y);
    atomicMin(&bounds[0],x);atomicMin(&bounds[1],y);atomicMax(&bounds[2],x);atomicMax(&bounds[3],y);atomicAdd(&bounds[4],1u);
}
"#;
        let d = &gpu.device;
        let module = d.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("visible_bounds"),
            source: wgpu::ShaderSource::Wgsl(source.into()),
        });
        let pipeline = d.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("visible_bounds"),
            layout: None,
            module: &module,
            entry_point: Some("reduce"),
            compilation_options: Default::default(),
            cache: None,
        });
        let result = d.create_buffer(&wgpu::BufferDescriptor {
            label: Some("visible_bounds"),
            size: 20,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_DST
                | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let bind = d.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("visible_bounds"),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: graph.positions.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: graph.sizes.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: result.as_entire_binding(),
                },
            ],
        });
        Self {
            pipeline,
            bind,
            result,
            count: graph.num_nodes,
            pending: None,
        }
    }
    pub fn request(&mut self, gpu: &Gpu) {
        if self.count == 0 || self.pending.is_some() {
            return;
        }
        gpu.queue.write_buffer(
            &self.result,
            0,
            bytemuck::cast_slice(&[u32::MAX, u32::MAX, 0, 0, 0]),
        );
        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("visible_bounds"),
            });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("visible_bounds"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind, &[]);
            let groups = self.count.div_ceil(256);
            let x = groups.min(65535);
            pass.dispatch_workgroups(x as u32, groups.div_ceil(x) as u32, 1);
        }
        let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("bounds_readback"),
            size: 20,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        encoder.copy_buffer_to_buffer(&self.result, 0, &buffer, 0, 20);
        gpu.queue.submit(Some(encoder.finish()));
        let (tx, rx) = mpsc::channel();
        buffer.slice(..).map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        self.pending = Some((buffer, rx));
    }
    pub fn poll(&mut self, gpu: &Gpu) -> Option<anyhow::Result<(glam::Vec2, glam::Vec2)>> {
        let _ = gpu.device.poll(wgpu::PollType::Poll);
        let (buffer, rx) = self.pending.as_ref()?;
        match rx.try_recv() {
            Ok(Ok(())) => {
                let data = buffer.slice(..).get_mapped_range();
                let values: &[u32] = bytemuck::cast_slice(&data);
                let inverse = |v: u32| {
                    f32::from_bits(
                        v ^ if v & 0x80000000 != 0 {
                            0x80000000
                        } else {
                            u32::MAX
                        },
                    )
                };
                let result = if values[4] == 0 {
                    Err(anyhow::anyhow!("No visible nodes to fit"))
                } else {
                    Ok((
                        glam::vec2(inverse(values[0]), inverse(values[1])),
                        glam::vec2(inverse(values[2]), inverse(values[3])),
                    ))
                };
                drop(data);
                buffer.unmap();
                self.pending = None;
                Some(result)
            }
            Ok(Err(e)) => {
                self.pending = None;
                Some(Err(anyhow::anyhow!(e)))
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                self.pending = None;
                Some(Err(anyhow::anyhow!("Bounds readback stopped")))
            }
            Err(mpsc::TryRecvError::Empty) => None,
        }
    }
}
