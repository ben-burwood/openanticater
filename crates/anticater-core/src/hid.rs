//! USB HID Transport (`PROTOCOL.md` §1–§2).
//!
//! Configuration on the knob's vendor HID interface `MI_00` (`usage_page == 0xFF00`, §1).
//! Frames are the report id `0x03` plus a 64-byte payload (§2).

use std::ffi::CString;

use hidapi::{DeviceInfo, HidApi, HidDevice};

use crate::device::KnobInfo;
use crate::error::{Error, Result};
use crate::protocol::{self, DeviceId};
use crate::transport::Transport;

/// The Knob's Vendor HID config Interface (`MI_00`, `usage_page == 0xFF00`, §1).
pub struct HidTransport {
    handle: HidDevice,
    /// The opened interface's OS path, so liveness tracks *this* device specifically rather than "any knob on the bus".
    path: CString,
}

impl HidTransport {
    /// Open the FIRST connected Knob on HID config Interface.
    pub fn open() -> Result<(Self, KnobInfo)> {
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
        let path = info.path().to_owned();
        let handle = api.open_path(info.path())?;
        Ok((Self { handle, path }, knob))
    }
}

impl Transport for HidTransport {
    fn write(&self, frame: &[u8]) -> Result<()> {
        self.handle.write(frame)?;
        Ok(())
    }

    fn read(&self, buf: &mut [u8], timeout_ms: i32) -> Result<usize> {
        Ok(self.handle.read_timeout(buf, timeout_ms)?)
    }

    fn is_alive(&self) -> bool {
        HidApi::new()
            .map(|api| api.device_list().any(|d| d.path() == self.path.as_c_str()))
            .unwrap_or(false)
    }

    fn kind(&self) -> &'static str {
        "USB"
    }
}

pub fn hid_present() -> bool {
    HidApi::new()
        .map(|api| api.device_list().any(is_config_interface))
        .unwrap_or(false)
}

fn is_config_interface(info: &DeviceInfo) -> bool {
    info.usage_page() == protocol::CONFIG_USAGE_PAGE && is_knob(info.vendor_id(), info.product_id())
}

/// VID/PID belongs to the Anticater firmware family (§1)?
pub(crate) fn is_knob(vendor_id: u16, product_id: u16) -> bool {
    let vendor_ok =
        vendor_id == DeviceId::DEFAULT.vendor_id || vendor_id == DeviceId::VENDOR_ID_ALT;
    vendor_ok && DeviceId::FAMILY_PRODUCT_IDS.contains(&product_id)
}
