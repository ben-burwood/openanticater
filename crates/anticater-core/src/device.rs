//! HID Transport: find, open, and talk to the Knob's config Interface.
//!
//! Configuration goes over interface `MI_00` -
//! vendor channel `usage_page == 0xFF00` (`PROTOCOL.md` §1).

use hidapi::{DeviceInfo, HidApi, HidDevice};

use crate::action::Action;
use crate::error::{Error, Result};
use crate::led::{self, LedMode, Palette};
use crate::protocol::{self, Command, Control, DeviceId, offset};

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
    handle: HidDevice,
    info: KnobInfo,
}

impl Device {
    /// Open the FIRST connected Knob found on the config Interface.
    pub fn open() -> Result<Self> {
        let api = HidApi::new()?;
        let info = api
            .device_list()
            .find(|d| is_config_interface(d))
            .ok_or(Error::DeviceNotFound)?;
        let knob = KnobInfo {
            vendor_id: info.vendor_id(),
            product_id: info.product_id(),
            manufacturer: info.manufacturer_string().map(str::to_owned),
            product: info.product_string().map(str::to_owned),
            serial: info.serial_number().map(str::to_owned),
        };
        let handle = api.open_path(info.path())?;
        Ok(Self { handle, info: knob })
    }

    pub fn info(&self) -> &KnobInfo {
        &self.info
    }

    /// Polled for Connect/Disconnect Events
    pub fn is_connected() -> bool {
        HidApi::new()
            .map(|api| api.device_list().any(is_config_interface))
            .unwrap_or(false)
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
        self.handle.write(&req)?;

        // Drain until the reply's (command, control, page) matches the request
        let mut buf = [0u8; protocol::OUTPUT_REPORT_LEN];
        for _ in 0..DRAIN_READS {
            let n = self.handle.read_timeout(&mut buf, READ_TIMEOUT_MS)?;
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
        self.handle.write(&action.encode(control, page))?;
        Ok(())
    }

    /// Persist all staged writes (`03 FD FE FF`, §3).
    pub fn commit(&self) -> Result<()> {
        let mut frame = [0u8; protocol::OUTPUT_REPORT_LEN];
        frame[0] = protocol::REPORT_ID;
        frame[1..1 + protocol::COMMIT_PAYLOAD.len()].copy_from_slice(&protocol::COMMIT_PAYLOAD);
        self.handle.write(&frame)?;
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
        self.handle.write(&req)?;

        let mut buf = [0u8; protocol::OUTPUT_REPORT_LEN];
        for _ in 0..DRAIN_READS {
            let n = self.handle.read_timeout(&mut buf, READ_TIMEOUT_MS)?;
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
            self.handle.write(&frame)?;
        }
        Ok(())
    }

    pub fn set_led(&self, mode: LedMode, palette: &Palette) -> Result<()> {
        self.write_led(mode, palette)?;
        self.commit()
    }
}

/// Is this the Knob's Vendor config Interface (right VID/PID and usage page)?
fn is_config_interface(info: &DeviceInfo) -> bool {
    info.usage_page() == protocol::CONFIG_USAGE_PAGE && is_knob(info.vendor_id(), info.product_id())
}

/// VID/PID belongs to Anticater Firmware Family (§1)?
pub(crate) fn is_knob(vendor_id: u16, product_id: u16) -> bool {
    let vendor_ok =
        vendor_id == DeviceId::DEFAULT.vendor_id || vendor_id == DeviceId::VENDOR_ID_ALT;
    vendor_ok && DeviceId::FAMILY_PRODUCT_IDS.contains(&product_id)
}
