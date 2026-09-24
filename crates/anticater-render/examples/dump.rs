//! Headless sanity check: render the VK-01 knob on a headless wgpu device via
//! the shared renderer and write it to a PNG — no window, no egui, no GPUI.
//!
//! Run: `cargo run -p anticater-render --example dump -- out.png`

use anticater_render::{FrameParams, KnobRenderer, ObjParams, Readback, OffscreenTargets};
use glam::{Mat4, Vec3};

const DIM: u32 = 512;
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Bgra8UnormSrgb;

fn main() {
    let path = std::env::args().nth(1).unwrap_or_else(|| "knob.png".into());

    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        force_fallback_adapter: false,
        compatible_surface: None,
    }))
    .expect("no adapter");
    let (device, queue) = pollster::block_on(adapter.request_device(
        &wgpu::DeviceDescriptor {
            label: Some("dump"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::downlevel_defaults(),
            memory_hints: Default::default(),
        },
        None,
    ))
    .expect("no device");

    let knob = KnobRenderer::new(&device, &queue, FORMAT);
    let targets = OffscreenTargets::new(&device, FORMAT, (DIM, DIM));
    let readback = Readback::new(&device, &queue, (DIM, DIM));

    knob.render(&targets, &frame(knob.center_z()));

    let mut bgra = readback.read(&targets);
    for px in bgra.chunks_exact_mut(4) {
        px.swap(0, 2); // BGRA → RGBA for a normal PNG
    }
    image::RgbaImage::from_raw(DIM, DIM, bgra)
        .expect("size")
        .save(&path)
        .expect("save");
    println!("wrote {path}");
}

/// A representative frame: orbit camera on the assembly, static cyan LED seam.
fn frame(center_z: f32) -> FrameParams {
    let target = Vec3::new(0.0, 0.0, center_z);
    let (yaw, pitch, dist) = (-1.2f32, 0.5f32, 120.0f32);
    let cp = pitch.cos();
    let dir = Vec3::new(cp * yaw.cos(), cp * yaw.sin(), pitch.sin());
    let eye = target + dir * dist;
    let view = Mat4::look_at_rh(eye, target, Vec3::Z);
    let proj = Mat4::perspective_rh(40f32.to_radians(), 1.0, 1.0, 600.0);

    let led = [0.15, 0.85, 1.0];
    FrameParams {
        view_proj: proj * view,
        cam_pos: eye,
        light_dir: Vec3::new(0.4, 0.5, -0.85).normalize(),
        clear: [0.043, 0.047, 0.058, 1.0],
        base: ObjParams {
            model: Mat4::IDENTITY,
            color: [0.62, 0.63, 0.66, 1.0],
            emissive: [0.0; 3],
            emissive_mix: 0.0,
            specular: 0.35,
        },
        seam: ObjParams {
            model: Mat4::IDENTITY,
            color: [led[0], led[1], led[2], 1.0],
            emissive: led,
            emissive_mix: 1.0,
            specular: 0.0,
        },
        knob: ObjParams {
            model: Mat4::IDENTITY,
            color: [0.15, 0.16, 0.19, 1.0],
            emissive: [0.0; 3],
            emissive_mix: 0.0,
            specular: 0.55,
        },
        glow: ObjParams {
            model: Mat4::IDENTITY,
            color: [led[0], led[1], led[2], 0.6],
            emissive: led,
            emissive_mix: 1.0,
            specular: 0.0,
        },
    }
}
