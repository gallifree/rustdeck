/// Developer tool (not shipped): paints test patterns on an Ampligame D6 to find its display geometry.
/// Usage: rustdeck-probe [icon.png] [sizes...] | rustdeck-probe frame [sizes...]
use mirajazz::{
    device::{Device, DeviceQuery, list_devices},
    types::{ImageFormat, ImageMirroring, ImageMode, ImageRotation},
};
use image::{DynamicImage, RgbImage};

const QUERIES: [DeviceQuery; 2] = [
    DeviceQuery::new(65440, 1, 0x3142, 0x0007),
    DeviceQuery::new(65440, 1, 0x3142, 0x0060),
];


fn solid(r: u8, g: u8, b: u8, size: u32) -> DynamicImage {
    let img = RgbImage::from_fn(size, size, |_, _| image::Rgb([r, g, b]));
    DynamicImage::ImageRgb8(img)
}

/// Full-device diagonal rainbow gradient (top-left → bottom-right), 5×3 buttons wide/tall.
/// Returns one sub-image per hw_key, pre-cropped to the key's physical position.
fn rainbow_crops(size: u32) -> Vec<DynamicImage> {
    // hw_key → UI position → (row, col)
    // MAP (ui_to_device): pos 0..14 → hw_keys [10,11,12,13,14,5,6,7,8,9,0,1,2,3,4]
    // Inverse: hw_key K → UI pos:
    const HW_TO_POS: [u32; 15] = [10,11,12,13,14, 5,6,7,8,9, 0,1,2,3,4];

    let cols = 5u32;
    let rows = 3u32;
    let w = cols * size;
    let h = rows * size;

    // Generate full-device gradient
    let full = RgbImage::from_fn(w, h, |x, y| {
        let t = (x as f32 + y as f32) / ((w + h - 2) as f32);
        let hue = t * 360.0;
        let (r, g, b) = hsv_to_rgb(hue, 1.0, 1.0);
        image::Rgb([r, g, b])
    });
    let full = DynamicImage::ImageRgb8(full);

    (0u32..15).map(|key| {
        let pos  = HW_TO_POS[key as usize];
        let row  = pos / cols;
        let col  = pos % cols;
        full.crop_imm(col * size, row * size, size, size)
    }).collect()
}

fn hsv_to_rgb(h: f32, s: f32, v: f32) -> (u8, u8, u8) {
    let c = v * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = v - c;
    let (r1, g1, b1) = match h as u32 {
        0..=59   => (c, x, 0.0),
        60..=119 => (x, c, 0.0),
        120..=179 => (0.0, c, x),
        180..=239 => (0.0, x, c),
        240..=299 => (x, 0.0, c),
        _         => (c, 0.0, x),
    };
    (((r1 + m) * 255.0) as u8, ((g1 + m) * 255.0) as u8, ((b1 + m) * 255.0) as u8)
}

/// Black square with a 4px white border, plus a red 8×8 marker in the visual top-left corner.
fn frame(w: u32, h: u32) -> DynamicImage {
    let img = RgbImage::from_fn(w, h, |x, y| {
        if x < 8 && y < 8 { image::Rgb([255, 0, 0]) }
        else if x < 4 || y < 4 || x >= w - 4 || y >= h - 4 { image::Rgb([255, 255, 255]) }
        else { image::Rgb([0, 0, 0]) }
    });
    DynamicImage::ImageRgb8(img)
}

async fn frame_test(dev: &Device, sizes: &[u32]) {
    // UI pos 0 (top-left) → hw 10, UI pos 7 (middle) → hw 7
    for &s in sizes {
        for key in 0u8..15 {
            dev.set_button_image(key, fmt(144), solid(0, 0, 0, 144)).await.ok();
        }
        dev.flush().await.ok();
        for key in [10u8, 7] {
            dev.set_button_image(key, fmt(s), frame(s, s)).await.ok();
        }
        dev.flush().await.ok();
        println!("--- frame {s}×{s} on top-left key and middle key ---");
        println!("Which white edges are visible INSIDE the button (top/bottom/left/right)? Red square in which corner?");
        println!("Press Enter for next size...");
        let mut line = String::new();
        std::io::stdin().read_line(&mut line).ok();
    }
}

fn resized(src: &DynamicImage, size: u32) -> DynamicImage {
    src.resize_exact(size, size, image::imageops::FilterType::Lanczos3)
}

fn fmt(size: u32) -> ImageFormat {
    ImageFormat {
        mode:     ImageMode::JPEG,
        size:     (size as usize, size as usize),
        rotation: ImageRotation::Rot180,
        mirror:   ImageMirroring::None,
    }
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.first().map(|a| a == "frame").unwrap_or(false) {
        let sizes: Vec<u32> = args[1..].iter().filter_map(|a| a.parse().ok()).collect();
        let sizes = if sizes.is_empty() { vec![144, 128, 120, 112, 104, 100, 96] } else { sizes };
        for hid_dev in list_devices(&QUERIES).await.expect("list_devices") {
            let dev = Device::connect(&hid_dev.to_device_info(), 3, 15, 0).await.expect("connect");
            frame_test(&dev, &sizes).await;
        }
        return;
    }

    // First arg may be an image path; remaining args (or all if first not a path) are sizes
    let (icon, size_args) = if args.first().map(|a| std::path::Path::new(a).exists()).unwrap_or(false) {
        let img = image::open(&args[0]).expect("Cannot open icon");
        (Some(img), args[1..].to_vec())
    } else {
        (None, args)
    };

    let sizes: Vec<u32> = size_args.iter().filter_map(|a| a.parse().ok()).collect();
    // Default: 110→160 in 2px steps
    let sizes = if sizes.is_empty() { (0u32..=25).map(|i| 110 + i * 2).collect() } else { sizes };

    let devs = list_devices(&QUERIES).await.expect("list_devices");
    for hid_dev in devs {
        let info = hid_dev.to_device_info();
        let dev = Device::connect(&info, 3, 15, 0).await.expect("connect");

        for size in &sizes {
            // Clear all keys to black before each test
            for key in 0u8..15 {
                dev.set_button_image(key, fmt(*size), solid(0, 0, 0, *size)).await.ok();
            }
            dev.flush().await.ok();

            match &icon {
                Some(src) => {
                    println!("--- size {size}×{size}: icon on all 15 keys ---");
                    println!("Look for: icon centered, no black gap, not cropped.");
                    let img = resized(src, *size);
                    for key in 0u8..15 {
                        dev.set_button_image(key, fmt(*size), img.clone()).await.ok();
                    }
                }
                None => {
                    println!("--- size {size}×{size}: rainbow gradient across full device ---");
                    println!("Look for: smooth gradient flowing across all 15 keys, no visible seams.");
                    let crops = rainbow_crops(*size);
                    for (key, crop) in crops.into_iter().enumerate() {
                        dev.set_button_image(key as u8, fmt(*size), crop).await.ok();
                    }
                }
            }
            dev.flush().await.ok();
            println!("Painted. Press Enter for next size...");
            let mut line = String::new();
            std::io::stdin().read_line(&mut line).ok();
        }
    }
}
