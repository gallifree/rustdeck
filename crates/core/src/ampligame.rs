/// Direct HID support for FIFINE Ampligame D6 (and compatible revisions).
/// Ported from https://github.com/3dRikal/opendeck-ampgd6 (MIT).
use std::collections::HashMap;
use std::sync::{Arc, LazyLock};
use tokio::sync::{RwLock, broadcast};
use tokio_util::sync::CancellationToken;

use mirajazz::{
    device::{Device, DeviceWatcher, DeviceQuery, list_devices},
    error::MirajazzError,
    state::DeviceStateUpdate,
    types::{DeviceLifecycleEvent, HidDeviceInfo, ImageFormat, ImageMirroring, ImageMode, ImageRotation},
};

use crate::event::{DeviceInfo as OurDeviceInfo, DeviceKind, Event};

// ── Constants ────────────────────────────────────────────────────────────────

const FIFINE_VID: u16 = 0x3142;
const AMPGD6_PID_V1: u16 = 0x0007;
const AMPGD6_PID_V2: u16 = 0x0060; // second hardware revision

const ROW_COUNT:     u8 = 3;
const COL_COUNT:     u8 = 5;
const KEY_COUNT:     usize = (ROW_COUNT * COL_COUNT) as usize;
const ENCODER_COUNT: usize = 0;

// HID usage page 0xFF60, usage id 1 — vendor interface
const QUERIES: [DeviceQuery; 2] = [
    DeviceQuery::new(65440, 1, FIFINE_VID, AMPGD6_PID_V1),
    DeviceQuery::new(65440, 1, FIFINE_VID, AMPGD6_PID_V2),
];

fn image_format() -> ImageFormat {
    ImageFormat {
        mode:     ImageMode::JPEG,
        size:     (112, 112), // frame probe: largest size that stays inside each button
        rotation: ImageRotation::Rot180,
        mirror:   ImageMirroring::None,
    }
}

// ── Index mappings ────────────────────────────────────────────────────────────

/// UI position → device button index (for image sending).
/// Firmware numbers buttons 0-14 bottom-left to top-right (bottom row first).
/// Probe confirmed: hw_key 0 = physical bottom-left = UI pos 10.
fn ui_to_device(key: u8) -> u8 {
    const MAP: [u8; KEY_COUNT] = [10, 11, 12, 13, 14, 5, 6, 7, 8, 9, 0, 1, 2, 3, 4];
    if (key as usize) < KEY_COUNT { MAP[key as usize] } else { key }
}

/// Device 1-based index → UI 0-based position (for button events).
fn device_to_ui(key: usize) -> usize {
    key.saturating_sub(1)
}

// ── Input parsing (called by mirajazz reader) ─────────────────────────────────

fn process_input(input: u8, state: u8) -> Result<mirajazz::types::DeviceInput, MirajazzError> {
    use mirajazz::types::DeviceInput;

    if input as usize > KEY_COUNT {
        return Err(MirajazzError::BadData);
    }

    let mut button_states = vec![0x01u8];
    button_states.extend(vec![0u8; KEY_COUNT + 1]);

    if input > 0 {
        let idx = device_to_ui(input as usize);
        if idx < KEY_COUNT {
            button_states[idx + 1] = state;
        }
    }

    let bools: Vec<bool> = (0..KEY_COUNT).map(|i| button_states[i + 1] != 0).collect();
    Ok(DeviceInput::ButtonStateChange(bools))
}

// ── Device ID ────────────────────────────────────────────────────────────────

fn device_id(dev: &HidDeviceInfo) -> Option<String> {
    if dev.vendor_id != FIFINE_VID { return None; }
    match dev.product_id {
        AMPGD6_PID_V1 => Some("ampgd6-v1-355499441494".into()), // all v1 share serial
        AMPGD6_PID_V2 => Some(format!(
            "ampgd6-v2-{}",
            dev.serial_number.clone().unwrap_or_else(|| "unknown".into())
        )),
        _ => None,
    }
}

// ── Active device registry ────────────────────────────────────────────────────

static ACTIVE: LazyLock<RwLock<HashMap<String, Arc<Device>>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

// ── Per-device task ───────────────────────────────────────────────────────────

fn protocol_version(info: &HidDeviceInfo) -> usize {
    match info.product_id {
        AMPGD6_PID_V1 => 1, // 512-byte packets, no per-report state
        // pv=3: 1024-byte packets + supports_both_states() → data[10] used as press/release state
        // Without pv=3, state is forced to 1 on every report, causing double ButtonDown per key
        _             => 3,
    }
}

async fn device_task(
    id: String,
    dev_info: HidDeviceInfo,
    tx: broadcast::Sender<Event>,
    token: CancellationToken,
) {
    let pv = protocol_version(&dev_info);
    log::info!("Ampligame: connecting to device id={id} protocol_version={pv}...");
    let device = match Device::connect(&dev_info, pv, KEY_COUNT, ENCODER_COUNT).await {
        Ok(d) => Arc::new(d),
        Err(e) => { log::warn!("Ampligame D6 connect failed: {e}"); return; }
    };
    log::info!("Ampligame: connected to {id}");

    // Best-effort init — do NOT call clear_all_button_images; CLR+STP corrupts device state
    let _ = device.set_brightness(50).await;

    // Register before announcing, so listeners can draw on it right away.
    ACTIVE.write().await.insert(id.clone(), device.clone());

    let _ = tx.send(Event::DeviceConnected(OurDeviceInfo {
        id: id.clone(),
        name: "FIFINE Ampligame D6".into(),
        rows: ROW_COUNT,
        columns: COL_COUNT,
        encoders: 0,
        kind: DeviceKind::Ampligame,
    }));

    let reader = device.get_reader(process_input);

    loop {
        let updates = tokio::select! {
            res = reader.read(None) => match res {
                Ok(u) => u,
                Err(e) => {
                    log::warn!("Ampligame read error: {e}");
                    break;
                }
            },
            _ = token.cancelled() => break,
        };

        for update in updates {
            let ev = match update {
                DeviceStateUpdate::ButtonDown(k) =>
                    Some(Event::ButtonPress   { device: id.clone(), position: k as u16 }),
                DeviceStateUpdate::ButtonUp(k) =>
                    Some(Event::ButtonRelease { device: id.clone(), position: k as u16 }),
                _ => None,
            };
            if let Some(ev) = ev { let _ = tx.send(ev); }
        }
    }

    ACTIVE.write().await.remove(&id);
    let _ = device.shutdown().await;
    let _ = tx.send(Event::DeviceDisconnected(id));
}

// ── Public: set image ─────────────────────────────────────────────────────────

/// Send one button image using the correct PV3 protocol:
///   BAT header → JPEG chunks → STP (per key) → PIXMAPEND → STP (commit)
async fn send_key_image(dev: &Device, hw_key: u8, jpeg: &[u8]) -> Result<(), MirajazzError> {
    const PACKET: usize = 1024;

    // BAT command: CRT\0\0 BAT\0\0 <len_hi> <len_lo> <slot>
    let mut hdr = vec![
        0x00,                                           // HID report id
        0x43, 0x52, 0x54, 0x00, 0x00,                 // CRT\0\0
        0x42, 0x41, 0x54, 0x00, 0x00,                 // BAT\0\0
        (jpeg.len() >> 8) as u8, jpeg.len() as u8,    // length big-endian
        hw_key + 1,                                    // 1-indexed slot
    ];
    dev.write_extended_data(&mut hdr).await?;

    // JPEG data in 1024-byte chunks
    let mut offset = 0;
    while offset < jpeg.len() {
        let n = (jpeg.len() - offset).min(PACKET);
        let mut buf = vec![0x00]; // HID report id
        buf.extend_from_slice(&jpeg[offset..offset + n]);
        buf.resize(PACKET + 1, 0);
        dev.write_data(&buf).await?;
        offset += n;
    }

    // STP after this key
    let mut stp = vec![0x00, 0x43, 0x52, 0x54, 0x00, 0x00, 0x53, 0x54, 0x50];
    dev.write_extended_data(&mut stp).await?;

    // PIXMAPEND + STP to commit all buffered images to screen
    let mut pme = vec![
        0x00,
        0x43, 0x52, 0x54, 0x00, 0x00,
        0x50, 0x49, 0x58, 0x4d, 0x41, 0x50, 0x45, 0x4e, 0x44, // PIXMAPEND
    ];
    dev.write_extended_data(&mut pme).await?;
    let mut stp2 = vec![0x00, 0x43, 0x52, 0x54, 0x00, 0x00, 0x53, 0x54, 0x50];
    dev.write_extended_data(&mut stp2).await?;

    Ok(())
}

pub async fn set_button_image(device_id: &str, position: u8, img: image::DynamicImage) {
    let guard = ACTIVE.read().await;
    let Some(dev) = guard.get(device_id) else {
        log::warn!("set_button_image: device {device_id} not in ACTIVE");
        return;
    };
    let hw_key = ui_to_device(position);
    log::info!("set_button_image: pos={position} hw_key={hw_key} device={device_id}");

    let jpeg = match mirajazz::images::convert_image_with_format(image_format(), img).await {
        Ok(j) => j,
        Err(e) => { log::warn!("image encode failed: {e}"); return; }
    };
    if let Err(e) = send_key_image(dev, hw_key, &jpeg).await {
        log::warn!("send_key_image failed: {e}");
    } else {
        log::info!("set_button_image: ok ({}B JPEG)", jpeg.len());
    }
}

pub async fn set_brightness(device_id: &str, brightness: u8) {
    let guard = ACTIVE.read().await;
    let Some(dev) = guard.get(device_id) else { return };
    let _ = dev.set_brightness(brightness).await;
}

// ── Public: discovery + watcher ───────────────────────────────────────────────

pub async fn discover_and_watch(tx: broadcast::Sender<Event>) {
    log::info!("Ampligame: scanning for devices...");
    let initial = match list_devices(&QUERIES).await {
        Ok(devs) => devs,
        Err(e) => { log::warn!("Ampligame list_devices failed: {e}"); return; }
    };
    log::info!("Ampligame: list_devices returned {} candidate(s)", initial.len());

    let mut tokens: HashMap<String, CancellationToken> = HashMap::new();

    for hid_dev in initial {
        // HidDevice derefs to HidDeviceInfo for the id check, then consume it into DeviceInfo
        let info: HidDeviceInfo = hid_dev.to_device_info();
        let Some(id) = device_id(&info) else { continue };
        // Use tokens as "already-handling" guard — ACTIVE isn't populated until device_task runs
        if tokens.contains_key(&id) || ACTIVE.read().await.contains_key(&id) { continue; }
        let token = CancellationToken::new();
        tokens.insert(id.clone(), token.clone());
        tokio::spawn(device_task(id, info, tx.clone(), token));
    }

    // Hot-plug watcher
    let mut watcher = DeviceWatcher::new();
    let stream = match watcher.watch(&QUERIES).await {
        Ok(s) => s,
        Err(e) => { log::warn!("Ampligame watcher failed: {e}"); return; }
    };

    use futures_lite::StreamExt;
    tokio::pin!(stream);

    while let Some(ev) = stream.next().await {
        match ev {
            DeviceLifecycleEvent::Connected(info) => {
                let Some(id) = device_id(&info) else { continue };
                if tokens.contains_key(&id) || ACTIVE.read().await.contains_key(&id) { continue; }
                let token = CancellationToken::new();
                tokens.insert(id.clone(), token.clone());
                tokio::spawn(device_task(id, info, tx.clone(), token));
            }
            DeviceLifecycleEvent::Disconnected(info) => {
                let Some(id) = device_id(&info) else { continue };
                // Cancel the task; device_task removes itself from ACTIVE on exit
                if let Some(t) = tokens.remove(&id) { t.cancel(); }
            }
        }
    }
}
