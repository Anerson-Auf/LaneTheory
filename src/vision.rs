//! One-shot, local-only draft vision.
//!
//! This module never reads Dota memory, injects code, sends input, or starts a
//! capture loop. The caller explicitly requests one Dota client frame during
//! hero selection; it is compared with cached public hero portraits and discarded.

use crate::models::HeroData;
use image::{DynamicImage, GenericImageView};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

const PORTRAIT_CACHE: &str = "cache/vision_portraits";
// 16×9 discards the face/silhouette that separates visually similar heroes.
// Vision is a one-shot action, so a 32×18 descriptor is still inexpensive.
const GRID_W: u32 = 32;
const GRID_H: u32 = 18;
// Dota's top player cards are not a 120 px grid at 1920: the right bank has
// a measured 122 px pitch. Keeping the coordinates explicit prevents a
// growing leftward error on slots 2–5.
const SLOT_CENTRES: [f32; 10] = [
    0.139, 0.201, 0.263, 0.325, 0.387,
    0.603, 0.6665, 0.7300, 0.7935, 0.8570,
];
// The visible player-card artwork is ~75 px tall at a 1080 px client.  The
// former 58 px crop cut off the lower part of every hero portrait.
const TOP_CARD_ART_HEIGHT: f32 = 0.069;
// `similarity` is a normalized image-distance score, not a calibrated
// probability.  Real Dota HUD cards against the public portrait assets land
// around 0.40–0.55, so treating 0.73 as "73% certain" made every valid match
// look uncertain and silently disabled the feature.
const AUTO_ACCEPT_SCORE: f32 = 0.52;
const AUTO_ACCEPT_MARGIN: f32 = 0.025;
const DRAFT_CARD_ASPECT: f32 = 1.39;

#[derive(Debug, Clone)]
pub struct VisionCandidate {
    pub hero_name: String,
    pub localized_name: String,
    pub confidence: f32,
    /// Difference from the next closest portrait template. A raw image score
    /// alone is not enough to safely auto-apply similar blue portraits.
    pub margin: f32,
    pub slot: usize,
    pub is_enemy: bool,
}

#[derive(Debug, Clone)]
pub struct VisionResult {
    pub candidates: Vec<VisionCandidate>,
    pub auto_accepted_enemies: Vec<String>,
    pub auto_accepted_allies: Vec<String>,
    pub status: String,
}

#[derive(Clone)]
struct PortraitTemplate {
    hero_name: String,
    localized_name: String,
    descriptor: Vec<f32>,
}

/// Capture exactly one frame and classify the five portrait slots of the enemy
/// bank. `enemy_is_right` is obtained from the player's GSI team field.
pub async fn scan_enemy_draft(
    heroes: Vec<HeroData>,
    enemy_is_right: bool,
) -> VisionResult {
    if heroes.is_empty() {
        return VisionResult { candidates: Vec::new(), auto_accepted_enemies: Vec::new(), auto_accepted_allies: Vec::new(), status: "Vision: каталог героев ещё загружается".into() };
    }

    if let Err(error) = ensure_portraits(&heroes).await {
        return VisionResult { candidates: Vec::new(), auto_accepted_enemies: Vec::new(), auto_accepted_allies: Vec::new(), status: format!("Vision: не подготовлены портреты ({error})") };
    }
    let templates = load_templates(&heroes);
    if templates.len() < 100 {
        return VisionResult { candidates: Vec::new(), auto_accepted_enemies: Vec::new(), auto_accepted_allies: Vec::new(), status: "Vision: кэш портретов неполный; повтори scan после загрузки".into() };
    }

    let frame = match capture_dota_client() {
        Ok(frame) => frame,
        Err(error) => return VisionResult { candidates: Vec::new(), auto_accepted_enemies: Vec::new(), auto_accepted_allies: Vec::new(), status: format!("Vision: не удалось снять кадр Dota ({error})") },
    };

    let mut candidates = Vec::new();
    let enemy_slots = if enemy_is_right { [5, 6, 7, 8, 9] } else { [0, 1, 2, 3, 4] };
    let ally_slots = if enemy_is_right { [0, 1, 2, 3, 4] } else { [5, 6, 7, 8, 9] };
    let mut already_seen = HashSet::new();
    for (is_enemy, slots) in [(true, enemy_slots), (false, ally_slots)] {
    for slot in slots {
        let sample = slot_descriptor(&frame, slot);
        let Some(sample) = sample else { continue };
        let mut best: Option<(&PortraitTemplate, f32)> = None;
        let mut runner_up = 0.0_f32;
        for template in &templates {
            if already_seen.contains(&template.hero_name) { continue; }
            let score = similarity(&sample, &template.descriptor);
            if best.as_ref().is_none_or(|(_, old)| score > *old) {
                runner_up = best.map(|(_, old)| old).unwrap_or(0.0);
                best = Some((template, score));
            } else if score > runner_up {
                runner_up = score;
            }
        }
        if let Some((template, confidence)) = best {
            // Always expose the best candidate. A low score is still valuable
            // diagnostics and lets the player correct it in the draft picker;
            // only cards above the empirical image-match floor are applied.
            already_seen.insert(template.hero_name.clone());
            candidates.push(VisionCandidate {
                hero_name: template.hero_name.clone(),
                localized_name: template.localized_name.clone(),
                confidence,
                margin: (confidence - runner_up).max(0.0),
                slot,
                is_enemy,
            });
        }
    }
    }
    // Similar portraits (notably Crystal Maiden/Naga) may produce a usable
    // score but virtually no lead over the runner-up. Keep those visible for
    // correction instead of silently making a false draft decision.
    let is_reliable = |candidate: &VisionCandidate| {
        candidate.confidence >= AUTO_ACCEPT_SCORE && candidate.margin >= AUTO_ACCEPT_MARGIN
    };
    let auto_accepted_enemies = candidates.iter().filter(|candidate| candidate.is_enemy && is_reliable(candidate))
        .map(|candidate| candidate.hero_name.clone()).collect::<Vec<_>>();
    let auto_accepted_allies = candidates.iter().filter(|candidate| !candidate.is_enemy && is_reliable(candidate))
        .map(|candidate| candidate.hero_name.clone()).collect::<Vec<_>>();
    let accepted_enemies_count = auto_accepted_enemies.len();
    let accepted_allies_count = auto_accepted_allies.len();
    let accepted_count = accepted_enemies_count + accepted_allies_count;
    let uncertain = candidates.len().saturating_sub(accepted_count);
    let side = if enemy_is_right { "справа" } else { "слева" };
    // Exactly one file, overwritten only by an explicit F10 scan. Keeping it
    // even after a successful match makes calibration inspectable; previously
    // a successful scan left an older, misleading debug image on disk.
    let debug_saved = save_debug_frame(&frame).is_ok();
    VisionResult {
        candidates,
        auto_accepted_enemies,
        auto_accepted_allies,
        status: format!(
            "Vision: враги {}, союзники {}, проверить {} · враги {side}{}",
            accepted_enemies_count,
            accepted_allies_count,
            uncertain,
            if debug_saved { " · debug: cache/vision_last_scan.png" } else { "" },
        ),
    }
}

async fn ensure_portraits(heroes: &[HeroData]) -> Result<(), String> {
    fs::create_dir_all(PORTRAIT_CACHE).map_err(|error| error.to_string())?;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .http1_only()
        .gzip(true)
        .user_agent("DotaAssistant/Vision")
        .build()
        .map_err(|error| error.to_string())?;

    // Bounded parallelism: this is an on-demand cache fill, never a constant
    // network load or a burst that competes with the game for bandwidth.
    let mut pending = tokio::task::JoinSet::new();
    let mut failures = 0usize;
    for hero in heroes {
        let path = portrait_path(&hero.name);
        if path.exists() { continue; }
        let url = hero.image_url();
        let client = client.clone();
        pending.spawn(async move {
            let bytes = client.get(url).send().await.map_err(|error| error.to_string())?
                .error_for_status().map_err(|error| error.to_string())?
                .bytes().await.map_err(|error| error.to_string())?;
            fs::write(path, bytes).map_err(|error| error.to_string())
        });
        if pending.len() >= 6 {
            if !matches!(pending.join_next().await, Some(Ok(Ok(())))) { failures += 1; }
        }
    }
    while let Some(result) = pending.join_next().await {
        if !matches!(result, Ok(Ok(()))) { failures += 1; }
    }
    if failures > 0 && !Path::new(PORTRAIT_CACHE).read_dir().map(|items| items.count() > 100).unwrap_or(false) {
        return Err(format!("ошибок загрузки: {failures}; проверь VPN"));
    }
    Ok(())
}

fn portrait_path(hero_name: &str) -> PathBuf {
    let clean = hero_name.strip_prefix("npc_dota_hero_").unwrap_or(hero_name);
    Path::new(PORTRAIT_CACHE).join(format!("{clean}.png"))
}

fn load_templates(heroes: &[HeroData]) -> Vec<PortraitTemplate> {
    heroes.iter().filter_map(|hero| {
        let image = image::open(portrait_path(&hero.name)).ok()?;
        Some(PortraitTemplate {
            hero_name: hero.name.clone(),
            localized_name: hero.localized_name.clone(),
            descriptor: image_descriptor(&image),
        })
    }).collect()
}

fn image_descriptor(image: &DynamicImage) -> Vec<f32> {
    let (width, height) = image.dimensions();
    let rgba = image.to_rgba8();
    // The top draft card is rendered with background-size: cover. Its visible
    // 1.39:1 rectangle is a centre crop of the public 16:9 portrait, not a
    // squashed full image. Comparing the full template to that crop is why
    // the old matcher preferred heroes with merely similar colours.
    let source_aspect = width as f32 / height.max(1) as f32;
    let crop_width = if source_aspect > DRAFT_CARD_ASPECT {
        (height as f32 * DRAFT_CARD_ASPECT).round() as u32
    } else {
        width
    }.clamp(1, width);
    let crop_height = if source_aspect < DRAFT_CARD_ASPECT {
        (width as f32 / DRAFT_CARD_ASPECT).round() as u32
    } else {
        height
    }.clamp(1, height);
    let offset_x = (width - crop_width) / 2;
    let offset_y = (height - crop_height) / 2;
    descriptor_from_pixels(crop_width, crop_height, |x, y| {
        let pixel = rgba.get_pixel(offset_x + x, offset_y + y).0;
        [pixel[0], pixel[1], pixel[2]]
    })
}

struct DesktopFrame {
    width: u32,
    height: u32,
    /// BGRA, top-to-bottom, captured once from the desktop compositor.
    pixels: Vec<u8>,
}

fn slot_descriptor(frame: &DesktopFrame, slot: usize) -> Option<Vec<f32>> {
    // Hero portrait centres in Dota's draft HUD, normalized to the Dota client.
    // The crop is deliberately inset to ignore card borders and player names.
    // Values are normalized to Dota's client area, not desktop resolution.
    let centre_x = *SLOT_CENTRES.get(slot)?;
    let crop_w = (frame.width as f32 * 0.054) as u32;
    let crop_h = (frame.height as f32 * TOP_CARD_ART_HEIGHT) as u32;
    let x = ((frame.width as f32 * centre_x) as i32 - crop_w as i32 / 2).max(0) as u32;
    let y = 0;
    if x + crop_w >= frame.width || y + crop_h >= frame.height || crop_w < 16 || crop_h < 16 { return None; }
    Some(descriptor_from_pixels(crop_w, crop_h, |cx, cy| {
        let index = (((y + cy) * frame.width + (x + cx)) * 4) as usize;
        // DIB is BGRA; descriptor uses RGB like the portrait templates.
        [frame.pixels[index + 2], frame.pixels[index + 1], frame.pixels[index]]
    }))
}

/// One overwrite-only debug artifact for UI-scale calibration. It is written
/// only by an explicit scan; there is no capture loop or recording.
fn save_debug_frame(frame: &DesktopFrame) -> Result<(), String> {
    let mut rgba = Vec::with_capacity(frame.pixels.len());
    for pixel in frame.pixels.chunks_exact(4) {
        rgba.extend_from_slice(&[pixel[2], pixel[1], pixel[0], 255]);
    }
    let mut image = image::RgbaImage::from_raw(frame.width, frame.height, rgba)
        .ok_or_else(|| "не удалось собрать debug frame".to_string())?;
    for slot in 0..10 {
        let width = (frame.width as f32 * 0.054) as u32;
        let height = (frame.height as f32 * TOP_CARD_ART_HEIGHT) as u32;
        let left = ((frame.width as f32 * SLOT_CENTRES[slot]) as i32 - width as i32 / 2).max(0) as u32;
        draw_box(&mut image, left, 0, width, height, if slot < 5 { [80, 170, 255, 255] } else { [255, 95, 95, 255] });
    }
    fs::create_dir_all("cache").map_err(|error| error.to_string())?;
    image.save("cache/vision_last_scan.png").map_err(|error| error.to_string())
}

fn draw_box(image: &mut image::RgbaImage, left: u32, top: u32, width: u32, height: u32, color: [u8; 4]) {
    let right = (left + width).min(image.width().saturating_sub(1));
    let bottom = (top + height).min(image.height().saturating_sub(1));
    for x in left..=right {
        image.put_pixel(x, top, image::Rgba(color));
        image.put_pixel(x, bottom, image::Rgba(color));
    }
    for y in top..=bottom {
        image.put_pixel(left, y, image::Rgba(color));
        image.put_pixel(right, y, image::Rgba(color));
    }
}

fn descriptor_from_pixels<F>(width: u32, height: u32, mut pixel_at: F) -> Vec<f32>
where F: FnMut(u32, u32) -> [u8; 3] {
    let mut colors = Vec::with_capacity((GRID_W * GRID_H * 3) as usize);
    let mut luma = vec![0.0_f32; (GRID_W * GRID_H) as usize];
    for gy in 0..GRID_H {
        for gx in 0..GRID_W {
            let x = ((gx as f32 + 0.5) * width as f32 / GRID_W as f32) as u32;
            let y = ((gy as f32 + 0.5) * height as f32 / GRID_H as f32) as u32;
            let rgb = pixel_at(x.min(width - 1), y.min(height - 1));
            let rgb = rgb.map(|value| value as f32 / 255.0);
            colors.extend(rgb);
            luma[(gy * GRID_W + gx) as usize] = 0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2];
        }
    }
    // Colour alone confuses similarly coloured heroes (e.g. Crystal Maiden
    // and Naga). Append a coarse luminance-gradient map, which preserves face,
    // staff and silhouette structure across the two render resolutions.
    let mut values = colors;
    for gy in 0..GRID_H {
        for gx in 0..GRID_W {
            let here = luma[(gy * GRID_W + gx) as usize];
            let right = luma[(gy * GRID_W + (gx + 1).min(GRID_W - 1)) as usize];
            let below = luma[((gy + 1).min(GRID_H - 1) * GRID_W + gx) as usize];
            values.push(((right - here).abs() + (below - here).abs()).min(1.0));
        }
    }
    let mean = values.iter().sum::<f32>() / values.len() as f32;
    let variance = values.iter().map(|value| (value - mean).powi(2)).sum::<f32>() / values.len() as f32;
    let scale = variance.sqrt().max(0.06);
    values.into_iter().map(|value| (value - mean) / scale).collect()
}

fn similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() { return 0.0; }
    let mse = a.iter().zip(b).map(|(left, right)| (left - right).powi(2)).sum::<f32>() / a.len() as f32;
    // 1.0 = identical normalized portraits, lower values = weak match.
    (1.0 / (1.0 + mse)).clamp(0.0, 1.0)
}

/// Capture the Dota client, not the virtual desktop.  A virtual-desktop frame
/// makes normalized draft coordinates wrong as soon as a second monitor is
/// connected (and can accidentally classify a portrait in another app).
fn capture_dota_client() -> Result<DesktopFrame, String> {
    const SRCCOPY: u32 = 0x00CC0020;
    const BI_RGB: u32 = 0;

    unsafe {
        let dota = find_dota_window().ok_or_else(|| {
            "окно Dota 2 не найдено; открой экран выбора героя и нажми F10".to_string()
        })?;
        let mut client = RECT::default();
        if GetClientRect(dota, &mut client) == 0 {
            return Err("не удалось получить область окна Dota 2".into());
        }
        let width = client.right - client.left;
        let height = client.bottom - client.top;
        let mut origin = POINT { x: 0, y: 0 };
        if ClientToScreen(dota, &mut origin) == 0 || width <= 0 || height <= 0 {
            return Err("область окна Dota 2 пуста".into());
        }
        let screen = GetDC(std::ptr::null_mut());
        if screen.is_null() { return Err("GetDC вернул пустой контекст".into()); }
        let memory = CreateCompatibleDC(screen);
        if memory.is_null() { ReleaseDC(std::ptr::null_mut(), screen); return Err("CreateCompatibleDC не удался".into()); }
        let mut info = BITMAPINFO {
            bmi_header: BITMAPINFOHEADER {
                bi_size: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                bi_width: width,
                bi_height: -height, // top-down DIB
                bi_planes: 1,
                bi_bit_count: 32,
                bi_compression: BI_RGB,
                bi_size_image: 0,
                bi_x_pels_per_meter: 0,
                bi_y_pels_per_meter: 0,
                bi_clr_used: 0,
                bi_clr_important: 0,
            },
            bmi_colors: [0; 3],
        };
        let mut raw = std::ptr::null_mut();
        let bitmap = CreateDIBSection(screen, &mut info, 0, &mut raw, std::ptr::null_mut(), 0);
        if bitmap.is_null() || raw.is_null() {
            DeleteDC(memory); ReleaseDC(std::ptr::null_mut(), screen);
            return Err("CreateDIBSection не удался".into());
        }
        let previous = SelectObject(memory, bitmap);
        let copied = BitBlt(memory, 0, 0, width, height, screen, origin.x, origin.y, SRCCOPY);
        let len = width as usize * height as usize * 4;
        let pixels = if copied != 0 { std::slice::from_raw_parts(raw as *const u8, len).to_vec() } else { Vec::new() };
        SelectObject(memory, previous);
        DeleteObject(bitmap);
        DeleteDC(memory);
        ReleaseDC(std::ptr::null_mut(), screen);
        if pixels.is_empty() { return Err("BitBlt не получил кадр; используй borderless windowed".into()); }
        Ok(DesktopFrame { width: width as u32, height: height as u32, pixels })
    }
}

fn find_dota_window() -> Option<*mut std::ffi::c_void> {
    // Current Windows builds of Dota use SDL_app.  The title fallback covers
    // configurations where SDL changes its window class.
    unsafe {
        let sdl_class = wide("SDL_app");
        let by_class = FindWindowW(sdl_class.as_ptr(), std::ptr::null());
        if !by_class.is_null() && window_title_contains(by_class, "Dota 2") {
            return Some(by_class);
        }
        let title = wide("Dota 2");
        let by_title = FindWindowW(std::ptr::null(), title.as_ptr());
        (!by_title.is_null()).then_some(by_title)
    }
}

fn window_title_contains(window: *mut std::ffi::c_void, expected: &str) -> bool {
    let mut title = [0u16; 256];
    let len = unsafe { GetWindowTextW(window, title.as_mut_ptr(), title.len() as i32) };
    len > 0 && String::from_utf16_lossy(&title[..len as usize]).contains(expected)
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

#[repr(C)]
struct BITMAPINFOHEADER {
    bi_size: u32, bi_width: i32, bi_height: i32, bi_planes: u16, bi_bit_count: u16,
    bi_compression: u32, bi_size_image: u32, bi_x_pels_per_meter: i32, bi_y_pels_per_meter: i32,
    bi_clr_used: u32, bi_clr_important: u32,
}

#[repr(C)]
struct BITMAPINFO { bmi_header: BITMAPINFOHEADER, bmi_colors: [u32; 3] }

#[repr(C)]
#[derive(Default)]
struct RECT { left: i32, top: i32, right: i32, bottom: i32 }

#[repr(C)]
struct POINT { x: i32, y: i32 }

#[link(name = "user32")]
unsafe extern "system" {
    fn FindWindowW(class_name: *const u16, window_name: *const u16) -> *mut std::ffi::c_void;
    fn GetWindowTextW(window: *mut std::ffi::c_void, text: *mut u16, max_count: i32) -> i32;
    fn GetClientRect(window: *mut std::ffi::c_void, rect: *mut RECT) -> i32;
    fn ClientToScreen(window: *mut std::ffi::c_void, point: *mut POINT) -> i32;
    fn GetDC(hwnd: *mut std::ffi::c_void) -> *mut std::ffi::c_void;
    fn ReleaseDC(hwnd: *mut std::ffi::c_void, dc: *mut std::ffi::c_void) -> i32;
}

#[link(name = "gdi32")]
unsafe extern "system" {
    fn CreateCompatibleDC(dc: *mut std::ffi::c_void) -> *mut std::ffi::c_void;
    fn CreateDIBSection(dc: *mut std::ffi::c_void, info: *mut BITMAPINFO, usage: u32, bits: *mut *mut std::ffi::c_void, section: *mut std::ffi::c_void, offset: u32) -> *mut std::ffi::c_void;
    fn SelectObject(dc: *mut std::ffi::c_void, object: *mut std::ffi::c_void) -> *mut std::ffi::c_void;
    fn DeleteObject(object: *mut std::ffi::c_void) -> i32;
    fn DeleteDC(dc: *mut std::ffi::c_void) -> i32;
    fn BitBlt(dest: *mut std::ffi::c_void, x: i32, y: i32, width: i32, height: i32, src: *mut std::ffi::c_void, src_x: i32, src_y: i32, rop: u32) -> i32;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn similarity_favors_identical_portraits() {
        let same = similarity(&[0.0, 1.0, -1.0], &[0.0, 1.0, -1.0]);
        let different = similarity(&[0.0, 1.0, -1.0], &[3.0, -2.0, 1.5]);
        assert!(same > different);
        assert_eq!(same, 1.0);
    }
}
