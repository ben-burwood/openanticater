use std::fmt;

#[derive(Debug)]
pub enum Error {
    /// No Knob was found on the config Interface (`usage_page == 0xFF00`)
    DeviceNotFound,
    /// HID Transport Error
    Hid(hidapi::HidError),
    /// Query was sent but no matching reply arrived within the drain window.
    NoResponse,
    /// Frame was too short or otherwise malformed to decode.
    Malformed(&'static str),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::DeviceNotFound => write!(f, "Anticater knob not found on the USB bus"),
            Error::Hid(e) => write!(f, "HID transport error: {e}"),
            Error::NoResponse => write!(f, "no matching response from the device"),
            Error::Malformed(what) => write!(f, "malformed frame: {what}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Hid(e) => Some(e),
            _ => None,
        }
    }
}

impl From<hidapi::HidError> for Error {
    fn from(e: hidapi::HidError) -> Self {
        Error::Hid(e)
    }
}

pub type Result<T> = std::result::Result<T, Error>;
