# ANTICATER knob — USB HID protocol

Definitive specification of the ANTICATER USB volume knob's vendor HID protocol: device
identity, transport, and the full read/write/LED command set. Every layout here is confirmed
against live hardware. The reference implementation is `library/anticater/`; the
reverse-engineering derivation (binary offsets, methodology) lives in `research/FINDINGS.md`.

---

## 1. Device

| | |
|---|---|
| Vendor ID | `0x514C` (firmware family also uses `0x1189`) |
| Product ID | `0x8850` — family PID table: `8842 8840 8830 8831 8832 8833 8850 8851` |
| Config interface | **`MI_00`** — vendor usage page **`0xFF00`**, usage `0x01` |
| Input interface | `MI_01` — the emulated keyboard (`0x06`), mouse (`0x02`), consumer (`0x0C`), system-control |

The knob is a composite HID device. `MI_01` is what the OS receives as key/media/mouse input;
`MI_00` is the vendor channel used for all configuration. Select the device by opening the
interface whose `usage_page == 0xFF00` by its `path`. This spec applies to the whole family
(any listed VID/PID), not only `0x8850`.

---

## 2. Transport

Report descriptor of `MI_00`:
```
06 00 ff 09 01 a1 01 85 03
09 02 15 00 26 00 ff 75 08 95 40 81 06   ; Input  report, 64 bytes
09 02 15 00 26 00 ff 75 08 95 40 91 06   ; Output report, 64 bytes
c0
```

- **Report ID `0x03`** on every transfer.
- **Host → device:** write `[0x03] + payload`, zero-padded to **65 bytes** (report id + 64).
- **Device → host:** read returns **64 bytes**; `data[0] == 0x03`, `data[1]` = command echo.
- **Responses are pipelined one deep.** After a query, read until the `(command, type, page)`
  in the reply matches the request (drain at most ~5 reads).

---

## 3. Command set

`payload[0]` is the command byte (the byte after the `0x03` report id).

| cmd | dir | request | response |
|-----|-----|---------|----------|
| `0xFA` | read | `FA <type> 00 <page>` | `03 FA <type> <page> <entry>` |
| `0xFB` | read | `FB FB FB` | `03 FB 00 01 <brightness> 00 …` |
| `0xFD` | write | `FD <type> <page> <entry>` | none |
| `0xFD FE FF` | write | commit / persist all pending writes | none |
| `0xFE B0` | write | `FE B0 <page> <mode> <48-byte RGB>` ×3 (LED upload) | none |

A configuration write and its read-back share the **same 64-byte entry body**; only the
command byte differs (`0xFA` on read, `0xFD` on write). After any batch of `0xFD` writes,
send the **commit** `03 FD FE FF` to persist them.

---

## 4. Configuration entry

Every control/page maps to one 64-byte entry with this header:

```
byte 0  0x03    report id
byte 1  command 0xFA (read) / 0xFD (write)
byte 2  type    physical control, 1..5
byte 3  page    config slot 1..3 (only page 1 is active on this unit — see §6)
byte 4  category  0x01 keyboard · 0x02 consumer · 0x03 mouse
byte 5  subtype: 0x01 single action · 0x00 keyboard macro · 0x04 mouse swipe
byte 6  keyboard: key count (1..18) · consumer: 0x01 (8-bit) / 0x02 (16-bit) · mouse: 0x04
byte 7  0x00
byte 8  0x00
byte 9+ action payload (per category, below)
```

To change a mapping: build the entry, write it with command `0xFD`, then commit `03 FD FE FF`.
Reading back (`0xFA`) returns the same body with byte 1 = `0xFA`.

### 4.1 Keyboard — `category 0x01`
The action payload is a sequence of **1..18 steps**, each **3 bytes at byte `9 + 3·i`**:
```
[ usage, delay_hi, delay_lo ]
```
- **usage** — HID keyboard usage (§7), or a **modifier pseudo-key** (below).
- **delay** — a **big-endian 16-bit value in milliseconds**: the pause *after* that key
  (0 = no delay). Timing is exact (measured 2003 ms for a 2000 ms request).
- **byte 6 = the number of steps.** A single key is a 1-step sequence (byte 5 = `0x01`,
  byte 6 = `0x01`); a macro of N keys uses byte 5 = `0x00`, byte 6 = `N`.

**Modifiers** are not a bitmap — they are **held pseudo-keys** placed as steps before the key
they modify:

| pseudo-key | modifier |
|-----------|----------|
| `0xF1` | Ctrl |
| `0xF2` | Shift |
| `0xF3` | Alt |
| `0xF4` | Win |

So `Ctrl+C` = 2-step sequence `[0xF1, 0x06]`; `Shift+A` = `[0xF2, 0x04]`.

Examples:
```
Enter          : 03 FD <t> <p> 01 01 01 00 00 28
'a' 'b' 'c'    : 03 FD <t> <p> 01 00 03 00 00 04 00 00 05 00 00 06
'a','b' @500ms : 03 FD <t> <p> 01 00 02 00 00 04 01 F4 05 00 00
Ctrl+C         : 03 FD <t> <p> 01 00 02 00 00 F1 00 00 06 00 00
```

### 4.2 Consumer (media) — `category 0x02`
The consumer HID usage (§7) can be 8- or 16-bit:

- **8-bit** (usage ≤ `0xFF`, e.g. Volume/Play/Mute/Brightness): byte 6 = `0x01`, `byte 9` =
  the usage. Bytes 12/21 = 0.
- **16-bit** (usage ≥ `0x100`, e.g. Calculator, browser/app launchers, Bass/Treble): byte 6 =
  `0x02`, `byte 9` = low byte, **`byte 12` = high byte**, `byte 21` = `0x01`.

```
Volume Down (0x00EA) : 03 FD <t> <p> 02 01 01 00 00 EA
Calculator  (0x0192) : 03 FD <t> <p> 02 01 02 00 00 92 00 00 01 …(byte 21)= 01
WWW Refresh (0x0227) : 03 FD <t> <p> 02 01 02 00 00 27 00 00 02 …(byte 21)= 01
```

### 4.3 Mouse — `category 0x03`
**byte 6 = `0x04`** (mouse subtype) for all mouse actions. Three sub-types:

- **Button** (byte 5 = `0x01`) — `byte 12` = HID button **bitmask**: `0x01` left, `0x02` right,
  `0x04` middle (OR-combine for chords). This hardware has only these three buttons — no
  back/forward. Bytes 9 and 21 = 0.
- **Scroll** (byte 5 = `0x01`) — `byte 21` = direction: `0x01` up, `0xFF` down. Byte 12 = 0.
  Optionally a **held modifier** during the scroll (Ctrl/Shift/Alt + wheel, e.g. zoom): put the
  modifier pseudo-key (`0xF1`/`0xF2`/`0xF3`) at `byte 9`.
- **Swipe** (byte 5 = `0x04`) — `byte 9` = direction: `1` left, `2` right, `3` up, `4` down.
  Bytes 12/21 = 0. (Written correctly; the on-screen gesture depends on OS touchpad support.)

```
Mouse left   : 03 FD <t> <p> 03 01 04 00 00 00 00 00 01
Mouse middle : 03 FD <t> <p> 03 01 04 00 00 00 00 00 04
Scroll up    : 03 FD <t> <p> 03 01 04 00 00 00 …(byte 21)= 01
Ctrl+Scroll↑ : 03 FD <t> <p> 03 01 04 00 00 F1 …(byte 21)= 01
Swipe left   : 03 FD <t> <p> 03 04 04 00 00 01
```

---

## 5. LED / RGB

### 5.1 Read — `0xFB`
`03 FB FB FB` → `03 FB 00 01 <brightness> 00 …`. The read reports only the **brightness byte**
(observed `0x0B`); it does not echo the palette.

### 5.2 Write — `0xFE B0`
Three frames, one per page:
```
03 FE B0 <page> <mode> <48-byte RGB>     page = 0, 1, 2
```
- **mode** (`0..5`) is the effective LED control:

  | mode | behaviour |
  |------|-----------|
  | 0 | off |
  | 1 | static white |
  | 2 | static green |
  | 3 | rainbow cycle |
  | 4 | animated |
  | 5 | animated |

- **48 bytes** = 16 RGB triples (the page palette). Follow the three frames with the commit
  `03 FD FE FF`.

**This knob does not support custom LED colour** — the displayed colour is fixed by the mode
preset; the uploaded palette has no visible effect, and there is no brightness-set command.
(The palette bytes are still sent as the vendor app sends them; RGB-capable variants of the
family use the same frames.) Default palette: red, orange, yellow, green, cyan, blue, purple,
dark-red, orange, pale-yellow, chartreuse, teal, navy, magenta, pink, gold.

---

## 6. Config model

- **type 1..6** — the control slots (entry byte 2). The firmware family numbers slots `1..6`;
  which are physically reachable depends on the variant:
  - **Button variant:** `T1`/`T5` = buttons, `T2` = turn CCW, `T4` = turn CW, `T3` = press.
  - **Knob-only variant** (e.g. `ANTICATER_MINI`): no buttons. `T1` is unused, and `T5`/`T6`
    become the **hold-and-turn** gestures — turning while the knob is pressed in, a separate
    slot from a plain turn:

    | type | gesture | factory default |
    |---|---|---|
    | `2` | turn CCW (left) | Volume Down |
    | `4` | turn CW (right) | Volume Up |
    | `3` | press | Play/Pause (or Mute) |
    | `5` | **hold + turn CCW (left)** | Prev Track / Brightness − |
    | `6` | **hold + turn CW (right)** | Next Track / Brightness + |

  A hold-and-turn entry is byte-identical to a plain turn entry (§4) — only the `type` byte
  differs (`5`/`6` instead of `2`/`4`). Direction pairs with the plain turns: `{2,5}` are the
  decrement/left family, `{4,6}` the increment/right family. (Verified against the vendor app's
  own frames, e.g. `03 FD 06 01 02 00 01 00 00 B5` = hold+turn-CW → Next Track.)
- **page 1..3** — three config slots per control (not macro steps; a macro lives entirely
  within one page — see §4.1). **On this unit only page 1 is functional**: pressing/turning a
  control always fires its page-1 action (single/double/long press all use page 1), and the
  vendor app only ever edits page 1. Pages 2/3 are stored and read/written correctly but the
  device has no layer-switch mechanism to activate them — so remap **L1** for anything that
  should take effect.

Factory default mapping (knob-only variant):

| control | page 1 | page 2 | page 3 |
|---------|--------|--------|--------|
| T2 (turn CCW) | Volume Down | Volume Down | Volume Down |
| T3 (press) | Mute | Play/Pause | Play/Pause |
| T4 (turn CW) | Volume Up | Volume Up | Volume Up |
| T5 (hold + turn CCW) | Brightness Down | Prev Track | Prev Track |
| T6 (hold + turn CW) | Brightness Up | Next Track | Next Track |

On the button variant, T1 (button) defaults to Enter and T5 is the second button.

---

## 7. Usage codes

Values from the standard HID Usage Tables (keyboard usages are 1 byte; consumer usages may be
1 or 2 bytes — see §4.2).

- **Keyboard** (category `0x01`, HID page 0x07): `0x04`=a … `0x1D`=z; `0x1E`–`0x27`=1..0;
  `0x28`=Enter, `0x29`=Esc, `0x2A`=Backspace, `0x2B`=Tab, `0x2C`=Space; `0x3A`–`0x45`=F1..F12;
  `0x4F`–`0x52`=arrows; etc.
- **Modifier pseudo-keys** (category `0x01`, used as macro steps): `0xF1`=Ctrl, `0xF2`=Shift,
  `0xF3`=Alt, `0xF4`=Win.
- **Consumer** (category `0x02`, HID page 0x0C) — 8-bit: `0xB5`=Next, `0xB6`=Prev, `0xB7`=Stop,
  `0xCD`=Play/Pause, `0xE2`=Mute, `0xE9`=Vol+, `0xEA`=Vol−, `0x6F`/`0x70`=Brightness+/−.
  16-bit (low@9, high@12): `0x0152`/`0x0153`=Bass+/−, `0x0154`/`0x0155`=Treble+/−,
  `0x0183`=Media Player, `0x018A`=Email, `0x0192`=Calculator, `0x0194`=My Computer,
  `0x0223`=WWW Home, `0x0225`=WWW Forward, `0x0227`=WWW Refresh.
- **Mouse** (category `0x03`): button bitmask at byte 12 (`0x01`/`0x02`/`0x04` = L/R/M); scroll
  at byte 21 (`0x01`/`0xFF` = up/down); swipe (byte 5 = `0x04`) direction at byte 9 (1/2/3/4 =
  left/right/up/down).

The full name↔code tables and lookup are in `library/anticater/keycodes.py`.

> Note: the standard HID *modifier bitmap* (ctrl `0x01`, shift `0x02`, …) appears only in the
> emulated-keyboard **input** reports on `MI_01` (decoded by `anticater monitor`); it is **not**
> used in the config protocol, which uses the `0xF1`–`0xF4` pseudo-keys above.

---

## 8. Bluetooth LE transport

The wireless variant of the knob (advertised name `ANTICATER_MINI`) speaks the **same command
language** as USB — identical `0x03` report id, identical `0xFA`/`0xFB`/`0xFD`/`0xFE` opcodes, and
the identical 64-byte entry body of §4–§5. Only the **pipe** differs: there is no `0xFF00` vendor
HID collection over the air, so config rides a pair of custom GATT services instead of a HID
report to `MI_00`. Everything in §3–§7 applies unchanged to the frame *contents*.

This section was verified against live hardware and cross-checked against the vendor app's own
BLE bridge (`ble_helper.exe`, a `bleak`/WinRT GATT client the Qt GUI drives over stdio JSON).

### 8.1 Why not HID-over-GATT

Over BLE the knob enumerates as a standard **HID-over-GATT** device (service `0x1812`) exposing
only the emulated **keyboard / mouse / consumer** collections. Windows denies user-mode writes to
all of them (the same anti-keylogger lockout that blocks `MI_01` on USB — §README), and crucially
**no `0xFF00` vendor collection is exposed**. The config channel therefore lives entirely in the
two vendor GATT services below.

### 8.2 Vendor GATT services

| service | role |
|---------|------|
| `0000AE40-0000-1000-8000-00805F9B34FB` | primary config tunnel |
| `0000AE30-0000-1000-8000-00805F9B34FB` | alternate config tunnel + status register |

Characteristics (16-bit aliases shown; full UUID = `0000AExx-0000-1000-8000-00805F9B34FB`):

| char | service | properties | role |
|------|---------|-----------|------|
| `AE41` | AE40 | write-without-response | **preferred write** (host → device frames) |
| `AE42` | AE40 | notify | **preferred notify** (device → host responses) |
| `AE01` / `AE03` | AE30 | write-without-response | fallback write |
| `AE02` / `AE04` | AE30 | notify | fallback notify |
| `AE05` | AE30 | indicate | fallback notify |
| `AE10` | AE30 | read / write | volatile status register (not part of the config flow) |

**Write preference order:** `AE41`, then `AE03`, then `AE01`.
**Notify preference order:** `AE42`, then `AE02`, `AE04`, `AE05`.
Pick the first present/writable (resp. notifiable) characteristic from each list.

### 8.3 Transfer

- **Host → device:** subscribe the notify characteristic's CCCD, then write the **same 65-byte
  frame** used on USB (report id `0x03` + 64-byte body, §2) to the write characteristic using
  **write-without-response**. A write-without-response cannot be fragmented by the stack, so it
  must fit **ATT_MTU − 3**; this device negotiates a 64-byte MTU (max 61-byte payload), and a
  full 65-byte write fails with `E_INVALIDARG` (`0x80070057`). Split every frame into chunks of
  **≤ 20 bytes** (the BLE-minimum ATT_MTU − 3, safe on any connection); the firmware reassembles
  the byte stream, so a frame — or the 3-frame LED upload (§5.2) — split across writes arrives
  intact. Do **not** assume a large MTU.
- **Device → host:** config replies arrive as **notifications** on the notify characteristic
  (not as a GATT read). A read query is therefore write-then-await-notification: write the `0xFA`
  request frame, then read the matching reply off the notify stream — the BLE analogue of the
  USB "drain until the reply matches" rule (§2).
- **Response marker:** a config-response frame begins **`0x03 0xFD`** (with a leading-`0x03`
  unwrap guard for an occasional doubled `03 03 FD` prefix). This distinguishes config replies
  from any live HID input the device may also emit.

### 8.4 Selecting the device

Match on the advertised name containing `ANTICATER`, or on the peripheral advertising service
`AE40`. On Windows, prefer resolving the already system-connected device: WinRT often owns the
link and `BluetoothLEDevice.FromBluetoothAddressAsync` can fail while it does — enumerate the
connected devices and open by device id instead. (BleakClient's Windows GattSession also
intermittently returns only the standard services, which is why a direct WinRT/GATT path is used
there rather than going through Bleak.)
