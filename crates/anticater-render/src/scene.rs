//! Shared knob presentation: camera math, materials and the per-frame assembly
//! both the standalone viewer and the control GUI use, so their look can't drift.
//!
//! Consumers keep their own *input* handling (egui vs GPUI) and their own LED
//! model, then hand the resolved camera pose, knob pose and LED look to
//! [`frame_params`], which builds the [`FrameParams`] for the renderer.

use glam::{Mat4, Vec3};

use crate::renderer::{FrameParams, ObjParams};

/// Metal colours, roughly matching the SCAD `col_base` / `col_knob`.
pub const BASE_COLOR: [f32; 4] = [0.62, 0.63, 0.66, 1.0];
pub const KNOB_COLOR: [f32; 4] = [0.15, 0.16, 0.19, 1.0];
/// How far the knob dips when pressed (mm).
pub const PRESS_DEPTH: f32 = 1.6;
/// Default camera orbit pose (yaw, pitch, distance).
pub const DEFAULT_VIEW: (f32, f32, f32) = (-1.2, 0.5, 120.0);

/// Orbit camera pose around the assembly's central axis.
#[derive(Clone, Copy)]
pub struct Camera {
    pub yaw: f32,
    pub pitch: f32,
    pub dist: f32,
}

impl Default for Camera {
    fn default() -> Self {
        Self {
            yaw: DEFAULT_VIEW.0,
            pitch: DEFAULT_VIEW.1,
            dist: DEFAULT_VIEW.2,
        }
    }
}

/// The knob's animated pose: rotation about its axis and how far it's pressed in.
#[derive(Clone, Copy, Default)]
pub struct KnobPose {
    pub spin_angle: f32,
    /// 0 = resting, 1 = fully pressed.
    pub press_amt: f32,
}

/// The resolved LED appearance for a frame. `intensity` already folds in the
/// consumer's brightness / pulse / press-boost — the scene only gates it by `on`.
#[derive(Clone, Copy)]
pub struct LedLook {
    pub color: [f32; 3],
    pub on: bool,
    pub intensity: f32,
}

/// Build the per-frame render inputs from a camera + knob pose + LED look.
pub fn frame_params(
    camera: Camera,
    center_z: f32,
    aspect: f32,
    pose: KnobPose,
    led: LedLook,
    clear: [f32; 4],
) -> FrameParams {
    let target = Vec3::new(0.0, 0.0, center_z);
    let cp = camera.pitch.cos();
    let dir = Vec3::new(
        cp * camera.yaw.cos(),
        cp * camera.yaw.sin(),
        camera.pitch.sin(),
    );
    let eye = target + dir * camera.dist;
    let view = Mat4::look_at_rh(eye, target, Vec3::Z);
    let proj = Mat4::perspective_rh(40f32.to_radians(), aspect.max(0.1), 1.0, 600.0);
    let light_dir = Vec3::new(0.4, 0.5, -0.85).normalize();

    let c = led.color;
    let inten = if led.on { led.intensity } else { 0.0 };
    let seam_emissive = c.map(|x| (x * inten).min(1.8));
    let glow_alpha = if led.on {
        (0.2 + 0.55 * inten.min(1.2)).clamp(0.0, 0.85)
    } else {
        0.0
    };
    let glow_emissive = c.map(|x| (x * inten.min(1.2)).min(1.5));

    // Knob transform: press dips it, drag spins it about the axis.
    let press_depth = pose.press_amt * PRESS_DEPTH;
    let knob_model = Mat4::from_translation(Vec3::new(0.0, 0.0, -press_depth))
        * Mat4::from_rotation_z(pose.spin_angle);

    FrameParams {
        view_proj: proj * view,
        cam_pos: eye,
        light_dir,
        clear,
        base: ObjParams {
            model: Mat4::IDENTITY,
            color: BASE_COLOR,
            emissive: [0.0; 3],
            emissive_mix: 0.0,
            specular: 0.35,
        },
        seam: ObjParams {
            model: Mat4::IDENTITY,
            color: [c[0], c[1], c[2], 1.0],
            emissive: seam_emissive,
            emissive_mix: 1.0,
            specular: 0.0,
        },
        knob: ObjParams {
            model: knob_model,
            color: KNOB_COLOR,
            emissive: [0.0; 3],
            emissive_mix: 0.0,
            specular: 0.55,
        },
        glow: ObjParams {
            model: Mat4::IDENTITY,
            color: [c[0], c[1], c[2], glow_alpha],
            emissive: glow_emissive,
            emissive_mix: 1.0,
            specular: 0.0,
        },
    }
}

/// HSV (all in 0..1) to linear-ish RGB.
pub fn hsv_to_rgb(h: f32, s: f32, v: f32) -> [f32; 3] {
    let h = (h.fract() + 1.0).fract() * 6.0;
    let i = h.floor() as i32;
    let f = h - i as f32;
    let p = v * (1.0 - s);
    let q = v * (1.0 - s * f);
    let t = v * (1.0 - s * (1.0 - f));
    match i.rem_euclid(6) {
        0 => [v, t, p],
        1 => [q, v, p],
        2 => [p, v, t],
        3 => [p, q, v],
        4 => [t, p, v],
        _ => [v, p, q],
    }
}
