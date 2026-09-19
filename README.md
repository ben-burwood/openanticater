# Open Anticater

Control GUI for the Anticater VK-01 Knob: https://clickclack.io/products/group-buy-anticater-vk-01-desktop-volume-control-knob?srsltid=AU7gw4W14Z61j5oU1fVFuCP5QoPLL1Dfp-nQWIMi8q26zb5gWYNAVefA

The Windows EXE provided for this is functionally fine but the UX isn't great and it isn't well packages (requires many DLLs to run), so why not do better.

This repository defines a Rust Workspace with a `core` Crate managing Device Read/Write and a GPUI Program.

Claude did a pretty good job in reverse engineering the AnticaterEN EXE program to understand the full protocol for the device - this is defined in PROTOCOL.md.
