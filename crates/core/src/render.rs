use image::{DynamicImage, Rgb, RgbImage};

use crate::ampligame;
use crate::config::{PageNav, Profile};
use crate::device;
use crate::event::{DeviceInfo, DeviceKind};

const SIZE: u32 = 144;

fn black() -> DynamicImage {
    DynamicImage::ImageRgb8(RgbImage::new(SIZE, SIZE))
}

/// Composite onto black — JPEG encoding drops alpha, leaving garbage under transparent pixels.
fn flatten(img: DynamicImage) -> DynamicImage {
    let rgba = img.to_rgba8();
    let out = RgbImage::from_fn(rgba.width(), rgba.height(), |x, y| {
        let p = rgba.get_pixel(x, y);
        let a = p[3] as u16;
        Rgb([(p[0] as u16 * a / 255) as u8, (p[1] as u16 * a / 255) as u8, (p[2] as u16 * a / 255) as u8])
    });
    DynamicImage::ImageRgb8(out)
}

fn nav_icon(nav: PageNav, page: usize, pages: usize) -> DynamicImage {
    let (tip, base) = match nav { PageNav::Prev => (40i32, 100i32), PageNav::Next => (104, 44) };
    let mid = 64i32;
    let half = 34i32;
    let span = (base - tip).abs();
    let img = RgbImage::from_fn(SIZE, SIZE, |x, y| {
        let (x, y) = (x as i32, y as i32);
        let d = (x - tip) * (base - tip).signum();
        let in_arrow = d >= 0 && d <= span && (y - mid).abs() * span <= d * half;
        if in_arrow { Rgb([230, 230, 230]) } else { Rgb([35, 35, 35]) }
    });
    // Page dots along the bottom: current page highlighted.
    let mut img = img;
    let dot = 8u32;
    let gap = 6u32;
    let shown = pages.min(8) as u32;
    let total = shown * dot + shown.saturating_sub(1) * gap;
    let x0 = (SIZE - total) / 2;
    for i in 0..shown {
        let color = if i as usize == page.min(7) { Rgb([255, 170, 0]) } else { Rgb([110, 110, 110]) };
        for dx in 0..dot {
            for dy in 0..dot {
                img.put_pixel(x0 + i * (dot + gap) + dx, 118 + dy, color);
            }
        }
    }
    DynamicImage::ImageRgb8(img)
}

fn button_image(profile: &Profile, page: usize, pos: u8) -> Option<DynamicImage> {
    if let Some(nav) = profile.nav_at(pos) {
        return Some(nav_icon(nav, page, profile.pages.len()));
    }
    let path = profile.button(page, pos)?.icon.as_ref()?;
    match image::open(path) {
        Ok(img) => Some(flatten(img)),
        Err(e) => { log::warn!("icon {}: {e}", path.display()); None }
    }
}

/// Draw every key of `page` on `dev`; keys without an icon are blanked.
pub async fn render_page(dev: &DeviceInfo, profile: &Profile, page: usize) {
    for pos in 0..dev.rows * dev.columns {
        let img = button_image(profile, page, pos);
        match dev.kind {
            DeviceKind::Ampligame => ampligame::set_button_image(&dev.id, pos, img.unwrap_or_else(black)).await,
            DeviceKind::Elgato => {
                let _ = match img {
                    Some(img) => device::set_button_image(&dev.id, pos, img).await,
                    None => device::clear_button(&dev.id, pos).await,
                };
            }
            DeviceKind::HidKeyboard { .. } => return,
        }
    }
}
