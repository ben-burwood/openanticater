//! Bridge prototype: headless wgpu → CPU readback → GPUI `img()`.
//!
//! This exists to de-risk the one unknown in putting a live 3D knob inside the
//! GPUI control GUI: GPUI (as published, 0.2.2) has **no** API to hand it an
//! external wgpu texture, and no per-frame GPU paint callback like egui's. The
//! only bridge is through CPU memory — render offscreen with wgpu, read the
//! pixels back, and display them as a GPUI image.
//!
//! It renders a spinning, lit 3D cube (a stand-in for the VK-01 mesh) on its own
//! headless wgpu device, reads it back as BGRA, and shows it in a GPUI window via
//! `img()`. It exercises every risky piece of the real integration:
//!
//!   * headless wgpu init (no surface/window),
//!   * offscreen colour + depth target,
//!   * `copy_texture_to_buffer` readback with 256-byte row-alignment handling,
//!   * a fresh `RenderImage` per frame (GPUI's sprite atlas caches by image id,
//!     so **reusing** an id would show a stale tile — each frame must be new),
//!   * `window.drop_image(prev)` every frame so the atlas doesn't grow forever
//!     — this is the leak the prototype is really here to prove is avoidable,
//!   * a ~60 Hz animation timer driving `cx.notify()`, and
//!   * left-drag → spin (with momentum), to feel the latency of the CPU bridge.
//!
//! Run: `cargo run --bin knob3d-proto`. Left-drag the cube to spin it; let go
//! and it coasts. If it animates smoothly and memory stays flat, the bridge is
//! viable and the full integration (swap the cube for the VK-01 renderer, feed
//! LED/press state in) follows the same shape.

#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

use std::sync::Arc;
use std::time::Duration;

use glam::{Mat4, Vec3};
use gpui::{
    App, AppContext, Application, Bounds, Context, InteractiveElement, IntoElement, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement, Render, RenderImage, SharedString,
    Styled, Window, WindowBounds, WindowOptions, div, img, px, rgb, size,
};
use image::{Frame, RgbaImage};
use wgpu::util::DeviceExt;

/// Fixed render resolution. A multiple of 64 keeps `width * 4` a multiple of the
/// 256-byte copy row-alignment, so readback needs no per-row unpadding — but we
/// handle padding generally anyway in case this changes.
const DIM: u32 = 512;
/// Animation tick (~60 Hz).
const TICK: Duration = Duration::from_millis(16);

// ---------------------------------------------------------------------------
// Headless wgpu renderer
// ---------------------------------------------------------------------------

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Vertex {
    pos: [f32; 3],
    normal: [f32; 3],
    color: [f32; 3],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    mvp: [f32; 16],
    model: [f32; 16],
    light: [f32; 4],
}

struct GpuRenderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::RenderPipeline,
    ubuf: wgpu::Buffer,
    bind: wgpu::BindGroup,
    vbuf: wgpu::Buffer,
    ibuf: wgpu::Buffer,
    index_count: u32,
    color: wgpu::Texture,
    depth_view: wgpu::TextureView,
    readback: wgpu::Buffer,
    padded_bytes_per_row: u32,
}

impl GpuRenderer {
    fn new() -> Self {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            compatible_surface: None,
        }))
        .expect("no suitable GPU adapter");
        let (device, queue) = pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("knob3d-proto"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::downlevel_defaults(),
                memory_hints: wgpu::MemoryHints::default(),
            },
            None,
        ))
        .expect("failed to create device");

        // GPUI's sprite atlas stores image data as BGRA; render straight into a
        // matching sRGB target so the bytes we read back are display-ready.
        let format = wgpu::TextureFormat::Bgra8UnormSrgb;

        let (verts, indices) = cube();
        let vbuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("cube-vertices"),
            contents: bytemuck::cast_slice(&verts),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let ibuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("cube-indices"),
            contents: bytemuck::cast_slice(&indices),
            usage: wgpu::BufferUsages::INDEX,
        });

        let ubuf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("uniforms"),
            size: std::mem::size_of::<Uniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("uniform-layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("uniform-bind"),
            layout: &bind_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: ubuf.as_entire_binding(),
            }],
        });

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("cube-shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let pipe_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("pipe-layout"),
            bind_group_layouts: &[&bind_layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("cube-pipeline"),
            layout: Some(&pipe_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Vertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Float32x3],
                }],
            },
            primitive: wgpu::PrimitiveState {
                cull_mode: Some(wgpu::Face::Back),
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                depth_write_enabled: true,
                depth_compare: wgpu::CompareFunction::Less,
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview: None,
            cache: None,
        });

        let extent = wgpu::Extent3d {
            width: DIM,
            height: DIM,
            depth_or_array_layers: 1,
        };
        let color = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("offscreen-color"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let depth = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("offscreen-depth"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Depth32Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let depth_view = depth.create_view(&Default::default());

        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let unpadded = DIM * 4;
        let padded_bytes_per_row = unpadded.div_ceil(align) * align;
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: (padded_bytes_per_row * DIM) as u64,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            device,
            queue,
            pipeline,
            ubuf,
            bind,
            vbuf,
            ibuf,
            index_count: indices.len() as u32,
            color,
            depth_view,
            readback,
            padded_bytes_per_row,
        }
    }

    /// Render the cube at `yaw`/`pitch` (radians) and return tight BGRA bytes
    /// (`DIM * DIM * 4`), ready to wrap in a `RenderImage`.
    fn render(&self, yaw: f32, pitch: f32) -> Vec<u8> {
        let model = Mat4::from_rotation_y(yaw) * Mat4::from_rotation_x(pitch);
        let view = Mat4::look_at_rh(Vec3::new(0.0, 0.8, 4.2), Vec3::ZERO, Vec3::Y);
        let proj = Mat4::perspective_rh(45f32.to_radians(), 1.0, 0.1, 50.0);
        let uniforms = Uniforms {
            mvp: (proj * view * model).to_cols_array(),
            model: model.to_cols_array(),
            light: [0.5, 0.8, 0.4, 0.0],
        };
        self.queue
            .write_buffer(&self.ubuf, 0, bytemuck::bytes_of(&uniforms));

        let color_view = self.color.create_view(&Default::default());
        let mut enc = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("enc") });
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &color_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.043,
                            g: 0.047,
                            b: 0.058,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Discard,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind, &[]);
            pass.set_vertex_buffer(0, self.vbuf.slice(..));
            pass.set_index_buffer(self.ibuf.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(0..self.index_count, 0, 0..1);
        }
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &self.color,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &self.readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(self.padded_bytes_per_row),
                    rows_per_image: Some(DIM),
                },
            },
            wgpu::Extent3d {
                width: DIM,
                height: DIM,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit(Some(enc.finish()));

        // Map the readback buffer and drop per-row padding into a tight buffer.
        let slice = self.readback.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        self.device.poll(wgpu::Maintain::Wait);
        rx.recv().unwrap().unwrap();

        let mapped = slice.get_mapped_range();
        let unpadded = (DIM * 4) as usize;
        let mut out = Vec::with_capacity(unpadded * DIM as usize);
        for row in 0..DIM as usize {
            let start = row * self.padded_bytes_per_row as usize;
            out.extend_from_slice(&mapped[start..start + unpadded]);
        }
        drop(mapped);
        self.readback.unmap();
        out
    }
}

/// A unit cube: 4 verts per face (flat per-face normals + a distinct colour).
fn cube() -> (Vec<Vertex>, Vec<u32>) {
    let faces: [([f32; 3], [f32; 3]); 6] = [
        ([0.0, 0.0, 1.0], [0.90, 0.30, 0.35]),  // +Z front, red
        ([0.0, 0.0, -1.0], [0.30, 0.55, 0.90]), // -Z back, blue
        ([1.0, 0.0, 0.0], [0.35, 0.80, 0.45]),  // +X right, green
        ([-1.0, 0.0, 0.0], [0.95, 0.75, 0.25]), // -X left, gold
        ([0.0, 1.0, 0.0], [0.75, 0.45, 0.90]),  // +Y top, purple
        ([0.0, -1.0, 0.0], [0.25, 0.75, 0.80]), // -Y bottom, cyan
    ];
    let mut verts = Vec::with_capacity(24);
    let mut indices = Vec::with_capacity(36);
    for (normal, color) in faces {
        let n = Vec3::from(normal);
        // Build an in-plane basis (u, v) for this face.
        let up = if n.z.abs() > 0.9 { Vec3::Y } else { Vec3::Z };
        let u = up.cross(n).normalize();
        let v = n.cross(u);
        let base = verts.len() as u32;
        for (su, sv) in [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
            let p = (n + u * su + v * sv) * 0.5;
            verts.push(Vertex {
                pos: p.to_array(),
                normal,
                color,
            });
        }
        indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }
    (verts, indices)
}

const SHADER: &str = r#"
struct U { mvp: mat4x4<f32>, model: mat4x4<f32>, light: vec4<f32> };
@group(0) @binding(0) var<uniform> u: U;

struct VO {
    @builtin(position) clip: vec4<f32>,
    @location(0) n: vec3<f32>,
    @location(1) col: vec3<f32>,
};

@vertex
fn vs(@location(0) p: vec3<f32>, @location(1) nrm: vec3<f32>, @location(2) c: vec3<f32>) -> VO {
    var o: VO;
    o.clip = u.mvp * vec4<f32>(p, 1.0);
    o.n = (u.model * vec4<f32>(nrm, 0.0)).xyz;
    o.col = c;
    return o;
}

@fragment
fn fs(i: VO) -> @location(0) vec4<f32> {
    let N = normalize(i.n);
    let L = normalize(u.light.xyz);
    let d = max(dot(N, L), 0.0);
    let c = i.col * (0.28 + 0.72 * d);
    return vec4<f32>(c, 1.0);
}
"#;

// ---------------------------------------------------------------------------
// GPUI view
// ---------------------------------------------------------------------------

struct Proto {
    gpu: GpuRenderer,
    yaw: f32,
    pitch: f32,
    vel: f32,
    dragging: bool,
    last_x: f32,
    /// Frame currently on screen.
    frame: Option<Arc<RenderImage>>,
    /// Superseded frames awaiting `window.drop_image` on the next `render`, so
    /// the sprite atlas doesn't accumulate a tile per animation frame.
    retire: Vec<Arc<RenderImage>>,
    frames: u64,
}

impl Proto {
    fn new(cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            gpu: GpuRenderer::new(),
            yaw: 0.6,
            pitch: 0.35,
            vel: 0.9,
            dragging: false,
            last_x: 0.0,
            frame: None,
            retire: Vec::new(),
            frames: 0,
        };
        this.produce_frame();
        this.start_anim(cx);
        this
    }

    /// Render one frame on the GPU and stage it, retiring the current frame so
    /// `render` can drop it from the atlas.
    fn produce_frame(&mut self) {
        let bgra = self.gpu.render(self.yaw, self.pitch);
        let buffer = RgbaImage::from_raw(DIM, DIM, bgra).expect("frame buffer size mismatch");
        let image = Arc::new(RenderImage::new(vec![Frame::new(buffer)]));
        if let Some(old) = self.frame.take() {
            self.retire.push(old);
        }
        self.frame = Some(image);
        self.frames += 1;
    }

    fn start_anim(&self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(TICK).await;
                let dt = TICK.as_secs_f32();
                let go = this.update(cx, |app, cx| {
                    if !app.dragging {
                        app.yaw += app.vel * dt;
                        app.vel *= 0.985; // gentle coast; keeps a small idle spin
                        if app.vel.abs() < 0.15 {
                            app.vel = app.vel.signum() * 0.15;
                        }
                    }
                    app.produce_frame();
                    cx.notify();
                });
                if go.is_err() {
                    break; // view dropped
                }
            }
        })
        .detach();
    }
}

impl Render for Proto {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Retire superseded frames' atlas tiles now that a newer one is staged.
        for old in self.retire.drain(..) {
            let _ = window.drop_image(old);
        }

        let stage = match self.frame.clone() {
            Some(image) => div()
                .w(px(DIM as f32))
                .h(px(DIM as f32))
                .child(img(image).w(px(DIM as f32)).h(px(DIM as f32)))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|app, ev: &MouseDownEvent, _w, _cx| {
                        app.dragging = true;
                        app.last_x = f32::from(ev.position.x);
                        app.vel = 0.0;
                    }),
                )
                .on_mouse_move(cx.listener(|app, ev: &MouseMoveEvent, _w, _cx| {
                    if app.dragging {
                        let x = f32::from(ev.position.x);
                        let dx = x - app.last_x;
                        app.last_x = x;
                        let gain = 0.01;
                        app.yaw += dx * gain;
                        app.vel = dx * gain / TICK.as_secs_f32();
                    }
                }))
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(|app, _ev: &MouseUpEvent, _w, _cx| {
                        app.dragging = false;
                    }),
                )
                .into_any_element(),
            None => div().child("rendering…").into_any_element(),
        };

        div()
            .flex()
            .flex_col()
            .items_center()
            .gap_3()
            .size_full()
            .p_4()
            .bg(rgb(0x11151c))
            .text_color(rgb(0xe6e9ef))
            .child(div().text_xl().child("GPUI ← wgpu image bridge"))
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(0x8b93a3))
                    .child(SharedString::from(format!(
                        "left-drag to spin · frame {} · {DIM}×{DIM} BGRA readback",
                        self.frames
                    ))),
            )
            .child(stage)
    }
}

fn main() {
    // Headless correctness check: `--dump <path.png>` renders a single frame and
    // writes it out (converting BGRA→RGBA), so the wgpu→pixels path can be
    // verified without a visible window. No GPUI involved.
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() == Some("--dump") {
        let path = args.next().unwrap_or_else(|| "knob3d.png".into());
        let gpu = GpuRenderer::new();
        let mut bgra = gpu.render(0.6, 0.35);
        for px in bgra.chunks_exact_mut(4) {
            px.swap(0, 2); // BGRA → RGBA for a normal PNG
        }
        let buf = RgbaImage::from_raw(DIM, DIM, bgra).expect("size mismatch");
        buf.save(&path).expect("failed to write PNG");
        println!("wrote {path}");
        return;
    }

    Application::new().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(640.), px(680.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_window, cx| cx.new(Proto::new),
        )
        .expect("failed to open window");
        cx.activate(true);
    });
}
