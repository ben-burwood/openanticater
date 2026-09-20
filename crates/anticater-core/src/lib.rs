//! Core Device Layer for the Anticater Knob - `PROTOCOL.md`
//!
//! - [`protocol`] — wire-format constants, byte offsets, usage tables.
//! - [`action`] — the [`Action`] model plus pure encode/decode of a config entry.
//! - [`led`] — LED mode/palette upload frames (§5).
//! - [`device`] — the [`Device`] HID transport: config read/write/commit + LED.

pub mod action;
pub mod device;
pub mod error;
pub mod led;
pub mod protocol;

pub use hut;

pub use action::{Action, Key, KeyStep, MouseAction, Swipe};
pub use device::{Device, KnobInfo};
pub use error::{Error, Result};
pub use led::{LedMode, Palette};
pub use protocol::{Category, Command, Control, DeviceId, Modifier};
