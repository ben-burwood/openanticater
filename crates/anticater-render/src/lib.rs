//! Shared 3D renderer for the Anticater VK-01 knob.
//!
//! This crate is **pure wgpu** — no windowing, no egui. It owns the knob's
//! geometry (rendered from `hardware/vk01.scad` at build time; see `build.rs`),
//! the shader, the render pipelines, and the per-frame draw. It does **not** own
//! a `wgpu::Device`: the caller injects one, so the same scene code serves both
//!
//!   * the standalone **viewer** (`anticater-viewer`), which shares eframe's
//!     wgpu device and hands the resolved texture straight to egui, and
//!   * the control **GUI** (`anticater-ui`), which renders on a headless device
//!     and reads the result back to CPU to display as a GPUI image.
//!
//! A caller builds a [`KnobRenderer`] once, creates [`OffscreenTargets`] for its
//! viewport size, fills in a [`FrameParams`] each frame, and calls
//! [`KnobRenderer::render`]. The GUI additionally uses [`Readback`] to pull the
//! rendered pixels back as BGRA.

pub mod geometry;
mod renderer;
mod scene;

pub use renderer::{
    DEPTH_FORMAT, FrameParams, KnobRenderer, ObjParams, OffscreenTargets, Readback, SAMPLE_COUNT,
};
pub use scene::{
    BASE_COLOR, Camera, DEFAULT_VIEW, KNOB_COLOR, KnobPose, LedLook, PRESS_DEPTH, frame_params,
    hsv_to_rgb,
};
