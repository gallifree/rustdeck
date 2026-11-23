use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub id: String,
    pub name: String,
    pub rows: u8,
    pub columns: u8,
    pub encoders: u8,
    /// How the physical device connects to the system
    pub kind: DeviceKind,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum DeviceKind {
    Elgato,
    Ampligame,
    /// Generic HID keyboard macro pad — buttons fire as keycodes
    HidKeyboard { event_path: String },
}

#[derive(Debug, Clone)]
pub enum Event {
    DeviceConnected(DeviceInfo),
    DeviceDisconnected(String),
    ButtonPress { device: String, position: u16 },
    ButtonRelease { device: String, position: u16 },
    EncoderRotate { device: String, position: u16, ticks: i32 },
    EncoderPress { device: String, position: u16 },
    EncoderRelease { device: String, position: u16 },
    LuaError { context: String, error: String },
    PageChanged(usize),
}

#[derive(Debug, Clone)]
pub enum Command {
    SetButtonImage { device: String, position: u8, png: Vec<u8> },
    ClearButton { device: String, position: u8 },
    SetBrightness { device: String, brightness: u8 },
    SwitchProfile(String),
    /// Re-read the profile file from disk and show `page`.
    ReloadProfile { page: usize },
    /// Show this page (0-based) on all devices.
    ShowPage(usize),
    Shutdown,
}
