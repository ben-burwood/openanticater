//! Background 3D render thread for the in-GUI knob.
//!
//! GPUI can't take an external GPU texture, so the knob is rendered on a
//! dedicated thread with a headless wgpu device (via [`anticater_render`]) and
//! streamed to the UI as CPU-side **BGRA** frames — the format GPUI's image
//! atlas wants. This mirrors the HID `worker`: two channels, [`RenderCmd`] in
//! from the UI, [`Frame`] out to it.
//!
//! The thread free-runs at ~60 Hz because the knob is always animating (LED
//! pulse, spin momentum, press easing) — like the standalone viewer's
//! continuous repaint. All camera/knob/LED easing lives here in [`KnobState`],
//! ported from the viewer's `app.rs`, so the UI only sends raw input deltas.

use std::sync::mpsc::{
    self, Receiver, RecvTimeoutError, Sender, SyncSender, TryRecvError, TrySendError,
};
use std::time::{Duration, Instant};

use anticater_core::LedMode;
use anticater_render::{
    Camera, DEFAULT_VIEW, FrameParams, KnobPose, KnobRenderer, LedLook, OffscreenTargets, Readback,
    frame_params as scene_frame_params, hsv_to_rgb,
};

/// Colour target format; `Bgra8*` so readback bytes are display-ready for GPUI.
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Bgra8UnormSrgb;
/// Target frame interval (~60 Hz).
const FRAME_INTERVAL: Duration = Duration::from_millis(16);
/// While idle (nothing animating), block this long waiting for input instead of
/// busy-rendering, so a resting knob costs ~nothing.
const IDLE_POLL: Duration = Duration::from_millis(250);
/// Fallback viewport until the UI reports its real size.
const DEFAULT_SIZE: (u32, u32) = (480, 480);
/// Cap the render resolution so readback stays cheap regardless of window size.
const MAX_DIM: u32 = 768;

/// Background clear colour = the GUI panel colour (white), so no square shows
/// behind the composited knob. GPUI blends images with straight alpha, so a
/// transparent clear would leave dark MSAA fringes on the silhouette — matching
/// the surface colour avoids that entirely.
const PANEL_CLEAR: [f32; 4] = [1.0, 1.0, 1.0, 1.0];

/// A rendered frame handed to the UI: tight BGRA, `width * height * 4` bytes.
pub struct Frame {
    pub bgra: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// Input from the UI to the render thread. All deltas are in the UI's own units
/// (pixels / wheel ticks); the thread turns them into camera/knob motion.
pub enum RenderCmd {
    /// Viewport size changed (logical pixels × scale already applied by the UI).
    Resize { width: u32, height: u32 },
    /// Right-drag: orbit the camera by a pointer delta.
    Orbit { dx: f32, dy: f32 },
    /// Left-drag: spin the knob by a pointer delta over `dt` seconds (imparts
    /// momentum so it coasts on release).
    Spin { dx: f32, dt: f32 },
    /// Pointer pressed (`true`) or released (`false`) over the knob.
    Press(bool),
    /// Scroll wheel over the knob: flick the spin (the knob's turn gesture).
    Scroll(f32),
    /// Reflect the device's current LED mode in the glow.
    SetLed(LedMode),
    /// Whether the knob is on screen (device connected). While inactive the
    /// thread idles and renders nothing.
    SetActive(bool),
}

/// Start the render thread; return the UI's frame receiver and command sender.
pub fn spawn() -> (Receiver<Frame>, Sender<RenderCmd>) {
    // Bounded to one in-flight frame: if the UI foreground stalls, the render
    // thread drops frames instead of piling up ~1.25 MB buffers unbounded.
    let (frame_tx, frame_rx) = mpsc::sync_channel(1);
    let (cmd_tx, cmd_rx) = mpsc::channel();
    let _ = std::thread::Builder::new()
        .name("knob-render".into())
        .spawn(move || run(frame_tx, cmd_rx));
    (frame_rx, cmd_tx)
}

fn run(frame_tx: SyncSender<Frame>, cmd_rx: Receiver<RenderCmd>) {
    let Some((device, queue)) = headless_device() else {
        // No usable GPU: exit quietly. The UI keeps working without the 3D view.
        return;
    };

    let knob = KnobRenderer::new(&device, &queue, FORMAT);
    let mut state = KnobState::new(knob.center_z());

    let mut size = DEFAULT_SIZE;
    let mut targets = OffscreenTargets::new(&device, FORMAT, size);
    let mut readback = Readback::new(&device, &queue, size);

    let mut active = false;
    let mut last = Instant::now();
    loop {
        // When nothing is moving, block for the next command (cheap idle);
        // while animating, poll so frames keep flowing.
        let idle = !(active && state.is_animating());
        let first = if idle {
            match cmd_rx.recv_timeout(IDLE_POLL) {
                Ok(cmd) => Ok(cmd),
                Err(RecvTimeoutError::Timeout) => Err(TryRecvError::Empty),
                Err(RecvTimeoutError::Disconnected) => return, // UI gone
            }
        } else {
            cmd_rx.try_recv()
        };

        // Apply the first command (if any) plus everything else queued.
        let mut spin_input = false;
        let mut new_size = size;
        let mut changed = false;
        let mut cmd = first;
        loop {
            match cmd {
                Ok(RenderCmd::Resize { width, height }) => {
                    new_size = clamp_size(width, height);
                    changed = true;
                }
                Ok(RenderCmd::SetActive(on)) => {
                    active = on;
                    changed = true;
                }
                Ok(c) => {
                    state.apply(&c, &mut spin_input);
                    changed = true;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return, // UI gone
            }
            cmd = cmd_rx.try_recv();
        }

        if new_size != size {
            size = new_size;
            targets = OffscreenTargets::new(&device, FORMAT, size);
            readback = Readback::new(&device, &queue, size);
        }

        // After an idle wait, start the clock fresh so animation eases smoothly
        // rather than jumping by the whole idle gap.
        let now = Instant::now();
        let dt = if idle {
            0.0
        } else {
            (now - last).as_secs_f32().clamp(0.0, 0.1)
        };
        last = now;
        if active {
            state.tick(dt, spin_input);
        }

        // Render only when on screen and something actually changed or moves.
        if !active || !(changed || state.is_animating()) {
            continue;
        }

        let aspect = size.0 as f32 / size.1 as f32;
        knob.render(&targets, &state.frame_params(aspect));
        let bgra = readback.read(&targets);

        match frame_tx.try_send(Frame {
            bgra,
            width: size.0,
            height: size.1,
        }) {
            Ok(()) => {}
            // UI hasn't taken the previous frame yet; drop this one.
            Err(TrySendError::Full(_)) => {}
            Err(TrySendError::Disconnected(_)) => return, // UI gone
        }

        // Pace to ~60 Hz (readback already cost some of the budget).
        let elapsed = now.elapsed();
        if let Some(rem) = FRAME_INTERVAL.checked_sub(elapsed) {
            std::thread::sleep(rem);
        }
    }
}

/// Create a headless wgpu device/queue, or `None` if no adapter is available.
fn headless_device() -> Option<(wgpu::Device, wgpu::Queue)> {
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        force_fallback_adapter: false,
        compatible_surface: None,
    }))?;
    pollster::block_on(adapter.request_device(
        &wgpu::DeviceDescriptor {
            label: Some("knob-render"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::downlevel_defaults(),
            memory_hints: wgpu::MemoryHints::default(),
        },
        None,
    ))
    .ok()
}

fn clamp_size(w: u32, h: u32) -> (u32, u32) {
    let clamp = |v: u32| v.clamp(1, MAX_DIM);
    (clamp(w), clamp(h))
}

/// Camera + knob + LED animation state, advanced each tick. Ported from the
/// viewer's `app.rs` so the GUI and standalone viewer animate identically.
struct KnobState {
    led_mode: LedMode,

    // Camera (orbit).
    yaw: f32,
    pitch: f32,
    dist: f32,

    // Knob motion.
    spin_angle: f32,
    spin_vel: f32,
    press_amt: f32,
    press_target: f32,

    time: f32,
    center_z: f32,
}

impl KnobState {
    fn new(center_z: f32) -> Self {
        Self {
            led_mode: LedMode::Off,
            yaw: DEFAULT_VIEW.0,
            pitch: DEFAULT_VIEW.1,
            dist: DEFAULT_VIEW.2,
            spin_angle: 0.0,
            spin_vel: 0.0,
            press_amt: 0.0,
            press_target: 0.0,
            time: 0.0,
            center_z,
        }
    }

    fn apply(&mut self, cmd: &RenderCmd, spin_input: &mut bool) {
        match *cmd {
            RenderCmd::Orbit { dx, dy } => {
                self.yaw -= dx * 0.01;
                self.pitch = (self.pitch + dy * 0.01).clamp(-1.4, 1.4);
            }
            RenderCmd::Spin { dx, dt } => {
                let gain = 0.012;
                self.spin_angle += dx * gain;
                self.spin_vel = dx * gain / dt.max(1e-3);
                *spin_input = true;
            }
            RenderCmd::Press(down) => {
                self.press_target = if down { 1.0 } else { 0.0 };
            }
            RenderCmd::Scroll(amount) => {
                // A wheel notch flicks the knob round (its turn gesture).
                self.spin_vel += amount * 2.0;
            }
            RenderCmd::SetLed(mode) => self.led_mode = mode,
            // Handled by the run loop, not per-state.
            RenderCmd::Resize { .. } | RenderCmd::SetActive(_) => {}
        }
    }

    fn tick(&mut self, dt: f32, spin_input: bool) {
        // Spin momentum when not actively dragging.
        if !spin_input {
            self.spin_angle += self.spin_vel * dt;
            self.spin_vel *= 0.92f32.powf(dt * 60.0);
            if self.spin_vel.abs() < 1e-4 {
                self.spin_vel = 0.0;
            }
        }
        // Ease the press toward its target.
        let k = (dt * 14.0).min(1.0);
        self.press_amt += (self.press_target - self.press_amt) * k;
        self.time += dt;
    }

    /// Is anything still moving that requires re-rendering? Used to idle the
    /// render thread when the knob is at rest with a static/off LED.
    fn is_animating(&self) -> bool {
        self.spin_vel != 0.0
            || (self.press_amt - self.press_target).abs() > 1e-3
            || matches!(
                self.led_mode,
                LedMode::Animated1 | LedMode::Animated2 | LedMode::Animated3
            )
    }

    /// The resolved LED look for this frame, from the device's LED mode.
    fn led_look(&self) -> LedLook {
        use std::f32::consts::{PI, TAU};
        // Base hue offsets keep the three animated modes visually distinct.
        let animated = |offset: f32| hsv_to_rgb((self.time * 0.15 + offset).fract(), 0.85, 1.0);
        let (color, pulsing) = match self.led_mode {
            LedMode::Off => ([0.0, 0.0, 0.0], false),
            LedMode::StaticWhite => ([1.0, 1.0, 1.0], false),
            LedMode::StaticGreen => ([0.05, 0.8, 0.2], false),
            LedMode::Animated1 => (animated(0.0), true),
            LedMode::Animated2 => (animated(PI / 3.0), true),
            LedMode::Animated3 => (animated(2.0 * PI / 3.0), true),
        };
        // Intensity: pulse (animated modes) brightened a touch while pressed.
        let pulse = if pulsing {
            let p = 0.5 + 0.5 * (self.time * TAU * 0.8).sin();
            1.0 - 0.55 * (1.0 - p)
        } else {
            1.0
        };
        let press_boost = 1.0 + 0.5 * self.press_amt;
        LedLook {
            color,
            on: self.led_mode != LedMode::Off,
            intensity: pulse * press_boost,
        }
    }

    fn frame_params(&self, aspect: f32) -> FrameParams {
        let camera = Camera {
            yaw: self.yaw,
            pitch: self.pitch,
            dist: self.dist,
        };
        let pose = KnobPose {
            spin_angle: self.spin_angle,
            press_amt: self.press_amt,
        };
        scene_frame_params(
            camera,
            self.center_z,
            aspect,
            pose,
            self.led_look(),
            PANEL_CLEAR,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// End-to-end: spawn the thread, drive it, and confirm it streams non-blank
    /// frames of the requested size. Writes a PNG when `KNOB3D_DUMP` is set.
    #[test]
    fn streams_frames() {
        let (frames, cmds) = spawn();
        cmds.send(RenderCmd::SetActive(true)).unwrap();
        cmds.send(RenderCmd::Resize {
            width: 512,
            height: 512,
        })
        .unwrap();
        cmds.send(RenderCmd::SetLed(LedMode::StaticGreen)).unwrap();

        // Grab a frame at the requested size (skip any default-size frames in
        // flight before the resize took effect).
        let mut got = None;
        for _ in 0..240 {
            let f = frames
                .recv_timeout(Duration::from_secs(5))
                .expect("no frame produced");
            if (f.width, f.height) == (512, 512) {
                got = Some(f);
                break;
            }
        }
        let frame = got.expect("no 512x512 frame produced");
        assert_eq!(frame.bgra.len(), 512 * 512 * 4);

        // The knob (dark metal) + green seam must put non-background pixels on
        // the white panel-coloured clear.
        let clear = [255u8, 255, 255];
        let non_bg = frame
            .bgra
            .chunks_exact(4)
            .filter(|px| {
                (px[2] as i32 - clear[0] as i32).abs() > 24
                    || (px[1] as i32 - clear[1] as i32).abs() > 24
                    || (px[0] as i32 - clear[2] as i32).abs() > 24
            })
            .count();
        assert!(non_bg > 5000, "frame looks blank ({non_bg} non-bg px)");

        if let Ok(path) = std::env::var("KNOB3D_DUMP") {
            let mut rgba = frame.bgra.clone();
            for px in rgba.chunks_exact_mut(4) {
                px.swap(0, 2);
            }
            image::RgbaImage::from_raw(frame.width, frame.height, rgba)
                .unwrap()
                .save(&path)
                .unwrap();
            eprintln!("wrote {path}");
        }
    }
}
