//! One-shot, local-only draft vision.
//!
//! This module never reads Dota memory, injects code, sends input, or starts a
//! capture loop. The caller explicitly requests one Dota client frame during
//! hero selection; it is compared with cached public hero portraits and discarded.

use crate::models::HeroData;
use image::{DynamicImage, GenericImageView};
use std::cmp::Reverse;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

const PORTRAIT_CACHE: &str = "cache/vision_portraits";
const VARIANT_CACHE: &str = "cache/vision_variants";
const PENDING_CACHE: &str = "cache/vision_pending";
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
const DRAFT_CARD_ASPECT: f32 = 1.39;
const PHASH_SIZE: usize = 8;
const PHASH_INPUT_SIZE: usize = PHASH_SIZE * 4;
const MAX_HAMMING_DISTANCE: u32 = 19; // round(0.30 * 64)
const MIN_HAMMING_MARGIN: u32 = 3; // round(0.05 * 64)
const SHIFT_FRACTION: f32 = 0.03;
const SHIFT_STEPS: i32 = 2;
// A black/unpicked card has no usable DCT signature. Hashing it produces an
// arbitrary 64-bit pattern and therefore a random hero. Measure the central
// artwork area first and keep such slots empty instead of guessing.
const EMPTY_SLOT_MAX_LUMA_STDDEV: f32 = 8.0;

#[derive(Debug, Clone)]
pub struct VisionCandidate {
    pub hero_name: String,
    pub localized_name: String,
    /// Hamming distance of the best 64-bit perceptual hash. This is a real
    /// metric, not a UI-looking percentage: lower is better.
    pub distance: u8,
    /// Difference to the closest *other* hero in Hamming bits. Higher means
    /// the recognition is less ambiguous.
    pub margin: u8,
    pub slot: usize,
    pub is_enemy: bool,
}

#[derive(Debug, Clone)]
pub struct VisionResult {
    pub candidates: Vec<VisionCandidate>,
    pub auto_accepted_enemies: Vec<String>,
    pub auto_accepted_allies: Vec<String>,
    pub status: String,
    pub enemy_is_right: bool,
    pub has_pending_slot_crops: bool,
}

#[derive(Clone)]
struct PortraitTemplate {
    hero_name: String,
    localized_name: String,
    hash: u64,
}

struct SlotHashes {
    hashes: Vec<u64>,
    luma_stddev: f32,
}

/// Capture exactly one frame and classify the five portrait slots of the enemy
/// bank. `enemy_is_right` is obtained from the player's GSI team field.
pub async fn scan_enemy_draft(
    heroes: Vec<HeroData>,
    enemy_is_right: bool,
) -> VisionResult {
    if heroes.is_empty() {
        return VisionResult { candidates: Vec::new(), auto_accepted_enemies: Vec::new(), auto_accepted_allies: Vec::new(), status: "Vision: каталог героев ещё загружается".into(), enemy_is_right, has_pending_slot_crops: false };
    }

    if let Err(error) = ensure_portraits(&heroes).await {
        return VisionResult { candidates: Vec::new(), auto_accepted_enemies: Vec::new(), auto_accepted_allies: Vec::new(), status: format!("Vision: не подготовлены портреты ({error})"), enemy_is_right, has_pending_slot_crops: false };
    }
    let templates = load_templates(&heroes);
    if templates.len() < 100 {
        return VisionResult { candidates: Vec::new(), auto_accepted_enemies: Vec::new(), auto_accepted_allies: Vec::new(), status: "Vision: кэш портретов неполный; повтори scan после загрузки".into(), enemy_is_right, has_pending_slot_crops: false };
    }

    let frame = match capture_dota_client() {
        Ok(frame) => frame,
        Err(error) => return VisionResult { candidates: Vec::new(), auto_accepted_enemies: Vec::new(), auto_accepted_allies: Vec::new(), status: format!("Vision: не удалось снять кадр Dota ({error})"), enemy_is_right, has_pending_slot_crops: false },
    };

    let mut candidates = Vec::new();
    let mut empty_slots = 0usize;
    let enemy_slots = if enemy_is_right { [5, 6, 7, 8, 9] } else { [0, 1, 2, 3, 4] };
    let ally_slots = if enemy_is_right { [0, 1, 2, 3, 4] } else { [5, 6, 7, 8, 9] };
    for (is_enemy, slots) in [(true, enemy_slots), (false, ally_slots)] {
        for slot in slots {
            let Some(query) = slot_hashes(&frame, slot) else {
                continue;
            };
            if query.luma_stddev <= EMPTY_SLOT_MAX_LUMA_STDDEV {
                empty_slots += 1;
                continue;
            }
            if let Some((template, distance, margin)) = best_match(&query.hashes, &templates) {
                candidates.push(VisionCandidate {
                    hero_name: template.hero_name.clone(),
                    localized_name: template.localized_name.clone(),
                    distance: distance as u8,
                    margin: margin as u8,
                    slot,
                    is_enemy,
                });
            }
        }
    }
    let auto_accepted_enemies = accepted_unique_heroes(&candidates, true);
    let auto_accepted_allies = accepted_unique_heroes(&candidates, false);
    let accepted_enemies_count = auto_accepted_enemies.len();
    let accepted_allies_count = auto_accepted_allies.len();
    let accepted_count = accepted_enemies_count + accepted_allies_count;
    let uncertain = 10usize
        .saturating_sub(empty_slots)
        .saturating_sub(accepted_count);
    let side = if enemy_is_right { "справа" } else { "слева" };
    // Exactly one file, overwritten only by an explicit F10 scan. Keeping it
    // even after a successful match makes calibration inspectable; previously
    // a successful scan left an older, misleading debug image on disk.
    let debug_saved = save_debug_frame(&frame).is_ok();
    let pending_saved = save_pending_slot_crops(&frame).is_ok();
    VisionResult {
        candidates,
        auto_accepted_enemies,
        auto_accepted_allies,
        status: format!(
            "Vision: враги {}, союзники {}, пустых {}, проверить {} · враги {side}{}{}",
            accepted_enemies_count,
            accepted_allies_count,
            empty_slots,
            uncertain,
            if debug_saved { " · debug: cache/vision_last_scan.png" } else { "" },
            if pending_saved { " · карточки сохранены для проверки после игры" } else { "" },
        ),
        enemy_is_right,
        has_pending_slot_crops: pending_saved,
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

fn hero_cache_key(hero_name: &str) -> String {
    hero_name.strip_prefix("npc_dota_hero_").unwrap_or(hero_name)
        .chars()
        .filter(|character| character.is_ascii_alphanumeric() || *character == '_')
        .collect()
}

fn variant_dir(hero_name: &str) -> PathBuf {
    Path::new(VARIANT_CACHE).join(hero_cache_key(hero_name))
}

fn pending_slot_path(slot: usize) -> PathBuf {
    Path::new(PENDING_CACHE).join(format!("slot_{slot}.png"))
}

/// Returns the exact card crop captured by the latest explicit F10 scan.
/// The overlay shows these images only in the post-game review, so training
/// never asks the player to remember who occupied a draft slot.
pub fn pending_slot_crop_bytes(slot: usize) -> Result<Vec<u8>, String> {
    if slot >= 10 {
        return Err("некорректный слот".into());
    }
    fs::read(pending_slot_path(slot))
        .map_err(|error| format!("нет снимка слота {slot}; сначала нажми F10 ({error})"))
}

/// Promotes one card from the last explicit F10 capture into the local
/// many-to-one portrait library. This API is intentionally called only after
/// the user has selected the hero in the manual picker; recognizer guesses
/// must never train the recognizer.
pub fn save_verified_slot_variant(slot: usize, hero_name: &str) -> Result<(), String> {
    if slot >= 10 || hero_cache_key(hero_name).is_empty() {
        return Err("некорректный слот или герой".into());
    }
    let source = pending_slot_path(slot);
    let image = image::open(&source)
        .map_err(|error| format!("нет снимка слота {slot}; сначала нажми F10 ({error})"))?;
    let destination_dir = variant_dir(hero_name);
    fs::create_dir_all(&destination_dir).map_err(|error| error.to_string())?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let destination = destination_dir.join(format!("{stamp}_slot_{slot}.png"));
    image.save(&destination).map_err(|error| error.to_string())
}

/// Removes only user-confirmed local variants; downloaded base portraits and
/// the latest pending F10 crop remain intact. This is the recovery path for a
/// mistakenly labelled card.
pub fn clear_verified_variants() -> Result<(), String> {
    let path = Path::new(VARIANT_CACHE);
    if path.exists() {
        fs::remove_dir_all(path).map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn load_templates(heroes: &[HeroData]) -> Vec<PortraitTemplate> {
    let mut templates = heroes.iter().filter_map(|hero| {
        let image = image::open(portrait_path(&hero.name)).ok()?;
        Some(PortraitTemplate {
            hero_name: hero.name.clone(),
            localized_name: hero.localized_name.clone(),
            hash: portrait_hash(&image)?,
        })
    }).collect::<Vec<_>>();

    // Local variants are deliberately separate from downloaded artwork. They
    // are a card crop the player explicitly labelled, so a persona/arcana or
    // HUD-specific appearance maps back to the same canonical hero name.
    for hero in heroes {
        let Ok(entries) = fs::read_dir(variant_dir(&hero.name)) else { continue; };
        for entry in entries.flatten() {
            let Ok(image) = image::open(entry.path()) else { continue; };
            if let Some(hash) = card_hash(&image) {
                templates.push(PortraitTemplate {
                    hero_name: hero.name.clone(),
                    localized_name: hero.localized_name.clone(),
                    hash,
                });
            }
        }
    }
    templates
}

fn portrait_hash(image: &DynamicImage) -> Option<u64> {
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
    perceptual_hash(crop_width, crop_height, |x, y| {
        let pixel = rgba.get_pixel(offset_x + x, offset_y + y).0;
        [pixel[0], pixel[1], pixel[2]]
    })
}

/// A verified local variant is already the exact player-card crop. Do not
/// centre-crop it again as if it were Steam's 16:9 source artwork.
fn card_hash(image: &DynamicImage) -> Option<u64> {
    let (width, height) = image.dimensions();
    let rgba = image.to_rgba8();
    perceptual_hash(width, height, |x, y| {
        let pixel = rgba.get_pixel(x, y).0;
        [pixel[0], pixel[1], pixel[2]]
    })
}

struct DesktopFrame {
    width: u32,
    height: u32,
    /// BGRA, top-to-bottom, captured once from the desktop compositor.
    pixels: Vec<u8>,
}

fn slot_hashes(frame: &DesktopFrame, slot: usize) -> Option<SlotHashes> {
    // Hero portrait centres in Dota's draft HUD, normalized to the Dota client.
    // The crop is deliberately inset to ignore card borders and player names.
    // Values are normalized to Dota's client area, not desktop resolution.
    let centre_x = *SLOT_CENTRES.get(slot)?;
    let crop_w = (frame.width as f32 * 0.054) as u32;
    let crop_h = (frame.height as f32 * TOP_CARD_ART_HEIGHT) as u32;
    let x = ((frame.width as f32 * centre_x) as i32 - crop_w as i32 / 2).max(0) as i32;
    let y = 0_i32;
    if crop_w >= frame.width || crop_h >= frame.height || crop_w < 16 || crop_h < 16 {
        return None;
    }
    let luma_stddev = crop_luma_stddev(frame, x as u32, y as u32, crop_w, crop_h);
    let mut hashes = Vec::with_capacity(((SHIFT_STEPS * 2 + 1).pow(2)) as usize);
    for dy in -SHIFT_STEPS..=SHIFT_STEPS {
        for dx in -SHIFT_STEPS..=SHIFT_STEPS {
            let shifted_x = (x + (dx as f32 * SHIFT_FRACTION * crop_w as f32).round() as i32)
                .clamp(0, (frame.width - crop_w) as i32) as u32;
            let shifted_y = (y + (dy as f32 * SHIFT_FRACTION * crop_h as f32).round() as i32)
                .clamp(0, (frame.height - crop_h) as i32) as u32;
            hashes.push(perceptual_hash(crop_w, crop_h, |cx, cy| {
                let index = (((shifted_y + cy) * frame.width + (shifted_x + cx)) * 4) as usize;
                [frame.pixels[index + 2], frame.pixels[index + 1], frame.pixels[index]]
            })?);
        }
    }
    Some(SlotHashes { hashes, luma_stddev })
}

fn slot_rect(frame: &DesktopFrame, slot: usize) -> Option<(u32, u32, u32, u32)> {
    let centre_x = *SLOT_CENTRES.get(slot)?;
    let width = (frame.width as f32 * 0.054) as u32;
    let height = (frame.height as f32 * TOP_CARD_ART_HEIGHT) as u32;
    let left = ((frame.width as f32 * centre_x) as i32 - width as i32 / 2).max(0) as u32;
    (width < frame.width && height < frame.height && width >= 16 && height >= 16)
        .then_some((left, 0, width, height))
}

fn save_pending_slot_crops(frame: &DesktopFrame) -> Result<(), String> {
    fs::create_dir_all(PENDING_CACHE).map_err(|error| error.to_string())?;
    for slot in 0..10 {
        let Some((left, top, width, height)) = slot_rect(frame, slot) else { continue; };
        let mut rgba = Vec::with_capacity((width * height * 4) as usize);
        for y in top..top + height {
            for x in left..left + width {
                let index = ((y * frame.width + x) * 4) as usize;
                rgba.extend_from_slice(&[
                    frame.pixels[index + 2], frame.pixels[index + 1], frame.pixels[index], 255,
                ]);
            }
        }
        let image = image::RgbaImage::from_raw(width, height, rgba)
            .ok_or_else(|| format!("не удалось сохранить слот {slot}"))?;
        image.save(pending_slot_path(slot)).map_err(|error| error.to_string())?;
    }
    Ok(())
}

/// Standard deviation over the interior of a slot. The two-pixel card frame
/// is intentionally excluded: an empty card can have a coloured frame while
/// its artwork area remains practically flat.
fn crop_luma_stddev(frame: &DesktopFrame, left: u32, top: u32, width: u32, height: u32) -> f32 {
    let inset_x = (width / 10).max(1);
    let inset_y = (height / 10).max(1);
    let start_x = (left + inset_x).min(frame.width);
    let start_y = (top + inset_y).min(frame.height);
    let end_x = left.saturating_add(width).saturating_sub(inset_x).min(frame.width);
    let end_y = top.saturating_add(height).saturating_sub(inset_y).min(frame.height);
    if start_x >= end_x || start_y >= end_y {
        return f32::INFINITY;
    }

    let mut count = 0_f64;
    let mut sum = 0_f64;
    let mut sum_squares = 0_f64;
    for y in start_y..end_y {
        for x in start_x..end_x {
            let index = ((y * frame.width + x) * 4) as usize;
            let blue = frame.pixels[index] as f64;
            let green = frame.pixels[index + 1] as f64;
            let red = frame.pixels[index + 2] as f64;
            let luma = 0.2126 * red + 0.7152 * green + 0.0722 * blue;
            count += 1.0;
            sum += luma;
            sum_squares += luma * luma;
        }
    }
    let mean = sum / count;
    ((sum_squares / count - mean * mean).max(0.0) as f32).sqrt()
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

/// 64-bit DCT perceptual hash.  The image is reduced to 32×32 greyscale,
/// transformed into low-frequency coefficients, then thresholded at their
/// median. It is deliberately insensitive to brightness/scale changes that
/// are common between Valve's portrait source and the in-game player card.
fn perceptual_hash<F>(width: u32, height: u32, mut pixel_at: F) -> Option<u64>
where
    F: FnMut(u32, u32) -> [u8; 3],
{
    if width == 0 || height == 0 {
        return None;
    }
    let side = PHASH_INPUT_SIZE;
    let mut luminance = vec![0.0_f32; side * side];
    for y in 0..side {
        for x in 0..side {
            let source_x = (((x as f32 + 0.5) * width as f32 / side as f32) as u32)
                .min(width - 1);
            let source_y = (((y as f32 + 0.5) * height as f32 / side as f32) as u32)
                .min(height - 1);
            let [red, green, blue] = pixel_at(source_x, source_y);
            luminance[y * side + x] = 0.2126 * red as f32
                + 0.7152 * green as f32
                + 0.0722 * blue as f32;
        }
    }

    let mut coefficients = [0.0_f32; PHASH_SIZE * PHASH_SIZE];
    let factor = std::f32::consts::PI / (side as f32 * 2.0);
    for v in 0..PHASH_SIZE {
        for u in 0..PHASH_SIZE {
            let mut sum = 0.0_f32;
            for y in 0..side {
                let vertical = ((2 * y + 1) as f32 * v as f32 * factor).cos();
                for x in 0..side {
                    let horizontal = ((2 * x + 1) as f32 * u as f32 * factor).cos();
                    sum += luminance[y * side + x] * horizontal * vertical;
                }
            }
            let scale_u = if u == 0 {
                (1.0 / side as f32).sqrt()
            } else {
                (2.0 / side as f32).sqrt()
            };
            let scale_v = if v == 0 {
                (1.0 / side as f32).sqrt()
            } else {
                (2.0 / side as f32).sqrt()
            };
            coefficients[v * PHASH_SIZE + u] = sum * scale_u * scale_v;
        }
    }

    let mut non_dc = coefficients[1..].to_vec();
    non_dc.sort_by(|left, right| left.total_cmp(right));
    let median = non_dc[non_dc.len() / 2];
    let mut hash = 0_u64;
    for (index, coefficient) in coefficients.iter().enumerate().skip(1) {
        if *coefficient > median {
            hash |= 1_u64 << index;
        }
    }
    Some(hash)
}

fn best_match<'a>(query_hashes: &[u64], templates: &'a [PortraitTemplate]) -> Option<(&'a PortraitTemplate, u32, u32)> {
    let mut matches = templates
        .iter()
        .map(|template| {
            let distance = query_hashes
                .iter()
                .map(|query| (query ^ template.hash).count_ones())
                .min()
                .unwrap_or(u32::MAX);
            (template, distance)
        })
        .collect::<Vec<_>>();
    matches.sort_by_key(|(_, distance)| *distance);
    let (best, best_distance) = matches.first().copied()?;
    let runner_up_distance = matches
        .iter()
        .find(|(candidate, _)| candidate.hero_name != best.hero_name)
        .map(|(_, distance)| *distance)
        .unwrap_or(64);
    Some((best, best_distance, runner_up_distance.saturating_sub(best_distance)))
}

fn accepted_unique_heroes(candidates: &[VisionCandidate], is_enemy: bool) -> Vec<String> {
    let mut ranked = candidates
        .iter()
        .filter(|candidate| {
            candidate.is_enemy == is_enemy
                && candidate.distance as u32 <= MAX_HAMMING_DISTANCE
                && candidate.margin as u32 >= MIN_HAMMING_MARGIN
        })
        .collect::<Vec<_>>();
    ranked.sort_by_key(|candidate| (candidate.distance, Reverse(candidate.margin)));
    let mut seen = HashSet::new();
    ranked
        .into_iter()
        .filter(|candidate| seen.insert(candidate.hero_name.clone()))
        .map(|candidate| candidate.hero_name.clone())
        .take(5)
        .collect()
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
    fn perceptual_hash_preserves_identical_art_and_changes_for_inverse_art() {
        let image = |invert: bool| {
            perceptual_hash(32, 32, |x, y| {
                let value = ((x * 7 + y * 11) % 255) as u8;
                let value = if invert { 255 - value } else { value };
                [value, value.saturating_add(x as u8 / 3), 255 - value]
            })
            .unwrap()
        };
        let first = image(false);
        let same = image(false);
        let inverse = image(true);
        assert_eq!((first ^ same).count_ones(), 0);
        assert!((first ^ inverse).count_ones() > 3);
    }

    #[test]
    fn empty_card_interior_is_not_sent_to_the_hero_matcher() {
        let frame = DesktopFrame {
            width: 20,
            height: 20,
            pixels: vec![14; 20 * 20 * 4],
        };
        assert!(crop_luma_stddev(&frame, 0, 0, 20, 20) <= EMPTY_SLOT_MAX_LUMA_STDDEV);

        let mut artwork = frame.pixels.clone();
        for (index, pixel) in artwork.iter_mut().enumerate().step_by(7) {
            *pixel = (index % 255) as u8;
        }
        let artwork = DesktopFrame { width: 20, height: 20, pixels: artwork };
        assert!(crop_luma_stddev(&artwork, 0, 0, 20, 20) > EMPTY_SLOT_MAX_LUMA_STDDEV);
    }
}
