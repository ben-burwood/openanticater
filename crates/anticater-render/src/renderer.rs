//! wgpu render backend for the knob, decoupled from any windowing layer.
//!
//! The scene is drawn to a multisampled colour target with a depth buffer and
//! resolved into a sampleable texture ([`OffscreenTargets`]). The caller decides
//! what to do with that texture: the viewer registers it with egui; the GUI
//! copies it back to CPU via [`Readback`]. This module never touches a surface.

use glam::{Mat3, Mat4, Vec3};
use wgpu::util::DeviceExt;

/// MSAA sample count baked into the pipelines and expected of every target.
pub const SAMPLE_COUNT: u32 = 4;
/// Depth format used by the offscreen targets.
pub const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

/// Global per-frame uniforms shared by every object.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Globals {
    view_proj: [f32; 16],
    cam_pos: [f32; 4],
    light_dir: [f32; 4],
}

/// Per-object uniforms.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ObjU {
    model: [f32; 16],
    nmat: [f32; 16],
    color: [f32; 4],
    emissive: [f32; 4],
    params: [f32; 4],
}

/// Material + transform inputs the caller supplies for one object each frame.
#[derive(Clone, Copy)]
pub struct ObjParams {
    pub model: Mat4,
    /// `rgb` = base colour. `a` is opacity for the opaque pipeline (always 1.0),
    /// but the additive glow pipeline reads it as the halo's blend strength.
    pub color: [f32; 4],
    pub emissive: [f32; 3],
    pub emissive_mix: f32,
    pub specular: f32,
}

impl ObjParams {
    fn to_uniform(self) -> ObjU {
        // Our transforms are rotation + translation only, so the normal matrix
        // is just the rotation part (embed the 3×3 into a mat4 for alignment).
        let nmat = Mat4::from_mat3(Mat3::from_mat4(self.model));
        ObjU {
            model: self.model.to_cols_array(),
            nmat: nmat.to_cols_array(),
            color: self.color,
            emissive: [self.emissive[0], self.emissive[1], self.emissive[2], 1.0],
            params: [self.emissive_mix, self.specular, 0.0, 0.0],
        }
    }
}

/// Everything the caller hands the renderer for a single frame.
pub struct FrameParams {
    pub view_proj: Mat4,
    pub cam_pos: Vec3,
    pub light_dir: Vec3,
    /// Background clear colour (RGBA). Consumers that composite the result over
    /// their own UI should clear to that surface's colour so no square shows —
    /// GPUI blends images with straight alpha, so a transparent clear would
    /// leave dark MSAA fringes on the silhouette.
    pub clear: [f32; 4],
    pub base: ObjParams,
    pub seam: ObjParams,
    pub knob: ObjParams,
    pub glow: ObjParams,
}

struct GpuMesh {
    vbuf: wgpu::Buffer,
    ibuf: wgpu::Buffer,
    count: u32,
}

struct Object {
    buf: wgpu::Buffer,
    bind: wgpu::BindGroup,
}

/// Offscreen render targets for one viewport size. Recreate on resize.
///
/// `color` is the resolved, single-sample texture; it carries `TEXTURE_BINDING`
/// (so egui can sample it) and `COPY_SRC` (so [`Readback`] can copy it), which
/// covers both consumers.
pub struct OffscreenTargets {
    pub color: wgpu::Texture,
    pub color_view: wgpu::TextureView,
    msaa_view: wgpu::TextureView,
    depth_view: wgpu::TextureView,
    pub size: (u32, u32),
}

impl OffscreenTargets {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat, size: (u32, u32)) -> Self {
        let size = (size.0.max(1), size.1.max(1));
        let extent = wgpu::Extent3d {
            width: size.0,
            height: size.1,
            depth_or_array_layers: 1,
        };

        let color = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("offscreen-color"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let color_view = color.create_view(&wgpu::TextureViewDescriptor::default());

        let msaa = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("offscreen-msaa"),
            size: extent,
            mip_level_count: 1,
            sample_count: SAMPLE_COUNT,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let msaa_view = msaa.create_view(&wgpu::TextureViewDescriptor::default());

        let depth = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("offscreen-depth"),
            size: extent,
            mip_level_count: 1,
            sample_count: SAMPLE_COUNT,
            dimension: wgpu::TextureDimension::D2,
            format: DEPTH_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let depth_view = depth.create_view(&wgpu::TextureViewDescriptor::default());

        Self {
            color,
            color_view,
            msaa_view,
            depth_view,
            size,
        }
    }
}

/// The knob renderer: pipelines, meshes and uniform buffers built once from a
/// device. Borrows nothing at render time except the caller's targets + frame.
pub struct KnobRenderer {
    device: wgpu::Device,
    queue: wgpu::Queue,

    pipe_opaque: wgpu::RenderPipeline,
    pipe_glow: wgpu::RenderPipeline,

    globals: Object,

    mesh_base: GpuMesh,
    mesh_seam: GpuMesh,
    mesh_knob: GpuMesh,
    mesh_glow: GpuMesh,

    obj_base: Object,
    obj_seam: Object,
    obj_knob: Object,
    obj_glow: Object,

    center_z: f32,
}

impl KnobRenderer {
    /// Build the renderer for a given colour `format` (the format the caller's
    /// [`OffscreenTargets`] and any downstream texture use).
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, format: wgpu::TextureFormat) -> Self {
        let device = device.clone();
        let queue = queue.clone();

        let scene = crate::geometry::build_scene();
        let center_z = scene.center_z;
        let mesh_base = upload_mesh(&device, &scene.base, "base");
        let mesh_seam = upload_mesh(&device, &scene.seam, "seam");
        let mesh_knob = upload_mesh(&device, &scene.knob, "knob");
        let mesh_glow = upload_mesh(&device, &scene.glow, "glow");

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("vk01-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
        });

        // Both group 0 (globals) and group 1 (per-object) are a single uniform
        // buffer at binding 0, so one layout serves both slots.
        let uniform_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("uniform-layout"),
            entries: &[uniform_entry(0)],
        });

        let globals_size = std::mem::size_of::<Globals>() as u64;
        let obj_size = std::mem::size_of::<ObjU>() as u64;
        let globals = make_uniform(&device, &uniform_layout, "globals", globals_size);
        let obj_base = make_uniform(&device, &uniform_layout, "base", obj_size);
        let obj_seam = make_uniform(&device, &uniform_layout, "seam", obj_size);
        let obj_knob = make_uniform(&device, &uniform_layout, "knob", obj_size);
        let obj_glow = make_uniform(&device, &uniform_layout, "glow", obj_size);

        let pipe_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("vk01-pipeline-layout"),
            bind_group_layouts: &[&uniform_layout, &uniform_layout],
            push_constant_ranges: &[],
        });

        let vbl = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<crate::geometry::Vertex>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &[
                wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32x3,
                    offset: 0,
                    shader_location: 0,
                },
                wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32x3,
                    offset: 12,
                    shader_location: 1,
                },
            ],
        };

        let pipe_opaque = make_pipeline(&device, &shader, &pipe_layout, format, &vbl, false);
        let pipe_glow = make_pipeline(&device, &shader, &pipe_layout, format, &vbl, true);

        Self {
            device,
            queue,
            pipe_opaque,
            pipe_glow,
            globals,
            mesh_base,
            mesh_seam,
            mesh_knob,
            mesh_glow,
            obj_base,
            obj_seam,
            obj_knob,
            obj_glow,
            center_z,
        }
    }

    /// Z of the assembly's mid-point — used as the camera orbit target.
    pub fn center_z(&self) -> f32 {
        self.center_z
    }

    /// Render one frame into `targets`. Submits its own command buffer; after
    /// this returns, `targets.color` holds the resolved image.
    pub fn render(&self, targets: &OffscreenTargets, frame: &FrameParams) {
        // Globals.
        let globals = Globals {
            view_proj: frame.view_proj.to_cols_array(),
            cam_pos: [frame.cam_pos.x, frame.cam_pos.y, frame.cam_pos.z, 1.0],
            light_dir: [frame.light_dir.x, frame.light_dir.y, frame.light_dir.z, 0.0],
        };
        self.queue
            .write_buffer(&self.globals.buf, 0, bytemuck::bytes_of(&globals));

        // Per-object uniforms.
        for (obj, p) in [
            (&self.obj_base, frame.base),
            (&self.obj_seam, frame.seam),
            (&self.obj_knob, frame.knob),
            (&self.obj_glow, frame.glow),
        ] {
            self.queue
                .write_buffer(&obj.buf, 0, bytemuck::bytes_of(&p.to_uniform()));
        }

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("vk01-encoder"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("vk01-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &targets.msaa_view,
                    resolve_target: Some(&targets.color_view),
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: frame.clear[0] as f64,
                            g: frame.clear[1] as f64,
                            b: frame.clear[2] as f64,
                            a: frame.clear[3] as f64,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &targets.depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            pass.set_bind_group(0, &self.globals.bind, &[]);

            // Opaque parts.
            pass.set_pipeline(&self.pipe_opaque);
            for (mesh, obj) in [
                (&self.mesh_base, &self.obj_base),
                (&self.mesh_seam, &self.obj_seam),
                (&self.mesh_knob, &self.obj_knob),
            ] {
                pass.set_bind_group(1, &obj.bind, &[]);
                pass.set_vertex_buffer(0, mesh.vbuf.slice(..));
                pass.set_index_buffer(mesh.ibuf.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..mesh.count, 0, 0..1);
            }

            // Additive glow halo, drawn last so it blends over the metal.
            pass.set_pipeline(&self.pipe_glow);
            pass.set_bind_group(1, &self.obj_glow.bind, &[]);
            pass.set_vertex_buffer(0, self.mesh_glow.vbuf.slice(..));
            pass.set_index_buffer(self.mesh_glow.ibuf.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(0..self.mesh_glow.count, 0, 0..1);
        }
        self.queue.submit(Some(encoder.finish()));
    }
}

/// CPU readback of an [`OffscreenTargets`] colour texture, for consumers (the
/// GUI) that display the render as an image rather than a GPU texture.
///
/// Owns a mappable buffer sized to the target, reused across frames. The
/// returned bytes are **tight** (row padding stripped) in the target's texel
/// order — for a `Bgra8*` target that is BGRA, which is what GPUI's atlas wants.
pub struct Readback {
    device: wgpu::Device,
    queue: wgpu::Queue,
    buffer: wgpu::Buffer,
    size: (u32, u32),
    padded_bytes_per_row: u32,
}

impl Readback {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, size: (u32, u32)) -> Self {
        let size = (size.0.max(1), size.1.max(1));
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let padded_bytes_per_row = (size.0 * 4).div_ceil(align) * align;
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("knob-readback"),
            size: (padded_bytes_per_row * size.1) as u64,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            device: device.clone(),
            queue: queue.clone(),
            buffer,
            size,
            padded_bytes_per_row,
        }
    }

    /// The size this readback buffer is sized for.
    pub fn size(&self) -> (u32, u32) {
        self.size
    }

    /// Copy `targets.color` to CPU and return tight texel bytes
    /// (`width * height * 4`). Blocks on the GPU (poll-wait) for the map.
    ///
    /// `targets.size` must equal this readback's size.
    pub fn read(&self, targets: &OffscreenTargets) -> Vec<u8> {
        debug_assert_eq!(targets.size, self.size, "readback/target size mismatch");
        let (w, h) = self.size;

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("knob-readback-encoder"),
            });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &targets.color,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &self.buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(self.padded_bytes_per_row),
                    rows_per_image: Some(h),
                },
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit(Some(encoder.finish()));

        let slice = self.buffer.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        self.device.poll(wgpu::Maintain::Wait);
        rx.recv().unwrap().unwrap();

        let mapped = slice.get_mapped_range();
        let unpadded = (w * 4) as usize;
        let mut out = Vec::with_capacity(unpadded * h as usize);
        for row in 0..h as usize {
            let start = row * self.padded_bytes_per_row as usize;
            out.extend_from_slice(&mapped[start..start + unpadded]);
        }
        drop(mapped);
        self.buffer.unmap();
        out
    }
}

fn uniform_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

/// A uniform buffer of `size` bytes plus its single-binding bind group.
fn make_uniform(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    name: &str,
    size: u64,
) -> Object {
    let buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(name),
        size,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some(name),
        layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: buf.as_entire_binding(),
        }],
    });
    Object { buf, bind }
}

fn upload_mesh(device: &wgpu::Device, mesh: &crate::geometry::Mesh, name: &str) -> GpuMesh {
    let vbuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some(name),
        contents: bytemuck::cast_slice(&mesh.vertices),
        usage: wgpu::BufferUsages::VERTEX,
    });
    let ibuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some(name),
        contents: bytemuck::cast_slice(&mesh.indices),
        usage: wgpu::BufferUsages::INDEX,
    });
    GpuMesh {
        vbuf,
        ibuf,
        count: mesh.indices.len() as u32,
    }
}

fn make_pipeline(
    device: &wgpu::Device,
    shader: &wgpu::ShaderModule,
    layout: &wgpu::PipelineLayout,
    format: wgpu::TextureFormat,
    vbl: &wgpu::VertexBufferLayout,
    additive: bool,
) -> wgpu::RenderPipeline {
    let blend = Some(if additive {
        // Additive weighted by src alpha, so the halo brightens what's behind it.
        wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::SrcAlpha,
                dst_factor: wgpu::BlendFactor::One,
                operation: wgpu::BlendOperation::Add,
            },
            alpha: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::Zero,
                dst_factor: wgpu::BlendFactor::One,
                operation: wgpu::BlendOperation::Add,
            },
        }
    } else {
        wgpu::BlendState::REPLACE
    });

    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("vk01-pipeline"),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("vs"),
            compilation_options: Default::default(),
            buffers: std::slice::from_ref(vbl),
        },
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            strip_index_format: None,
            front_face: wgpu::FrontFace::Ccw,
            cull_mode: None, // two-sided shading; robust to winding
            unclipped_depth: false,
            polygon_mode: wgpu::PolygonMode::Fill,
            conservative: false,
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: DEPTH_FORMAT,
            // The glow pass tests depth but doesn't write it.
            depth_write_enabled: !additive,
            depth_compare: wgpu::CompareFunction::LessEqual,
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        }),
        multisample: wgpu::MultisampleState {
            count: SAMPLE_COUNT,
            mask: !0,
            alpha_to_coverage_enabled: false,
        },
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some("fs"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview: None,
        cache: None,
    })
}
