//! Background HID worker.
//!
//! All blocking device I/O runs here on a dedicated `std::thread`, so the UI
//! thread never stalls on a HID call. The worker supervises the connection
//! (plug/unplug flips the UI live) and applies remap commands sent from the UI.
//! Two channels: `WorkerMsg` out to the UI, `UiCommand` in from the UI.

use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::time::Duration;

use anticater_core::{
    Action, Control, Device, Error, KnobInfo, LedMode, led, protocol::ACTIVE_PAGE,
};

/// Messages from the worker thread to the UI.
pub enum WorkerMsg {
    /// Device opened (or mappings changed); carries info, brightness, battery,
    /// mappings. `battery` is `Some` only on Bluetooth (§8); `None` over USB.
    Connected {
        info: KnobInfo,
        brightness: Option<u8>,
        battery: Option<u8>,
        mappings: Vec<(String, String)>,
    },
    /// No device present. `Some(reason)` when an open attempt failed.
    Disconnected(Option<String>),
    /// A transient status line (write result, etc.).
    Notice(String),
}

/// Commands from the UI to the worker.
pub enum UiCommand {
    /// Write `action` to `control` on the active page and persist it.
    SetAction { control: Control, action: Action },
    /// Set the LED effect mode (palette is the fixed default; inert on this unit).
    SetLed(LedMode),
}

/// The gestures this knob exposes, in display order: plain turn/press plus the
/// hold-and-turn pair (turning while pressed), which are distinct control slots.
pub const CONTROLS: [(&str, Control); 5] = [
    ("Turn left", Control::TURN_CCW),
    ("Press", Control::PRESS),
    ("Turn right", Control::TURN_CW),
    ("Hold + turn left", Control::HOLD_TURN_CCW),
    ("Hold + turn right", Control::HOLD_TURN_CW),
];

/// Main loop tick — short so remap commands feel responsive.
const LOOP_INTERVAL: Duration = Duration::from_millis(30);
/// Presence is re-checked every this many ticks (~800 ms).
const PRESENCE_TICKS: u32 = 26;
/// How long to wait between reconnect attempts while searching.
const RECONNECT_INTERVAL: Duration = Duration::from_millis(800);
/// Attempt Bluetooth only every Nth reconnect cycle. USB opens/fails in ~ms, but
/// a BLE scan blocks for `SCAN_SECS` and spins up a runtime; scanning every cycle
/// would balloon USB hot-plug latency, so keep USB snappy and probe BLE ~every 4s.
const BLE_EVERY: u32 = 5;

/// Open the knob, keeping USB responsive: try USB every cycle, but only fall back
/// to the (slow, blocking) Bluetooth scan on every `BLE_EVERY`th attempt.
fn open_knob(attempt: u32) -> anticater_core::Result<Device> {
    match Device::open() {
        Ok(dev) => Ok(dev),
        Err(Error::DeviceNotFound) if attempt % BLE_EVERY == 0 => Device::open_ble(),
        Err(e) => Err(e),
    }
}

/// Start the worker thread; return the UI's receiver and command sender.
pub fn spawn() -> (Receiver<WorkerMsg>, Sender<UiCommand>) {
    let (tx, rx) = mpsc::channel();
    let (cmd_tx, cmd_rx) = mpsc::channel();
    let _ = std::thread::Builder::new()
        .name("hid-worker".into())
        .spawn(move || run(tx, cmd_rx));
    (rx, cmd_tx)
}

fn run(tx: Sender<WorkerMsg>, cmd_rx: Receiver<UiCommand>) {
    // The last `Disconnected` payload sent, to dedupe repeats while searching.
    let mut last_offline: Option<Option<String>> = None;
    let mut attempt = 0u32;

    loop {
        let opened = open_knob(attempt);
        attempt = attempt.wrapping_add(1);
        match opened {
            Ok(device) => {
                // Reset so the next offline event (clean disconnect or a real
                // open error) after this connection is announced afresh.
                last_offline = None;

                let info = device.info().clone();
                let mut brightness = device.read_brightness().ok();
                let mut battery = device.battery();
                if send_snapshot(&tx, &info, brightness, battery, &device).is_err() {
                    return;
                }

                let mut ticks = 0u32;
                loop {
                    // Apply any pending remap commands.
                    let mut changed = false;
                    loop {
                        match cmd_rx.try_recv() {
                            Ok(UiCommand::SetAction { control, action }) => {
                                match device.set_action(control, ACTIVE_PAGE, &action) {
                                    Ok(()) => {
                                        let _ = tx.send(WorkerMsg::Notice(format!(
                                            "Set {} → {action}",
                                            control_label(control)
                                        )));
                                        changed = true;
                                    }
                                    Err(e) => {
                                        let _ = tx
                                            .send(WorkerMsg::Notice(format!("Write failed: {e}")));
                                    }
                                }
                            }
                            Ok(UiCommand::SetLed(mode)) => {
                                match device.set_led(mode, &led::DEFAULT_PALETTE) {
                                    Ok(()) => {
                                        let _ =
                                            tx.send(WorkerMsg::Notice(format!("LED: {mode:?}")));
                                    }
                                    Err(e) => {
                                        let _ = tx.send(WorkerMsg::Notice(format!(
                                            "LED write failed: {e}"
                                        )));
                                    }
                                }
                            }
                            Err(TryRecvError::Empty) => break,
                            Err(TryRecvError::Disconnected) => return, // UI gone
                        }
                    }
                    if changed && send_snapshot(&tx, &info, brightness, battery, &device).is_err() {
                        return;
                    }

                    ticks += 1;
                    if ticks >= PRESENCE_TICKS {
                        ticks = 0;
                        if !device.is_present() {
                            break;
                        }
                        // Refresh live readings (battery drains, brightness may
                        // change) and push a fresh snapshot only when they move.
                        let new_brightness = device.read_brightness().ok();
                        let new_battery = device.battery();
                        if (new_brightness, new_battery) != (brightness, battery) {
                            brightness = new_brightness;
                            battery = new_battery;
                            if send_snapshot(&tx, &info, brightness, battery, &device).is_err() {
                                return;
                            }
                        }
                    }
                    std::thread::sleep(LOOP_INTERVAL);
                }

                if announce_offline(&tx, &mut last_offline, None).is_err() {
                    return;
                }
            }
            Err(e) => {
                // Routine absence stays a generic "searching"; a real error
                // (permission, driver, BLE) surfaces its reason. Either way it's
                // only sent when the payload changes.
                let payload = match e {
                    Error::DeviceNotFound => None,
                    other => Some(other.to_string()),
                };
                if announce_offline(&tx, &mut last_offline, payload).is_err() {
                    return;
                }
                std::thread::sleep(RECONNECT_INTERVAL);
            }
        }
    }
}

/// Send a `Disconnected(payload)` only when it differs from the last one sent,
/// so a steady offline state isn't re-announced every reconnect tick while a
/// genuinely new reason (or a clean disconnect) always gets through. `last`
/// holds the last payload sent; reset it to `None` on connect so the next
/// offline event is announced afresh.
fn announce_offline(
    tx: &Sender<WorkerMsg>,
    last: &mut Option<Option<String>>,
    payload: Option<String>,
) -> Result<(), mpsc::SendError<WorkerMsg>> {
    if last.as_ref() != Some(&payload) {
        tx.send(WorkerMsg::Disconnected(payload.clone()))?;
        *last = Some(payload);
    }
    Ok(())
}

/// Re-read the mappings and send a fresh `Connected` snapshot.
fn send_snapshot(
    tx: &Sender<WorkerMsg>,
    info: &KnobInfo,
    brightness: Option<u8>,
    battery: Option<u8>,
    device: &Device,
) -> Result<(), mpsc::SendError<WorkerMsg>> {
    tx.send(WorkerMsg::Connected {
        info: info.clone(),
        brightness,
        battery,
        mappings: read_mappings(device),
    })
}

/// Read every control's mapping on the active page for display.
fn read_mappings(device: &Device) -> Vec<(String, String)> {
    CONTROLS
        .into_iter()
        .map(|(label, control)| {
            let value = match device.read_action(control, ACTIVE_PAGE) {
                Ok(action) => action.to_string(),
                Err(e) => format!("— ({e})"),
            };
            (label.to_string(), value)
        })
        .collect()
}

fn control_label(control: Control) -> &'static str {
    CONTROLS
        .iter()
        .find(|(_, c)| *c == control)
        .map(|(label, _)| *label)
        .unwrap_or("control")
}
