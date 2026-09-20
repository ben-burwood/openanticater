//! Transport
//! - Vendor **HID** Interface on USB (§1–§2)
//! - **GATT** Services over Bluetooth LE (§8)
//!
//! Single Command Language (`PROTOCOL.md` §3–§7).

use crate::error::Result;

/// Raw Frame Pipe to the Knob.
/// One frame = report id + payload (§2).
pub trait Transport: Send {
    /// Send one output frame verbatim (report id `0x03` + up to 64 payload bytes).
    fn write(&self, frame: &[u8]) -> Result<()>;

    /// Read one input frame into `buf`, waiting up to `timeout_ms`.
    ///
    /// Returns the number of bytes placed in `buf`; **`0` means the read timed
    /// out** with nothing available (same contract as `hidapi::read_timeout`).
    fn read(&self, buf: &mut [u8], timeout_ms: i32) -> Result<usize>;

    fn is_alive(&self) -> bool;

    fn kind(&self) -> &'static str;
}
