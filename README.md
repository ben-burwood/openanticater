# Open Anticater

Control GUI for the Anticater VK-01 Knob: https://clickclack.io/products/group-buy-anticater-vk-01-desktop-volume-control-knob?srsltid=AU7gw4W14Z61j5oU1fVFuCP5QoPLL1Dfp-nQWIMi8q26zb5gWYNAVefA

The Windows EXE provided for this is functionally fine but the UX isn't great and it isn't well packages (requires many DLLs to run), so why not do better.

This repository defines a Rust Workspace with a `core` Crate managing Device Read/Write and a GPUI Program.

Claude did a pretty good job in reverse engineering the AnticaterEN EXE program to understand the full protocol for the device - this is defined in PROTOCOL.md.

## Live input monitoring (not implemented)

Showing the knob's *live* output (what it emits as you turn/press it) is
intentionally left out for now. The knob's events come out of its emulated HID
interface (`MI_01`) as standard keyboard/consumer/mouse input reports, and on
Windows the OS **denies user-mode `ReadFile` access to the system keyboard and
mouse HID collections** (anti-keylogger protection). A plain `hidapi` reader can
still read the *consumer* collection (volume/media), so it sees the factory
mapping, but it cannot observe controls remapped to keyboard keys or mouse
actions.

Full live monitoring on Windows therefore requires the **Raw Input API**
(`RegisterRawInputDevices` + `WM_INPUT`), which is the OS-sanctioned, read-only
path and captures all of the knob's emulated input regardless of collection. The
Python reference implementation does exactly this — its default Windows backend
uses Raw Input for "full coverage incl. button keys", and falls back to the
`hidapi` (volume/media only) backend elsewhere. If live monitoring is added
here, it should follow the same approach.
