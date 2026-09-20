//! Wire-format constants for the Anticater Vendor HID Protocol.

/// USB Identity of the Knob (`PROTOCOL.md` §1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceId {
    pub vendor_id: u16,
    pub product_id: u16,
}

impl DeviceId {
    /// Primary VID/PID
    pub const DEFAULT: Self = Self {
        vendor_id: 0x514C,
        product_id: 0x8850,
    };

    /// Alternate Firmware Family VendorID
    pub const VENDOR_ID_ALT: u16 = 0x1189;

    /// ProductIDs for the Firmware Family
    pub const FAMILY_PRODUCT_IDS: [u16; 8] = [
        0x8842, 0x8840, 0x8830, 0x8831, 0x8832, 0x8833, 0x8850, 0x8851,
    ];
}

/// Vendor usage page of the config interface `MI_00` (§1).
/// Device is selected by opening the interface whose `usage_page == 0xFF00`.
pub const CONFIG_USAGE_PAGE: u16 = 0xFF00;

/// HID ReportID present on every transfer (§2).
pub const REPORT_ID: u8 = 0x03;

/// Host → device writes are the report id plus a 64-byte payload (§2).
pub const OUTPUT_REPORT_LEN: usize = 65;

/// Device → host reads return 64 bytes (§2).
pub const INPUT_REPORT_LEN: usize = 64;

/// Command byte (`payload[0]`, the byte after the report id) — §3.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Command {
    /// Read a configuration entry: `FA <type> 00 <page>`.
    ReadEntry = 0xFA,
    /// Read LED brightness: `FB FB FB`.
    ReadLed = 0xFB,
    /// Write a configuration entry: `FD <type> <page> <entry>`.
    WriteEntry = 0xFD,
    /// Upload an LED page: `FE B0 <page> <mode> <48-byte RGB>`.
    WriteLed = 0xFE,
}

/// Commit / persist all pending writes: `03 FD FE FF` (§3).
pub const COMMIT_PAYLOAD: [u8; 3] = [0xFD, 0xFE, 0xFF];

/// Action category, entry byte 4 (§4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Category {
    Keyboard = 0x01,
    Consumer = 0x02,
    Mouse = 0x03,
}

/// Modifier pseudo-keys used as macro steps (§4.1, §7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Modifier {
    Ctrl = 0xF1,
    Shift = 0xF2,
    Alt = 0xF3,
    Win = 0xF4,
}

impl Modifier {
    /// Recognize a modifier pseudo-key byte, if the value is one.
    pub const fn from_u8(value: u8) -> Option<Self> {
        match value {
            0xF1 => Some(Self::Ctrl),
            0xF2 => Some(Self::Shift),
            0xF3 => Some(Self::Alt),
            0xF4 => Some(Self::Win),
            _ => None,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Ctrl => "Ctrl",
            Self::Shift => "Shift",
            Self::Alt => "Alt",
            Self::Win => "Win",
        }
    }
}

impl core::fmt::Display for Modifier {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.label())
    }
}

/// Control slots, entry byte 2 (§6).
///
/// Firmware Family numbers slots 1..=6.
/// On the **button** variant, T1/T5 are the two buttons.
/// On the **knob-only** variant (e.g. `ANTICATER_MINI`), T1 is unused and T5/T6 are the
/// **hold-and-turn** gestures — turning while the knob is pressed, distinct from a plain turn (T2/T4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Control {
    /// Button
    T1 = 1,
    /// Knob Counter-Clockwise
    T2 = 2,
    /// Knob Press
    T3 = 3,
    /// Knob Clockwise
    T4 = 4,
    /// Hold + CCW
    T5 = 5,
    /// Hold + CW
    T6 = 6,
}

impl Control {
    /// Knob Counter-Clockwise — alias for [`Control::T2`].
    pub const TURN_CCW: Self = Self::T2;
    /// Knob Press — alias for [`Control::T3`].
    pub const PRESS: Self = Self::T3;
    /// Knob Clockwise — alias for [`Control::T4`].
    pub const TURN_CW: Self = Self::T4;
    /// Knob Hold and Counter-Clockwise — alias for [`Control::T5`] (§6).
    pub const HOLD_TURN_CCW: Self = Self::T5;
    /// Knob Hold and Clockwise — alias for [`Control::T6`] (§6).
    pub const HOLD_TURN_CW: Self = Self::T6;
}

/// Config slot, entry byte 3 (§6).
/// Only page 1 is functional on this unit.
pub const ACTIVE_PAGE: u8 = 1;

/// Byte offsets within a 64/65-byte config frame.
/// Index 0 is always the report id (§4).
/// Reads, writes, and read-back responses all share this layout except the read *request* (see [`crate::device`]).
pub mod offset {
    /// Command byte (`0xFA`/`0xFD`/…).
    pub const COMMAND: usize = 1;
    /// Physical control slot (`Control`).
    pub const CONTROL: usize = 2;
    /// Config page.
    pub const PAGE: usize = 3;
    /// Action category (`Category`).
    pub const CATEGORY: usize = 4;
    /// Action subtype.
    pub const SUBTYPE: usize = 5;
    /// Keyboard step count / consumer size selector / mouse `0x04`.
    pub const COUNT: usize = 6;
    /// First action-payload byte; also the keyboard step base (`PAYLOAD + 3*i`).
    pub const PAYLOAD: usize = 9;

    /// Consumer usage low byte (8- and 16-bit).
    pub const CONSUMER_LOW: usize = 9;
    /// Consumer usage high byte (16-bit only).
    pub const CONSUMER_HIGH: usize = 12;
    /// Consumer 16-bit marker (set to `0x01`).
    pub const CONSUMER_WIDE_FLAG: usize = 21;

    /// Mouse button bitmask.
    pub const MOUSE_BUTTON: usize = 12;
    /// Mouse scroll direction (`0x01` up / `0xFF` down).
    pub const MOUSE_SCROLL: usize = 21;
    /// Held modifier during a mouse scroll.
    pub const MOUSE_MODIFIER: usize = 9;
    /// Mouse swipe direction (`1`..`4`).
    pub const SWIPE_DIR: usize = 9;
}

/// LED write sub-command: the byte after `0xFE` on an upload frame (§5.2).
pub const LED_SUBCOMMAND: u8 = 0xB0;
/// Bytes of RGB palette per LED page: 16 triples (§5.2).
pub const LED_PALETTE_LEN: usize = 48;
/// LED upload sends one frame per page (§5.2).
pub const LED_PAGE_COUNT: usize = 3;
/// First palette byte within an LED upload frame (after report id/cmd/B0/page/mode).
pub const LED_PALETTE_OFFSET: usize = 5;
/// Brightness byte within an LED read response `03 FB 00 01 <brightness>` (§5.1).
pub const LED_BRIGHTNESS_OFFSET: usize = 4;

/// Entry subtype (byte 5): a single keyboard key or single mouse action.
pub const SUBTYPE_SINGLE: u8 = 0x01;
/// Entry subtype (byte 5): a keyboard macro of multiple steps.
pub const SUBTYPE_MACRO: u8 = 0x00;
/// Entry subtype (byte 5): a mouse swipe gesture.
pub const SUBTYPE_SWIPE: u8 = 0x04;
/// Entry count (byte 6) for every mouse action.
pub const MOUSE_COUNT: u8 = 0x04;

pub mod usages {
    /// Keyboard usage defined by HID Tables (HID page 0x07).
    /// Wire codes are 1 byte, so the id space is `0..=0xFF`.
    pub fn keyboard() -> impl Iterator<Item = hut::KeyboardKeypad> {
        (0u16..=0xFF).filter_map(|id| hut::KeyboardKeypad::try_from(id).ok())
    }

    /// Consumer (media) usage defined by the HID Tables (HID page 0x0C),
    /// Including the 16-bit ones (Calculator, browser controls, Bass/Treble).
    pub fn consumer() -> impl Iterator<Item = hut::Consumer> {
        (0u16..=0xFFFF).filter_map(|id| hut::Consumer::try_from(id).ok())
    }
}
