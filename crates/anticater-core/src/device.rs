//! The [`Device`]: the knob's config API over any [`Transport`].
//!
//! Read/write/commit and LED control are transport-agnostic — they build the
//! frames of `PROTOCOL.md` §3–§5 and hand them to a [`Transport`] (USB HID §1–§2, or Bluetooth LE §8).
//! The "drain until the reply matches" rule (§2) lives here and works identically on both pipes.

use crate::action::Action;
use crate::error::{Error, Result};
use crate::hid::HidTransport;
use crate::led::{self, LedMode, Palette};
use crate::protocol::{self, Command, Control, offset};
use crate::transport::Transport;

const READ_TIMEOUT_MS: i32 = 200;
/// Frames to Drain whilst looking for Response (§2).
const DRAIN_READS: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnobInfo {
    pub vendor_id: u16,
    pub product_id: u16,
    pub manufacturer: Option<String>,
    pub product: Option<String>,
    pub serial: Option<String>,
}

pub struct Device {
    transport: Box<dyn Transport>,
    info: KnobInfo,
}

impl Device {
    /// Open the first connected Knob over **USB** (HID config interface).
    pub fn open() -> Result<Self> {
        let (transport, info) = HidTransport::open()?;
        Ok(Self {
            transport: Box::new(transport),
            info,
        })
    }

    /// Open the first connected Knob over **Bluetooth LE** (§8).
    pub fn open_ble() -> Result<Self> {
        let (transport, info) = crate::ble::BleTransport::open()?;
        Ok(Self {
            transport: Box::new(transport),
            info,
        })
    }

    /// Open the Knob on whatever Transport is available: USB first, then Bluetooth LE.
    pub fn open_any() -> Result<Self> {
        match Self::open() {
            Ok(dev) => Ok(dev),
            Err(Error::DeviceNotFound) => Self::open_ble(),
            Err(e) => Err(e),
        }
    }

    /// Build a device over a caller-supplied Transport (e.g. a mock, or a
    /// pre-connected peripheral). `info` describes the underlying device.
    pub fn with_transport(transport: Box<dyn Transport>, info: KnobInfo) -> Self {
        Self { transport, info }
    }

    pub fn info(&self) -> &KnobInfo {
        &self.info
    }

    pub fn transport_kind(&self) -> &'static str {
        self.transport.kind()
    }

    pub fn is_present(&self) -> bool {
        self.transport.is_alive()
    }

    /// Battery charge `0..=100` - BLE Transport Only
    pub fn battery(&self) -> Option<u8> {
        self.transport.battery()
    }

    /// Polled for Connect/Disconnect Events over **USB**.
    ///
    /// Transport-agnostic presence lives on [`Device::is_present`].
    pub fn is_connected() -> bool {
        crate::hid::hid_present()
    }

    // ----- Read API ---------------------------------------------------------

    /// Read the action currently mapped to `control` on `page`
    ///
    /// The read request has its own small layout — `FA <type> 00 <page>` (§3),
    /// with a `0x00` where an entry would carry the page.
    pub fn read_action(&self, control: Control, page: u8) -> Result<Action> {
        let mut req = [0u8; protocol::OUTPUT_REPORT_LEN];
        req[0] = protocol::REPORT_ID;
        req[offset::COMMAND] = Command::ReadEntry as u8;
        req[offset::CONTROL] = control as u8;
        req[3] = 0x00;
        req[4] = page;
        self.transport.write(&req)?;

        // Drain until the reply's (command, control, page) matches the request
        let mut buf = [0u8; protocol::OUTPUT_REPORT_LEN];
        for _ in 0..DRAIN_READS {
            let n = self.transport.read(&mut buf, READ_TIMEOUT_MS)?;
            if n == 0 {
                continue; // timed out; try again within the drain budget
            }
            let frame = &buf[..n];
            if frame.get(offset::COMMAND) == Some(&(Command::ReadEntry as u8))
                && frame.get(offset::CONTROL) == Some(&(control as u8))
                && frame.get(offset::PAGE) == Some(&page)
            {
                return Action::decode(frame);
            }
        }
        Err(Error::NoResponse)
    }

    /// Read the Counter-Clockwise Knob turn on the active page.
    pub fn read_turn_ccw(&self) -> Result<Action> {
        self.read_action(Control::TURN_CCW, protocol::ACTIVE_PAGE)
    }

    /// Read the Clockwise Knob turn on the active page.
    pub fn read_turn_cw(&self) -> Result<Action> {
        self.read_action(Control::TURN_CW, protocol::ACTIVE_PAGE)
    }

    /// Read the Knob Press on the active page.
    pub fn read_press(&self) -> Result<Action> {
        self.read_action(Control::PRESS, protocol::ACTIVE_PAGE)
    }

    // ----- Write API --------------------------------------------------------

    /// Stage a mapping for `control` on `page`.
    /// Not persisted until [`commit`](Device::commit).
    pub fn write_action(&self, control: Control, page: u8, action: &Action) -> Result<()> {
        self.transport.write(&action.encode(control, page))?;
        Ok(())
    }

    /// Persist all staged writes (`03 FD FE FF`, §3).
    pub fn commit(&self) -> Result<()> {
        let mut frame = [0u8; protocol::OUTPUT_REPORT_LEN];
        frame[0] = protocol::REPORT_ID;
        frame[1..1 + protocol::COMMIT_PAYLOAD.len()].copy_from_slice(&protocol::COMMIT_PAYLOAD);
        self.transport.write(&frame)?;
        Ok(())
    }

    /// Set a mapping and persist it immediately (write + commit).
    pub fn set_action(&self, control: Control, page: u8, action: &Action) -> Result<()> {
        self.write_action(control, page, action)?;
        self.commit()
    }

    // ----- LED API ----------------------------------------------------------

    /// Read the LED Brightness byte (`03 FB FB FB` → `03 FB 00 01 <b>`, §5.1).
    pub fn read_brightness(&self) -> Result<u8> {
        let mut req = [0u8; protocol::OUTPUT_REPORT_LEN];
        req[0] = protocol::REPORT_ID;
        req[1] = Command::ReadLed as u8;
        req[2] = Command::ReadLed as u8;
        req[3] = Command::ReadLed as u8;
        self.transport.write(&req)?;

        let mut buf = [0u8; protocol::OUTPUT_REPORT_LEN];
        for _ in 0..DRAIN_READS {
            let n = self.transport.read(&mut buf, READ_TIMEOUT_MS)?;
            if n == 0 {
                continue;
            }
            let frame = &buf[..n];
            if frame.get(offset::COMMAND) == Some(&(Command::ReadLed as u8)) {
                return frame
                    .get(protocol::LED_BRIGHTNESS_OFFSET)
                    .copied()
                    .ok_or(Error::Malformed("LED response too short"));
            }
        }
        Err(Error::NoResponse)
    }

    /// Stage LED mode + Palette (three page frames, §5.2).
    /// Not persisted until [`commit`](Device::commit).
    pub fn write_led(&self, mode: LedMode, palette: &Palette) -> Result<()> {
        for frame in led::upload_frames(mode, palette) {
            self.transport.write(&frame)?;
        }
        Ok(())
    }

    pub fn set_led(&self, mode: LedMode, palette: &Palette) -> Result<()> {
        self.write_led(mode, palette)?;
        self.commit()
    }
}
