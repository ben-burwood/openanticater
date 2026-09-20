//! Bluetooth LE Transport (`PROTOCOL.md` §8).
//!
//! Config frames are written **write-without-response** to the first available of `AE41`/`AE03`/`AE01`,
//! and responses arrive as **notifications** on the first of `AE42`/`AE02`/`AE04`/`AE05` (§8.2).
//!
//! `btleplug` is async (tokio)
//! [`BleTransport`] owns a tokio [`Runtime`] and presents a blocking face: writes
//! `block_on` the GATT write, and a background task drains the notification stream
//! into an `mpsc` channel that [`read`](Transport::read) pops with a timeout.

use std::collections::BTreeSet;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::Duration;

use btleplug::api::{
    Central, CharPropFlags, Characteristic, Manager as _, Peripheral as _, ScanFilter, WriteType,
    bleuuid::uuid_from_u16,
};
use btleplug::platform::{Manager, Peripheral};
use futures::StreamExt;
use tokio::runtime::Runtime;

use crate::device::KnobInfo;
use crate::error::{Error, Result};
use crate::transport::Transport;

/// Vendor GATT service carrying the primary config tunnel (§8.2).
const SERVICE_AE40: u16 = 0xAE40;
/// Write characteristics, in preference order (§8.2).
const WRITE_CHARS: [u16; 3] = [0xAE41, 0xAE03, 0xAE01];
/// Notify/indicate characteristics, in preference order (§8.2).
const NOTIFY_CHARS: [u16; 4] = [0xAE42, 0xAE02, 0xAE04, 0xAE05];
/// Standard GATT Battery Level characteristic (Battery Service `0x180F`).
const BATTERY_LEVEL: u16 = 0x2A19;
/// Max bytes per GATT write; longer frames are chunked (§8.3).
/// Sized to the BLE floor: the minimum ATT_MTU is 23, so a write-without-response
/// (which WinRT/GATT cannot fragment) is capped at MTU−3 = 20 bytes.
/// Device negotiates a 64-byte MTU, so a full 65-byte frame in one write fails with
/// `E_INVALIDARG` (0x80070057) — chunking at 20 fits every connection.
/// Firmware reassembles the byte stream (the vendor's LED upload is itself split
/// mid-frame), so a frame split across writes arrives intact.
const CHUNK_SIZE: usize = 20;
/// How long to scan for the advertising knob before giving up.
const SCAN_SECS: u64 = 3;

/// A GATT connection to the knob's vendor config services (§8).
pub struct BleTransport {
    rt: Runtime,
    peripheral: Peripheral,
    write_char: Characteristic,
    /// Standard GATT Battery Level (`0x2A19`).
    battery_char: Option<Characteristic>,
    /// Notifications forwarded off the GATT stream by a background task.
    rx: Receiver<Vec<u8>>,
}

impl BleTransport {
    /// Scan for, connect to, and subscribe the first advertising knob.
    pub fn open() -> Result<(Self, KnobInfo)> {
        let rt = Runtime::new().map_err(|e| Error::BleSetup(format!("tokio runtime: {e}")))?;
        let (peripheral, write_char, notify_char, battery_char, info) = rt.block_on(connect())?;

        // Subscribe, then forward every notification value into a channel that
        // the synchronous `read` drains (§8.3: replies come as notifications).
        rt.block_on(peripheral.subscribe(&notify_char))?;
        let (tx, rx) = mpsc::channel();
        let source = peripheral.clone();
        rt.spawn(async move {
            if let Ok(mut stream) = source.notifications().await {
                while let Some(notification) = stream.next().await {
                    if tx.send(notification.value).is_err() {
                        break; // transport dropped
                    }
                }
            }
        });

        Ok((
            Self {
                rt,
                peripheral,
                write_char,
                battery_char,
                rx,
            },
            info,
        ))
    }
}

impl Transport for BleTransport {
    fn write(&self, frame: &[u8]) -> Result<()> {
        self.rt
            .block_on(async {
                for chunk in frame.chunks(CHUNK_SIZE) {
                    self.peripheral
                        .write(&self.write_char, chunk, WriteType::WithoutResponse)
                        .await?;
                }
                Ok::<(), btleplug::Error>(())
            })
            .map_err(Error::Ble)
    }

    fn read(&self, buf: &mut [u8], timeout_ms: i32) -> Result<usize> {
        let dur = Duration::from_millis(timeout_ms.max(0) as u64);
        match self.rx.recv_timeout(dur) {
            Ok(value) => {
                // §8.3: a config reply can arrive with a doubled report-id prefix
                // (`03 03 FD …`). Drop the extra leading `0x03` so the frame aligns
                // to the `03 <cmd> …` layout the device layer matches on. No command
                // byte is `0x03`, so a leading `03 03` is unambiguously the doubled id.
                let frame: &[u8] = match value.as_slice() {
                    [0x03, 0x03, ..] => &value[1..],
                    other => other,
                };
                let n = frame.len().min(buf.len());
                buf[..n].copy_from_slice(&frame[..n]);
                Ok(n)
            }
            Err(RecvTimeoutError::Timeout) => Ok(0),
            Err(RecvTimeoutError::Disconnected) => {
                Err(Error::BleSetup("notification stream closed".into()))
            }
        }
    }

    fn is_alive(&self) -> bool {
        self.rt
            .block_on(self.peripheral.is_connected())
            .unwrap_or(false)
    }

    fn kind(&self) -> &'static str {
        "BLE"
    }

    fn battery(&self) -> Option<u8> {
        let ch = self.battery_char.as_ref()?;
        let value = self.rt.block_on(self.peripheral.read(ch)).ok()?;
        value.first().copied()
    }
}

/// Scan, connect, discover, and resolve the write + notify (+ battery) characteristics (§8.4).
type ConnectResult = (
    Peripheral,
    Characteristic,
    Characteristic,
    Option<Characteristic>,
    KnobInfo,
);
async fn connect() -> Result<ConnectResult> {
    let manager = Manager::new().await?;
    let adapter = manager
        .adapters()
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| Error::BleSetup("no Bluetooth adapter found".into()))?;

    // Unfiltered scan: some units advertise only their name (no service UUID in
    // the advert), so a service-filtered scan would drop them before the
    // name-or-service check below ever runs (§8.4).
    adapter.start_scan(ScanFilter::default()).await?;
    tokio::time::sleep(Duration::from_secs(SCAN_SECS)).await;

    // Select by advertised name (contains ANTICATER) or by the vendor service.
    let mut chosen: Option<(Peripheral, Option<String>)> = None;
    for peripheral in adapter.peripherals().await? {
        let props = peripheral.properties().await?;
        let matches = props.as_ref().is_some_and(|p| {
            p.local_name
                .as_deref()
                .is_some_and(|n| n.to_uppercase().contains("ANTICATER"))
                || p.services.contains(&uuid_from_u16(SERVICE_AE40))
        });
        if matches {
            let name = props.and_then(|p| p.local_name);
            chosen = Some((peripheral, name));
            break;
        }
    }
    let (peripheral, name) = chosen.ok_or(Error::DeviceNotFound)?;
    let _ = adapter.stop_scan().await;

    if !peripheral.is_connected().await? {
        peripheral.connect().await?;
    }
    peripheral.discover_services().await?;

    let chars = peripheral.characteristics();
    let write_char = pick(
        &chars,
        &WRITE_CHARS,
        CharPropFlags::WRITE | CharPropFlags::WRITE_WITHOUT_RESPONSE,
    )
    .ok_or_else(|| Error::BleSetup("no writable AE41/AE03/AE01 characteristic".into()))?;
    let notify_char = pick(
        &chars,
        &NOTIFY_CHARS,
        CharPropFlags::NOTIFY | CharPropFlags::INDICATE,
    )
    .ok_or_else(|| Error::BleSetup("no notify AE42/AE02/AE04/AE05 characteristic".into()))?;
    let battery_char = chars
        .iter()
        .find(|c| c.uuid == uuid_from_u16(BATTERY_LEVEL))
        .cloned();

    let info = KnobInfo {
        vendor_id: 0,
        product_id: 0,
        manufacturer: None,
        product: name,
        serial: Some(peripheral.address().to_string()),
    };
    Ok((peripheral, write_char, notify_char, battery_char, info))
}

/// First characteristic from `prefs` (by 16-bit alias) that carries `want` props.
fn pick(
    chars: &BTreeSet<Characteristic>,
    prefs: &[u16],
    want: CharPropFlags,
) -> Option<Characteristic> {
    prefs.iter().find_map(|&short| {
        let uuid = uuid_from_u16(short);
        chars
            .iter()
            .find(|c| c.uuid == uuid && c.properties.intersects(want))
            .cloned()
    })
}
