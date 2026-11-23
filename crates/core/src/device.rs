use std::collections::HashMap;
use std::sync::{Arc, LazyLock};
use tokio::sync::{RwLock, broadcast};

use elgato_streamdeck::{AsyncStreamDeck, DeviceStateUpdate};

use crate::event::{DeviceInfo, DeviceKind, Event};

static DEVICES: LazyLock<RwLock<HashMap<String, AsyncStreamDeck>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

pub async fn set_button_image(device_id: &str, position: u8, img: image::DynamicImage) -> anyhow::Result<()> {
    if let Some(device) = DEVICES.read().await.get(device_id) {
        device.set_button_image(position, img).await?;
        device.flush().await?;
    }
    Ok(())
}

pub async fn clear_button(device_id: &str, position: u8) -> anyhow::Result<()> {
    if let Some(device) = DEVICES.read().await.get(device_id) {
        device.clear_button_image(position).await?;
        device.flush().await?;
    }
    Ok(())
}

pub async fn set_brightness(device_id: &str, brightness: u8) {
    if let Some(device) = DEVICES.read().await.get(device_id) {
        let _ = device.set_brightness(brightness.clamp(0, 100)).await;
        let _ = device.flush().await;
    }
}

pub async fn clear_all(device_id: &str) {
    if let Some(device) = DEVICES.read().await.get(device_id) {
        let _ = device.clear_all_button_images().await;
        let _ = device.flush().await;
    }
}

async fn run_device(device: AsyncStreamDeck, id: String, tx: broadcast::Sender<Event>) {
    if DEVICES.read().await.contains_key(&id) {
        return;
    }

    let name = device.product().await.unwrap_or_else(|_| "Unknown".into());
    let kind = device.kind();
    let reader = device.get_reader();

    DEVICES.write().await.insert(id.clone(), device);

    let _ = tx.send(Event::DeviceConnected(DeviceInfo {
        id: id.clone(),
        name,
        rows: kind.row_count(),
        columns: kind.column_count(),
        encoders: kind.encoder_count(),
        kind: DeviceKind::Elgato,
    }));

    loop {
        let updates = match reader.read(100.0).await {
            Ok(u) => u,
            Err(_) => break,
        };
        for update in updates {
            let event = match update {
                DeviceStateUpdate::ButtonDown(pos) =>
                    Some(Event::ButtonPress { device: id.clone(), position: pos as u16 }),
                DeviceStateUpdate::ButtonUp(pos) =>
                    Some(Event::ButtonRelease { device: id.clone(), position: pos as u16 }),
                DeviceStateUpdate::EncoderTwist(pos, ticks) =>
                    Some(Event::EncoderRotate { device: id.clone(), position: pos as u16, ticks: ticks as i32 }),
                DeviceStateUpdate::EncoderDown(pos) =>
                    Some(Event::EncoderPress { device: id.clone(), position: pos as u16 }),
                DeviceStateUpdate::EncoderUp(pos) =>
                    Some(Event::EncoderRelease { device: id.clone(), position: pos as u16 }),
                _ => None,
            };
            if let Some(ev) = event {
                let _ = tx.send(ev);
            }
        }
    }

    DEVICES.write().await.remove(&id);
    let _ = tx.send(Event::DeviceDisconnected(id));
}

/// Scan /dev/input/by-id for USB HID keyboard macro pads.
/// Returns (event_path, serial, name, rows, cols) for each new device found.
pub fn discover_hid_keyboards() -> Vec<(String, String, String, u8, u8)> {
    let by_id = std::path::Path::new("/dev/input/by-id");
    let Ok(entries) = std::fs::read_dir(by_id) else { return vec![] };
    let mut found = vec![];
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        // Skip devices handled by dedicated drivers (Ampligame D6 uses vendor HID)
        if name.contains("HOTSPOTEKUSB") { continue; }
        // Match USB HID keyboard macro pads (not mice, touchpads, etc.)
        if name.contains("event-kbd") && name.contains("usb") {
            if let Ok(real) = std::fs::canonicalize(entry.path()) {
                let path = real.to_string_lossy().to_string();
                // Extract a stable serial from the by-id name
                let serial = name
                    .trim_end_matches("-event-kbd")
                    .trim_start_matches("usb-")
                    .to_string();
                // Try to get device name from sysfs
                let display_name = extract_hid_name(&name).unwrap_or_else(|| serial.clone());
                // Default layout guess: 3×5 for 15-key pads, override via config later
                found.push((path, serial, display_name, 3u8, 5u8));
            }
        }
    }
    found
}

fn extract_hid_name(by_id_name: &str) -> Option<String> {
    // "usb-MANUFACTURER_PRODUCT_SERIAL-if01-event-kbd" → "MANUFACTURER PRODUCT"
    let stripped = by_id_name
        .strip_prefix("usb-")?
        .split("_C5")  // common serial prefix separator
        .next()?;
    Some(stripped.replace('_', " ").trim().to_string())
}

static HID_KBD_DEVICES: std::sync::LazyLock<std::sync::RwLock<std::collections::HashSet<String>>> =
    std::sync::LazyLock::new(|| std::sync::RwLock::new(std::collections::HashSet::new()));

fn run_hid_kbd(event_path: String, id: String, name: String, rows: u8, cols: u8, tx: broadcast::Sender<Event>) {
    // Open the file BEFORE announcing the device — permission failures stay silent
    let mut f = match std::fs::File::open(&event_path) {
        Ok(f) => f,
        Err(e) => {
            log::warn!("Cannot open {event_path}: {e}  (hint: add user to 'input' group)");
            return;
        }
    };

    HID_KBD_DEVICES.write().unwrap().insert(id.clone());
    log::info!("HID keyboard device: {name} at {event_path}");

    let _ = tx.send(Event::DeviceConnected(DeviceInfo {
        id: id.clone(),
        name: name.clone(),
        rows,
        columns: cols,
        encoders: 0,
        kind: DeviceKind::HidKeyboard { event_path: event_path.clone() },
    }));

    // evdev input_event on 64-bit Linux: sec(8) usec(8) type(2) code(2) value(4) = 24 bytes
    use std::io::Read;
    let mut buf = [0u8; 24];
    loop {
        match f.read_exact(&mut buf) {
            Err(_) => break,
            Ok(_) => {
                let typ  = u16::from_ne_bytes([buf[16], buf[17]]);
                let code = u16::from_ne_bytes([buf[18], buf[19]]);
                let val  = i32::from_ne_bytes([buf[20], buf[21], buf[22], buf[23]]);
                const EV_KEY: u16 = 1;
                match (typ, val) {
                    (EV_KEY, 1) => { let _ = tx.send(Event::ButtonPress   { device: id.clone(), position: code }); }
                    (EV_KEY, 0) => { let _ = tx.send(Event::ButtonRelease { device: id.clone(), position: code }); }
                    _ => {}
                }
            }
        }
    }

    HID_KBD_DEVICES.write().unwrap().remove(&id);
    let _ = tx.send(Event::DeviceDisconnected(id));
}

pub async fn discover(tx: broadcast::Sender<Event>) {
    let hid = match elgato_streamdeck::new_hidapi() {
        Ok(h) => Arc::new(h),
        Err(e) => {
            log::warn!("hidapi init failed: {e}");
            return;
        }
    };

    for (kind, serial) in elgato_streamdeck::asynchronous::list_devices_async(&hid) {
        let id = format!("sd-{serial}");
        if DEVICES.read().await.contains_key(&id) {
            continue;
        }
        match AsyncStreamDeck::connect(&hid, kind, &serial) {
            Ok(device) => {
                tokio::spawn(run_device(device, id, tx.clone()));
            }
            Err(e) => log::warn!("Failed to connect to {serial}: {e}"),
        }
    }

    // Scan for generic HID keyboard macro pads
    for (path, serial, name, rows, cols) in discover_hid_keyboards() {
        let id = format!("kbd-{serial}");
        if HID_KBD_DEVICES.read().unwrap().contains(&id) {
            continue;
        }
        let tx2 = tx.clone();
        std::thread::spawn(move || run_hid_kbd(path, id, name, rows, cols, tx2));
    }
}
