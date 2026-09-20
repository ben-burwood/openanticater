//! Core Device Layer for the Anticater Knob - `PROTOCOL.md`
//!
//! - [`protocol`] — wire-format constants, byte offsets, usage tables.
//! - [`action`] — the [`Action`] model plus pure encode/decode of a config entry.
//! - [`led`] — LED mode/palette upload frames (§5).
//! - [`transport`] — the [`Transport`] pipe abstraction over the two wire paths.
//! - [`hid`] — the USB HID transport (§1–§2); [`ble`] — the Bluetooth LE one (§8).
//! - [`device`] — the [`Device`]: config read/write/commit + LED over any transport.

pub mod action;
pub mod ble;
pub mod device;
pub mod error;
pub mod hid;
pub mod led;
pub mod protocol;
pub mod transport;

pub use hut;

pub use action::{Action, Key, KeyStep, MouseAction, Swipe};
pub use ble::BleTransport;
pub use device::{Device, KnobInfo};
pub use error::{Error, Result};
pub use hid::HidTransport;
pub use led::{LedMode, Palette};
pub use protocol::{Category, Command, Control, DeviceId, Modifier};
pub use transport::Transport;
