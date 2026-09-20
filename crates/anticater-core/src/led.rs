//! LED / RGB control (`PROTOCOL.md` §5).
//!
//! Note: unit does not support custom LED colour — the displayed colour is fixed
//! by the [`LedMode`] preset and the uploaded palette has no visible effect (§5).

use crate::protocol::{self, Command, REPORT_ID, offset::PAGE};

/// LED Effect Mode — byte 4 of an upload frame (§5.2).
///
/// Animated Modes are all variants of Rainbow RGB
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum LedMode {
    Off = 0,
    StaticWhite = 1,
    StaticGreen = 2,
    Animated1 = 3,
    Animated2 = 4,
    Animated3 = 5,
}

/// 16-triple RGB palette (`R,G,B` × 16)
pub type Palette = [u8; protocol::LED_PALETTE_LEN];

/// Vendor default Palette (§5)
pub const DEFAULT_PALETTE: Palette = [
    0xFF, 0x00, 0x00, // red
    0xFF, 0x80, 0x00, // orange
    0xFF, 0xFF, 0x00, // yellow
    0x00, 0xFF, 0x00, // green
    0x00, 0xFF, 0xFF, // cyan
    0x00, 0x00, 0xFF, // blue
    0x80, 0x00, 0xFF, // purple
    0x80, 0x00, 0x00, // dark-red
    0xFF, 0x80, 0x00, // orange
    0xFF, 0xFF, 0x80, // pale-yellow
    0x80, 0xFF, 0x00, // chartreuse
    0x00, 0x80, 0x80, // teal
    0x00, 0x00, 0x80, // navy
    0xFF, 0x00, 0xFF, // magenta
    0xFF, 0x80, 0xC0, // pink
    0xFF, 0xD7, 0x00, // gold
];

/// Build the three per-page LED Upload Frames for a mode + palette (§5.2)
///
/// `03 FE B0 <page> <mode> <48-byte RGB>` for pages 0, 1, 2.
/// Persist with a commit (`03 FD FE FF`) after sending all three.
pub fn upload_frames(
    mode: LedMode,
    palette: &Palette,
) -> [[u8; protocol::OUTPUT_REPORT_LEN]; protocol::LED_PAGE_COUNT] {
    let mut frames = [[0u8; protocol::OUTPUT_REPORT_LEN]; protocol::LED_PAGE_COUNT];
    for (page, frame) in frames.iter_mut().enumerate() {
        frame[0] = REPORT_ID;
        frame[protocol::offset::COMMAND] = Command::WriteLed as u8;
        frame[2] = protocol::LED_SUBCOMMAND;
        frame[PAGE] = page as u8;
        frame[4] = mode as u8;
        frame[protocol::LED_PALETTE_OFFSET
            ..protocol::LED_PALETTE_OFFSET + protocol::LED_PALETTE_LEN]
            .copy_from_slice(palette);
    }
    frames
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upload_frames_match_spec() {
        // 03 FE B0 <page> <mode> <48-byte RGB>, one frame per page.
        let frames = upload_frames(LedMode::Animated1, &DEFAULT_PALETTE);
        for (page, f) in frames.iter().enumerate() {
            assert_eq!(f[0], REPORT_ID);
            assert_eq!(f[1], Command::WriteLed as u8); // 0xFE
            assert_eq!(f[2], protocol::LED_SUBCOMMAND); // 0xB0
            assert_eq!(f[3], page as u8);
            assert_eq!(f[4], LedMode::Animated1 as u8);
            assert_eq!(&f[5..5 + 3], &[0xFF, 0x00, 0x00]); // first triple = red
        }
    }
}
