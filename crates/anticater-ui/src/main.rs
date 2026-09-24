//! Anticater control GUI — view the connected device and remap its controls.
//!
//! All HID I/O happens on a background thread (`worker`); this file is the GPUI
//! view. Select a control, then apply a preset, capture a key/chord, record a
//! macro, or set the LED mode. A foreground timer drains the worker channel and
//! re-renders, so plug/unplug and write results appear live.

// Keep the console attached in debug builds so logs stay visible; hide it in
// release so launching the app doesn't pop a terminal behind the window.
#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

mod knob3d;
mod worker;

use std::sync::Arc;
use std::sync::LazyLock;
use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::time::Duration;

use std::collections::HashSet;

use anticater_core::{
    Action, Control, Key, KeyStep, KnobInfo, LedMode, Modifier, MouseAction, Swipe,
    hut::{Consumer, KeyboardKeypad as Kbd},
};
use gpui::{
    App, AppContext, Application, Bounds, Context, FocusHandle, InteractiveElement, IntoElement,
    KeyDownEvent, Keystroke, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    ParentElement, Render, RenderImage, ScrollDelta, ScrollWheelEvent, SharedString,
    StatefulInteractiveElement, Styled, Window, WindowBounds, WindowOptions, div, img, px, rgb,
    size,
};
use image::{Frame as ImageFrame, RgbaImage};

use knob3d::RenderCmd;
use worker::{CONTROLS, UiCommand, WorkerMsg};

// Palette (light theme).
const BG: u32 = 0xf4f6f9;
const PANEL: u32 = 0xffffff;
const BORDER: u32 = 0xd6dae2;
const TEXT: u32 = 0x1b1f2a;
const MUTED: u32 = 0x6b7280;
const GREEN: u32 = 0x1f9d57;
const AMBER: u32 = 0xb5791f;
const ROW: u32 = 0xf0f2f6;
const ROW_HOVER: u32 = 0xe4e8f0;
const SELECTED: u32 = 0xd6e4ff;
const BTN: u32 = 0xe9ecf2;
const BTN_HOVER: u32 = 0xdbdfe8;
const ACCENT: u32 = 0x2563eb;
const CAPTURE_BG: u32 = 0xe7ddff;
const KEYBG: u32 = 0xeef0f4;
const KEY_HOVER: u32 = 0xe0e4ec;

/// How often the UI drains the worker channel.
const DRAIN_INTERVAL: Duration = Duration::from_millis(100);
/// How often the UI pulls the latest 3D frame (~60 Hz).
const FRAME_DRAIN: Duration = Duration::from_millis(16);
/// Render resolution requested of the render thread (device pixels). Rendered
/// larger than displayed so the down-scaled image stays crisp.
const KNOB_RENDER: (u32, u32) = (600, 520);
/// On-screen size of the 3D knob viewport (logical px), same aspect as above.
const KNOB_DISP: (f32, f32) = (260.0, 225.0);
/// Delay added per "+delay" click when building a macro.
const DELAY_STEP_MS: u16 = 100;

/// Connection state, mirrored from the worker.
enum Status {
    /// Not connected; actively retrying. `Some` carries the last open error.
    Searching(Option<String>),
    Connected {
        info: KnobInfo,
        brightness: Option<u8>,
        /// Battery charge, Bluetooth only (`None` over USB).
        battery: Option<u8>,
        mappings: Vec<(String, String)>,
    },
}

/// Keyboard-capture mode.
#[derive(PartialEq)]
enum Capture {
    Off,
    /// Bind the next key/chord to the selected control.
    SingleKey,
    /// Append each key/chord to a macro buffer.
    Macro,
}

/// Which category tab is showing.
#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Keyboard,
    Media,
    Mouse,
    Led,
}

struct AnticaterApp {
    status: Status,
    cmd_tx: Sender<UiCommand>,
    /// Which control the palette/capture will write to.
    selected: Option<Control>,
    /// A transient status line (write results, hints).
    notice: Option<String>,
    focus_handle: FocusHandle,
    capture: Capture,
    macro_steps: Vec<KeyStep>,
    /// Sticky modifiers folded into keyboard presets and captured keys.
    mods: ModState,
    /// Active category tab.
    tab: Tab,
    /// Last LED mode set from the UI. The device doesn't report its mode back,
    /// so we track it here to drive the knob's glow.
    led_mode: LedMode,

    /// Commands to the 3D render thread (camera/knob input, LED mode).
    render_tx: Sender<RenderCmd>,
    /// Latest 3D frame to display, as a GPUI image.
    knob_frame: Option<Arc<RenderImage>>,
    /// Superseded frames awaiting `window.drop_image` (see the bridge prototype).
    knob_retire: Vec<Arc<RenderImage>>,
    /// Left-drag (spin) in progress over the knob.
    knob_dragging: bool,
    /// Right-drag (orbit) in progress over the knob.
    knob_orbiting: bool,
    /// Last pointer position during a knob drag (stage-local px).
    knob_last: (f32, f32),
}

/// LED modes offered, with labels.
const LED_MODES: [(&str, LedMode); 6] = [
    ("Off", LedMode::Off),
    ("White", LedMode::StaticWhite),
    ("Green", LedMode::StaticGreen),
    ("Animated 1", LedMode::Animated1),
    ("Animated 2", LedMode::Animated2),
    ("Animated 3", LedMode::Animated3),
];

/// Sticky modifier toggles folded into keyboard actions.
#[derive(Default)]
struct ModState {
    ctrl: bool,
    shift: bool,
    alt: bool,
    win: bool,
}

impl ModState {
    fn list(&self) -> Vec<Modifier> {
        let mut v = Vec::new();
        if self.ctrl {
            v.push(Modifier::Ctrl);
        }
        if self.shift {
            v.push(Modifier::Shift);
        }
        if self.alt {
            v.push(Modifier::Alt);
        }
        if self.win {
            v.push(Modifier::Win);
        }
        v
    }
}

/// Prepend the active modifiers to a keyboard action (deduped). No-op for
/// non-keyboard actions and when no modifiers are active.
fn apply_mods(action: Action, mods: &[Modifier]) -> Action {
    if mods.is_empty() {
        return action;
    }
    match action {
        Action::Keyboard(steps) => Action::Keyboard(merge_steps(steps, mods)),
        other => other,
    }
}

/// Prepend `mods` as modifier steps ahead of `steps`, skipping duplicates.
fn merge_steps(steps: Vec<KeyStep>, mods: &[Modifier]) -> Vec<KeyStep> {
    let mut out = Vec::new();
    let mut seen: HashSet<u8> = HashSet::new();
    for m in mods {
        if seen.insert(*m as u8) {
            out.push(KeyStep::modifier(*m));
        }
    }
    for step in steps {
        match step.key {
            Key::Modifier(m) => {
                if seen.insert(m as u8) {
                    out.push(step);
                }
            }
            Key::Usage(_) => out.push(step),
        }
    }
    out
}

/// Resolve a known key string to its usage (panics only on a programmer typo
/// in the tables below).
fn u(key: &str) -> Kbd {
    base_usage(key).expect("known key usage")
}

/// A modifier + key chord (e.g. Ctrl+C).
fn chord(modifier: Modifier, usage: Kbd) -> Action {
    Action::Keyboard(vec![KeyStep::modifier(modifier), KeyStep::key(usage)])
}

/// A scroll with an optional held modifier.
fn scroll(up: bool, modifier: Option<Modifier>) -> Action {
    Action::Mouse(MouseAction::Scroll { up, modifier })
}

/// Common editing shortcuts (Ctrl chords) for the Keyboard tab. Built once — the
/// remap panel is on the (up to 60 Hz) render path while the knob animates.
static SHORTCUT_ACTIONS: LazyLock<Vec<(&'static str, Action)>> = LazyLock::new(|| {
    vec![
        ("Copy", chord(Modifier::Ctrl, u("c"))),
        ("Paste", chord(Modifier::Ctrl, u("v"))),
        ("Cut", chord(Modifier::Ctrl, u("x"))),
        ("Undo", chord(Modifier::Ctrl, u("z"))),
        ("Redo", chord(Modifier::Ctrl, u("y"))),
        ("Select all", chord(Modifier::Ctrl, u("a"))),
    ]
});

/// The full clickable keyboard layout: rows of `(label, key-string)`. A key
/// string of `mod:<name>` is a sticky modifier toggle rather than a bound key.
static KEYBOARD_ROWS: LazyLock<Vec<Vec<(&'static str, &'static str)>>> = LazyLock::new(|| {
    vec![
        vec![
            ("Esc", "escape"),
            ("F1", "f1"),
            ("F2", "f2"),
            ("F3", "f3"),
            ("F4", "f4"),
            ("F5", "f5"),
            ("F6", "f6"),
            ("F7", "f7"),
            ("F8", "f8"),
            ("F9", "f9"),
            ("F10", "f10"),
            ("F11", "f11"),
            ("F12", "f12"),
        ],
        vec![
            ("`", "`"),
            ("1", "1"),
            ("2", "2"),
            ("3", "3"),
            ("4", "4"),
            ("5", "5"),
            ("6", "6"),
            ("7", "7"),
            ("8", "8"),
            ("9", "9"),
            ("0", "0"),
            ("-", "-"),
            ("=", "="),
            ("Bksp", "backspace"),
        ],
        vec![
            ("Tab", "tab"),
            ("Q", "q"),
            ("W", "w"),
            ("E", "e"),
            ("R", "r"),
            ("T", "t"),
            ("Y", "y"),
            ("U", "u"),
            ("I", "i"),
            ("O", "o"),
            ("P", "p"),
            ("[", "["),
            ("]", "]"),
            ("\\", "\\"),
        ],
        vec![
            ("Caps", "capslock"),
            ("A", "a"),
            ("S", "s"),
            ("D", "d"),
            ("F", "f"),
            ("G", "g"),
            ("H", "h"),
            ("J", "j"),
            ("K", "k"),
            ("L", "l"),
            (";", ";"),
            ("'", "'"),
            ("Enter", "enter"),
        ],
        vec![
            ("Shift", "mod:shift"),
            ("Z", "z"),
            ("X", "x"),
            ("C", "c"),
            ("V", "v"),
            ("B", "b"),
            ("N", "n"),
            ("M", "m"),
            (",", ","),
            (".", "."),
            ("/", "/"),
            ("Shift", "mod:shift"),
        ],
        vec![
            ("Ctrl", "mod:ctrl"),
            ("Win", "mod:win"),
            ("Alt", "mod:alt"),
            ("Space", "space"),
            ("Alt", "mod:alt"),
            ("Ctrl", "mod:ctrl"),
        ],
        vec![
            ("Ins", "insert"),
            ("Home", "home"),
            ("PgUp", "pageup"),
            ("Del", "delete"),
            ("End", "end"),
            ("PgDn", "pagedown"),
            ("←", "left"),
            ("↑", "up"),
            ("↓", "down"),
            ("→", "right"),
        ],
    ]
});

/// Consumer/media presets grouped by category (Media tab).
static MEDIA_GROUPS: LazyLock<Vec<(&'static str, Vec<(&'static str, Action)>)>> =
    LazyLock::new(|| {
        use Consumer as C;
        vec![
        (
            "Media",
            vec![
                ("Vol +", Action::Consumer(C::VolumeIncrement)),
                ("Vol −", Action::Consumer(C::VolumeDecrement)),
                ("Mute", Action::Consumer(C::Mute)),
                ("Play/Pause", Action::Consumer(C::PlayPause)),
                ("Next", Action::Consumer(C::ScanNextTrack)),
                ("Prev", Action::Consumer(C::ScanPreviousTrack)),
                ("Stop", Action::Consumer(C::Stop)),
            ],
        ),
        (
            "Audio",
            vec![
                ("Bass +", Action::Consumer(C::BassIncrement)),
                ("Bass −", Action::Consumer(C::BassDecrement)),
                ("Treble +", Action::Consumer(C::TrebleIncrement)),
                ("Treble −", Action::Consumer(C::TrebleDecrement)),
            ],
        ),
        (
            "Display",
            vec![
                (
                    "Brightness +",
                    Action::Consumer(C::DisplayBrightnessIncrement),
                ),
                (
                    "Brightness −",
                    Action::Consumer(C::DisplayBrightnessDecrement),
                ),
            ],
        ),
        (
            "Apps",
            vec![
                ("Calculator", Action::Consumer(C::ALCalculator)),
                ("My Computer", Action::Consumer(C::ALLocalMachineBrowser)),
                (
                    "Media Player",
                    Action::Consumer(C::ALConsumerControlConfiguration),
                ),
                ("Email", Action::Consumer(C::ALEmailReader)),
            ],
        ),
        (
            "Web",
            vec![
                ("Home", Action::Consumer(C::ACHome)),
                ("Forward", Action::Consumer(C::ACForward)),
                ("Refresh", Action::Consumer(C::ACRefresh)),
            ],
        ),
    ]
    });

/// Mouse presets (Mouse tab).
static MOUSE_ACTIONS: LazyLock<Vec<(&'static str, Action)>> = LazyLock::new(|| {
    vec![
        ("Left click", Action::Mouse(MouseAction::Button(0x01))),
        ("Right click", Action::Mouse(MouseAction::Button(0x02))),
        ("Middle click", Action::Mouse(MouseAction::Button(0x04))),
        ("Scroll ↑", scroll(true, None)),
        ("Scroll ↓", scroll(false, None)),
        ("Zoom in (Ctrl+↑)", scroll(true, Some(Modifier::Ctrl))),
        ("Zoom out (Ctrl+↓)", scroll(false, Some(Modifier::Ctrl))),
        ("Swipe ←", Action::Mouse(MouseAction::Swipe(Swipe::Left))),
        ("Swipe →", Action::Mouse(MouseAction::Swipe(Swipe::Right))),
        ("Swipe ↑", Action::Mouse(MouseAction::Swipe(Swipe::Up))),
        ("Swipe ↓", Action::Mouse(MouseAction::Swipe(Swipe::Down))),
    ]
});

/// Pixel width for a keycap, so wide keys look right.
fn key_width(keystr: &str, label: &str) -> f32 {
    match keystr {
        "space" => 200.,
        "backspace" | "enter" => 66.,
        "capslock" | "mod:shift" => 62.,
        "tab" => 52.,
        "mod:ctrl" | "mod:alt" | "mod:win" => 46.,
        _ if label.chars().count() >= 3 => 40.,
        _ => 30.,
    }
}

/// Map a keystroke to its keyboard action steps (modifiers first, then key).
/// Returns `None` for keys we don't have a HID usage for.
fn keystroke_to_steps(ks: &Keystroke) -> Option<Vec<KeyStep>> {
    let base = base_usage(&ks.key)?;
    let mut steps = Vec::new();
    let m = &ks.modifiers;
    if m.control {
        steps.push(KeyStep::modifier(Modifier::Ctrl));
    }
    if m.shift {
        steps.push(KeyStep::modifier(Modifier::Shift));
    }
    if m.alt {
        steps.push(KeyStep::modifier(Modifier::Alt));
    }
    if m.platform {
        steps.push(KeyStep::modifier(Modifier::Win));
    }
    steps.push(KeyStep::key(base));
    Some(steps)
}

/// Map a GPUI key string to a HID keyboard usage.
fn base_usage(key: &str) -> Option<Kbd> {
    if key.len() == 1 {
        let c = key.chars().next().unwrap();
        if c.is_ascii_alphabetic() {
            return Kbd::try_from(0x04 + (c.to_ascii_lowercase() as u16 - b'a' as u16)).ok();
        }
        if c.is_ascii_digit() {
            let d = c as u16 - b'0' as u16;
            let byte = if d == 0 { 0x27 } else { 0x1E + (d - 1) };
            return Kbd::try_from(byte).ok();
        }
    }
    let byte: u16 = match key {
        "enter" | "return" => 0x28,
        "escape" | "esc" => 0x29,
        "backspace" => 0x2A,
        "tab" => 0x2B,
        "space" => 0x2C,
        "capslock" => 0x39,
        "-" | "minus" => 0x2D,
        "=" => 0x2E,
        "[" => 0x2F,
        "]" => 0x30,
        "\\" => 0x31,
        ";" => 0x33,
        "'" => 0x34,
        "`" => 0x35,
        "," => 0x36,
        "." => 0x37,
        "/" => 0x38,
        "right" => 0x4F,
        "left" => 0x50,
        "down" => 0x51,
        "up" => 0x52,
        "delete" => 0x4C,
        "home" => 0x4A,
        "end" => 0x4D,
        "pageup" => 0x4B,
        "pagedown" => 0x4E,
        "insert" => 0x49,
        f if f.starts_with('f') && f[1..].parse::<u8>().is_ok() => {
            let n: u8 = f[1..].parse().ok()?;
            if (1..=12).contains(&n) {
                0x3A + (n as u16 - 1)
            } else {
                return None;
            }
        }
        _ => return None,
    };
    Kbd::try_from(byte).ok()
}

impl AnticaterApp {
    fn new(cx: &mut Context<Self>) -> Self {
        let (rx, cmd_tx) = worker::spawn();
        Self::drain_loop(rx, cx);

        // 3D knob render thread: request our render resolution, then stream
        // frames into `knob_frame` via the frame loop.
        let (frame_rx, render_tx) = knob3d::spawn();
        let _ = render_tx.send(RenderCmd::Resize {
            width: KNOB_RENDER.0,
            height: KNOB_RENDER.1,
        });
        Self::frame_loop(frame_rx, cx);

        Self {
            status: Status::Searching(None),
            cmd_tx,
            selected: None,
            notice: None,
            focus_handle: cx.focus_handle(),
            capture: Capture::Off,
            macro_steps: Vec::new(),
            mods: ModState::default(),
            tab: Tab::Keyboard,
            led_mode: LedMode::Off,
            render_tx,
            knob_frame: None,
            knob_retire: Vec::new(),
            knob_dragging: false,
            knob_orbiting: false,
            knob_last: (0.0, 0.0),
        }
    }

    /// Foreground task that pulls the latest 3D frame and stores it as an image.
    fn frame_loop(rx: Receiver<knob3d::Frame>, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(FRAME_DRAIN).await;

                // Keep only the newest frame; drop any that piled up.
                let mut latest = None;
                let mut gone = false;
                loop {
                    match rx.try_recv() {
                        Ok(frame) => latest = Some(frame),
                        Err(TryRecvError::Empty) => break,
                        Err(TryRecvError::Disconnected) => {
                            gone = true;
                            break;
                        }
                    }
                }

                if let Some(frame) = latest {
                    let image = frame_to_image(frame);
                    let applied = this.update(cx, |app, cx| {
                        if let Some(old) = app.knob_frame.take() {
                            app.knob_retire.push(old);
                        }
                        app.knob_frame = Some(image);
                        cx.notify();
                    });
                    if applied.is_err() {
                        break;
                    }
                }
                if gone {
                    break;
                }
            }
        })
        .detach();
    }

    /// Foreground task that drains the worker channel and applies messages.
    fn drain_loop(rx: Receiver<WorkerMsg>, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(DRAIN_INTERVAL).await;

                let mut batch = Vec::new();
                let mut worker_gone = false;
                loop {
                    match rx.try_recv() {
                        Ok(msg) => batch.push(msg),
                        Err(TryRecvError::Empty) => break,
                        Err(TryRecvError::Disconnected) => {
                            worker_gone = true;
                            break;
                        }
                    }
                }

                if !batch.is_empty() {
                    let applied = this.update(cx, |app, cx| {
                        for msg in batch {
                            app.apply(msg);
                        }
                        cx.notify();
                    });
                    if applied.is_err() {
                        break;
                    }
                }
                if worker_gone {
                    break;
                }
            }
        })
        .detach();
    }

    fn apply(&mut self, msg: WorkerMsg) {
        match msg {
            WorkerMsg::Connected {
                info,
                brightness,
                battery,
                mappings,
            } => {
                self.status = Status::Connected {
                    info,
                    brightness,
                    battery,
                    mappings,
                };
                // The knob is on screen now — wake the render thread and show
                // the current LED mode.
                let _ = self.render_tx.send(RenderCmd::SetActive(true));
                let _ = self.render_tx.send(RenderCmd::SetLed(self.led_mode));
            }
            WorkerMsg::Disconnected(reason) => {
                self.status = Status::Searching(reason);
                self.selected = None;
                self.capture = Capture::Off;
                self.macro_steps.clear();
                self.mods = ModState::default();
                self.led_mode = LedMode::Off;
                let _ = self.render_tx.send(RenderCmd::SetLed(LedMode::Off));
                // Knob no longer shown — idle the render thread.
                let _ = self.render_tx.send(RenderCmd::SetActive(false));
            }
            WorkerMsg::Notice(text) => self.notice = Some(text),
        }
    }

    fn set_action(&mut self, action: Action) {
        match self.selected {
            Some(control) => {
                let _ = self.cmd_tx.send(UiCommand::SetAction { control, action });
            }
            None => self.notice = Some("Select a control first".into()),
        }
    }

    /// Handle a captured key during a capture/record session.
    fn handle_key(&mut self, ev: &KeyDownEvent, cx: &mut Context<Self>) {
        if self.capture == Capture::Off || ev.is_held {
            return;
        }
        let Some(steps) = keystroke_to_steps(&ev.keystroke) else {
            self.notice = Some(format!("Unmapped key: {}", ev.keystroke.key));
            cx.notify();
            return;
        };
        // Fold in the sticky modifier toggles.
        let steps = merge_steps(steps, &self.mods.list());
        match self.capture {
            Capture::SingleKey => {
                self.capture = Capture::Off;
                self.set_action(Action::Keyboard(steps));
            }
            Capture::Macro => {
                self.macro_steps.extend(steps);
                self.notice = Some(format!("Macro: {}", macro_label(&self.macro_steps)));
            }
            Capture::Off => {}
        }
        cx.notify();
    }
}

fn macro_label(steps: &[KeyStep]) -> String {
    if steps.is_empty() {
        "(empty)".to_string()
    } else {
        Action::Keyboard(steps.to_vec()).to_string()
    }
}

/// Wrap a render-thread frame (tight BGRA) as a GPUI image. GPUI's atlas keys on
/// the fresh id `RenderImage::new` assigns, so each frame is a new image; the
/// superseded one is dropped in `render` (see `knob_retire`).
fn frame_to_image(frame: knob3d::Frame) -> Arc<RenderImage> {
    let buf = RgbaImage::from_raw(frame.width, frame.height, frame.bgra)
        .expect("frame byte length matches width*height*4");
    Arc::new(RenderImage::new(vec![ImageFrame::new(buf)]))
}

// ---- small element helpers ------------------------------------------------

fn panel() -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .p_4()
        .bg(rgb(PANEL))
        .border_1()
        .border_color(rgb(BORDER))
        .rounded_lg()
}

fn section_title(text: impl Into<SharedString>) -> gpui::Div {
    div().text_sm().text_color(rgb(MUTED)).child(text.into())
}

impl AnticaterApp {
    fn header(&self) -> impl IntoElement {
        let (label, color) = match &self.status {
            Status::Searching(_) => ("Searching…", AMBER),
            Status::Connected { .. } => ("Connected", GREEN),
        };
        div()
            .flex()
            .flex_row()
            .justify_between()
            .items_center()
            .child(div().text_xl().child("Open Anticater"))
            .child(
                div()
                    .px_3()
                    .py_1()
                    .rounded_md()
                    .bg(rgb(BORDER))
                    .text_sm()
                    .text_color(rgb(color))
                    .child(SharedString::from(label.to_string())),
            )
    }

    fn info_line(&self, label: &str, value: impl Into<SharedString>) -> impl IntoElement {
        div()
            .flex()
            .flex_row()
            .gap_2()
            .child(
                div()
                    .w(px(84.))
                    .flex_shrink()
                    .text_color(rgb(MUTED))
                    .child(SharedString::from(label.to_string())),
            )
            .child(div().child(value.into()))
    }

    /// A generic pill button that runs `on_click` when pressed.
    fn button(
        &self,
        id: usize,
        label: impl Into<SharedString>,
        cx: &mut Context<Self>,
        on_click: impl Fn(&mut Self, &mut Context<Self>) + 'static,
    ) -> impl IntoElement {
        div()
            .px_3()
            .py_1()
            .rounded_md()
            .bg(rgb(BTN))
            .text_sm()
            .cursor_pointer()
            .hover(|s| s.bg(rgb(BTN_HOVER)))
            .child(label.into())
            .id(("btn", id))
            .on_click(cx.listener(move |this, _ev, _window, cx| {
                on_click(this, cx);
                cx.notify();
            }))
    }

    /// The visual knob: the live 3D render (streamed from the render thread),
    /// flanked by the five clickable gesture callouts (turn left/right, press, and
    /// the hold-and-turn pair). `mappings` is the per-control action list from the
    /// connected snapshot, indexed in `CONTROLS` order (0 = turn left, 1 = press,
    /// 2 = turn right, 3 = hold + turn left, 4 = hold + turn right). Left-drag spins
    /// the knob, holding presses it, right-drag orbits the camera, and the wheel
    /// flicks it round — reflecting the live LED glow.
    fn knob(&self, mappings: &[(String, String)], cx: &mut Context<Self>) -> impl IntoElement {
        let map = |idx: usize| -> String {
            mappings
                .get(idx)
                .map(|(_, v)| v.clone())
                .unwrap_or_else(|| "—".into())
        };

        // The 3D viewport: the latest streamed frame, or a placeholder until the
        // first one arrives.
        let viewport = match self.knob_frame.clone() {
            Some(image) => img(image)
                .w(px(KNOB_DISP.0))
                .h(px(KNOB_DISP.1))
                .into_any_element(),
            None => div()
                .w(px(KNOB_DISP.0))
                .h(px(KNOB_DISP.1))
                .flex()
                .items_center()
                .justify_center()
                .text_color(rgb(MUTED))
                .child("Starting 3D view…")
                .into_any_element(),
        };

        // Centre the viewport in the stage, between the flanking callout cards,
        // and route pointer input to the render thread.
        let viewport = div()
            .absolute()
            .left(px(170.))
            .top(px(8.))
            .w(px(KNOB_DISP.0))
            .h(px(KNOB_DISP.1))
            .cursor_pointer()
            .child(viewport)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, ev: &MouseDownEvent, _w, _cx| {
                    this.knob_dragging = true;
                    this.knob_last = (f32::from(ev.position.x), f32::from(ev.position.y));
                    let _ = this.render_tx.send(RenderCmd::Press(true));
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, ev: &MouseDownEvent, _w, _cx| {
                    this.knob_orbiting = true;
                    this.knob_last = (f32::from(ev.position.x), f32::from(ev.position.y));
                }),
            )
            .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _w, _cx| {
                let (x, y) = (f32::from(ev.position.x), f32::from(ev.position.y));
                let (dx, dy) = (x - this.knob_last.0, y - this.knob_last.1);
                this.knob_last = (x, y);
                if this.knob_dragging {
                    // dt matches the render tick, so the flick velocity feels right.
                    let _ = this.render_tx.send(RenderCmd::Spin { dx, dt: 0.016 });
                } else if this.knob_orbiting {
                    let _ = this.render_tx.send(RenderCmd::Orbit { dx, dy });
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _ev: &MouseUpEvent, _w, _cx| {
                    this.knob_dragging = false;
                    let _ = this.render_tx.send(RenderCmd::Press(false));
                }),
            )
            .on_mouse_up(
                MouseButton::Right,
                cx.listener(|this, _ev: &MouseUpEvent, _w, _cx| {
                    this.knob_orbiting = false;
                }),
            )
            // Releases outside the viewport still end the gesture, so the knob
            // never latches pressed/dragging when the pointer leaves first.
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _ev: &MouseUpEvent, _w, _cx| {
                    if this.knob_dragging {
                        this.knob_dragging = false;
                        let _ = this.render_tx.send(RenderCmd::Press(false));
                    }
                }),
            )
            .on_mouse_up_out(
                MouseButton::Right,
                cx.listener(|this, _ev: &MouseUpEvent, _w, _cx| {
                    this.knob_orbiting = false;
                }),
            )
            .on_scroll_wheel(cx.listener(|this, ev: &ScrollWheelEvent, _w, _cx| {
                let dy = match ev.delta {
                    ScrollDelta::Pixels(p) => f32::from(p.y) / 40.0,
                    ScrollDelta::Lines(p) => p.y,
                };
                let _ = this.render_tx.send(RenderCmd::Scroll(dy));
            }));

        // Fixed-size stage; callout cards (160×64) flank the viewport and drive
        // the Remap panel's selection, as the old drawn control rows did. Left
        // column = CCW gestures, right column = CW, with the hold-and-turn pair
        // (turn while pressed, §6) stacked below their plain-turn counterparts.
        let stage = div()
            .relative()
            .w(px(600.))
            .h(px(360.))
            .child(viewport)
            .child(self.knob_callout(0, "↺", "Turn left", Control::TURN_CCW, map(0), 0., 78., cx))
            .child(self.knob_callout(
                3,
                "●↺",
                "Hold + turn left",
                Control::HOLD_TURN_CCW,
                map(3),
                0.,
                154.,
                cx,
            ))
            .child(self.knob_callout(
                2,
                "↻",
                "Turn right",
                Control::TURN_CW,
                map(2),
                440.,
                78.,
                cx,
            ))
            .child(self.knob_callout(
                4,
                "●↻",
                "Hold + turn right",
                Control::HOLD_TURN_CW,
                map(4),
                440.,
                154.,
                cx,
            ))
            .child(self.knob_callout(1, "●", "Press", Control::PRESS, map(1), 220., 280., cx));

        div().flex().justify_center().child(stage)
    }

    /// One clickable gesture callout, absolutely positioned at (`x`,`y`) in the
    /// knob stage. Shows a glyph, the gesture label, and its current mapping;
    /// selecting it drives the Remap panel, like the old control rows did.
    #[allow(clippy::too_many_arguments)]
    fn knob_callout(
        &self,
        idx: usize,
        glyph: &'static str,
        label: &'static str,
        control: Control,
        mapping: String,
        x: f32,
        y: f32,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let selected = self.selected == Some(control);
        div()
            .absolute()
            .left(px(x))
            .top(px(y))
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_1()
            .w(px(160.))
            .h(px(64.))
            .px_3()
            .rounded_lg()
            .border_1()
            .border_color(rgb(if selected { ACCENT } else { BORDER }))
            .bg(rgb(if selected { SELECTED } else { ROW }))
            .cursor_pointer()
            .hover(|s| s.bg(rgb(ROW_HOVER)))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .text_color(rgb(if selected { ACCENT } else { TEXT }))
                            .child(SharedString::from(glyph)),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(if selected { ACCENT } else { MUTED }))
                            .child(SharedString::from(label)),
                    ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(TEXT))
                    .child(SharedString::from(mapping)),
            )
            .id(("knob-callout", idx))
            .on_click(cx.listener(move |this, _ev, _window, cx| {
                this.selected = Some(control);
                this.notice = None;
                cx.notify();
            }))
    }

    fn device_panel(&self, cx: &mut Context<Self>) -> impl IntoElement {
        match &self.status {
            Status::Searching(reason) => {
                let message = match reason {
                    Some(e) => format!("Not connected — {e}"),
                    None => "Looking for an Anticater knob on the USB bus…".to_string(),
                };
                panel().child(section_title("Device")).child(
                    div()
                        .text_color(rgb(MUTED))
                        .child(SharedString::from(message)),
                )
            }
            Status::Connected {
                info,
                brightness,
                battery,
                mappings,
            } => {
                let ids = format!("{:04X}:{:04X}", info.vendor_id, info.product_id);
                let product = info.product.clone().unwrap_or_else(|| "—".into());
                let brightness = brightness
                    .map(|b| b.to_string())
                    .unwrap_or_else(|| "—".into());

                panel()
                    .child(section_title("Device"))
                    .child(self.info_line("Product", product))
                    .child(self.info_line("VID:PID", ids))
                    // Battery is Bluetooth-only; show it only when the device reports one.
                    .children(battery.map(|b| self.info_line("Battery", format!("{b}%"))))
                    .child(self.info_line("Brightness", brightness))
                    .child(div().h(px(4.)))
                    .child(section_title("Knob — click a gesture to select"))
                    .child(self.knob(mappings, cx))
            }
        }
    }

    fn remap_panel(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let target = match self.selected {
            Some(control) => CONTROLS
                .iter()
                .find(|(_, c)| *c == control)
                .map(|(l, _)| *l)
                .unwrap_or("—")
                .to_string(),
            None => "select a control above".to_string(),
        };

        let p = panel().child(
            div()
                .flex()
                .flex_row()
                .gap_2()
                .items_center()
                .child(section_title("Remap"))
                .child(
                    div()
                        .text_color(rgb(ACCENT))
                        .child(SharedString::from(target)),
                ),
        );

        if !matches!(self.status, Status::Connected { .. }) {
            return p.child(
                div()
                    .text_color(rgb(MUTED))
                    .child("Connect a knob to remap its controls."),
            );
        }

        let mut id = 0usize;
        let sidebar = self.tab_sidebar(&mut id, cx);
        let content = div().flex().flex_col().gap_2().flex_1().min_w_0();
        let content = match self.tab {
            Tab::Keyboard => self.keyboard_tab(content, &mut id, cx),
            Tab::Media => self.media_tab(content, &mut id, cx),
            Tab::Mouse => self.mouse_tab(content, &mut id, cx),
            Tab::Led => self.led_tab(content, &mut id, cx),
        };
        p.child(
            div()
                .flex()
                .flex_row()
                .gap_3()
                .items_start()
                .child(sidebar)
                .child(content),
        )
    }

    /// Vertical tab list down the left side.
    fn tab_sidebar(&self, id: &mut usize, cx: &mut Context<Self>) -> gpui::Div {
        let tabs = [
            ("Keyboard", Tab::Keyboard),
            ("Media", Tab::Media),
            ("Mouse", Tab::Mouse),
            ("LED", Tab::Led),
        ];
        let mut col = div().flex().flex_col().gap_1().w(px(104.)).flex_shrink();
        for (label, tab) in tabs {
            let active = self.tab == tab;
            col = col.child(
                div()
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .border_l_2()
                    .border_color(rgb(if active { ACCENT } else { BORDER }))
                    .bg(rgb(if active { SELECTED } else { ROW }))
                    .text_sm()
                    .text_color(rgb(if active { ACCENT } else { MUTED }))
                    .cursor_pointer()
                    .hover(|s| s.bg(rgb(ROW_HOVER)))
                    .child(SharedString::from(label))
                    .id(("tab", *id))
                    .on_click(cx.listener(move |this, _ev, _window, cx| {
                        this.tab = tab;
                        cx.notify();
                    })),
            );
            *id += 1;
        }
        col
    }

    fn keyboard_tab(&self, mut p: gpui::Div, id: &mut usize, cx: &mut Context<Self>) -> gpui::Div {
        // Common editing shortcuts.
        let mut sc = div().flex().flex_row().flex_wrap().gap_2();
        for (label, action) in SHORTCUT_ACTIONS.iter() {
            let act = action.clone();
            sc = sc.child(self.button(*id, *label, cx, move |this, _cx| {
                let mods = this.mods.list();
                this.set_action(apply_mods(act.clone(), &mods));
            }));
            *id += 1;
        }
        p = p.child(section_title("Shortcuts")).child(sc);

        // Full clickable keyboard. Modifier keys are sticky toggles.
        p = p.child(section_title(
            "Keyboard — modifiers are sticky; toggle then click a key",
        ));
        let mut grid = div()
            .id("kbd")
            .overflow_x_scroll()
            .flex()
            .flex_col()
            .gap_1();
        for row_def in KEYBOARD_ROWS.iter() {
            let mut krow = div().flex().flex_row().gap_1();
            // label/keystr are &&'static str here (match ergonomics over &Vec).
            for (label, keystr) in row_def {
                krow = krow.child(self.keycap(*id, *label, *keystr, cx));
                *id += 1;
            }
            grid = grid.child(krow);
        }
        p = p.child(grid);

        // Capture / macro builder.
        p.child(section_title("Capture / macro"))
            .child(self.capture_row(id, cx))
    }

    fn media_tab(&self, mut p: gpui::Div, id: &mut usize, cx: &mut Context<Self>) -> gpui::Div {
        for (group, items) in MEDIA_GROUPS.iter() {
            let mut row = div().flex().flex_row().flex_wrap().gap_2();
            for (label, action) in items {
                let act = action.clone();
                row = row.child(self.button(*id, *label, cx, move |this, _cx| {
                    this.set_action(act.clone());
                }));
                *id += 1;
            }
            p = p.child(section_title(*group)).child(row);
        }
        p
    }

    fn mouse_tab(&self, p: gpui::Div, id: &mut usize, cx: &mut Context<Self>) -> gpui::Div {
        let mut row = div().flex().flex_row().flex_wrap().gap_2();
        for (label, action) in MOUSE_ACTIONS.iter() {
            let act = action.clone();
            row = row.child(self.button(*id, *label, cx, move |this, _cx| {
                this.set_action(act.clone());
            }));
            *id += 1;
        }
        p.child(section_title("Mouse")).child(row)
    }

    fn led_tab(&self, p: gpui::Div, id: &mut usize, cx: &mut Context<Self>) -> gpui::Div {
        let mut row = div().flex().flex_row().flex_wrap().gap_2();
        for (label, mode) in LED_MODES {
            row = row.child(self.button(*id, label, cx, move |this, _cx| {
                this.led_mode = mode;
                let _ = this.cmd_tx.send(UiCommand::SetLed(mode));
                let _ = this.render_tx.send(RenderCmd::SetLed(mode));
            }));
            *id += 1;
        }
        p.child(section_title("LED mode")).child(row)
    }

    /// A single keycap: `mod:*` entries toggle a sticky modifier; every other
    /// key binds its HID usage (with the active modifiers) on click.
    fn keycap(
        &self,
        id: usize,
        label: &'static str,
        keystr: &'static str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let base = div()
            .h(px(30.))
            .w(px(key_width(keystr, label)))
            .flex()
            .items_center()
            .justify_center()
            .rounded_md()
            .text_xs()
            .cursor_pointer()
            .hover(|s| s.bg(rgb(KEY_HOVER)))
            .child(SharedString::from(label));

        if let Some(name) = keystr.strip_prefix("mod:") {
            let active = match name {
                "ctrl" => self.mods.ctrl,
                "shift" => self.mods.shift,
                "alt" => self.mods.alt,
                "win" => self.mods.win,
                _ => false,
            };
            base.bg(rgb(if active { SELECTED } else { KEYBG }))
                .text_color(rgb(if active { ACCENT } else { TEXT }))
                .id(("key", id))
                .on_click(cx.listener(move |this, _ev, _window, cx| {
                    match name {
                        "ctrl" => this.mods.ctrl = !this.mods.ctrl,
                        "shift" => this.mods.shift = !this.mods.shift,
                        "alt" => this.mods.alt = !this.mods.alt,
                        "win" => this.mods.win = !this.mods.win,
                        _ => {}
                    }
                    cx.notify();
                }))
        } else {
            base.bg(rgb(KEYBG)).id(("key", id)).on_click(cx.listener(
                move |this, _ev, _window, cx| {
                    if let Some(usage) = base_usage(keystr) {
                        let mods = this.mods.list();
                        this.set_action(apply_mods(
                            Action::Keyboard(vec![KeyStep::key(usage)]),
                            &mods,
                        ));
                        cx.notify();
                    }
                },
            ))
        }
    }

    /// The capture/record controls, which change shape with `self.capture`.
    fn capture_row(&self, id: &mut usize, cx: &mut Context<Self>) -> impl IntoElement {
        let mut row = div().flex().flex_col().gap_2();

        match self.capture {
            Capture::Off => {
                let buttons = div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .gap_2()
                    .child(self.button(*id, "Capture key…", cx, |this, _cx| {
                        this.capture = Capture::SingleKey;
                        this.notice = Some("Press any key or chord…".into());
                    }));
                *id += 1;
                let buttons =
                    buttons.child(self.button(*id, "Record macro…", cx, |this, _cx| {
                        this.capture = Capture::Macro;
                        this.macro_steps.clear();
                        this.notice = Some("Recording — press keys, then Save.".into());
                    }));
                *id += 1;
                row = row.child(buttons);
            }
            Capture::SingleKey => {
                *id += 1;
                row = row
                    .child(
                        div()
                            .px_3()
                            .py_2()
                            .rounded_md()
                            .bg(rgb(CAPTURE_BG))
                            .child("Press any key or chord to bind…"),
                    )
                    .child(self.button(*id, "Cancel", cx, |this, _cx| {
                        this.capture = Capture::Off;
                    }));
                *id += 1;
            }
            Capture::Macro => {
                row = row.child(div().px_3().py_2().rounded_md().bg(rgb(CAPTURE_BG)).child(
                    SharedString::from(format!("Recording: {}", macro_label(&self.macro_steps))),
                ));
                let mut actions = div().flex().flex_row().flex_wrap().gap_2();
                actions = actions.child(self.button(*id, "Save", cx, |this, _cx| {
                    if this.macro_steps.is_empty() {
                        this.notice = Some("Nothing recorded yet.".into());
                    } else {
                        let steps = std::mem::take(&mut this.macro_steps);
                        this.capture = Capture::Off;
                        this.set_action(Action::Keyboard(steps));
                    }
                }));
                *id += 1;
                actions = actions.child(self.button(
                    *id,
                    format!("+{DELAY_STEP_MS}ms"),
                    cx,
                    |this, _cx| {
                        if let Some(last) = this.macro_steps.last_mut() {
                            last.delay_ms = last.delay_ms.saturating_add(DELAY_STEP_MS);
                            this.notice =
                                Some(format!("Macro: {}", macro_label(&this.macro_steps)));
                        }
                    },
                ));
                *id += 1;
                actions = actions.child(self.button(*id, "Clear", cx, |this, _cx| {
                    this.macro_steps.clear();
                }));
                *id += 1;
                actions = actions.child(self.button(*id, "Cancel", cx, |this, _cx| {
                    this.capture = Capture::Off;
                    this.macro_steps.clear();
                }));
                *id += 1;
                row = row.child(actions);
            }
        }
        row
    }
}

impl Render for AnticaterApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        // Retire superseded 3D frames' atlas tiles now a newer one is staged, so
        // the sprite atlas doesn't accumulate a tile per frame.
        for old in self.knob_retire.drain(..) {
            let _ = window.drop_image(old);
        }

        let mut root = div()
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, _window, cx| {
                this.handle_key(ev, cx);
            }))
            .flex()
            .flex_col()
            .size_full()
            .gap_3()
            .p_4()
            .bg(rgb(BG))
            .text_color(rgb(TEXT))
            .child(self.header());

        if let Some(note) = &self.notice {
            root = root.child(
                div()
                    .text_sm()
                    .text_color(rgb(AMBER))
                    .child(SharedString::from(note.clone())),
            );
        }

        // Everything below the header scrolls, so the window never overflows.
        let content = div()
            .id("content")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_3()
            .child(self.device_panel(cx))
            .child(self.remap_panel(cx));

        root.child(content)
    }
}

fn main() {
    Application::new().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(760.), px(720.)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(px(420.), px(360.))),
            ..Default::default()
        };
        cx.open_window(options, |window, cx| {
            let view = cx.new(AnticaterApp::new);
            window.focus(&view.read(cx).focus_handle);
            view
        })
        .expect("failed to open window");
        cx.activate(true);
    });
}
