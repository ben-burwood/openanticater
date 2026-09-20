//! The decoded action model and the pure encode/decode of a config entry.
//! Maps to/from the 64-byte entry body defined in `PROTOCOL.md` §4.
//!
//! [`Action`] is what a control does when triggered.
//!
//! Keyboard and consumer usages are the typed [`hut`] tables (§7).
//! Vendor modifier pseudo-keys `0xF1..=0xF4` are *not* standard HID usages,
//! so they keep their own [`Modifier`] type.

use std::fmt;

use crate::error::{Error, Result};
use crate::protocol::{self, Category, Command, Control, Modifier, offset};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Keyboard key or a macro: an ordered list of 1..=18 steps (§4.1).
    Keyboard(Vec<KeyStep>),
    /// Consumer/media usage, 8- or 16-bit (§4.2).
    Consumer(hut::Consumer),
    /// Mouse button, scroll, or swipe (§4.3).
    Mouse(MouseAction),
}

/// Keyboard Key
/// Either a real HID usage or a held modifier pseudo-key placed before the key it modifies (§4.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// Standard HID Keyboard (§7).
    Usage(hut::KeyboardKeypad),
    /// Vendor modifier pseudo-key (`0xF1..=0xF4`).
    Modifier(Modifier),
}

impl Key {
    /// wire byte for this key (usage id or modifier pseudo-key).
    fn to_byte(self) -> u8 {
        match self {
            Key::Usage(usage) => u16::from(&usage) as u8,
            Key::Modifier(modifier) => modifier as u8,
        }
    }

    /// Decode a wire byte
    /// Modifier pseudo-key takes precedence, otherwise a keyboard usage.
    fn from_byte(byte: u8) -> Result<Self> {
        if let Some(modifier) = Modifier::from_u8(byte) {
            return Ok(Key::Modifier(modifier));
        }
        hut::KeyboardKeypad::try_from(byte as u16)
            .map(Key::Usage)
            .map_err(|_| Error::Malformed("unknown keyboard usage"))
    }
}

/// Keyboard Action Step
/// Key (or a modifier held for a chord), followed by a delay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyStep {
    pub key: Key,
    /// Delay in milliseconds after this step (0 = none).
    pub delay_ms: u16,
}

impl KeyStep {
    /// Key with no trailing delay.
    pub const fn key(usage: hut::KeyboardKeypad) -> Self {
        Self {
            key: Key::Usage(usage),
            delay_ms: 0,
        }
    }

    /// Modifier pseudo-key (placed before the key it modifies).
    pub const fn modifier(modifier: Modifier) -> Self {
        Self {
            key: Key::Modifier(modifier),
            delay_ms: 0,
        }
    }

    /// Key held, then a delay before the next step.
    pub const fn key_after(usage: hut::KeyboardKeypad, delay_ms: u16) -> Self {
        Self {
            key: Key::Usage(usage),
            delay_ms,
        }
    }
}

/// Mouse action (§4.3).
/// This hardware has only three buttons and no back/forward.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseAction {
    /// Button bitmask: `0x01` left, `0x02` right, `0x04` middle (OR-combined).
    Button(u8),
    /// Wheel scroll, optionally with a held modifier (e.g. Ctrl+wheel to zoom).
    Scroll {
        up: bool,
        modifier: Option<Modifier>,
    },
    /// Swipe gesture (depends on OS touchpad-gesture support).
    Swipe(Swipe),
}

/// Swipe Gesture Direction (§4.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Swipe {
    Left = 1,
    Right = 2,
    Up = 3,
    Down = 4,
}

impl Swipe {
    const fn from_u8(value: u8) -> Result<Self> {
        match value {
            1 => Ok(Self::Left),
            2 => Ok(Self::Right),
            3 => Ok(Self::Up),
            4 => Ok(Self::Down),
            _ => Err(Error::Malformed("invalid swipe direction")),
        }
    }
}

impl Key {
    pub fn label(self) -> String {
        match self {
            Key::Modifier(modifier) => modifier.label().to_string(),
            Key::Usage(usage) => {
                let name = usage.name();
                name.strip_prefix("Keyboard ").unwrap_or(&name).to_string()
            }
        }
    }
}

impl fmt::Display for Swipe {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let dir = match self {
            Swipe::Left => "Left",
            Swipe::Right => "Right",
            Swipe::Up => "Up",
            Swipe::Down => "Down",
        };
        write!(f, "Swipe {dir}")
    }
}

impl fmt::Display for MouseAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MouseAction::Button(mask) => {
                let mut names = Vec::new();
                if mask & 0x01 != 0 {
                    names.push("Left");
                }
                if mask & 0x02 != 0 {
                    names.push("Right");
                }
                if mask & 0x04 != 0 {
                    names.push("Middle");
                }
                if names.is_empty() {
                    write!(f, "Mouse (none)")
                } else {
                    write!(f, "Mouse {}", names.join("+"))
                }
            }
            MouseAction::Scroll { up, modifier } => {
                let dir = if *up { "Up" } else { "Down" };
                match modifier {
                    Some(m) => write!(f, "{m}+Scroll {dir}"),
                    None => write!(f, "Scroll {dir}"),
                }
            }
            MouseAction::Swipe(swipe) => write!(f, "{swipe}"),
        }
    }
}

impl fmt::Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Action::Keyboard(steps) => {
                let parts: Vec<String> = steps
                    .iter()
                    .map(|step| {
                        let label = step.key.label();
                        if step.delay_ms > 0 {
                            format!("{label} ({}ms)", step.delay_ms)
                        } else {
                            label
                        }
                    })
                    .collect();
                write!(f, "{}", parts.join(" + "))
            }
            Action::Consumer(usage) => write!(f, "{}", usage.name()),
            Action::Mouse(mouse) => write!(f, "{mouse}"),
        }
    }
}

/// Read byte `i` of a frame, erroring cleanly if the frame is too short.
fn at(frame: &[u8], i: usize) -> Result<u8> {
    frame
        .get(i)
        .copied()
        .ok_or(Error::Malformed("frame ended mid-entry"))
}

impl Action {
    /// Decode an action from a read-back frame (`03 FA <type> <page> <entry>`).
    /// Indexing follows [`offset`] — byte 0 is the report id.
    pub fn decode(frame: &[u8]) -> Result<Self> {
        let category = at(frame, offset::CATEGORY)?;
        match category {
            c if c == Category::Keyboard as u8 => Self::decode_keyboard(frame),
            c if c == Category::Consumer as u8 => Self::decode_consumer(frame),
            c if c == Category::Mouse as u8 => Self::decode_mouse(frame),
            _ => Err(Error::Malformed("unknown action category")),
        }
    }

    fn decode_keyboard(frame: &[u8]) -> Result<Self> {
        let count = at(frame, offset::COUNT)? as usize;
        if !(1..=18).contains(&count) {
            return Err(Error::Malformed("keyboard step count out of range"));
        }
        let mut steps = Vec::with_capacity(count);
        for i in 0..count {
            let base = offset::PAYLOAD + 3 * i;
            steps.push(KeyStep {
                key: Key::from_byte(at(frame, base)?)?,
                delay_ms: u16::from_be_bytes([at(frame, base + 1)?, at(frame, base + 2)?]),
            });
        }
        Ok(Action::Keyboard(steps))
    }

    fn decode_consumer(frame: &[u8]) -> Result<Self> {
        let low = at(frame, offset::CONSUMER_LOW)? as u16;
        // byte 6 == 0x02 marks a 16-bit usage (high byte at CONSUMER_HIGH).
        let usage_id = if at(frame, offset::COUNT)? == 0x02 {
            low | ((at(frame, offset::CONSUMER_HIGH)? as u16) << 8)
        } else {
            low
        };
        let usage = hut::Consumer::try_from(usage_id)
            .map_err(|_| Error::Malformed("unknown consumer usage"))?;
        Ok(Action::Consumer(usage))
    }

    fn decode_mouse(frame: &[u8]) -> Result<Self> {
        if at(frame, offset::SUBTYPE)? == protocol::SUBTYPE_SWIPE {
            let dir = Swipe::from_u8(at(frame, offset::SWIPE_DIR)?)?;
            return Ok(Action::Mouse(MouseAction::Swipe(dir)));
        }
        let button = at(frame, offset::MOUSE_BUTTON)?;
        if button != 0 {
            return Ok(Action::Mouse(MouseAction::Button(button)));
        }
        // Otherwise it's a scroll: direction at byte 21, optional modifier at 9.
        let up = at(frame, offset::MOUSE_SCROLL)? == 0x01;
        let modifier = Modifier::from_u8(at(frame, offset::MOUSE_MODIFIER)?);
        Ok(Action::Mouse(MouseAction::Scroll { up, modifier }))
    }

    /// Encode this action into a full write frame (`03 FD <type> <page> …`),
    /// zero-padded to [`protocol::OUTPUT_REPORT_LEN`]. Ready to hand to the HID
    /// layer; a matching read-back returns the same body with byte 1 = `0xFA`.
    pub fn encode(&self, control: Control, page: u8) -> [u8; protocol::OUTPUT_REPORT_LEN] {
        let mut f = [0u8; protocol::OUTPUT_REPORT_LEN];
        f[0] = protocol::REPORT_ID;
        f[offset::COMMAND] = Command::WriteEntry as u8;
        f[offset::CONTROL] = control as u8;
        f[offset::PAGE] = page;

        match self {
            Action::Keyboard(steps) => {
                f[offset::CATEGORY] = Category::Keyboard as u8;
                f[offset::SUBTYPE] = if steps.len() == 1 {
                    protocol::SUBTYPE_SINGLE
                } else {
                    protocol::SUBTYPE_MACRO
                };
                f[offset::COUNT] = steps.len() as u8;
                for (i, step) in steps.iter().enumerate() {
                    let base = offset::PAYLOAD + 3 * i;
                    f[base] = step.key.to_byte();
                    let [hi, lo] = step.delay_ms.to_be_bytes();
                    f[base + 1] = hi;
                    f[base + 2] = lo;
                }
            }
            Action::Consumer(usage) => {
                let usage_id = u16::from(usage);
                f[offset::CATEGORY] = Category::Consumer as u8;
                f[offset::SUBTYPE] = protocol::SUBTYPE_SINGLE;
                if usage_id > 0xFF {
                    f[offset::COUNT] = 0x02;
                    f[offset::CONSUMER_LOW] = (usage_id & 0xFF) as u8;
                    f[offset::CONSUMER_HIGH] = (usage_id >> 8) as u8;
                    f[offset::CONSUMER_WIDE_FLAG] = 0x01;
                } else {
                    f[offset::COUNT] = 0x01;
                    f[offset::CONSUMER_LOW] = usage_id as u8;
                }
            }
            Action::Mouse(mouse) => {
                f[offset::CATEGORY] = Category::Mouse as u8;
                f[offset::COUNT] = protocol::MOUSE_COUNT;
                match mouse {
                    MouseAction::Button(mask) => {
                        f[offset::SUBTYPE] = protocol::SUBTYPE_SINGLE;
                        f[offset::MOUSE_BUTTON] = *mask;
                    }
                    MouseAction::Scroll { up, modifier } => {
                        f[offset::SUBTYPE] = protocol::SUBTYPE_SINGLE;
                        f[offset::MOUSE_SCROLL] = if *up { 0x01 } else { 0xFF };
                        if let Some(m) = modifier {
                            f[offset::MOUSE_MODIFIER] = *m as u8;
                        }
                    }
                    MouseAction::Swipe(dir) => {
                        f[offset::SUBTYPE] = protocol::SUBTYPE_SWIPE;
                        f[offset::SWIPE_DIR] = *dir as u8;
                    }
                }
            }
        }
        f
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hut::{Consumer, KeyboardKeypad as Kbd};

    // The <type>/<page> the doc leaves as placeholders; any values round-trip.
    const T: Control = Control::T4;
    const P: u8 = protocol::ACTIVE_PAGE;

    /// Body bytes (skip report id / cmd / type / page) for readable assertions.
    fn body(frame: &[u8]) -> &[u8] {
        &frame[offset::CATEGORY..]
    }

    #[test]
    fn volume_down_matches_spec() {
        // 03 FD <t> <p> 02 01 01 00 00 EA
        let f = Action::Consumer(Consumer::VolumeDecrement).encode(T, P);
        assert_eq!(f[offset::COMMAND], Command::WriteEntry as u8);
        assert_eq!(f[offset::CONTROL], T as u8);
        assert_eq!(f[offset::PAGE], P);
        assert_eq!(body(&f)[..6], [0x02, 0x01, 0x01, 0x00, 0x00, 0xEA]);
    }

    #[test]
    fn calculator_is_16_bit() {
        // 03 FD <t> <p> 02 01 02 00 00 92 00 00 01 …(byte 21)=01
        let f = Action::Consumer(Consumer::ALCalculator).encode(T, P);
        assert_eq!(f[offset::COUNT], 0x02);
        assert_eq!(f[offset::CONSUMER_LOW], 0x92);
        assert_eq!(f[offset::CONSUMER_HIGH], 0x01);
        assert_eq!(f[offset::CONSUMER_WIDE_FLAG], 0x01);
    }

    #[test]
    fn ctrl_c_macro() {
        // 03 FD <t> <p> 01 00 02 00 00 F1 00 00 06 00 00
        let f = Action::Keyboard(vec![
            KeyStep::modifier(Modifier::Ctrl),
            KeyStep::key(Kbd::KeyboardC),
        ])
        .encode(T, P);
        assert_eq!(f[offset::CATEGORY], 0x01);
        assert_eq!(f[offset::SUBTYPE], protocol::SUBTYPE_MACRO);
        assert_eq!(f[offset::COUNT], 0x02);
        assert_eq!(f[offset::PAYLOAD], 0xF1); // Ctrl pseudo-key
        assert_eq!(f[offset::PAYLOAD + 3], 0x06); // 'c'
    }

    #[test]
    fn single_key_uses_single_subtype() {
        // Enter: 03 FD <t> <p> 01 01 01 00 00 28
        let f = Action::Keyboard(vec![KeyStep::key(Kbd::KeyboardReturnEnter)]).encode(T, P);
        assert_eq!(f[offset::SUBTYPE], protocol::SUBTYPE_SINGLE);
        assert_eq!(f[offset::COUNT], 0x01);
        assert_eq!(f[offset::PAYLOAD], 0x28);
    }

    #[test]
    fn keyboard_delay_is_big_endian() {
        // 'a','b' @500ms: step 'a' carries a 500ms (0x01F4) delay.
        let f = Action::Keyboard(vec![
            KeyStep::key_after(Kbd::KeyboardA, 500),
            KeyStep::key(Kbd::KeyboardB),
        ])
        .encode(T, P);
        assert_eq!(f[offset::PAYLOAD], 0x04);
        assert_eq!(f[offset::PAYLOAD + 1], 0x01); // hi
        assert_eq!(f[offset::PAYLOAD + 2], 0xF4); // lo
    }

    #[test]
    fn mouse_button_and_swipe() {
        // Mouse left: byte 12 = 0x01. Swipe left: byte 5 = 0x04, byte 9 = 0x01.
        let btn = Action::Mouse(MouseAction::Button(0x01)).encode(T, P);
        assert_eq!(btn[offset::SUBTYPE], protocol::SUBTYPE_SINGLE);
        assert_eq!(btn[offset::MOUSE_BUTTON], 0x01);

        let swipe = Action::Mouse(MouseAction::Swipe(Swipe::Left)).encode(T, P);
        assert_eq!(swipe[offset::SUBTYPE], protocol::SUBTYPE_SWIPE);
        assert_eq!(swipe[offset::SWIPE_DIR], 0x01);
    }

    #[test]
    fn scroll_with_modifier() {
        // Ctrl+Scroll up: byte 9 = 0xF1 (Ctrl), byte 21 = 0x01 (up).
        let f = Action::Mouse(MouseAction::Scroll {
            up: true,
            modifier: Some(Modifier::Ctrl),
        })
        .encode(T, P);
        assert_eq!(f[offset::MOUSE_MODIFIER], 0xF1);
        assert_eq!(f[offset::MOUSE_SCROLL], 0x01);
    }

    // A write frame and its read-back share the same body, so encode→decode
    // round-trips (rewrite byte 1 to the read command to mimic the reply).
    fn roundtrip(action: Action) {
        let mut frame = action.encode(T, P);
        frame[offset::COMMAND] = Command::ReadEntry as u8;
        assert_eq!(Action::decode(&frame).unwrap(), action);
    }

    #[test]
    fn display_is_human_readable() {
        assert_eq!(
            Action::Consumer(Consumer::VolumeDecrement).to_string(),
            "Volume Decrement"
        );
        assert_eq!(
            Action::Keyboard(vec![
                KeyStep::modifier(Modifier::Ctrl),
                KeyStep::key(Kbd::KeyboardC),
            ])
            .to_string(),
            "Ctrl + C"
        );
        assert_eq!(
            Action::Keyboard(vec![KeyStep::key(Kbd::KeyboardReturnEnter)]).to_string(),
            "Return Enter"
        );
        assert_eq!(
            Action::Mouse(MouseAction::Button(0x01)).to_string(),
            "Mouse Left"
        );
        assert_eq!(
            Action::Mouse(MouseAction::Swipe(Swipe::Left)).to_string(),
            "Swipe Left"
        );
    }

    #[test]
    fn round_trips() {
        roundtrip(Action::Consumer(Consumer::VolumeIncrement));
        roundtrip(Action::Consumer(Consumer::ALCalculator));
        roundtrip(Action::Keyboard(vec![KeyStep::key(
            Kbd::KeyboardReturnEnter,
        )]));
        roundtrip(Action::Keyboard(vec![
            KeyStep::modifier(Modifier::Ctrl),
            KeyStep::key(Kbd::KeyboardC),
        ]));
        roundtrip(Action::Mouse(MouseAction::Button(0x04)));
        roundtrip(Action::Mouse(MouseAction::Scroll {
            up: false,
            modifier: None,
        }));
        roundtrip(Action::Mouse(MouseAction::Swipe(Swipe::Down)));
    }
}
