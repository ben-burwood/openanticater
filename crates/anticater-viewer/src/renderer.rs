//! eframe/egui adapter around the shared [`anticater_render`] renderer.
//!
//! The 3D scene, pipelines and geometry all live in `anticater-render`. This
//! wrapper owns only the egui-specific bits: it keeps the offscreen targets
//! sized to the viewport and registers the resolved colour texture with egui so
//! it can be drawn as an image. Rendering itself is delegated to the shared
//! [`KnobRenderer`], sharing eframe's own wgpu device (no CPU readback).

use std::sync::Arc;

use eframe::egui;
use eframe::egui::mutex::RwLock;
use eframe::egui_wgpu::{Renderer as EguiRenderer, RenderState};
use eframe::wgpu;

// Re-exported so the app can name the frame type without importing the lib.
pub use anticater_render::FrameParams;
use anticater_render::{KnobRenderer, OffscreenTargets};

/// Offscreen targets plus the egui texture id registered for the resolved image.
struct Targets {
    inner: OffscreenTargets,
    tex_id: egui::TextureId,
}

pub struct Renderer {
    device: wgpu::Device,
    egui_renderer: Arc<RwLock<EguiRenderer>>,
    format: wgpu::TextureFormat,
    knob: KnobRenderer,
    targets: Option<Targets>,
}

impl Renderer {
    pub fn new(rs: &RenderState) -> Self {
        let device = rs.device.clone();
        let queue = rs.queue.clone();
        let format = rs.target_format;
        let egui_renderer = rs.renderer.clone();
        let knob = KnobRenderer::new(&device, &queue, format);
        Self {
            device,
            egui_renderer,
            format,
            knob,
            targets: None,
        }
    }

    /// Z of the assembly's mid-point — used as the camera orbit target.
    pub fn center_z(&self) -> f32 {
        self.knob.center_z()
    }

    /// Ensure the offscreen targets match `size`; (re)register the egui texture
    /// when they change. Returns the egui texture id to display.
    fn ensure_targets(&mut self, size: (u32, u32)) -> egui::TextureId {
        let size = (size.0.max(1), size.1.max(1));
        if let Some(t) = &self.targets {
            if t.inner.size == size {
                return t.tex_id;
            }
        }

        let inner = OffscreenTargets::new(&self.device, self.format, size);

        let mut egui_renderer = self.egui_renderer.write();
        if let Some(old) = self.targets.take() {
            egui_renderer.free_texture(&old.tex_id);
        }
        let tex_id = egui_renderer.register_native_texture(
            &self.device,
            &inner.color_view,
            wgpu::FilterMode::Linear,
        );

        self.targets = Some(Targets { inner, tex_id });
        tex_id
    }

    /// Render the scene into the offscreen target and return its egui texture id.
    pub fn render(&mut self, size: (u32, u32), frame: &FrameParams) -> egui::TextureId {
        let tex_id = self.ensure_targets(size);
        let targets = self.targets.as_ref().unwrap();
        self.knob.render(&targets.inner, frame);
        tex_id
    }
}
