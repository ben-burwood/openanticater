use std::fmt;

#[derive(Debug)]
pub enum Error {
    /// No Knob was found on any supported Transport
    /// (USB `usage_page == 0xFF00` or `AE40`/`AE30` vendor GATT services over Bluetooth LE)
    DeviceNotFound,
    /// HID Transport Error from the HID stack.
    Hid(hidapi::HidError),
    /// Bluetooth LE Transport Error from the GATT stack (scan/connect/read/write)
    Ble(btleplug::Error),
    /// Bluetooth LE setup problem not originating from `btleplug`
    BleSetup(String),
    /// Query was sent but no matching reply arrived within the drain window.
    NoResponse,
    /// Frame was too short or otherwise malformed to decode.
    Malformed(&'static str),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::DeviceNotFound => write!(f, "Anticater knob not found on USB or Bluetooth"),
            Error::Hid(e) => write!(f, "HID transport error: {e}"),
            Error::Ble(e) => write!(f, "Bluetooth transport error: {e}"),
            Error::BleSetup(what) => write!(f, "Bluetooth setup error: {what}"),
            Error::NoResponse => write!(f, "no matching response from the device"),
            Error::Malformed(what) => write!(f, "malformed frame: {what}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Hid(e) => Some(e),
            Error::Ble(e) => Some(e),
            _ => None,
        }
    }
}

impl From<hidapi::HidError> for Error {
    fn from(e: hidapi::HidError) -> Self {
        Error::Hid(e)
    }
}

impl From<btleplug::Error> for Error {
    fn from(e: btleplug::Error) -> Self {
        Error::Ble(e)
    }
}

pub type Result<T> = std::result::Result<T, Error>;
