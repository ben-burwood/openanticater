//! egui application: side-panel controls plus the interactive 3D viewport.
//!
//! Drag with the **left** button over the knob to spin it (it keeps momentum);
//! hold left to press it down. Drag with the **right** button to orbit the
//! camera, and scroll to zoom. The LED seam colour, brightness, rainbow cycle
//! and pulse are all live.

use std::f32::consts::TAU;

use anticater_render::{
    Camera, DEFAULT_VIEW, KnobPose, LedLook, frame_params as scene_frame_params, hsv_to_rgb,
};
use eframe::egui;

use crate::renderer::{FrameParams, Renderer};

/// The SCAD `col_led` cyan — the default LED colour and the "Cyan" preset.
const LED_CYAN: [f32; 3] = [0.15, 0.85, 1.0];

/// LED colour presets shown as buttons.
const PRESETS: [(&str, [f32; 3]); 4] = [
    ("Cyan", LED_CYAN),
    ("Green", [0.05, 0.8, 0.2]),
    ("Magenta", [1.0, 0.1, 0.7]),
    ("White", [1.0, 1.0, 1.0]),
];

pub struct App {
    renderer: Renderer,

    // LED / glow state.
    led_on: bool,
    glow_rgb: [f32; 3], // linear RGB
    rainbow: bool,
    rainbow_speed: f32,
    brightness: f32,
    pulse: bool,
    pulse_speed: f32,
    pulse_depth: f32, // how deep the pulse dims (0 = none, 1 = to black)

    // Camera (orbit).
    yaw: f32,
    pitch: f32,
    dist: f32,

    // Knob animation.
    spin_angle: f32,
    spin_vel: f32,
    press_amt: f32,
    press_target: f32,

    time: f32,
    /// Camera orbit target height, taken from the model's bounds.
    center_z: f32,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let rs = cc
            .wgpu_render_state
            .as_ref()
            .expect("viewer requires the wgpu backend");
        let renderer = Renderer::new(rs);
        let center_z = renderer.center_z();
        Self {
            renderer,
            center_z,
            led_on: true,
            glow_rgb: LED_CYAN,
            rainbow: false,
            rainbow_speed: 0.15,
            brightness: 1.0,
            pulse: true,
            pulse_speed: 0.8,
            pulse_depth: 0.55,
            yaw: DEFAULT_VIEW.0,
            pitch: DEFAULT_VIEW.1,
            dist: DEFAULT_VIEW.2,
            spin_angle: 0.0,
            spin_vel: 0.0,
            press_amt: 0.0,
            press_target: 0.0,
            time: 0.0,
        }
    }

    fn reset_view(&mut self) {
        (self.yaw, self.pitch, self.dist) = DEFAULT_VIEW;
    }

    /// The current LED colour (rainbow overrides the picker).
    fn led_color(&self) -> [f32; 3] {
        if self.rainbow {
            hsv_to_rgb((self.time * self.rainbow_speed).fract(), 0.85, 1.0)
        } else {
            self.glow_rgb
        }
    }

    fn controls(&mut self, ui: &mut egui::Ui) {
        ui.heading("Anticater VK-01");
        ui.label(egui::RichText::new("Volume knob viewer").weak());
        ui.add_space(8.0);

        egui::CollapsingHeader::new("LED glow")
            .default_open(true)
            .show(ui, |ui| {
                ui.checkbox(&mut self.led_on, "LED on");

                ui.horizontal(|ui| {
                    ui.label("Colour");
                    ui.add_enabled_ui(!self.rainbow, |ui| {
                        ui.color_edit_button_rgb(&mut self.glow_rgb);
                    });
                    if self.rainbow {
                        ui.label(egui::RichText::new("(rainbow)").weak());
                    }
                });

                ui.horizontal(|ui| {
                    ui.label("Presets:");
                    for (name, rgb) in PRESETS {
                        if ui.button(name).clicked() {
                            self.glow_rgb = rgb;
                            self.rainbow = false;
                            self.led_on = true;
                        }
                    }
                });

                ui.checkbox(&mut self.rainbow, "Rainbow cycle");
                ui.add_enabled(
                    self.rainbow,
                    egui::Slider::new(&mut self.rainbow_speed, 0.02..=1.0).text("cycle speed"),
                );

                ui.add(egui::Slider::new(&mut self.brightness, 0.0..=1.5).text("brightness"));
            });

        egui::CollapsingHeader::new("Pulse")
            .default_open(true)
            .show(ui, |ui| {
                ui.checkbox(&mut self.pulse, "Pulsing animation");
                ui.add_enabled(
                    self.pulse,
                    egui::Slider::new(&mut self.pulse_speed, 0.1..=3.0).text("pulses/sec"),
                );
                ui.add_enabled(
                    self.pulse,
                    egui::Slider::new(&mut self.pulse_depth, 0.0..=1.0).text("depth"),
                );
            });

        egui::CollapsingHeader::new("Knob")
            .default_open(true)
            .show(ui, |ui| {
                ui.label(format!("Spin: {:+.0}°/s", self.spin_vel.to_degrees()));
                let pressed = self.press_amt > 0.5;
                ui.label(if pressed { "State: PRESSED" } else { "State: idle" });
                if ui.button("Stop spinning").clicked() {
                    self.spin_vel = 0.0;
                }
            });

        ui.add_space(8.0);
        if ui.button("Reset view").clicked() {
            self.reset_view();
        }

        ui.add_space(12.0);
        ui.separator();
        ui.label(
            egui::RichText::new(
                "Left-drag: spin knob\nHold left: press\nRight-drag: orbit\nScroll: zoom",
            )
            .weak()
            .small(),
        );
    }

    /// Update camera + animation from input on the viewport response.
    fn handle_input(&mut self, ctx: &egui::Context, resp: &egui::Response, dt: f32) {
        let mut dragging_spin = false;

        if resp.dragged_by(egui::PointerButton::Secondary) {
            let d = resp.drag_delta();
            self.yaw -= d.x * 0.01;
            self.pitch = (self.pitch + d.y * 0.01).clamp(-1.4, 1.4);
        }

        if resp.dragged_by(egui::PointerButton::Primary) {
            let d = resp.drag_delta();
            let gain = 0.012;
            self.spin_angle += d.x * gain;
            self.spin_vel = d.x * gain / dt.max(1e-3);
            dragging_spin = true;
        }

        // Press: primary button held down over the knob.
        let primary_down =
            resp.is_pointer_button_down_on() && ctx.input(|i| i.pointer.primary_down());
        self.press_target = if primary_down { 1.0 } else { 0.0 };

        if resp.hovered() {
            let scroll = ctx.input(|i| i.raw_scroll_delta.y);
            if scroll != 0.0 {
                self.dist = (self.dist * (1.0 - scroll * 0.0015)).clamp(60.0, 320.0);
            }
        }

        // Spin momentum when not actively dragging.
        if !dragging_spin {
            self.spin_angle += self.spin_vel * dt;
            self.spin_vel *= 0.92f32.powf(dt * 60.0);
            if self.spin_vel.abs() < 1e-4 {
                self.spin_vel = 0.0;
            }
        }

        // Ease the press toward its target.
        let k = (dt * 14.0).min(1.0);
        self.press_amt += (self.press_target - self.press_amt) * k;
    }

    /// Assemble the per-frame render parameters from the current state.
    fn frame_params(&self, aspect: f32) -> FrameParams {
        // LED intensity: brightness × pulse, brightened a touch while pressed.
        let pulse = if self.pulse {
            let p = 0.5 + 0.5 * (self.time * TAU * self.pulse_speed).sin();
            1.0 - self.pulse_depth * (1.0 - p)
        } else {
            1.0
        };
        let press_boost = 1.0 + 0.5 * self.press_amt;
        let led = LedLook {
            color: self.led_color(),
            on: self.led_on,
            intensity: self.brightness * pulse * press_boost,
        };
        let camera = Camera {
            yaw: self.yaw,
            pitch: self.pitch,
            dist: self.dist,
        };
        let pose = KnobPose {
            spin_angle: self.spin_angle,
            press_amt: self.press_amt,
        };
        // The viewer's own dark studio background.
        scene_frame_params(
            camera,
            self.center_z,
            aspect,
            pose,
            led,
            [0.043, 0.047, 0.058, 1.0],
        )
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let dt = ctx.input(|i| i.stable_dt).clamp(0.0, 0.1);
        self.time += dt;

        egui::SidePanel::left("controls")
            .resizable(false)
            .default_width(240.0)
            .show(ctx, |ui| {
                self.controls(ui);
            });

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ctx, |ui| {
                let available = ui.available_size();
                let ppp = ctx.pixels_per_point();
                let w = (available.x * ppp).round().max(1.0) as u32;
                let h = (available.y * ppp).round().max(1.0) as u32;

                let params = self.frame_params(w as f32 / h as f32);
                let tex_id = self.renderer.render((w, h), &params);

                let img = egui::Image::new(egui::load::SizedTexture::new(tex_id, available))
                    .sense(egui::Sense::click_and_drag());
                let resp = ui.add(img);
                self.handle_input(ctx, &resp, dt);
            });

        // Animate continuously.
        ctx.request_repaint();
    }
}
