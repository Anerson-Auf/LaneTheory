use crate::advisor::{self, Advisor};
use crate::api::DotaApiClient;
use crate::models::{LiveGameState, OverlaySettings, PlayerPosition, PopularItemEntry, RecommendedHero, SituationalItem};
use eframe::egui;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Mutex};

pub static HOTKEY_F6_TRIGGERED: AtomicBool = AtomicBool::new(false);
pub static HOTKEY_F7_TRIGGERED: AtomicBool = AtomicBool::new(false);
pub static HOTKEY_F8_TRIGGERED: AtomicBool = AtomicBool::new(false);
pub static HOTKEY_F9_TRIGGERED: AtomicBool = AtomicBool::new(false);
pub static HOTKEY_F5_TRIGGERED: AtomicBool = AtomicBool::new(false);
pub static VISION_SCAN_TRIGGERED: AtomicBool = AtomicBool::new(false);
static PASSIVE_SCROLL_STEPS: AtomicI32 = AtomicI32::new(0);
static ULTIMATE_CLICK_INDEX: AtomicI32 = AtomicI32::new(-1);
static ULTIMATE_HITBOXES: std::sync::Mutex<Vec<HitBox>> = std::sync::Mutex::new(Vec::new());
static D2PT_LINK_HITBOX: std::sync::Mutex<Option<HitBox>> = std::sync::Mutex::new(None);
static D2PT_LINK_REQUESTED: AtomicBool = AtomicBool::new(false);
static DRAFT_PICKER_HITBOX: std::sync::Mutex<Option<HitBox>> = std::sync::Mutex::new(None);
static DRAFT_PICKER_REQUESTED: AtomicBool = AtomicBool::new(false);
static ROSHAN_HITBOXES: std::sync::Mutex<Vec<HitBox>> = std::sync::Mutex::new(Vec::new());
static ROSHAN_ACTION: AtomicI32 = AtomicI32::new(0);

/// A network-backed draft analysis must never keep the overlay in a loading
/// state indefinitely. The result retains its signature so stale work cannot
/// overwrite a newer draft.
enum DraftAnalysisResult {
    Ready {
        signature: Vec<String>,
        counters: Vec<RecommendedHero>,
        situational_items: Vec<SituationalItem>,
        weaknesses: Vec<String>,
    },
    TimedOut {
        signature: Vec<String>,
    },
}

pub const AEGIS_IMAGE_URL: &str = "https://cdn.cloudflare.steamstatic.com/apps/dota2/images/dota_react/items/aegis.png";
pub const ROSHAN_IMAGE_URL: &str = "https://cdn.cloudflare.steamstatic.com/apps/dota2/images/dota_react/abilities/roshan_spell_block.png";

#[repr(C)]
struct POINT {
    x: i32,
    y: i32,
}

#[repr(C)]
#[allow(non_snake_case)]
struct MSG {
    hwnd: *mut std::ffi::c_void,
    message: u32,
    wParam: usize,
    lParam: isize,
    time: u32,
    pt: POINT,
    lPrivate: u32,
}

#[repr(C)]
struct MSLLHOOKSTRUCT {
    pt: POINT,
    mouse_data: u32,
    flags: u32,
    time: u32,
    extra_info: usize,
}

#[derive(Clone, Copy)]
struct HitBox {
    index: i32,
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

unsafe extern "system" fn passive_mouse_hook(code: i32, message: usize, data: isize) -> isize {
    const HC_ACTION: i32 = 0;
    const WM_MOUSEWHEEL: usize = 0x020A;
    if code == HC_ACTION && !data.eq(&0) {
        let event = unsafe { &*(data as *const MSLLHOOKSTRUCT) };
        if message == WM_MOUSEWHEEL {
            let delta = ((event.mouse_data >> 16) as i16) as i32;
            if delta != 0 {
                PASSIVE_SCROLL_STEPS.fetch_add(delta.signum(), Ordering::Relaxed);
            }
        } else if message == 0x0201 {
            if let Ok(hitboxes) = ULTIMATE_HITBOXES.try_lock() {
                if let Some(hit) = hitboxes.iter().find(|hit| {
                    event.pt.x >= hit.left && event.pt.x <= hit.right
                        && event.pt.y >= hit.top && event.pt.y <= hit.bottom
                }) {
                    ULTIMATE_CLICK_INDEX.store(hit.index, Ordering::Relaxed);
                }
            }
            if let Ok(link) = D2PT_LINK_HITBOX.try_lock() {
                if let Some(hit) = *link {
                    if event.pt.x >= hit.left && event.pt.x <= hit.right
                        && event.pt.y >= hit.top && event.pt.y <= hit.bottom
                    {
                        D2PT_LINK_REQUESTED.store(true, Ordering::Relaxed);
                    }
                }
            }
            if let Ok(picker) = DRAFT_PICKER_HITBOX.try_lock() {
                if let Some(hit) = *picker {
                    if event.pt.x >= hit.left && event.pt.x <= hit.right
                        && event.pt.y >= hit.top && event.pt.y <= hit.bottom
                    {
                        DRAFT_PICKER_REQUESTED.store(true, Ordering::Relaxed);
                    }
                }
            }
            if let Ok(hitboxes) = ROSHAN_HITBOXES.try_lock() {
                if let Some(hit) = hitboxes.iter().find(|hit| {
                    event.pt.x >= hit.left && event.pt.x <= hit.right
                        && event.pt.y >= hit.top && event.pt.y <= hit.bottom
                }) {
                    ROSHAN_ACTION.store(hit.index, Ordering::Relaxed);
                }
            }
        }
    }
    unsafe { CallNextHookEx(std::ptr::null_mut(), code, message, data) }
}

/// Mouse hooks report physical screen pixels, while egui uses logical points.
/// Convert through the overlay window so hit testing also works at 125/150% DPI.
fn response_screen_hitbox(index: i32, rect: egui::Rect, pixels_per_point: f32) -> HitBox {
    let title = std::ffi::CString::new("LaneTheory").unwrap();
    let hwnd = unsafe { FindWindowA(std::ptr::null(), title.as_ptr()) };
    let scale = pixels_per_point.max(1.0);
    let mut top_left = POINT {
        x: (rect.left() * scale).round() as i32,
        y: (rect.top() * scale).round() as i32,
    };
    let mut bottom_right = POINT {
        x: (rect.right() * scale).round() as i32,
        y: (rect.bottom() * scale).round() as i32,
    };
    if !hwnd.is_null() {
        unsafe {
            ClientToScreen(hwnd, &mut top_left);
            ClientToScreen(hwnd, &mut bottom_right);
        }
    }
    HitBox {
        index,
        left: top_left.x,
        top: top_left.y,
        right: bottom_right.x,
        bottom: bottom_right.y,
    }
}

fn tracked_spell_key(spell: &crate::models::TrackedSpell) -> String {
    // The ability's internal id stays stable across localization and is the
    // durable identity for a manually selected ultimate tier.
    spell.ability_key.to_lowercase()
}

fn apply_ultimate_tier(spell: &mut crate::models::TrackedSpell, tier: u8) {
    let tier = tier.clamp(1, 3);
    spell.ultimate_level = Some(tier);
    if let Some(cooldown) = spell.cooldowns.get((tier - 1) as usize) {
        spell.base_cd = *cooldown;
    }
}

fn start_global_hotkey_listener() {
    std::thread::spawn(|| {
        unsafe {
            // RegisterHotKey(hWnd, id, fsModifiers, vk)
            // MOD_NOREPEAT = 0x4000
            // VK_F5 = 0x74, VK_F6 = 0x75, VK_F7 = 0x76, VK_F8 = 0x77, VK_F9 = 0x78, VK_F10 = 0x79
            let _ = RegisterHotKey(std::ptr::null_mut(), 1, 0x4000, 0x75);
            let _ = RegisterHotKey(std::ptr::null_mut(), 2, 0x4000, 0x77);
            let _ = RegisterHotKey(std::ptr::null_mut(), 3, 0x4000, 0x76);
            let _ = RegisterHotKey(std::ptr::null_mut(), 4, 0x4000, 0x78);
            let _ = RegisterHotKey(std::ptr::null_mut(), 5, 0x4000, 0x79);
            let _ = RegisterHotKey(std::ptr::null_mut(), 6, 0x4000, 0x74);
            let _ = SetWindowsHookExW(14, Some(passive_mouse_hook), std::ptr::null_mut(), 0);

            let mut msg: MSG = std::mem::zeroed();
            while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
                if msg.message == 0x0312 /* WM_HOTKEY */ {
                    match msg.wParam {
                        1 => HOTKEY_F6_TRIGGERED.store(true, Ordering::SeqCst),
                        2 => HOTKEY_F8_TRIGGERED.store(true, Ordering::SeqCst),
                        3 => HOTKEY_F7_TRIGGERED.store(true, Ordering::SeqCst),
                        4 => HOTKEY_F9_TRIGGERED.store(true, Ordering::SeqCst),
                        5 => VISION_SCAN_TRIGGERED.store(true, Ordering::SeqCst),
                        6 => HOTKEY_F5_TRIGGERED.store(true, Ordering::SeqCst),
                        _ => {}
                    }
                }
            }
        }
    });
}

fn speak_tactical_text(text: &str) {
    let t = text.to_string();
    std::thread::spawn(move || {
        use std::os::windows::process::CommandExt;
        let script = format!(
            "Add-Type -AssemblyName System.Speech; $s = New-Object System.Speech.Synthesis.SpeechSynthesizer; $s.Speak('{}')",
            t.replace('\'', "''")
        );
        let _ = std::process::Command::new("powershell")
            .arg("-NoProfile")
            .arg("-Command")
            .arg(script)
            .creation_flags(0x08000000)
            .output();
    });
}

fn short_tts_phrase(title: &str) -> String {
    let normalized = title.to_ascii_lowercase();
    if normalized.contains("water") { "Вода" }
    else if normalized.contains("power rune") { "Руна" }
    else if normalized.contains("wisdom") { "Мудрость" }
    else if normalized.contains("bounty") { "Баунти" }
    else if normalized.contains("lotus") { "Лотос" }
    else if normalized.contains("tormentor") { "Торментор" }
    else if normalized.contains("roshan") { "Рошан" }
    else if normalized.contains("catapult") { "Катапульта" }
    else { title }
    .to_string()
}

fn d2pt_url(hero_name: &str, position: PlayerPosition) -> String {
    let hero = hero_name.strip_prefix("npc_dota_hero_").unwrap_or(hero_name);
    let hero = hero
        .split('_')
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<String>();
    format!("https://dota2protracker.com/hero/{hero}?role={}", position.d2pt_role())
}

fn open_external_url(url: &str) {
    use std::os::windows::process::CommandExt;
    let _ = std::process::Command::new("cmd")
        .args(["/C", "start", "", url])
        .creation_flags(0x08000000)
        .spawn();
}

pub struct OverlayApp {
    state: Arc<Mutex<LiveGameState>>,
    api: Arc<tokio::sync::Mutex<DotaApiClient>>,
    tokio_handle: tokio::runtime::Handle,
    is_visible: bool,
    /// Focus mode hides all panels but preserves time-sensitive alerts.
    focus_mode: bool,
    prev_f5_down: bool,
    prev_f6_down: bool,
    prev_f7_down: bool,
    prev_f8_down: bool,
    prev_f9_down: bool,
    hwnd_applied: bool,

    // Settings & interactivity
    pub settings: OverlaySettings,
    pub is_settings_open: bool,

    // Audio & TTS debounce
    last_tts_text: String,

    // Position handling
    manual_position: Option<PlayerPosition>,

    // Draft recommendations
    last_enemy_signature: Vec<String>,
    all_counters: Vec<RecommendedHero>,
    situational_items: Vec<SituationalItem>,
    draft_weaknesses: Vec<String>,
    // The signature travels with an async result so an obsolete draft request
    // cannot overwrite the analysis for a newly entered enemy.
    draft_rx: Option<std::sync::mpsc::Receiver<DraftAnalysisResult>>,
    is_analyzing_draft: bool,
    draft_analysis_error: Option<String>,
    vision_rx: Option<std::sync::mpsc::Receiver<crate::vision::VisionResult>>,
    vision_candidates: Vec<crate::vision::VisionCandidate>,
    vision_status: String,
    vision_capture_in_progress: bool,
    vision_enemy_is_right: bool,
    vision_capture_available: bool,
    vision_clear_confirmation: bool,
    /// Review happens after a completed game, never in the draft. The slot
    /// images are the actual F10 crops, not a prompt to recall a past pick.
    vision_review_open: bool,
    vision_review_slot: Option<usize>,
    vision_review_search: String,
    vision_review_images: HashMap<usize, Vec<u8>>,
    vision_review_labels: HashMap<usize, String>,

    // Builds recommendations
    last_hero_for_builds: String,
    is_loading_builds: bool,
    start_items: Vec<PopularItemEntry>,
    early_items: Vec<PopularItemEntry>,
    mid_items: Vec<PopularItemEntry>,
    late_items: Vec<PopularItemEntry>,
    builds_rx: Option<std::sync::mpsc::Receiver<(Vec<PopularItemEntry>, Vec<PopularItemEntry>, Vec<PopularItemEntry>, Vec<PopularItemEntry>)>>,

    // Key enemy ultimates tracker
    tracked_ultimates: Vec<crate::models::TrackedSpell>,

    // Legal fallback for All Pick: Valve does not expose enemy draft picks via
    // GSI there. Observer/replay GSI data remains authoritative when available.
    manual_enemy_heroes: Vec<String>,
    manual_ally_heroes: Vec<String>,
    /// When the player edits an already known draft, their corrected list is
    /// intentionally authoritative for this match instead of stale GSI data.
    manual_draft_override: bool,
    manual_enemy_search: String,
    manual_ultimate_levels: HashMap<String, u8>,
    draft_picker_open: bool,
    previous_game_state: String,
    previous_match_id: Option<String>,

    // Local visual test mode, never used in a real match.
    test_enemies: Vec<String>,
    test_mode_enabled: bool,
    simulated_clock_time: Option<i32>,
    left_panel_scroll: f32,
    right_panel_scroll: f32,
}

impl OverlayApp {
    fn apply_passive_scroll(&mut self, screen_w: f32, screen_h: f32) {
        let steps = PASSIVE_SCROLL_STEPS.swap(0, Ordering::Relaxed);
        if steps == 0 {
            return;
        }

        let mut cursor: POINT = unsafe { std::mem::zeroed() };
        if unsafe { GetCursorPos(&mut cursor) } == 0 {
            return;
        }

        let panel_h = (screen_h - 140.0).clamp(460.0, 720.0);
        let amount = -(steps as f32) * 72.0;
        let on_left = (14.0..=294.0).contains(&(cursor.x as f32))
            && (44.0..=(44.0 + panel_h)).contains(&(cursor.y as f32));
        let right_x = (screen_w - 286.0).max(400.0);
        let on_right = (right_x..=(right_x + 286.0)).contains(&(cursor.x as f32))
            && (44.0..=(44.0 + panel_h)).contains(&(cursor.y as f32));

        if on_left {
            self.left_panel_scroll = (self.left_panel_scroll + amount).max(0.0);
        }
        if on_right {
            self.right_panel_scroll = (self.right_panel_scroll + amount).max(0.0);
        }
    }

    fn meta_heroes(&self, pos: PlayerPosition) -> Vec<(String, String, f32)> {
        let bracket = if self.settings.enable_rank_filter {
            self.settings.selected_rank
        } else {
            crate::models::RankBracket::All
        };
        self.api.try_lock().ok()
            .map(|api| api.meta_heroes_for_position(pos, bracket))
            .unwrap_or_default()
    }

    fn hero_picker_options(&self, search: &str, include_selected: bool) -> Vec<crate::models::HeroData> {
        let query = search.trim().to_lowercase();
        let selected_enemy = &self.manual_enemy_heroes;
        let selected_ally = &self.manual_ally_heroes;
        let mut heroes = self.api.try_lock().ok()
            .map(|api| api.heroes.values()
                .filter(|hero| {
                    (include_selected || (!selected_enemy.iter().any(|name| name == &hero.name)
                        && !selected_ally.iter().any(|name| name == &hero.name)))
                        && (query.is_empty()
                            || hero.localized_name.to_lowercase().contains(&query)
                            || hero.short_name().replace('_', " ").contains(&query))
                })
                .cloned()
                .collect::<Vec<_>>())
            .unwrap_or_default();
        heroes.sort_by(|a, b| a.localized_name.cmp(&b.localized_name));
        heroes.truncate(8);
        heroes
    }

    fn manual_picker_options(&self) -> Vec<crate::models::HeroData> {
        self.hero_picker_options(&self.manual_enemy_search, false)
    }

    fn load_vision_review_images(&mut self) {
        self.vision_review_images.clear();
        for slot in 0..10 {
            if let Ok(bytes) = crate::vision::pending_slot_crop_bytes(slot) {
                self.vision_review_images.insert(slot, bytes);
            }
        }
    }

    fn open_pick_editor(&mut self, live_state: &LiveGameState) {
        // If GSI did reveal a roster, display it as the starting point. It is
        // not made authoritative until the player actually edits something.
        if !self.manual_draft_override {
            if !live_state.enemy_heroes.is_empty() {
                self.manual_enemy_heroes = live_state.enemy_heroes.iter().take(5).cloned().collect();
            }
            if !live_state.ally_heroes.is_empty() {
                self.manual_ally_heroes = live_state.ally_heroes.iter().take(5).cloned().collect();
            }
        }
        self.draft_picker_open = true;
        self.set_click_through(false);
    }

    fn start_vision_scan(&mut self, live_state: &LiveGameState) {
        if self.vision_rx.is_some() {
            return;
        }
        let heroes = match self.api.try_lock() {
            Ok(api) => api.heroes.values().cloned().collect::<Vec<_>>(),
            Err(_) => {
                self.vision_status = "Vision: каталог занят загрузкой, повтори через секунду".to_string();
                return;
            }
        };
        if heroes.len() < 100 {
            self.vision_status = "Vision: список героев ещё не готов".to_string();
            return;
        }
        // In Dota's top draft HUD Radiant is on the left and Dire on the
        // right. Player GSI provides the ally team even when it hides picks.
        let enemy_is_right = live_state.my_team.as_deref().unwrap_or("radiant") != "dire";
        let (tx, rx) = std::sync::mpsc::channel();
        self.vision_rx = Some(rx);
        self.vision_capture_in_progress = true;
        self.draft_picker_open = false;
        self.set_click_through(true);
        self.vision_status = "Vision: снимаю один кадр драфта…".to_string();
        self.tokio_handle.spawn(async move {
            // Give egui one paint to remove every overlay surface before the
            // desktop copy. This is a one-off delay, not a scanning loop.
            tokio::time::sleep(std::time::Duration::from_millis(180)).await;
            let _ = tx.send(crate::vision::scan_enemy_draft(heroes, enemy_is_right).await);
        });
    }

    pub fn new(
        state: Arc<Mutex<LiveGameState>>,
        api: Arc<tokio::sync::Mutex<DotaApiClient>>,
        tokio_handle: tokio::runtime::Handle,
    ) -> Self {
        start_global_hotkey_listener();

        Self {
            state,
            api,
            tokio_handle,
            is_visible: true,
            focus_mode: false,
            prev_f5_down: false,
            prev_f6_down: false,
            prev_f7_down: false,
            prev_f8_down: false,
            prev_f9_down: false,
            hwnd_applied: false,

            settings: OverlaySettings::load(),
            is_settings_open: false,

            last_tts_text: String::new(),

            manual_position: None,

            last_enemy_signature: Vec::new(),
            all_counters: Vec::new(),
            situational_items: Vec::new(),
            draft_weaknesses: Vec::new(),
            draft_rx: None,
            is_analyzing_draft: false,
            draft_analysis_error: None,
            vision_rx: None,
            vision_candidates: Vec::new(),
            vision_status: "Vision: по кнопке, один кадр".to_string(),
            vision_capture_in_progress: false,
            vision_enemy_is_right: true,
            vision_capture_available: false,
            vision_clear_confirmation: false,
            vision_review_open: false,
            vision_review_slot: None,
            vision_review_search: String::new(),
            vision_review_images: HashMap::new(),
            vision_review_labels: HashMap::new(),

            last_hero_for_builds: String::new(),
            is_loading_builds: false,
            start_items: Vec::new(),
            early_items: Vec::new(),
            mid_items: Vec::new(),
            late_items: Vec::new(),
            builds_rx: None,

            tracked_ultimates: Vec::new(),

            manual_enemy_heroes: Vec::new(),
            manual_ally_heroes: Vec::new(),
            manual_draft_override: false,
            manual_enemy_search: String::new(),
            manual_ultimate_levels: HashMap::new(),
            draft_picker_open: false,
            previous_game_state: String::new(),
            previous_match_id: None,
            test_enemies: vec![
                "phantom_assassin".to_string(),
                "pudge".to_string(),
                "bristleback".to_string(),
            ],
            test_mode_enabled: false,
            simulated_clock_time: None,
            left_panel_scroll: 0.0,
            right_panel_scroll: 0.0,
        }
    }

    fn check_hotkeys(&mut self) {
        let f5_hotkey = HOTKEY_F5_TRIGGERED.swap(false, Ordering::SeqCst);
        let f6_hotkey = HOTKEY_F6_TRIGGERED.swap(false, Ordering::SeqCst);
        let f7_hotkey = HOTKEY_F7_TRIGGERED.swap(false, Ordering::SeqCst);
        let f8_hotkey = HOTKEY_F8_TRIGGERED.swap(false, Ordering::SeqCst);

        unsafe {
            // VK_F5 = 0x74 (Cycle active position: 1 -> 2 -> 3 -> 4 -> 5)
            let f5_down = (GetAsyncKeyState(0x74) as u16 & 0x8000) != 0;
            if f5_hotkey || (f5_down && !self.prev_f5_down) {
                let cur = self.manual_position.unwrap_or(self.settings.preferred_position);
                self.manual_position = Some(cur.next());
            }
            self.prev_f5_down = f5_down;

            // VK_F6 = 0x75 (Focus mode: alerts only)
            let f6_down = (GetAsyncKeyState(0x75) as u16 & 0x8000) != 0;
            if f6_hotkey || (f6_down && !self.prev_f6_down) {
                self.focus_mode = !self.focus_mode;
                self.is_visible = true;
                if self.is_settings_open {
                    self.toggle_settings();
                }
            }
            self.prev_f6_down = f6_down;

            // VK_F7 = 0x76 (Hide/show every overlay surface)
            let f7_down = (GetAsyncKeyState(0x76) as u16 & 0x8000) != 0;
            if f7_hotkey || (f7_down && !self.prev_f7_down) {
                self.is_visible = !self.is_visible;
                self.focus_mode = false;
                if self.is_settings_open {
                    self.toggle_settings();
                }
            }
            self.prev_f7_down = f7_down;

            // VK_F8 = 0x77 (Toggle settings & interactive mode)
            let f8_down = (GetAsyncKeyState(0x77) as u16 & 0x8000) != 0;
            if f8_hotkey || (f8_down && !self.prev_f8_down) {
                self.is_visible = true;
                self.focus_mode = false;
                self.toggle_settings();
            }
            self.prev_f8_down = f8_down;

            // VK_F9 = 0x78 (Toggle Roshan timer)
            let f9_hotkey = HOTKEY_F9_TRIGGERED.swap(false, Ordering::SeqCst);
            let f9_down = (GetAsyncKeyState(0x78) as u16 & 0x8000) != 0;
            if f9_hotkey || (f9_down && !self.prev_f9_down) {
                self.toggle_roshan(true);
            }
            self.prev_f9_down = f9_down;
        }
    }

    pub fn toggle_roshan(&mut self, is_ally: bool) {
        let mut st = self.state.lock().unwrap();
        let cur_clock = if self.test_mode_enabled { self.simulated_clock_time.unwrap_or(300) } else { st.clock_time.max(0) };
        if st.roshan.is_tracking {
            st.roshan = crate::models::RoshanState::default();
        } else {
            st.roshan = crate::models::RoshanState {
                is_tracking: true,
                kill_clock_time: cur_clock,
                is_ally,
            };
        }
    }

    pub fn set_roshan_ally(&mut self, is_ally: bool) {
        let mut st = self.state.lock().unwrap();
        if st.roshan.is_tracking {
            st.roshan.is_ally = is_ally;
        } else {
            let cur_clock = if self.test_mode_enabled { self.simulated_clock_time.unwrap_or(300) } else { st.clock_time.max(0) };
            st.roshan = crate::models::RoshanState {
                is_tracking: true,
                kill_clock_time: cur_clock,
                is_ally,
            };
        }
    }

    pub fn reset_roshan(&mut self) {
        let mut st = self.state.lock().unwrap();
        st.roshan = crate::models::RoshanState::default();
    }

    pub fn set_click_through(&self, click_through: bool) {
        unsafe {
            let title = std::ffi::CString::new("LaneTheory").unwrap();
            let hwnd = FindWindowA(std::ptr::null(), title.as_ptr());
            if !hwnd.is_null() {
                let cur = GetWindowLongPtrW(hwnd, -20); // GWL_EXSTYLE
                let new_style = if click_through {
                    (cur | 0x20) | 0x80000 // WS_EX_TRANSPARENT | WS_EX_LAYERED
                } else {
                    (cur & !0x20) | 0x80000 // Remove WS_EX_TRANSPARENT, keep WS_EX_LAYERED
                };
                SetWindowLongPtrW(hwnd, -20, new_style);
                if !click_through {
                    SetForegroundWindow(hwnd);
                }
            }
        }
    }

    pub fn toggle_settings(&mut self) {
        self.is_settings_open = !self.is_settings_open;
        if self.is_settings_open {
            self.set_click_through(false);
        } else {
            self.settings.save();
            self.set_click_through(true);
        }
    }

    fn apply_window_transparency(&self) {
        unsafe {
            let title = std::ffi::CString::new("LaneTheory").unwrap();
            let hwnd = FindWindowA(std::ptr::null(), title.as_ptr());
            if !hwnd.is_null() {
                // Remove background GDI brush so Windows never clears with white
                SetClassLongPtrW(hwnd, -10, 0); // GCLP_HBRBACKGROUND = -10

                // Initially click-through: WS_EX_TRANSPARENT (0x20) | WS_EX_LAYERED (0x80000)
                let cur = GetWindowLongPtrW(hwnd, -20); // GWL_EXSTYLE
                SetWindowLongPtrW(hwnd, -20, cur | 0x20 | 0x80000);
            }
        }
    }
}

impl eframe::App for OverlayApp {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0]
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.check_hotkeys();

        if !self.hwnd_applied {
            self.apply_window_transparency();
            self.hwnd_applied = true;
        }

        let ctx = ui.ctx().clone();

        // F7 is a hard hide. F6 focus mode still renders time-sensitive
        // notifications below, while every panel stays absent.
        if !self.is_visible {
            ctx.request_repaint_after(std::time::Duration::from_millis(150));
            return;
        }

        let mut visuals = egui::Visuals::dark();
        visuals.panel_fill = egui::Color32::TRANSPARENT;
        visuals.window_fill = egui::Color32::TRANSPARENT;
        ctx.set_visuals(visuals);

        let (live_state, enemies, my_hero) = {
            let st = self.state.lock().unwrap();
            (st.clone(), st.enemy_heroes.clone(), st.my_hero_name.clone())
        };

        // Do not carry manually entered heroes into a subsequent match. GSI
        // can stop after a game and resume at the next draft without a menu
        // payload, so match_id is the primary boundary.
        let entered_new_match = self.previous_match_id.is_some()
            && live_state.match_id.is_some()
            && self.previous_match_id != live_state.match_id;
        let entered_strategy_time = live_state.game_state == "DOTA_GAMERULES_STATE_STRATEGY_TIME"
            && self.previous_game_state != "DOTA_GAMERULES_STATE_STRATEGY_TIME";
        let finished_captured_match = live_state.game_state == "menu"
            && matches!(
                self.previous_game_state.as_str(),
                "DOTA_GAMERULES_STATE_GAME_IN_PROGRESS" | "DOTA_GAMERULES_STATE_PRE_GAME"
            )
            && self.vision_capture_available;
        if (live_state.game_state == "menu"
            && !self.previous_game_state.is_empty()
            && self.previous_game_state != "menu")
            || entered_new_match
        {
            self.manual_enemy_heroes.clear();
            self.manual_ally_heroes.clear();
            self.manual_draft_override = false;
            self.manual_enemy_search.clear();
            self.manual_ultimate_levels.clear();
            // F5 is deliberately a match-local override. A fresh match starts
            // from the saved pre-queue position selected in Settings.
            self.manual_position = None;
            self.draft_picker_open = false;
            self.vision_candidates.clear();
            self.vision_rx = None;
            self.vision_capture_in_progress = false;
            self.vision_clear_confirmation = false;
            if finished_captured_match && !entered_new_match {
                self.vision_review_open = true;
                self.vision_review_slot = None;
                self.vision_review_search.clear();
                self.load_vision_review_images();
                self.vision_status = "Vision: после игры проверь реальные карточки".to_string();
                self.set_click_through(false);
            } else {
                self.vision_capture_available = false;
                self.vision_review_open = false;
                self.vision_review_slot = None;
                self.vision_review_search.clear();
                self.vision_review_images.clear();
                self.vision_review_labels.clear();
                self.vision_clear_confirmation = false;
                self.vision_status = "Vision: по кнопке, один кадр".to_string();
            }
        }
        self.previous_game_state = live_state.game_state.clone();
        self.previous_match_id = live_state.match_id.clone();

        // Auto-hide when Dota 2 is not the active foreground window
        // (Overlay will not show over browser, Discord, Telegram, etc.)
        if is_dota_running_or_connected(live_state.is_connected) && !is_dota_focused() && !self.test_mode_enabled {
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
            return;
        }

        let effective_clock_time = self.simulated_clock_time.unwrap_or(live_state.clock_time);

        match ROSHAN_ACTION.swap(0, Ordering::Relaxed) {
            1 => self.set_roshan_ally(true),
            2 => self.set_roshan_ally(false),
            3 => self.reset_roshan(),
            _ => {}
        }

        // The native overlay is click-through. A low-level mouse hook turns a click on a
        // registered ultimate chip into a cooldown event without activating this window.
        let clicked_ultimate = ULTIMATE_CLICK_INDEX.swap(-1, Ordering::Relaxed);
        if clicked_ultimate >= 1000 {
            let index = (clicked_ultimate - 1000) as usize;
            if let Some(spell) = self.tracked_ultimates.get_mut(index) {
                let next_tier = match spell.ultimate_level.unwrap_or(0) {
                    0 | 3 => 1,
                    tier => tier + 1,
                };
                self.manual_ultimate_levels.insert(tracked_spell_key(spell), next_tier);
                apply_ultimate_tier(spell, next_tier);
            }
        } else if let Some(spell) = self.tracked_ultimates.get_mut(clicked_ultimate.max(0) as usize)
            .filter(|_| clicked_ultimate >= 0)
        {
            spell.on_cooldown_until = match spell.on_cooldown_until {
                Some(until) if until > effective_clock_time => None,
                _ => Some(effective_clock_time + spell.base_cd),
            };
        }

        if let Some(rx) = &self.vision_rx {
            if let Ok(result) = rx.try_recv() {
                self.vision_capture_in_progress = false;
                self.vision_status = result.status;
                self.vision_candidates = result.candidates;
                self.vision_enemy_is_right = result.enemy_is_right;
                self.vision_capture_available = result.has_pending_slot_crops;
                self.vision_review_open = false;
                self.vision_review_slot = None;
                self.vision_review_search.clear();
                self.vision_review_images.clear();
                self.vision_review_labels.clear();
                // F10 is a full snapshot, not an append action. Replacing the
                // lists prevents stale early-draft heroes from surviving after
                // all five slots have changed.
                self.manual_enemy_heroes = result.auto_accepted_enemies.into_iter().take(5).collect();
                self.manual_ally_heroes = result.auto_accepted_allies.into_iter().take(5).collect();
                self.manual_draft_override = !self.manual_enemy_heroes.is_empty()
                    || !self.manual_ally_heroes.is_empty();
                self.vision_rx = None;
            }
        }

        // GSI is authoritative by default. An explicit in-game correction is
        // the exception: it remains authoritative until this match ends.
        let active_enemies: Vec<String> = if self.manual_draft_override {
            self.manual_enemy_heroes.clone()
        } else if !enemies.is_empty() {
            enemies
        } else if !self.manual_enemy_heroes.is_empty() {
            self.manual_enemy_heroes.clone()
        } else if self.test_mode_enabled {
            self.test_enemies.clone()
        } else {
            Vec::new()
        };
        let mut active_allies = if self.manual_draft_override {
            self.manual_ally_heroes.clone()
        } else {
            live_state.ally_heroes.clone()
        };
        if !self.manual_draft_override {
            for hero in &self.manual_ally_heroes {
                if !active_allies.iter().any(|existing| existing == hero) {
                    active_allies.push(hero.clone());
                }
            }
        }

        // Trigger draft counter analysis if enemies changed
        if active_enemies.is_empty() {
            if !self.all_counters.is_empty() || !self.situational_items.is_empty() {
                self.all_counters.clear();
                self.situational_items.clear();
                self.draft_weaknesses.clear();
                self.last_enemy_signature.clear();
                self.tracked_ultimates.clear();
            }
        } else {
            // Rank is input to the recommendation model, so changing it must invalidate
            // the result even if the draft itself has not changed.
            let mut analysis_signature = active_enemies.clone();
            let mut ally_signature = active_allies.clone();
            ally_signature.sort();
            analysis_signature.extend(ally_signature.into_iter().map(|hero| format!("__ally={hero}")));
            analysis_signature.push(format!("__rank={:?}", if self.settings.enable_rank_filter { self.settings.selected_rank } else { crate::models::RankBracket::All }));
            if analysis_signature != self.last_enemy_signature {
            self.last_enemy_signature = analysis_signature;
            self.is_analyzing_draft = true;
            self.draft_analysis_error = None;

            // Every ultimate comes from current OpenDota hero ability constants,
            // not a hand-maintained subset of "important" heroes.
            let mut new_spells = self.api.try_lock().ok()
                .map(|api| api.ultimates_for_enemies(&active_enemies))
                .unwrap_or_default();
            for spell in &mut new_spells {
                let manual_tier = self.manual_ultimate_levels.get(&tracked_spell_key(spell)).copied();
                if let Some(tier) = manual_tier {
                    // A user who saw the level-up knows the ult tier more
                    // precisely than player-perspective GSI can report it.
                    apply_ultimate_tier(spell, tier);
                } else if let Some((_, level)) = live_state.enemy_levels.iter().find(|(hero, _)| {
                    spell.ability_key.starts_with(hero.strip_prefix("npc_dota_hero_").unwrap_or(hero))
                }) {
                    spell.enemy_level = Some(*level);
                    // GSI exposes enemy hero level in spectator/coach payloads, but not the
                    // enemy's exact skill point allocation. Use the highest ult tier that
                    // is legal at this hero level, and label it as an estimate in the HUD.
                    let tier = if *level >= 18 { 2 } else if *level >= 12 { 1 } else { 0 };
                    if let Some(cooldown) = spell.cooldowns.get(tier) {
                        spell.base_cd = *cooldown;
                    }
                    spell.ultimate_level = Some((tier + 1) as u8);
                }
            }
            let mut updated_ultimates = Vec::new();
            for mut spell in new_spells {
                if let Some(existing) = self.tracked_ultimates.iter().find(|s| s.hero_name == spell.hero_name && s.spell_name == spell.spell_name) {
                    spell.on_cooldown_until = existing.on_cooldown_until;
                }
                updated_ultimates.push(spell);
            }
            self.tracked_ultimates = updated_ultimates.clone();
            if let Ok(mut st) = self.state.lock() {
                st.tracked_spells = updated_ultimates;
            }

            let api_clone = self.api.clone();
            let enemies_clone = active_enemies.clone();
            let allies_clone = active_allies.clone();
            let rank = if self.settings.enable_rank_filter {
                self.settings.selected_rank
            } else {
                crate::models::RankBracket::All
            };
            let signature_for_result = self.last_enemy_signature.clone();
            let (tx, rx) = std::sync::mpsc::channel();
            self.draft_rx = Some(rx);
            self.tokio_handle.spawn(async move {
                let result = tokio::time::timeout(std::time::Duration::from_secs(12), async {
                    let mut api = api_clone.lock().await;
                    let counters = Advisor::recommend_counter_picks(&mut api, &enemies_clone, &allies_clone, rank).await;
                    let situational_items = Advisor::get_situational_items(&api, &enemies_clone);
                    let weaknesses = Advisor::analyze_draft_weaknesses(&api, &enemies_clone);
                    (counters, situational_items, weaknesses)
                }).await;
                let message = match result {
                    Ok((counters, situational_items, weaknesses)) => DraftAnalysisResult::Ready {
                        signature: signature_for_result,
                        counters,
                        situational_items,
                        weaknesses,
                    },
                    Err(_) => DraftAnalysisResult::TimedOut {
                        signature: signature_for_result,
                    },
                };
                let _ = tx.send(message);
            });
            }
        }

        if let Some(rx) = &self.draft_rx {
            if let Ok(result) = rx.try_recv() {
                match result {
                    DraftAnalysisResult::Ready { signature, counters, situational_items, weaknesses } => {
                        if signature == self.last_enemy_signature {
                            self.all_counters = counters;
                            self.situational_items = situational_items;
                            self.draft_weaknesses = weaknesses;
                        }
                    }
                    DraftAnalysisResult::TimedOut { signature } => {
                        if signature == self.last_enemy_signature {
                            self.draft_analysis_error = Some("Сеть не ответила за 12 с: используй ручные пики или повтори после драфта.".to_string());
                        }
                    }
                }
                self.draft_rx = None;
                self.is_analyzing_draft = false;
            }
        }

        // Determine active hero for builds: only if player chose a hero or testing
        let active_hero_name = if let Some(h) = my_hero {
            Some(h)
        } else if self.test_mode_enabled {
            Some("npc_dota_hero_storm_spirit".to_string())
        } else {
            None
        };

        if let Some(hero_name) = active_hero_name {
            let build_request = format!("{}:{:?}", hero_name, self.settings.build_source);
            if build_request != self.last_hero_for_builds {
                self.last_hero_for_builds = build_request;
                self.start_items.clear();
                self.early_items.clear();
                self.mid_items.clear();
                self.late_items.clear();
                if self.settings.build_source == crate::models::BuildSource::Dota2ProTracker {
                    // D2PT is protected by a browser-only Cloudflare challenge.  Do not
                    // pretend that an HTML scrape is stable or role-specific data.
                    self.is_loading_builds = false;
                } else {
                    self.is_loading_builds = true;
                let api_clone = self.api.clone();
                let h_name = hero_name.clone();

                let (tx, rx) = std::sync::mpsc::channel();
                self.builds_rx = Some(rx);
                self.tokio_handle.spawn(async move {
                    let mut api = api_clone.lock().await;
                    let hero_id = api.find_hero(&h_name).map(|h| h.id);
                    let pop = if let Some(id) = hero_id {
                        api.get_item_popularity(id).await
                    } else {
                        crate::models::ItemPopularity::default()
                    };
                    let (start, early, mid, late) = Advisor::get_hero_build(&api, &h_name, &pop);
                    let _ = tx.send((start, early, mid, late));
                });
                }
            }
        } else {
            if !self.start_items.is_empty() || !self.early_items.is_empty() || !self.mid_items.is_empty() || !self.late_items.is_empty() {
                self.start_items.clear();
                self.early_items.clear();
                self.mid_items.clear();
                self.late_items.clear();
                self.last_hero_for_builds.clear();
                self.is_loading_builds = false;
            }
        }

        if let Some(rx) = &self.builds_rx {
            if let Ok((start, early, mid, late)) = rx.try_recv() {
                self.start_items = start;
                self.early_items = early;
                self.mid_items = mid;
                self.late_items = late;
                self.builds_rx = None;
                self.is_loading_builds = false;
            }
        }

        // Position must come from the queue selection, not from a hero's most
        // popular role or an old player profile. Those guesses caused support
        // builds to appear in mid and carried across games. F5 may override
        // the saved pre-queue position for the current match only.
        let effective_position = self.manual_position.unwrap_or(self.settings.preferred_position);

        if self.settings.build_source == crate::models::BuildSource::Dota2ProTracker {
            if D2PT_LINK_REQUESTED.swap(false, Ordering::Relaxed) {
                let hero_name = live_state
                    .my_hero_name
                    .as_deref()
                    .unwrap_or("npc_dota_hero_storm_spirit");
                open_external_url(&d2pt_url(hero_name, effective_position));
            }
        } else if let Ok(mut link) = D2PT_LINK_HITBOX.lock() {
            *link = None;
        }

        let screen_rect = ui.max_rect();
        let screen_w = screen_rect.width();
        let screen_h = screen_rect.height();
        self.apply_passive_scroll(screen_w, screen_h);

        let is_in_menu = live_state.game_state == "menu"
            || (!live_state.is_connected && !self.test_mode_enabled)
            || (live_state.clock_time < -30 && live_state.enemy_heroes.is_empty() && live_state.my_hero_name.is_none() && live_state.game_state.is_empty());

        let is_draft_active = (live_state.game_state == "DOTA_GAMERULES_STATE_HERO_SELECTION"
            || live_state.game_state == "DOTA_GAMERULES_STATE_STRATEGY_TIME")
            && !is_in_menu;
        let is_match_active = (live_state.game_state == "DOTA_GAMERULES_STATE_GAME_IN_PROGRESS"
            || live_state.game_state == "DOTA_GAMERULES_STATE_PRE_GAME")
            && !is_draft_active
            && !is_in_menu;
        let show_hud = !self.vision_capture_in_progress && !self.focus_mode;
        let show_notifications = !self.vision_capture_in_progress;

        // Every card is visible at Strategy Time. Capture exactly once on that
        // state transition so a slow counter-pick request can never make F10
        // the only chance to preserve the real draft cards.
        if entered_strategy_time {
            self.start_vision_scan(&live_state);
        }

        if VISION_SCAN_TRIGGERED.swap(false, Ordering::SeqCst) {
            if is_draft_active {
                self.start_vision_scan(&live_state);
            } else {
                self.vision_status = "Vision: F10 работает только во время выбора героя".to_string();
            }
        }

        // The overlay is normally click-through. The small left-panel button
        // is hit-tested globally, then opens a purpose-built draft drawer;
        // entering a hero never requires opening Settings.
        if DRAFT_PICKER_REQUESTED.swap(false, Ordering::Relaxed) && (is_draft_active || is_match_active) {
            self.open_pick_editor(&live_state);
        }
        if !(is_draft_active || is_match_active) && self.draft_picker_open {
            self.draft_picker_open = false;
            self.set_click_through(!self.is_settings_open);
        }

        // 1. Render Top Status Bar
        if show_hud && (self.settings.show_top_bar || self.is_settings_open) {
            self.render_top_bar(&ctx, &live_state, effective_position, effective_clock_time, is_match_active);
        }
        if show_hud && (is_match_active || self.test_mode_enabled) {
            self.render_tactical_side_stack(&ctx, effective_clock_time, screen_h);
        }

        // 2. Left Panel (Draft & Counters):
        // Only shown during draft or test mode! During active match, left panel is hidden to give 100% clean vision of the lane.
        let should_show_left_panel = show_hud && self.settings.show_left_panel && (self.test_mode_enabled || is_draft_active);
        if should_show_left_panel {
            self.render_left_panel(&ctx, &live_state, &active_enemies, effective_position, screen_h);
        } else if let Ok(mut hitbox) = DRAFT_PICKER_HITBOX.lock() {
            *hitbox = None;
        }
        if self.draft_picker_open && (is_draft_active || is_match_active) {
            self.render_draft_picker(&ctx, screen_w, &live_state);
        }
        if self.vision_review_open && is_in_menu {
            self.render_vision_post_game_review(&ctx);
        }

        // 3. Right Panel (Builds & Situational Items):
        // Shown during match (when hero is chosen), or during draft/test mode!
        let should_show_right_panel = show_hud && self.settings.show_right_panel && (self.test_mode_enabled || is_draft_active || (is_match_active && live_state.my_hero_name.is_some()));
        if should_show_right_panel {
            self.render_right_panel(
                &ctx,
                &live_state,
                screen_w,
                screen_h,
                effective_position,
                !active_enemies.is_empty(),
            );
        }

        // 4. Center Tactical Alert Banner (ONLY during active match and NOT during draft!)
        let should_show_alerts = show_notifications && self.settings.show_center_alerts && ((is_match_active && effective_clock_time >= -25) || self.simulated_clock_time.is_some());
        if should_show_alerts {
            let alerts = get_tactical_alerts(effective_clock_time, effective_position, &self.settings, &live_state);
            render_tactical_alert_banner(&ctx, &alerts, screen_w);

            // Audio & TTS synthesizer announcement
            if self.settings.enable_tts && !alerts.is_empty() {
                let top = &alerts[0];
                // The countdown changes every second; it is not a new event. The target
                // timestamp in parentheses is stable across the whole alert window.
                let target = top.title.rsplit_once('(')
                    .and_then(|(_, tail)| tail.split_once(')'))
                    .map(|(value, _)| value)
                    .unwrap_or(&top.badge_text);
                let alert_key = format!("{}:{target}", top.title.split_whitespace().next().unwrap_or("alert"));
                if alert_key != self.last_tts_text {
                    self.last_tts_text = alert_key;
                    speak_tactical_text(&short_tts_phrase(&top.title));
                }
            } else {
                self.last_tts_text.clear();
            }
        }

        // 5. Camp Timing Pill (Stacks & Pulls): subtle 1-line chip on left side, NOT covering center!
        if show_notifications && self.settings.show_camp_pill && ((is_match_active && effective_clock_time >= 30) || self.simulated_clock_time.is_some()) {
            if let Some(camp_alert) = get_subtle_camp_alert(effective_clock_time, effective_position, &self.settings) {
                render_subtle_camp_pill(&ctx, &camp_alert);
            }
        }

        // 6. Interactive Settings Window (F8)
        if show_hud && self.is_settings_open {
            self.render_settings_window(&ctx, screen_w, screen_h);
        }

        ctx.request_repaint_after(std::time::Duration::from_millis(60));
    }
}

impl OverlayApp {
    fn render_settings_window(&mut self, ctx: &egui::Context, screen_w: f32, screen_h: f32) {
        let win_w = 480.0;
        let win_h = 560.0;
        let win_x = ((screen_w - win_w) / 2.0).max(20.0);
        let win_y = ((screen_h - win_h) / 2.0).max(20.0);
        let live_state = self.state.lock().ok().map(|state| state.clone()).unwrap_or_default();
        let is_live_match = matches!(
            live_state.game_state.as_str(),
            "DOTA_GAMERULES_STATE_GAME_IN_PROGRESS" | "DOTA_GAMERULES_STATE_PRE_GAME"
        );
        let mut open_match_picker = false;

        egui::Area::new(egui::Id::new("hud_settings_window"))
            .fixed_pos(egui::pos2(win_x, win_y))
            .show(ctx, |ui| {
                egui::Frame::NONE
                    .fill(egui::Color32::from_rgba_unmultiplied(12, 16, 26, 250))
                    .stroke(egui::Stroke::new(1.5, egui::Color32::from_rgb(240, 204, 75)))
                    .corner_radius(8)
                    .inner_margin(egui::Margin::symmetric(14, 12))
                    .show(ui, |ui| {
                        ui.set_width(win_w);
                        ui.set_max_height(win_h);

                        // Header
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new("Settings • Dota 2 Assistant")
                                    .color(egui::Color32::from_rgb(240, 204, 75))
                                    .strong()
                                    .size(13.5),
                            );

                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.add(egui::Button::new(egui::RichText::new("[X]").strong().size(12.0).color(egui::Color32::WHITE)).corner_radius(4)).clicked() {
                                    self.toggle_settings();
                                }
                            });
                        });

                        ui.add_space(2.0);
                        ui.label(
                            egui::RichText::new("Hotkeys: [F5] Role, [F6] Focus alerts, [F7] Hide all, [F8] Settings, [F9] Roshan, [F10] Vision.")
                                .color(egui::Color32::from_rgb(148, 163, 184))
                                .size(11.0),
                        );
                        ui.separator();

                        egui::ScrollArea::vertical().max_height(win_h - 110.0).show(ui, |ui| {
                            // Section 1: Rank selection
                            ui.label(
                                egui::RichText::new("Rank Filter & Meta")
                                    .color(egui::Color32::from_rgb(234, 179, 8))
                                    .strong()
                                    .size(12.0),
                            );
                            ui.add_space(3.0);

                            if ui.checkbox(&mut self.settings.enable_rank_filter, "Включить фильтрацию данных по рейтингу игрока").changed() {
                                self.settings.save();
                            }

                            if self.settings.enable_rank_filter {
                                ui.horizontal(|ui| {
                                    ui.label(egui::RichText::new("Выбранный ранг:").size(11.0).color(egui::Color32::from_rgb(203, 213, 225)));
                                    egui::ComboBox::from_id_salt("rank_selector_box")
                                        .selected_text(self.settings.selected_rank.title_ru())
                                        .show_ui(ui, |ui| {
                                            for r in [
                                                crate::models::RankBracket::All,
                                                crate::models::RankBracket::Herald,
                                                crate::models::RankBracket::Guardian,
                                                crate::models::RankBracket::Crusader,
                                                crate::models::RankBracket::Archon,
                                                crate::models::RankBracket::Legend,
                                                crate::models::RankBracket::Ancient,
                                                crate::models::RankBracket::Divine,
                                                crate::models::RankBracket::Immortal,
                                            ] {
                                                if ui.selectable_value(&mut self.settings.selected_rank, r, r.title_ru()).clicked() {
                                                    self.settings.save();
                                                    self.last_enemy_signature.clear();
                                                }
                                            }
                                        });
                                });

                                ui.horizontal(|ui| {
                                    ui.label(egui::RichText::new("Источник билдов:").size(11.0).color(egui::Color32::from_rgb(203, 213, 225)));
                                    egui::ComboBox::from_id_salt("build_source_selector")
                                        .selected_text(self.settings.build_source.title())
                                        .show_ui(ui, |ui| {
                                            for source in [
                                                crate::models::BuildSource::OpenDotaAggregate,
                                                crate::models::BuildSource::Dota2ProTracker,
                                            ] {
                                                if ui.selectable_value(&mut self.settings.build_source, source, source.title()).clicked() {
                                                    self.settings.save();
                                                    self.last_hero_for_builds.clear();
                                                    self.start_items.clear();
                                                    self.early_items.clear();
                                                    self.mid_items.clear();
                                                    self.late_items.clear();
                                                }
                                            }
                                        });
                                });
                                if self.settings.build_source == crate::models::BuildSource::Dota2ProTracker {
                                    ui.label(egui::RichText::new("D2PT: role-specific данные открываются по ссылке в правой панели.")
                                        .size(9.5)
                                        .color(egui::Color32::from_rgb(148, 163, 184)));
                                }

                            }

                            ui.add_space(8.0);
                            ui.separator();
                            ui.add_space(4.0);

                            ui.label(
                                egui::RichText::new("Роль перед поиском")
                                    .color(egui::Color32::from_rgb(96, 165, 250))
                                    .strong()
                                    .size(12.0),
                            );
                            ui.add_space(3.0);
                            ui.horizontal(|ui| {
                                ui.label(egui::RichText::new("Роль Ranked Roles:").size(11.0).color(egui::Color32::from_rgb(203, 213, 225)));
                                egui::ComboBox::from_id_salt("preferred_position_selector")
                                    .selected_text(self.settings.preferred_position.title_ru())
                                    .show_ui(ui, |ui| {
                                        for position in [
                                            PlayerPosition::Pos1Carry,
                                            PlayerPosition::Pos2Mid,
                                            PlayerPosition::Pos3Offlane,
                                            PlayerPosition::Pos4SoftSupport,
                                            PlayerPosition::Pos5HardSupport,
                                        ] {
                                            if ui.selectable_value(
                                                &mut self.settings.preferred_position,
                                                position,
                                                position.title_ru(),
                                            ).changed() {
                                                // Selecting a role in Settings is an explicit
                                                // request to make it active now as well.
                                                self.manual_position = None;
                                                self.settings.save();
                                                self.last_enemy_signature.clear();
                                                self.last_hero_for_builds.clear();
                                            }
                                        }
                                    });
                            });
                            ui.label(
                                egui::RichText::new("Применяется автоматически при новой игре. F5 временно меняет роль только до конца текущего матча.")
                                    .size(9.5)
                                    .color(egui::Color32::from_rgb(148, 163, 184)),
                            );

                            if is_live_match {
                                ui.add_space(7.0);
                                ui.separator();
                                ui.add_space(4.0);
                                ui.label(
                                    egui::RichText::new("Пики текущей игры")
                                        .color(egui::Color32::from_rgb(248, 113, 113))
                                        .strong()
                                        .size(12.0),
                                );
                                ui.label(
                                    egui::RichText::new("Если Vision или GSI ошиблись, исправь врагов и союзников без выхода из матча.")
                                        .size(9.5)
                                        .color(egui::Color32::from_rgb(203, 213, 225)),
                                );
                                if ui.add(egui::Button::new(
                                    egui::RichText::new("Исправить пики").size(10.0),
                                ).corner_radius(4)).clicked() {
                                    open_match_picker = true;
                                }
                            }

                            ui.add_space(8.0);
                            ui.separator();
                            ui.add_space(4.0);

                            // Section 2: Audio & TTS
                            ui.label(
                                egui::RichText::new("Voice Alerts (TTS)")
                                    .color(egui::Color32::from_rgb(192, 132, 252))
                                    .strong()
                                    .size(12.0),
                            );
                            ui.add_space(3.0);

                            if ui.checkbox(&mut self.settings.enable_tts, "Включить голосовые подсказки (Windows TTS синтез речи)").changed() {
                                self.settings.save();
                            }

                            ui.add_space(8.0);
                            ui.separator();
                            ui.add_space(4.0);

                            // Section 3: Tactical Timers
                            ui.label(
                                egui::RichText::new("Tactical Timers")
                                    .color(egui::Color32::from_rgb(96, 165, 250))
                                    .strong()
                                    .size(12.0),
                            );
                            ui.add_space(3.0);

                            if ui.checkbox(&mut self.settings.alert_roshan, "Панель Рошана и Аегиса (таймер 5 мин + окно 8-11 мин) [F9]").changed() {
                                self.settings.save();
                            }
                            if ui.checkbox(&mut self.settings.alert_ultimates, "Трекер ультимейтов всех врагов").changed() {
                                self.settings.save();
                            }
                            if ui.checkbox(&mut self.settings.alert_farm_benchmark, "Бенчмарк фарма крипов (каждые 5 минут по роли)").changed() {
                                self.settings.save();
                            }
                            if ui.checkbox(&mut self.settings.alert_catapults, "Катапульты (5:00, 10:00... за 30 сек до выхода)").changed() {
                                self.settings.save();
                            }
                            if ui.checkbox(&mut self.settings.alert_neutrals, "Neutral Items (T1 с 0:00, T2-T5 по таймингам)").changed() {
                                self.settings.save();
                            }
                            if ui.checkbox(&mut self.settings.alert_wisdom_runes, "Руны мудрости (7:00, 14:00, 21:00...)").changed() {
                                self.settings.save();
                            }
                            if ui.checkbox(&mut self.settings.alert_tormentor, "Терзатель (20:00 бесплатный Shard)").changed() {
                                self.settings.save();
                            }
                            if ui.checkbox(&mut self.settings.alert_water_runes, "Руны воды (2:00 и 4:00 на миде)").changed() {
                                self.settings.save();
                            }
                            if ui.checkbox(&mut self.settings.alert_power_runes, "Активные руны реки (6:00, 8:00... каждые 2 мин)").changed() {
                                self.settings.save();
                            }
                            if ui.checkbox(&mut self.settings.alert_lotus, "Лотосы в прудах (каждые 3 минуты)").changed() {
                                self.settings.save();
                            }
                            if ui.checkbox(&mut self.settings.alert_bounty_runes, "Руны богатства (0:00 и каждые 3 минуты)").changed() {
                                self.settings.save();
                            }
                            if ui.checkbox(&mut self.settings.alert_stacks, "Стаки лесных лагерей (:53-:55 секунда)").changed() {
                                self.settings.save();
                            }
                            if ui.checkbox(&mut self.settings.alert_pulls, "Отводы крипов на линию (:15/:45 секунда)").changed() {
                                self.settings.save();
                            }

                            ui.add_space(8.0);
                            ui.separator();
                            ui.add_space(4.0);

                            // Section 4: UI Elements
                            ui.label(
                                egui::RichText::new("Interface (UI)")
                                    .color(egui::Color32::from_rgb(74, 222, 128))
                                    .strong()
                                    .size(12.0),
                            );
                            ui.add_space(3.0);

                            if ui.checkbox(&mut self.settings.show_top_bar, "Верхняя статусная полоса (HUD Top Bar)").changed() {
                                self.settings.save();
                            }
                            if ui.checkbox(&mut self.settings.show_top_timers, "Таймеры рун и терзателя слева по центру").changed() {
                                self.settings.save();
                            }
                            if ui.checkbox(&mut self.settings.show_left_panel, "Левая панель (Meta-герои и контрпики во время драфта)").changed() {
                                self.settings.save();
                            }
                            if ui.checkbox(&mut self.settings.show_right_panel, "Правая панель (Сборки предметов и контр-айтемы)").changed() {
                                self.settings.save();
                            }
                            if ui.checkbox(&mut self.settings.show_center_alerts, "Центральный баннер тактических алертов").changed() {
                                self.settings.save();
                            }
                            if ui.checkbox(&mut self.settings.show_camp_pill, "Плашка стаков и отводов").changed() {
                                self.settings.save();
                            }
                        });

                        ui.add_space(8.0);
                        ui.separator();
                        ui.add_space(4.0);

                        ui.horizontal(|ui| {
                            if ui.add(egui::Button::new(egui::RichText::new("Save & Close [F8]").strong().color(egui::Color32::BLACK)).fill(egui::Color32::from_rgb(74, 222, 128)).corner_radius(4)).clicked() {
                                self.toggle_settings();
                            }

                            if ui.add(egui::Button::new(egui::RichText::new("Reset Defaults").size(11.0).color(egui::Color32::from_rgb(203, 213, 225))).corner_radius(4)).clicked() {
                                self.settings = OverlaySettings::default();
                                self.settings.save();
                            }
                        });
                    });
            });

        if open_match_picker {
            // Close settings first so the compact editor is the only
            // interactive layer above a live match.
            self.is_settings_open = false;
            self.settings.save();
            self.open_pick_editor(&live_state);
        }

    }

    fn render_top_bar(
        &mut self,
        ctx: &egui::Context,
        live_state: &LiveGameState,
        pos: PlayerPosition,
        clock_time: i32,
        is_match_active: bool,
    ) {
        egui::Area::new(egui::Id::new("hud_top_bar"))
            .fixed_pos(egui::pos2(16.0, 10.0))
            .show(ctx, |ui| {
                egui::Frame::NONE
                    .fill(egui::Color32::from_rgba_unmultiplied(10, 14, 22, 135))
                    .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(40, 55, 75, 120)))
                    .corner_radius(6)
                    .inner_margin(egui::Margin::symmetric(9, 5))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new("LaneTheory")
                                    .color(egui::Color32::from_rgb(235, 195, 80))
                                    .strong()
                                    .size(12.5),
                            );

                            ui.separator();

                            if live_state.is_connected {
                                let (phase, color) = match live_state.game_state.as_str() {
                                    "menu" => ("Menu • Searching / Lobby", egui::Color32::from_rgb(148, 163, 184)),
                                    "DOTA_GAMERULES_STATE_HERO_SELECTION" => ("Draft Phase", egui::Color32::from_rgb(240, 204, 75)),
                                    "DOTA_GAMERULES_STATE_STRATEGY_TIME" => ("Strategy Time", egui::Color32::from_rgb(240, 204, 75)),
                                    "DOTA_GAMERULES_STATE_PRE_GAME" => ("Pre-Game", egui::Color32::from_rgb(96, 165, 250)),
                                    "DOTA_GAMERULES_STATE_GAME_IN_PROGRESS" => ("Live Match", egui::Color32::from_rgb(74, 222, 128)),
                                    "" => ("GSI Connected", egui::Color32::from_rgb(74, 222, 128)),
                                    other => (other, egui::Color32::from_rgb(74, 222, 128)),
                                };
                                ui.label(egui::RichText::new(phase).color(color).strong().size(12.0));
                            } else {
                                ui.label(egui::RichText::new("Waiting for GSI")
                                    .color(egui::Color32::from_rgb(148, 163, 184)).size(12.0));
                                ui.label(egui::RichText::new(&live_state.gsi_listener_status)
                                    .color(egui::Color32::from_rgb(148, 163, 184)).size(9.0));
                            }

                            ui.separator();

                            // Player profile info (if loaded)
                            if let Some(prof) = &live_state.player_profile {
                                ui.label(egui::RichText::new(&prof.personaname).color(egui::Color32::WHITE).strong().size(12.0));
                                ui.label(egui::RichText::new(&prof.rank_label).color(egui::Color32::from_rgb(240, 204, 75)).strong().size(11.5));
                                ui.label(egui::RichText::new(format!("({:.1}% WR)", prof.winrate)).color(egui::Color32::from_rgb(74, 222, 128)).strong().size(11.5));
                                ui.separator();
                            }

                            // Active Position Indicator
                            let pos_text = format!("{} [F5]", pos.title_en());
                            ui.label(egui::RichText::new(pos_text).color(egui::Color32::from_rgb(96, 165, 250)).strong().size(12.0));

                            // Live Game Stats (CS, Denies, Net Worth, Buyback)
                            if is_match_active {
                                ui.separator();
                                let cs_text = format!("CS: {}/{}", live_state.last_hits, live_state.denies);
                                ui.label(egui::RichText::new(cs_text).color(egui::Color32::from_rgb(226, 232, 240)).strong().size(11.5));

                                if live_state.net_worth > 0 {
                                    let nw_text = format!("NW: {}g", live_state.net_worth);
                                    ui.label(egui::RichText::new(nw_text).color(egui::Color32::from_rgb(234, 179, 8)).strong().size(11.5));
                                }

                                if live_state.buyback_cooldown > 0 {
                                    ui.label(egui::RichText::new(format!("Buyback: CD {}s", live_state.buyback_cooldown)).color(egui::Color32::from_rgb(239, 68, 68)).strong().size(11.5));
                                } else if live_state.buyback_cost > 0 && clock_time >= 900 {
                                    if live_state.gold >= live_state.buyback_cost {
                                        ui.label(egui::RichText::new("Buyback: Ready").color(egui::Color32::from_rgb(34, 197, 94)).strong().size(11.5));
                                    } else {
                                        let diff = live_state.buyback_cost - live_state.gold;
                                        ui.label(egui::RichText::new(format!("No Buyback: -{}g", diff)).color(egui::Color32::from_rgb(239, 68, 68)).strong().size(11.5));
                                    }
                                }
                            }

                            // If not in match, show test mode toggle
                            if !is_match_active {
                                ui.separator();
                                let test_btn_text = if self.test_mode_enabled { "Hide Test [X]" } else { "Draft Test" };
                                if ui.add(egui::Button::new(egui::RichText::new(test_btn_text).size(11.0).color(egui::Color32::from_rgb(240, 204, 75))).corner_radius(3)).clicked() {
                                    self.test_mode_enabled = !self.test_mode_enabled;
                                    if !self.test_mode_enabled {
                                        self.simulated_clock_time = None;
                                    }
                                }
                            }

                            ui.separator();
                            let settings_label = if self.is_settings_open { "Close [F8]" } else { "Settings [F8]" };
                            if ui.add(egui::Button::new(egui::RichText::new(settings_label).size(11.0).color(egui::Color32::from_rgb(147, 197, 253))).corner_radius(3)).clicked() {
                                self.toggle_settings();
                            }

                            ui.separator();
                            ui.label(egui::RichText::new("Focus alerts [F6] · Hide all [F7]").color(egui::Color32::from_rgb(156, 163, 175)).size(11.0));
                        });
                    });
            });
    }

    fn render_tactical_side_stack(&mut self, ctx: &egui::Context, clock_time: i32, screen_h: f32) {
        let mut hitboxes = Vec::new();
        let mut direct_click = None;
        let mut direct_level_click = None;
        let mut roshan_hitboxes = Vec::new();
        let mut direct_roshan_action = None;
        let roshan = self.state.lock().ok().map(|state| state.roshan.clone()).unwrap_or_default();
        let is_day = self.state.lock().ok().map(|state| state.is_day).unwrap_or(true);
        egui::Area::new(egui::Id::new("hud_tactical_side_stack"))
            .fixed_pos(egui::pos2(16.0, (screen_h * 0.32).clamp(220.0, 460.0)))
            .show(ctx, |ui| {
                egui::Frame::NONE
                    .fill(egui::Color32::from_rgba_unmultiplied(10, 14, 22, 150))
                    .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(40, 55, 75, 145)))
                    .corner_radius(6)
                    .inner_margin(egui::Margin::symmetric(7, 6))
                    .show(ui, |ui| {
                        ui.set_width(164.0);
                        if self.settings.show_top_timers {
                            render_badge(ui, "Objectives", egui::Color32::from_rgb(147, 197, 253), 11.0);
                            let display = |name: &str, seconds: i32, color: egui::Color32, ui: &mut egui::Ui| {
                                let seconds = seconds.max(0);
                                ui.label(egui::RichText::new(format!("{name}  {}:{:02}", seconds / 60, seconds % 60)).color(color).strong().size(11.0));
                            };
                            if clock_time < 0 {
                                display("Bounty", -clock_time, egui::Color32::from_rgb(250, 204, 21), ui);
                            } else {
                                let timers = UpcomingTimers::calculate(clock_time);
                                if let Some(water) = timers.water {
                                    display("Water", water, egui::Color32::from_rgb(56, 189, 248), ui);
                                } else if let Some(power) = timers.power {
                                    display("Power", power, egui::Color32::from_rgb(251, 146, 60), ui);
                                }
                                display("Wisdom", timers.wisdom, egui::Color32::from_rgb(250, 204, 21), ui);
                                display("Lotus", timers.lotus, egui::Color32::from_rgb(244, 114, 182), ui);
                                if let Some(tormentor) = timers.tormentor {
                                    display("Tormentor", tormentor, egui::Color32::from_rgb(192, 132, 252), ui);
                                }
                            }
                        }

                        if self.settings.alert_roshan {
                            ui.add_space(5.0);
                            render_badge(ui, "Roshan", egui::Color32::from_rgb(245, 158, 11), 11.0);
                            if roshan.is_tracking {
                                let aegis_rem = roshan.aegis_expires_at() - clock_time;
                                if aegis_rem > 0 {
                                    let (owner, color) = if roshan.is_ally {
                                        ("Our Aegis", egui::Color32::from_rgb(74, 222, 128))
                                    } else {
                                        ("Enemy Aegis", egui::Color32::from_rgb(248, 113, 113))
                                    };
                                    ui.horizontal(|ui| {
                                        ui.add(egui::Image::new(AEGIS_IMAGE_URL)
                                            .fit_to_exact_size(egui::vec2(18.0, 13.0)).corner_radius(2));
                                        ui.label(egui::RichText::new(format!("{owner}  {}:{:02}", aegis_rem / 60, aegis_rem % 60))
                                            .color(color).strong().size(10.5));
                                    });
                                } else {
                                    let early = (roshan.respawn_early_at() - clock_time).max(0);
                                    let late = (roshan.respawn_late_at() - clock_time).max(0);
                                    let pit = if is_day { "Bot" } else { "Top" };
                                    ui.horizontal(|ui| {
                                        ui.add(egui::Image::new(ROSHAN_IMAGE_URL)
                                            .fit_to_exact_size(egui::vec2(16.0, 16.0)).corner_radius(2));
                                        ui.label(egui::RichText::new(format!("{pit}  {}:{:02}–{}:{:02}", early / 60, early % 60, late / 60, late % 60))
                                            .color(egui::Color32::from_rgb(251, 191, 36)).strong().size(10.5));
                                    });
                                }
                            } else {
                                ui.label(egui::RichText::new("F9: Our · выбери сторону ниже")
                                    .size(9.0).color(egui::Color32::from_rgb(148, 163, 184)));
                            }
                            ui.horizontal(|ui| {
                                let our = ui.add(egui::Button::new(egui::RichText::new("Our").size(9.5))
                                    .fill(if roshan.is_tracking && roshan.is_ally { egui::Color32::from_rgb(22, 101, 52) } else { egui::Color32::TRANSPARENT })
                                    .corner_radius(3));
                                if our.clicked() { direct_roshan_action = Some(1); }
                                roshan_hitboxes.push(response_screen_hitbox(1, our.rect, ctx.pixels_per_point()));
                                let enemy = ui.add(egui::Button::new(egui::RichText::new("Enemy").size(9.5))
                                    .fill(if roshan.is_tracking && !roshan.is_ally { egui::Color32::from_rgb(127, 29, 29) } else { egui::Color32::TRANSPARENT })
                                    .corner_radius(3));
                                if enemy.clicked() { direct_roshan_action = Some(2); }
                                roshan_hitboxes.push(response_screen_hitbox(2, enemy.rect, ctx.pixels_per_point()));
                                if roshan.is_tracking {
                                    let reset = ui.add(egui::Button::new(egui::RichText::new("×").size(10.0)).corner_radius(3)).on_hover_text("Сбросить таймер Рошана");
                                    if reset.clicked() { direct_roshan_action = Some(3); }
                                    roshan_hitboxes.push(response_screen_hitbox(3, reset.rect, ctx.pixels_per_point()));
                                }
                            });
                        }

                        if self.settings.show_ultimates_panel && self.settings.alert_ultimates {
                            if self.settings.show_top_timers {
                                ui.add_space(5.0);
                            }
                            render_badge(ui, "Enemy ultimates", egui::Color32::from_rgb(248, 113, 113), 11.0);
                            if self.tracked_ultimates.is_empty() {
                                ui.label(egui::RichText::new("Нет пиков: добавь их в draft picker").color(egui::Color32::from_rgb(148, 163, 184)).size(9.5));
                            } else {
                                ui.label(egui::RichText::new("ЛКМ: CD/сброс · R: уровень").color(egui::Color32::from_rgb(148, 163, 184)).size(9.0));
                            }
                            for (index, spell) in self.tracked_ultimates.iter().enumerate() {
                                let (text, color) = match spell.on_cooldown_until {
                                    Some(until) if until > clock_time => (format!("{}  {}с", spell.localized_spell, until - clock_time), egui::Color32::from_rgb(248, 113, 113)),
                                    _ => (format!("{}  ready", spell.localized_spell), egui::Color32::from_rgb(74, 222, 128)),
                                };
                                let level_hint = spell.ultimate_level
                                    .map(|tier| format!("  R{tier}"))
                                    .or_else(|| spell.enemy_level.map(|level| format!("  L{level} · R?")))
                                    .unwrap_or_else(|| "  R?".to_string());
                                ui.horizontal(|ui| {
                                    if !spell.ability_image.is_empty() {
                                        ui.add(egui::Image::new(&spell.ability_image).fit_to_exact_size(egui::vec2(18.0, 14.0)).corner_radius(2));
                                    }
                                    let response = ui.add(egui::Button::new(egui::RichText::new(text).color(color).strong().size(10.5)).min_size(egui::vec2(99.0, 19.0)).corner_radius(3));
                                    if response.clicked() {
                                        direct_click = Some(index as i32);
                                    }
                                    hitboxes.push(response_screen_hitbox(index as i32, response.rect, ctx.pixels_per_point()));
                                    let level_response = ui.add(egui::Button::new(
                                        egui::RichText::new(level_hint).size(9.5).color(egui::Color32::from_rgb(216, 180, 254))
                                    ).min_size(egui::vec2(35.0, 19.0)).corner_radius(3)).on_hover_text("Меняет уровень ультимейта: R1 → R2 → R3");
                                    if level_response.clicked() {
                                        direct_level_click = Some(index as i32);
                                    }
                                    hitboxes.push(response_screen_hitbox(1000 + index as i32, level_response.rect, ctx.pixels_per_point()));
                                });
                            }
                        }
                    });
            });
        if let Ok(mut targets) = ULTIMATE_HITBOXES.lock() {
            *targets = hitboxes;
        }
        if let Ok(mut targets) = ROSHAN_HITBOXES.lock() {
            *targets = roshan_hitboxes;
        }
        if let Some(index) = direct_level_click {
            ULTIMATE_CLICK_INDEX.store(1000 + index, Ordering::Relaxed);
        } else if let Some(index) = direct_click {
            ULTIMATE_CLICK_INDEX.store(index, Ordering::Relaxed);
        }
        if let Some(action) = direct_roshan_action {
            ROSHAN_ACTION.store(action, Ordering::Relaxed);
        }
    }

    fn render_left_panel(
        &mut self,
        ctx: &egui::Context,
        live_state: &LiveGameState,
        enemies: &[String],
        pos: PlayerPosition,
        screen_h: f32,
    ) {
        let panel_h = (screen_h - 140.0).clamp(460.0, 720.0);
        let using_manual_fallback = self.manual_draft_override
            || (live_state.enemy_heroes.is_empty() && !self.manual_enemy_heroes.is_empty());
        let draft_status = if using_manual_fallback {
            format!("Ручные пики: {} · правка игрока до конца матча", enemies.len())
        } else if enemies.is_empty() && !self.test_mode_enabled {
            "All Pick: GSI не отдаёт пики врага · добавить: Пики +".to_string()
        } else {
            live_state.draft_status.clone()
        };
        let mut picker_hitbox = None;
        let mut open_picker = false;

        egui::Area::new(egui::Id::new("hud_left_panel"))
            .fixed_pos(egui::pos2(14.0, 44.0))
            .show(ctx, |ui| {
                egui::Frame::NONE
                    .fill(egui::Color32::from_rgba_unmultiplied(10, 14, 22, 135))
                    .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(40, 55, 75, 120)))
                    .corner_radius(6)
                    .inner_margin(egui::Margin::symmetric(8, 6))
                    .show(ui, |ui| {
                        ui.set_width(280.0);
                        ui.set_max_height(panel_h);

                        ui.label(
                            egui::RichText::new(draft_status)
                                .size(9.5)
                                .color(if enemies.is_empty() {
                                    egui::Color32::from_rgb(251, 146, 60)
                                } else {
                                    egui::Color32::from_rgb(74, 222, 128)
                                }),
                        );
                        ui.add_space(2.0);

                        let vision_color = if self.vision_status.contains("не удалось")
                            || self.vision_status.contains("не подготовлены")
                            || self.vision_status.contains("неполный")
                            || self.vision_status.contains("ошибок загрузки")
                        {
                            egui::Color32::from_rgb(248, 113, 113)
                        } else if self.vision_rx.is_some() {
                            egui::Color32::from_rgb(251, 191, 36)
                        } else {
                            egui::Color32::from_rgb(147, 197, 253)
                        };
                        ui.label(egui::RichText::new(format!("{} · F10", self.vision_status))
                            .size(9.0).color(vision_color));

                        ui.horizontal(|ui| {
                            let response = ui.add(
                                egui::Button::new(
                                    egui::RichText::new("Пики +")
                                        .size(10.0)
                                        .color(egui::Color32::from_rgb(248, 210, 110)),
                                )
                                .corner_radius(4),
                            );
                            if response.clicked() {
                                open_picker = true;
                            }
                            picker_hitbox = Some(response_screen_hitbox(0, response.rect, ctx.pixels_per_point()));
                            ui.label(egui::RichText::new("авто GSI / ручной ввод в драфте")
                                .size(9.0).color(egui::Color32::from_rgb(148, 163, 184)));
                        });

                        // Enemy picks received from GSI.
                        if !enemies.is_empty() {
                            ui.add_space(3.0);
                            ui.horizontal(|ui| {
                                let title = if using_manual_fallback { "Ручные пики:" } else { "Пики врага:" };
                                ui.label(egui::RichText::new(title).strong().size(10.0).color(egui::Color32::from_rgb(248, 113, 113)));
                                for enemy in enemies.iter().take(5) {
                                    let clean = enemy.strip_prefix("npc_dota_hero_").unwrap_or(enemy);
                                    let cdn_name = advisor::hero_cdn_name(clean);
                                    let img_url = format!(
                                        "https://cdn.cloudflare.steamstatic.com/apps/dota2/images/dota_react/heroes/{cdn_name}.png"
                                    );
                                    ui.add(
                                        egui::Image::new(&img_url)
                                            .fit_to_exact_size(egui::vec2(28.0, 16.0))
                                            .corner_radius(2),
                                    );
                                }
                            });
                        }

                        let mut visible_allies = live_state.ally_heroes.clone();
                        for ally in &self.manual_ally_heroes {
                            if !visible_allies.iter().any(|existing| existing == ally) {
                                visible_allies.push(ally.clone());
                            }
                        }
                        if !visible_allies.is_empty() {
                            ui.horizontal(|ui| {
                                ui.label(egui::RichText::new("Союзники (малый вес):")
                                    .size(9.5).color(egui::Color32::from_rgb(96, 165, 250)));
                                for ally in visible_allies.iter().take(5) {
                                    let clean = ally.strip_prefix("npc_dota_hero_").unwrap_or(ally);
                                    let cdn_name = advisor::hero_cdn_name(clean);
                                    let img_url = format!(
                                        "https://cdn.cloudflare.steamstatic.com/apps/dota2/images/dota_react/heroes/{cdn_name}.png"
                                    );
                                    ui.add(egui::Image::new(&img_url)
                                        .fit_to_exact_size(egui::vec2(28.0, 16.0)).corner_radius(2));
                                }
                            });
                        }

                        ui.add_space(4.0);

                        if enemies.is_empty() {
                            // --- DRAFT WITHOUT DETECTED ENEMIES: Show Meta Heroes ---
                            let meta_title = format!("Meta • {}", pos.title_en());
                            render_badge(ui, &meta_title, egui::Color32::from_rgb(240, 204, 75), 12.0);
                            ui.add_space(3.0);

                            let scroll = egui::ScrollArea::vertical()
                                .id_salt("draft_meta_scroll")
                                .vertical_scroll_offset(self.left_panel_scroll)
                                .max_height(panel_h - 100.0)
                                .show(ui, |ui| {
                                let meta_heroes = self.meta_heroes(pos);
                                for (short_name, loc_name, wr) in meta_heroes {
                                    render_meta_hero_row(ui, &short_name, &loc_name, wr);
                                    ui.add_space(2.0);
                                }
                            });
                            self.left_panel_scroll = scroll.state.offset.y;
                        } else {
                            // --- DRAFT WITH ENEMIES: Show Counter-picks & Weaknesses ---
                            let rank_tag = if self.settings.enable_rank_filter {
                                format!(" [{}]", self.settings.selected_rank.title_en())
                            } else {
                                "".to_string()
                            };
                            let title = format!("Counter picks{} • {}", rank_tag, pos.title_en());
                            ui.horizontal(|ui| {
                                render_badge(ui, &title, egui::Color32::from_rgb(240, 204, 75), 12.0);
                                if self.is_analyzing_draft {
                                    ui.spinner();
                                }
                            });
                            ui.add_space(3.0);

                            if let Some(error) = &self.draft_analysis_error {
                                ui.label(egui::RichText::new(error)
                                    .size(9.0).color(egui::Color32::from_rgb(251, 146, 60)));
                                ui.add_space(3.0);
                            }

                            // Draft Weaknesses section
                            if !self.draft_weaknesses.is_empty() {
                                render_badge(ui, "Enemy weaknesses", egui::Color32::from_rgb(248, 113, 113), 11.0);
                                ui.add_space(2.0);
                                for w in &self.draft_weaknesses {
                                    ui.label(egui::RichText::new(w).color(egui::Color32::from_rgb(254, 202, 202)).size(10.0));
                                }
                                ui.add_space(3.0);
                            }

                            let position_counters = advisor::Advisor::filter_counters_by_position(&self.all_counters, pos);

                            let scroll = egui::ScrollArea::vertical()
                                .id_salt("draft_counters_scroll")
                                .vertical_scroll_offset(self.left_panel_scroll)
                                .max_height(panel_h - 100.0)
                                .show(ui, |ui| {
                                if position_counters.is_empty() {
                                    let message = if self.is_analyzing_draft {
                                        "Анализ контрпиков..."
                                    } else {
                                        "Нет контрпиков с достаточной пригодностью к выбранной роли."
                                    };
                                    ui.label(egui::RichText::new(message).color(egui::Color32::from_rgb(148, 163, 184)).size(11.0));
                                } else {
                                    for (index, rec) in position_counters.iter().enumerate() {
                                        render_counter_card(ui, rec, advisor::Advisor::position_confidence(&rec.hero, pos), index == 0);
                                        ui.add_space(3.0);
                                    }
                                }

                                ui.add_space(6.0);
                                let meta_title = format!("Meta • {}", pos.title_en());
                                render_badge(ui, &meta_title, egui::Color32::from_rgb(147, 197, 253), 11.5);
                                ui.add_space(2.0);

                                let meta_heroes = self.meta_heroes(pos);
                                for (short_name, loc_name, wr) in meta_heroes.into_iter().take(4) {
                                    render_meta_hero_row(ui, &short_name, &loc_name, wr);
                                    ui.add_space(2.0);
                                }
                            });
                            self.left_panel_scroll = scroll.state.offset.y;
                        }
                    });
            });

        if let Ok(mut hitbox) = DRAFT_PICKER_HITBOX.lock() {
            *hitbox = picker_hitbox;
        }
        if open_picker {
            self.draft_picker_open = true;
            self.set_click_through(false);
        }
    }

    fn render_draft_picker(
        &mut self,
        ctx: &egui::Context,
        screen_w: f32,
        live_state: &LiveGameState,
    ) {
        let is_live_match = matches!(
            live_state.game_state.as_str(),
            "DOTA_GAMERULES_STATE_GAME_IN_PROGRESS" | "DOTA_GAMERULES_STATE_PRE_GAME"
        );
        let picker_options = self.manual_picker_options();
        let mut add_pick: Option<(String, bool)> = None;
        let mut remove_enemy: Option<usize> = None;
        let mut remove_ally: Option<usize> = None;
        let mut clear_picks = false;
        let mut set_ultimate_tier: Option<(String, u8)> = None;
        let mut close = ctx.input(|input| input.key_pressed(egui::Key::Escape));
        let mut run_vision = false;
        let drawer_x = 302.0_f32.min((screen_w - 328.0).max(14.0));

        egui::Area::new(egui::Id::new("draft_picker_drawer"))
            .fixed_pos(egui::pos2(drawer_x, 44.0))
            .show(ctx, |ui| {
                egui::Frame::NONE
                    .fill(egui::Color32::from_rgba_unmultiplied(10, 14, 22, 245))
                    .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(248, 113, 113)))
                    .corner_radius(6)
                    .inner_margin(egui::Margin::symmetric(9, 8))
                    .show(ui, |ui| {
                        ui.set_width(310.0);
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new(if is_live_match { "Пики текущей игры" } else { "Пики драфта" })
                                .strong().size(12.5).color(egui::Color32::from_rgb(226, 232, 240)));
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.button(egui::RichText::new("Закрыть").size(10.0)).clicked() {
                                    close = true;
                                }
                            });
                        });
                        ui.label(egui::RichText::new(
                            if is_live_match {
                                "Игра идёт: исправления здесь сразу пересчитывают контр-советы и трекер ультимейтов."
                            } else if live_state.enemy_heroes.is_empty() {
                                "All Pick: F10 заменяет весь Vision-снимок; ниже можно исправить обе команды."
                            } else {
                                "GSI уже прислал пики; ручные нужны только для исправления."
                            }
                        ).size(9.5).color(egui::Color32::from_rgb(203, 213, 225)));
                        ui.add_space(4.0);
                        if is_live_match {
                            ui.label(egui::RichText::new("Vision-снимок уже сохраняется автоматически в Strategy Time; в матче доступны только правки пиков.")
                                .size(9.0).color(egui::Color32::from_rgb(148, 163, 184)));
                        } else {
                            ui.horizontal(|ui| {
                                if ui.add_enabled(self.vision_rx.is_none(), egui::Button::new(
                                    egui::RichText::new("Vision scan [F10]").size(10.0)
                                ).corner_radius(4)).clicked() {
                                    run_vision = true;
                                }
                                if self.vision_rx.is_some() {
                                    ui.spinner();
                                }
                                ui.label(egui::RichText::new(&self.vision_status).size(9.0)
                                    .color(egui::Color32::from_rgb(147, 197, 253)));
                            });
                        }
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new("Найти:").size(10.5));
                            ui.add_sized([185.0, 22.0], egui::TextEdit::singleline(&mut self.manual_enemy_search)
                                .hint_text("Pudge / Пудж"));
                            if ui.add_enabled(!self.manual_enemy_heroes.is_empty() || !self.manual_ally_heroes.is_empty(),
                                egui::Button::new(egui::RichText::new("Очистить").size(9.5))).clicked() {
                                clear_picks = true;
                            }
                        });

                        if self.manual_enemy_search.trim().is_empty() {
                            ui.label(egui::RichText::new("Введи часть имени героя.")
                                .size(9.5).color(egui::Color32::from_rgb(148, 163, 184)));
                        } else if picker_options.is_empty() {
                            ui.label(egui::RichText::new("Совпадений нет.")
                                .size(9.5).color(egui::Color32::from_rgb(148, 163, 184)));
                        } else {
                            ui.horizontal_wrapped(|ui| {
                                for hero in &picker_options {
                                    ui.label(egui::RichText::new(&hero.localized_name).size(10.0));
                                    if ui.add_enabled(self.manual_enemy_heroes.len() < 5,
                                        egui::Button::new(egui::RichText::new("В+").size(9.0)).corner_radius(4)
                                    ).on_hover_text("Добавить как врага").clicked() {
                                        add_pick = Some((hero.name.clone(), true));
                                    }
                                    if ui.add_enabled(self.manual_ally_heroes.len() < 5,
                                        egui::Button::new(egui::RichText::new("С+").size(9.0)).corner_radius(4)
                                    ).on_hover_text("Добавить как союзника").clicked() {
                                        add_pick = Some((hero.name.clone(), false));
                                    }
                                }
                            });
                        }

                        if !self.manual_enemy_heroes.is_empty() {
                            ui.add_space(4.0);
                            ui.horizontal_wrapped(|ui| {
                                ui.label(egui::RichText::new("Враги:").size(10.0)
                                    .color(egui::Color32::from_rgb(248, 113, 113)));
                                for (index, name) in self.manual_enemy_heroes.iter().enumerate() {
                                    let label = name.strip_prefix("npc_dota_hero_").unwrap_or(name).replace('_', " ");
                                    if ui.add(egui::Button::new(egui::RichText::new(format!("{label} ×")).size(10.0))
                                        .corner_radius(12)).on_hover_text("Убрать героя").clicked() {
                                        remove_enemy = Some(index);
                                    }
                                }
                            });
                        }
                        if !self.manual_ally_heroes.is_empty() {
                            ui.horizontal_wrapped(|ui| {
                                ui.label(egui::RichText::new("Союзники:").size(10.0)
                                    .color(egui::Color32::from_rgb(96, 165, 250)));
                                for (index, name) in self.manual_ally_heroes.iter().enumerate() {
                                    let label = name.strip_prefix("npc_dota_hero_").unwrap_or(name).replace('_', " ");
                                    if ui.add(egui::Button::new(egui::RichText::new(format!("{label} ×")).size(10.0))
                                        .corner_radius(12)).on_hover_text("Убрать союзника").clicked() {
                                        remove_ally = Some(index);
                                    }
                                }
                            });
                        }

                        if !self.vision_candidates.is_empty() {
                            ui.add_space(4.0);
                            ui.label(egui::RichText::new("Vision: проверь распознавание")
                                .size(10.0).color(egui::Color32::from_rgb(147, 197, 253)));
                            ui.horizontal_wrapped(|ui| {
                                for candidate in self.vision_candidates.iter().take(10) {
                                    let selected = if candidate.is_enemy { self.manual_enemy_heroes.iter() } else { self.manual_ally_heroes.iter() }
                                        .any(|hero| hero == &candidate.hero_name);
                                    let side = if candidate.is_enemy { "В" } else { "С" };
                                    let is_full = if candidate.is_enemy { self.manual_enemy_heroes.len() >= 5 } else { self.manual_ally_heroes.len() >= 5 };
                                    let caption = format!(
                                        "{side}: {} · d{}/64",
                                        candidate.localized_name,
                                        candidate.distance
                                    );
                                    if ui.add_enabled(!selected && !is_full,
                                        egui::Button::new(egui::RichText::new(caption).size(9.5)).corner_radius(4)
                                    ).on_hover_text(format!(
                                        "Слот {} · Hamming distance {}/64 · запас до следующего героя: {} бит",
                                        candidate.slot + 1,
                                        candidate.distance,
                                        candidate.margin
                                    )).clicked() {
                                        add_pick = Some((candidate.hero_name.clone(), candidate.is_enemy));
                                    }
                                }
                            });
                        }

                        ui.add_space(6.0);
                        ui.separator();
                        ui.add_space(3.0);
                        ui.label(egui::RichText::new("Уровень ультимейта")
                            .strong().size(11.0).color(egui::Color32::from_rgb(192, 132, 252)));
                        if self.tracked_ultimates.is_empty() {
                            ui.label(egui::RichText::new("Добавь героя с ключевым ультимейтом — здесь появится выбор R I / II / III.")
                                .size(9.5).color(egui::Color32::from_rgb(148, 163, 184)));
                        } else {
                            for spell in &self.tracked_ultimates {
                                let key = tracked_spell_key(spell);
                                let current = self.manual_ultimate_levels.get(&key)
                                    .copied().or(spell.ultimate_level).unwrap_or(1);
                                ui.horizontal(|ui| {
                                    ui.label(egui::RichText::new(format!("{}:", spell.localized_spell))
                                        .size(10.0).color(egui::Color32::from_rgb(226, 232, 240)));
                                    for (tier, label) in [(1_u8, "R I"), (2, "R II"), (3, "R III")] {
                                        let selected = current == tier;
                                        if ui.add(egui::Button::new(egui::RichText::new(label).size(9.5))
                                            .fill(if selected { egui::Color32::from_rgb(124, 58, 237) } else { egui::Color32::TRANSPARENT })
                                            .corner_radius(3)).clicked() {
                                            set_ultimate_tier = Some((key.clone(), tier));
                                        }
                                    }
                                });
                            }
                        }
                        ui.label(egui::RichText::new("R? означает, что уровень не подтверждён. Выбор меняет cooldown для таймера.")
                            .size(8.8).color(egui::Color32::from_rgb(148, 163, 184)));
                    });
            });

        if clear_picks {
            self.manual_enemy_heroes.clear();
            self.manual_ally_heroes.clear();
            self.manual_draft_override = false;
            self.manual_enemy_search.clear();
            self.manual_ultimate_levels.clear();
            self.vision_candidates.clear();
        } else if let Some(index) = remove_enemy {
            self.manual_enemy_heroes.remove(index);
            self.manual_draft_override = true;
        } else if let Some(index) = remove_ally {
            self.manual_ally_heroes.remove(index);
            self.manual_draft_override = true;
        } else if let Some((hero_name, is_enemy)) = add_pick {
            let picks = if is_enemy {
                &mut self.manual_enemy_heroes
            } else {
                &mut self.manual_ally_heroes
            };
            let can_add = picks.len() < 5 && !picks.iter().any(|name| name == &hero_name);
            if can_add {
                picks.push(hero_name.clone());
                self.manual_draft_override = true;
            }
            self.manual_enemy_search.clear();
        }
        if let Some((key, tier)) = set_ultimate_tier {
            self.manual_ultimate_levels.insert(key.clone(), tier);
            for spell in &mut self.tracked_ultimates {
                if tracked_spell_key(spell) == key {
                    apply_ultimate_tier(spell, tier);
                }
            }
        }
        if run_vision {
            self.start_vision_scan(live_state);
        }
        if close {
            self.draft_picker_open = false;
            self.set_click_through(!self.is_settings_open);
        }
    }

    /// Displays the literal cards captured by F10 only after a completed
    /// game. This keeps hero selection free of training controls while still
    /// giving Vision unambiguous, human-verified examples afterwards.
    fn render_vision_post_game_review(&mut self, ctx: &egui::Context) {
        let picker_options = self.vision_review_slot
            .map(|_| self.hero_picker_options(&self.vision_review_search, true))
            .unwrap_or_default();
        let mut open = self.vision_review_open;
        let mut select_slot = None;
        let mut save_label: Option<(usize, String, String)> = None;
        let mut clear_variants = false;
        let mut close = ctx.input(|input| input.key_pressed(egui::Key::Escape));

        egui::Window::new("Vision · проверка после игры")
            .id(egui::Id::new("vision_post_game_review"))
            .open(&mut open)
            .resizable(false)
            .collapsible(false)
            .default_width(720.0)
            .default_pos(egui::pos2(420.0, 150.0))
            .frame(egui::Frame::new()
                .fill(egui::Color32::from_rgb(10, 14, 22))
                .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(91, 33, 182)))
                .corner_radius(8)
                .inner_margin(egui::Margin::symmetric(12, 10)))
            .show(ctx, |ui| {
                ui.label(egui::RichText::new("Это реальные кропы последнего F10, а не догадки Vision.")
                    .strong().size(12.0).color(egui::Color32::from_rgb(216, 180, 254)));
                ui.label(egui::RichText::new("Выбери карточку, посмотри на неё и подпиши настоящего героя. Можно пропустить всё, что не хочешь подтверждать.")
                    .size(10.5).color(egui::Color32::from_rgb(203, 213, 225)));
                ui.add_space(7.0);

                for row in 0..2 {
                    ui.horizontal(|ui| {
                        for column in 0..5 {
                            let slot = row * 5 + column;
                            let is_enemy = if self.vision_enemy_is_right { slot >= 5 } else { slot < 5 };
                            let side = if is_enemy { "Враг" } else { "Союзник" };
                            let selected = self.vision_review_slot == Some(slot);
                            egui::Frame::NONE
                                .fill(if selected {
                                    egui::Color32::from_rgb(54, 30, 92)
                                } else {
                                    egui::Color32::from_rgb(20, 27, 40)
                                })
                                .stroke(egui::Stroke::new(
                                    if selected { 1.5 } else { 1.0 },
                                    if selected { egui::Color32::from_rgb(196, 181, 253) } else { egui::Color32::from_rgb(51, 65, 85) },
                                ))
                                .corner_radius(5)
                                .inner_margin(egui::Margin::same(3))
                                .show(ui, |ui| {
                                    ui.vertical(|ui| {
                                        ui.label(egui::RichText::new(format!("{side} {}", slot % 5 + 1)).size(9.0)
                                            .color(if is_enemy { egui::Color32::from_rgb(252, 165, 165) } else { egui::Color32::from_rgb(147, 197, 253) }));
                                        if let Some(bytes) = self.vision_review_images.get(&slot) {
                                            let response = ui.add(
                                                egui::Image::from_bytes(
                                                    format!("bytes://lanetheory-vision-review-{slot}"),
                                                    bytes.clone(),
                                                )
                                                .fit_to_exact_size(egui::vec2(120.0, 84.0))
                                                .sense(egui::Sense::click()),
                                            );
                                            if response.on_hover_text("Выбрать эту карточку для подписи").clicked() {
                                                select_slot = Some(slot);
                                            }
                                        } else {
                                            ui.add_sized([120.0, 84.0], egui::Label::new(
                                                egui::RichText::new("Кроп не найден").size(9.0)
                                                    .color(egui::Color32::from_rgb(148, 163, 184)),
                                            ));
                                        }
                                        if let Some(label) = self.vision_review_labels.get(&slot) {
                                            ui.label(egui::RichText::new(label).size(9.0)
                                                .color(egui::Color32::from_rgb(134, 239, 172)));
                                        } else {
                                            ui.label(egui::RichText::new("не подписано").size(9.0)
                                                .color(egui::Color32::from_rgb(148, 163, 184)));
                                        }
                                    });
                                });
                        }
                    });
                    ui.add_space(4.0);
                }

                ui.separator();
                if let Some(slot) = self.vision_review_slot {
                    let is_enemy = if self.vision_enemy_is_right { slot >= 5 } else { slot < 5 };
                    let side = if is_enemy { "врага" } else { "союзника" };
                    ui.label(egui::RichText::new(format!("Подпись для карточки {side} {}", slot % 5 + 1))
                        .strong().size(11.0));
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("Герой:").size(10.0));
                        ui.add_sized([245.0, 23.0], egui::TextEdit::singleline(&mut self.vision_review_search)
                            .hint_text("Pudge / Пудж"));
                    });
                    if self.vision_review_search.trim().is_empty() {
                        ui.label(egui::RichText::new("Введи часть имени, затем подтверди результат.")
                            .size(9.5).color(egui::Color32::from_rgb(148, 163, 184)));
                    } else if picker_options.is_empty() {
                        ui.label(egui::RichText::new("Совпадений нет.").size(9.5)
                            .color(egui::Color32::from_rgb(148, 163, 184)));
                    } else {
                        ui.horizontal_wrapped(|ui| {
                            for hero in &picker_options {
                                if ui.add(egui::Button::new(egui::RichText::new(format!("Запомнить {}", hero.localized_name))
                                    .size(9.5)).corner_radius(4)).clicked() {
                                    save_label = Some((slot, hero.name.clone(), hero.localized_name.clone()));
                                }
                            }
                        });
                    }
                } else {
                    ui.label(egui::RichText::new("Выбери любую карточку выше — изображение остаётся перед глазами.")
                        .size(10.0).color(egui::Color32::from_rgb(148, 163, 184)));
                }

                ui.add_space(5.0);
                ui.horizontal(|ui| {
                    if self.vision_clear_confirmation {
                        if ui.add(egui::Button::new(egui::RichText::new("Точно удалить обучение").size(9.5))
                            .fill(egui::Color32::from_rgb(127, 29, 29)).corner_radius(4)).clicked() {
                            clear_variants = true;
                        }
                    } else if ui.button(egui::RichText::new("Сбросить обучение").size(9.5)).clicked() {
                        self.vision_clear_confirmation = true;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button(egui::RichText::new("Готово").size(10.0)).clicked() {
                            close = true;
                        }
                    });
                });
            });

        if let Some(slot) = select_slot {
            self.vision_review_slot = Some(slot);
            self.vision_review_search.clear();
        }
        if let Some((slot, hero_name, localized_name)) = save_label {
            match crate::vision::save_verified_slot_variant(slot, &hero_name) {
                Ok(()) => {
                    self.vision_review_labels.insert(slot, localized_name);
                    self.vision_review_search.clear();
                    self.vision_status = format!("Vision: карточка {} сохранена · следующий F10 учтёт образ", slot % 5 + 1);
                }
                Err(error) => self.vision_status = format!("Vision: не удалось сохранить образ ({error})"),
            }
        }
        if clear_variants {
            match crate::vision::clear_verified_variants() {
                Ok(()) => {
                    self.vision_review_labels.clear();
                    self.vision_clear_confirmation = false;
                    self.vision_status = "Vision: локальное обучение сброшено".to_string();
                }
                Err(error) => self.vision_status = format!("Vision: не удалось сбросить обучение ({error})"),
            }
        }
        if close || !open {
            self.vision_review_open = false;
            self.vision_review_slot = None;
            self.vision_review_search.clear();
            self.set_click_through(!self.is_settings_open);
        }
    }

    fn render_right_panel(
        &mut self,
        ctx: &egui::Context,
        live_state: &LiveGameState,
        screen_w: f32,
        screen_h: f32,
        pos: PlayerPosition,
        has_enemies: bool,
    ) {
        let panel_h = (screen_h - 140.0).clamp(460.0, 720.0);
        let panel_x = (screen_w - 286.0).max(400.0);

        egui::Area::new(egui::Id::new("hud_right_panel"))
            .fixed_pos(egui::pos2(panel_x, 44.0))
            .show(ctx, |ui| {
                egui::Frame::NONE
                    .fill(egui::Color32::from_rgba_unmultiplied(10, 14, 22, 135))
                    .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(40, 55, 75, 120)))
                    .corner_radius(6)
                    .inner_margin(egui::Margin::symmetric(8, 6))
                    .show(ui, |ui| {
                        ui.set_width(270.0);
                        ui.set_max_height(panel_h);

                        if !has_enemies && live_state.my_hero_name.is_none() {
                            // --- PRE-MATCH ROLE GUIDE ---
                            let guide_title = format!("Guide • {}", pos.title_en());
                            render_badge(ui, &guide_title, egui::Color32::from_rgb(96, 165, 250), 12.0);
                            ui.add_space(4.0);

                            egui::Frame::NONE
                                .fill(egui::Color32::from_rgba_unmultiplied(14, 18, 26, 125))
                                .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(45, 58, 78, 110)))
                                .corner_radius(5)
                                .inner_margin(egui::Margin::symmetric(8, 6))
                                .show(ui, |ui| {
                                    let tips: &[(&str, &str)] = match pos {
                                        PlayerPosition::Pos1Carry => &[
                                            ("0-15 мин:", "Фокус на ластхитах, безопасный фарм малого леса, держи ТП на готовности."),
                                            ("15-25 мин:", "Выбивание ключевого слота (BKB/Manta/Diffusal), подключение к командным пушам."),
                                            ("25+ мин:", "Контроль Рошана, правильное позиционирование сзади, всегда держи выкуп (Buyback)."),
                                        ],
                                        PlayerPosition::Pos2Mid => &[
                                            ("2, 4, 6 мин:", "Контроль активных рун и рун воды, стакай малый кемп нейтралов."),
                                            ("6-12 мин:", "Получение ультимейта — активный выход на боковые линии в смоках."),
                                            ("15+ мин:", "Диктовка темпа драфта, контроль центра карты и выбивание сейв-кнопок врагов."),
                                        ],
                                        PlayerPosition::Pos3Offlane => &[
                                            ("0-10 мин:", "Агрессивный прессинг вражеского керри, снос Т1 вышки на легкой линии соперника."),
                                            ("10-20 мин:", "Раш Blink Dagger или аур (Pipe/Crimson), открытие пространства для фарма керри."),
                                            ("20+ мин:", "Первая линия инициации, контроль опасных саппортов и танкование урона."),
                                        ],
                                        PlayerPosition::Pos4SoftSupport => &[
                                            ("0-7 мин:", "Блок большого кемпа, помощь мидеру на рунах, контроль руны Мудрости на 7:00."),
                                            ("7-20 мин:", "Смок-ганги с мидером/оффлейнером, агрессивные варды во вражеском лесу."),
                                            ("20+ мин:", "Сейв-артефакты (Glimmer/Force/Lotus), сбивание ченнелинг-способностей."),
                                        ],
                                        PlayerPosition::Pos5HardSupport => &[
                                            ("0-10 мин:", "Отводы на малый кемп, размены по HP с врагами, полная защита своего керри."),
                                            ("7:00+:", "Контроль рун Мудрости и лотосов, постоянный обзор ключевых подходов к линиям."),
                                            ("15+ мин:", "Позиция сзади в файтах, сейв тиммейтов кнопками и предметами."),
                                        ],
                                    };

                                    for (timing, tip) in tips {
                                        ui.horizontal(|ui| {
                                            ui.label(egui::RichText::new(*timing).strong().color(egui::Color32::from_rgb(240, 204, 75)).size(11.0));
                                        });
                                        ui.label(egui::RichText::new(*tip).color(egui::Color32::from_rgb(226, 232, 240)).size(10.5));
                                        ui.add_space(3.0);
                                    }
                                });

                            ui.add_space(6.0);
                            render_badge(ui, "Item build", egui::Color32::from_rgb(148, 163, 184), 11.5);
                            ui.add_space(3.0);
                            egui::Frame::NONE
                                .fill(egui::Color32::from_rgba_unmultiplied(14, 18, 26, 125))
                                .corner_radius(5)
                                .inner_margin(egui::Margin::symmetric(8, 6))
                                .show(ui, |ui| {
                                    ui.label(
                                        egui::RichText::new("Популярные предметы по стадиям матча появятся здесь после выбора героя.")
                                            .color(egui::Color32::from_rgb(148, 163, 184))
                                            .size(11.0),
                                    );
                                });
                        } else {
                            // --- DRAFT OR MATCH ACTIVE ---
                            let scroll = egui::ScrollArea::vertical()
                                .id_salt("builds_scroll")
                                .vertical_scroll_offset(self.right_panel_scroll)
                                .max_height(panel_h - 16.0)
                                .show(ui, |ui| {
                                // In-match neutral item & buyback state
                                if live_state.my_hero_name.is_some() {
                                    if let Some(neutral) = &live_state.my_neutral_item {
                                        let clean_n = neutral.strip_prefix("item_").unwrap_or(neutral);
                                        let n_text = format!("Neutral item: {clean_n}");
                                        render_badge(ui, &n_text, egui::Color32::from_rgb(168, 85, 247), 11.5);
                                    } else if live_state.clock_time >= 0 {
                                        render_badge(ui, "Neutral item: empty (T1 from 0:00)", egui::Color32::from_rgb(239, 68, 68), 11.5);
                                    }
                                    ui.add_space(3.0);
                                }

                                // 1. Situational Items Header & Cards
                                if !self.situational_items.is_empty() {
                                    render_badge(ui, "Situational items", egui::Color32::from_rgb(235, 195, 80), 12.0);
                                    ui.add_space(3.0);

                                    for item in self.situational_items.iter().take(8) {
                                        render_situational_item_card(ui, item);
                                        ui.add_space(3.0);
                                    }
                                    ui.add_space(6.0);
                                }

                                // 2. Builds for Current / Selected Hero (All 4 stages: Start, Early, Mid, Late)
                                if live_state.my_hero_name.is_some() || self.test_mode_enabled {
                                    let current_hero_label = if let Some(h) = &live_state.my_hero_name {
                                        let clean = h.strip_prefix("npc_dota_hero_").unwrap_or(h);
                                        format!("Build: {clean}")
                                    } else {
                                        "Build: Storm Spirit (test)".to_string()
                                    };

                                    render_badge(ui, &current_hero_label, egui::Color32::from_rgb(240, 204, 75), 12.0);
                                    ui.add_space(4.0);

                                    if self.settings.build_source == crate::models::BuildSource::Dota2ProTracker {
                                        let hero_name = live_state.my_hero_name.as_deref()
                                            .unwrap_or("npc_dota_hero_storm_spirit");
                                        let url = d2pt_url(hero_name, pos);
                                        ui.label(egui::RichText::new(format!("D2PT · {} · 7000+ MMR", pos.title_en()))
                                            .color(egui::Color32::from_rgb(96, 165, 250))
                                            .strong()
                                            .size(11.0));
                                        ui.label(egui::RichText::new("Роль-специфичные стартовые предметы, core-тайминги, skill build, таланты и matchup spectrum.")
                                            .color(egui::Color32::from_rgb(203, 213, 225))
                                            .size(10.0));
                                        let response = ui.link("Открыть D2PT build / counters ↗");
                                        if response.clicked() {
                                            D2PT_LINK_REQUESTED.store(true, Ordering::Relaxed);
                                        }
                                        if let Ok(mut link) = D2PT_LINK_HITBOX.lock() {
                                            *link = Some(response_screen_hitbox(-1, response.rect, ctx.pixels_per_point()));
                                        }
                                        ui.label(egui::RichText::new(url).monospace().size(8.5).color(egui::Color32::from_rgb(148, 163, 184)));
                                    } else {

                                    let has_any_items = !self.start_items.is_empty()
                                        || !self.early_items.is_empty()
                                        || !self.mid_items.is_empty()
                                        || !self.late_items.is_empty();

                                    if has_any_items {
                                        if !self.start_items.is_empty() {
                                            render_item_stage_group(ui, "Start", &self.start_items, egui::Color32::from_rgb(250, 204, 21));
                                            ui.add_space(4.0);
                                        }
                                        if !self.early_items.is_empty() {
                                            render_item_stage_group(ui, "Early  0-15 min", &self.early_items, egui::Color32::from_rgb(74, 222, 128));
                                            ui.add_space(4.0);
                                        }
                                        if !self.mid_items.is_empty() {
                                            render_item_stage_group(ui, "Mid  15-30 min", &self.mid_items, egui::Color32::from_rgb(96, 165, 250));
                                            ui.add_space(4.0);
                                        }
                                        if !self.late_items.is_empty() {
                                            render_item_stage_group(ui, "Late  30+ min", &self.late_items, egui::Color32::from_rgb(216, 180, 254));
                                        }
                                    } else if self.is_loading_builds {
                                        ui.horizontal(|ui| {
                                            ui.spinner();
                                            ui.label(egui::RichText::new("Загрузка сборки героя...").color(egui::Color32::from_rgb(148, 163, 184)).size(11.0));
                                        });
                                    } else {
                                        ui.label(egui::RichText::new("OpenDota: агрегат по всем ролям, не role-specific build.").color(egui::Color32::from_rgb(148, 163, 184)).size(11.0));
                                    }
                                    }
                                }
                            });
                            self.right_panel_scroll = scroll.state.offset.y;
                        }
                    });
            });
    }
}

// Sleek Badge Plate Helper for Titles and Headers
fn render_badge(ui: &mut egui::Ui, text: &str, text_color: egui::Color32, size: f32) {
    egui::Frame::NONE
        .fill(egui::Color32::from_rgba_unmultiplied(12, 16, 24, 135))
        .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(50, 65, 88, 120)))
        .corner_radius(4)
        .inner_margin(egui::Margin::symmetric(6, 3))
        .show(ui, |ui| {
            ui.label(egui::RichText::new(text).color(text_color).strong().size(size));
        });
}

// Hero Counter Card Renderer
fn render_counter_card(ui: &mut egui::Ui, rec: &RecommendedHero, role_confidence: f32, is_top_pick: bool) {
    let (priority_label, priority_color, fill, stroke) = if is_top_pick
        && rec.countered_enemies >= 3
        && rec.priority_score >= 85
        && rec.avg_winrate >= 53.5
    {
        (
            "ОЧЕНЬ РЕКОМЕНДУЕТСЯ",
            egui::Color32::from_rgb(250, 204, 21),
            egui::Color32::from_rgba_unmultiplied(64, 48, 12, 155),
            egui::Color32::from_rgb(234, 179, 8),
        )
    } else if rec.countered_enemies >= 1 && rec.priority_score >= 45 {
        (
            "РЕКОМЕНДУЕТСЯ",
            egui::Color32::from_rgb(74, 222, 128),
            egui::Color32::from_rgba_unmultiplied(12, 48, 32, 145),
            egui::Color32::from_rgb(34, 197, 94),
        )
    } else {
        (
            "СИТУАЦИОННО",
            egui::Color32::from_rgb(148, 163, 184),
            egui::Color32::from_rgba_unmultiplied(14, 18, 26, 130),
            egui::Color32::from_rgba_unmultiplied(45, 60, 80, 110),
        )
    };
    egui::Frame::NONE
        .fill(fill)
        .stroke(egui::Stroke::new(1.0, stroke))
        .corner_radius(5)
        .inner_margin(egui::Margin::symmetric(6, 4))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.add(
                    egui::Image::new(&rec.hero.image_url())
                        .fit_to_exact_size(egui::vec2(44.0, 25.0))
                        .corner_radius(3),
                );

                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(&rec.hero.localized_name)
                                .strong()
                                .size(12.5)
                                .color(egui::Color32::WHITE),
                        );

                        let adv_sign = if rec.advantage >= 0.0 { "+" } else { "" };
                        let adv_color = if rec.advantage >= 0.0 {
                            egui::Color32::from_rgb(74, 222, 128)
                        } else {
                            egui::Color32::from_rgb(248, 113, 113)
                        };

                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(
                                egui::RichText::new(format!("{} · {}/100", priority_label, rec.priority_score))
                                    .color(priority_color)
                                    .strong()
                                    .size(8.5),
                            );
                            ui.label(
                                egui::RichText::new(format!("{adv_sign}{:.1}%", rec.advantage))
                                    .color(adv_color)
                                    .strong()
                                    .size(12.0),
                            );
                        });
                    });

                    if !rec.reasons.is_empty() {
                        for reason in rec.reasons.iter().take(2) {
                            ui.label(
                                egui::RichText::new(format!("• {reason}"))
                                    .color(egui::Color32::from_rgb(195, 215, 240))
                                    .size(10.5),
                            );
                        }
                    } else {
                        ui.label(
                            egui::RichText::new(format!("Винрейт по мете: {:.1}%", rec.avg_winrate))
                                .color(egui::Color32::from_rgb(148, 163, 184))
                                .size(10.5),
                        );
                    }
                    ui.label(
                        egui::RichText::new(format!(
                            "Контрит {}/{} врагов · Роль {:.0}% · {} матчей",
                            rec.countered_enemies, rec.evaluated_enemies, role_confidence * 100.0, rec.evidence_games
                        ))
                        .color(egui::Color32::from_rgb(148, 163, 184))
                        .size(9.5),
                    );
                });
            });
        });
}

// Meta Hero Row Renderer
fn render_meta_hero_row(ui: &mut egui::Ui, short_name: &str, loc_name: &str, wr: f32) {
    let cdn_name = advisor::hero_cdn_name(short_name);
    let img_url = format!(
        "https://cdn.cloudflare.steamstatic.com/apps/dota2/images/dota_react/heroes/{cdn_name}.png"
    );

    egui::Frame::NONE
        .fill(egui::Color32::from_rgba_unmultiplied(14, 18, 26, 130))
        .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(45, 60, 80, 110)))
        .corner_radius(4)
        .inner_margin(egui::Margin::symmetric(6, 4))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.add(
                    egui::Image::new(&img_url)
                        .fit_to_exact_size(egui::vec2(32.0, 18.0))
                        .corner_radius(2),
                );

                ui.label(egui::RichText::new(loc_name).color(egui::Color32::from_rgb(235, 240, 248)).size(12.0));

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        egui::RichText::new(format!("{wr:.1}% WR"))
                            .color(egui::Color32::from_rgb(74, 222, 128))
                            .strong()
                            .size(11.5),
                    );
                });
            });
        });
}

// Situational Item Card Renderer
fn render_situational_item_card(ui: &mut egui::Ui, item: &SituationalItem) {
    egui::Frame::NONE
        .fill(egui::Color32::from_rgba_unmultiplied(14, 18, 26, 130))
        .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(45, 60, 80, 110)))
        .corner_radius(5)
        .inner_margin(egui::Margin::symmetric(6, 4))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.add(
                    egui::Image::new(&item.image_url)
                        .fit_to_exact_size(egui::vec2(32.0, 23.0))
                        .corner_radius(3),
                );

                ui.vertical(|ui| {
                    ui.label(
                        egui::RichText::new(&item.localized_name)
                            .strong()
                            .size(12.0)
                            .color(egui::Color32::from_rgb(240, 204, 75)),
                    );

                    ui.label(
                        egui::RichText::new(&item.reason)
                            .color(egui::Color32::from_rgb(195, 220, 248))
                            .size(10.5),
                    );
                });
            });
        });
}

// Item stage group renderer for builds
fn render_item_stage_group(
    ui: &mut egui::Ui,
    title: &str,
    items: &[PopularItemEntry],
    title_color: egui::Color32,
) {
    render_badge(ui, title, title_color, 11.5);
    ui.add_space(2.0);

    for item in items.iter().take(4) {
        egui::Frame::NONE
            .fill(egui::Color32::from_rgba_unmultiplied(14, 18, 26, 130))
            .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(45, 58, 78, 100)))
            .corner_radius(4)
            .inner_margin(egui::Margin::symmetric(5, 3))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.add(
                        egui::Image::new(&item.image_url)
                            .fit_to_exact_size(egui::vec2(28.0, 20.0))
                            .corner_radius(2),
                    );

                    ui.vertical(|ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new(&item.localized_name)
                                    .color(egui::Color32::WHITE)
                                    .size(11.5),
                            );
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                ui.label(
                                    egui::RichText::new(format!("{:.0}%", item.percentage))
                                        .strong()
                                        .color(egui::Color32::from_rgb(235, 195, 80))
                                        .size(11.0),
                                );
                            });
                        });

                        let progress = (item.percentage / 100.0).clamp(0.0, 1.0);
                        ui.add(
                            egui::ProgressBar::new(progress)
                                .desired_height(2.5)
                                .fill(egui::Color32::from_rgb(190, 145, 55)),
                        );
                    });
                });
            });
        ui.add_space(2.0);
    }
}


#[derive(Debug, Clone)]
pub struct ActiveTimerAlert {
    pub title: String,
    pub subtitle: String,
    pub badge_text: String,
    pub border_color: egui::Color32,
    pub bg_color: egui::Color32,
    pub priority: u8,
}

pub struct UpcomingTimers {
    pub water: Option<i32>,
    pub power: Option<i32>,
    pub wisdom: i32,
    pub tormentor: Option<i32>,
    pub lotus: i32,
}

impl UpcomingTimers {
    pub fn calculate(clock_time: i32) -> Self {
        // Water: 2:00 (120) and 4:00 (240)
        let water = if clock_time < 120 {
            Some(120 - clock_time)
        } else if clock_time < 240 {
            Some(240 - clock_time)
        } else {
            None
        };

        // Power runes: every even minute starting from 6:00 (360)
        let power = if clock_time < 360 {
            Some(360 - clock_time)
        } else {
            let next_target = ((clock_time / 120) + 1) * 120;
            Some(next_target - clock_time)
        };

        // Wisdom rune: every 7 minutes (420)
        let wisdom = if clock_time < 0 {
            420 - clock_time
        } else {
            let next_target = ((clock_time / 420) + 1) * 420;
            next_target - clock_time
        };

        // Tormentor: 20:00 (1200)
        let tormentor = if clock_time < 1200 {
            Some(1200 - clock_time)
        } else {
            None
        };

        // Lotus: every 3 minutes (180)
        let lotus = if clock_time < 0 {
            180 - clock_time
        } else {
            let next_target = ((clock_time / 180) + 1) * 180;
            next_target - clock_time
        };

        Self {
            water,
            power,
            wisdom,
            tormentor,
            lotus,
        }
    }
}

#[derive(Debug, Clone)]
pub struct CampTimingAlert {
    pub label: String,
    pub is_pull: bool,
    pub badge: String,
}

pub fn get_subtle_camp_alert(clock_time: i32, pos: PlayerPosition, settings: &OverlaySettings) -> Option<CampTimingAlert> {
    if clock_time < 30 || clock_time > 600 {
        return None;
    }

    let sec = clock_time % 60;

    // 1. Stack timing: :48 to :55 (strike at :53-:55)
    if settings.alert_stacks && sec >= 48 && sec <= 55 {
        let rem = 54 - sec;
        let badge = if rem > 0 { format!("{rem}с") } else { "СЕЙЧАС!".to_string() };
        return Some(CampTimingAlert {
            label: "Стак кемпа :54".to_string(),
            is_pull: false,
            badge,
        });
    }

    // 2. Pull timing:
    // Creep waves spawn every 30 seconds (:00 and :30)!
    // Wave 1 (:00 spawn) -> arrives at camp around :15-:18. Alert active during :10 to :18 (pull at :16).
    // Wave 2 (:30 spawn) -> arrives at camp around :45-:48. Alert active during :40 to :47 (pull at :46).
    let is_lane_pos = pos != PlayerPosition::Pos2Mid;
    if settings.alert_pulls && is_lane_pos {
        if sec >= 10 && sec <= 18 {
            let rem = 16 - sec;
            let badge = if rem > 0 { format!("{rem}с") } else { "СЕЙЧАС!".to_string() };
            return Some(CampTimingAlert {
                label: "Отвод крипов :16".to_string(),
                is_pull: true,
                badge,
            });
        }
        if sec >= 40 && sec <= 47 {
            let rem = 46 - sec;
            let badge = if rem > 0 { format!("{rem}с") } else { "СЕЙЧАС!".to_string() };
            return Some(CampTimingAlert {
                label: "Отвод крипов :46".to_string(),
                is_pull: true,
                badge,
            });
        }
    }

    None
}

fn render_subtle_camp_pill(ctx: &egui::Context, alert: &CampTimingAlert) {
    egui::Area::new(egui::Id::new("hud_subtle_camp_pill"))
        .fixed_pos(egui::pos2(16.0, 44.0))
        .show(ctx, |ui| {
            let (bg, border) = if alert.is_pull {
                (egui::Color32::from_rgba_unmultiplied(10, 24, 38, 140), egui::Color32::from_rgb(56, 189, 248))
            } else {
                (egui::Color32::from_rgba_unmultiplied(10, 32, 22, 140), egui::Color32::from_rgb(52, 211, 153))
            };

            egui::Frame::NONE
                .fill(bg)
                .stroke(egui::Stroke::new(1.0, border))
                .corner_radius(4)
                .inner_margin(egui::Margin::symmetric(8, 3))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let text = format!("{} in {}", alert.label, alert.badge);
                        ui.label(egui::RichText::new(text).color(border).strong().size(11.0));
                    });
                });
        });
}

fn get_tactical_alerts(
    clock_time: i32,
    pos: PlayerPosition,
    settings: &OverlaySettings,
    live_state: &LiveGameState,
) -> Vec<ActiveTimerAlert> {
    let mut alerts = Vec::new();

    // 0. Roshan & Aegis alerts
    if settings.alert_roshan && live_state.roshan.is_tracking {
        let aegis_expires = live_state.roshan.aegis_expires_at();
        let early_respawn = live_state.roshan.respawn_early_at();
        let late_respawn = live_state.roshan.respawn_late_at();
        let pit_location = if live_state.is_day { "Низ (Radiant)" } else { "Верх (Dire)" };

        let aegis_diff = aegis_expires - clock_time;
        if aegis_diff > 0 {
            let m = aegis_diff / 60;
            let s = aegis_diff % 60;
            if live_state.roshan.is_ally {
                if aegis_diff <= 35 {
                    alerts.push(ActiveTimerAlert {
                        title: format!("Aegis (наш) спадает через {}с!", aegis_diff),
                        subtitle: "Не лезьте под Т4 — кор скоро без второй жизни!".to_string(),
                        badge_text: format!("{}с", aegis_diff),
                        border_color: egui::Color32::from_rgb(239, 68, 68),
                        bg_color: egui::Color32::from_rgba_unmultiplied(20, 10, 10, 160),
                        priority: 110,
                    });
                } else {
                    alerts.push(ActiveTimerAlert {
                        title: format!("Aegis (наш) — {}:{:02}", m, s),
                        subtitle: "Давите вышки и забирайте территорию!".to_string(),
                        badge_text: format!("{}:{:02}", m, s),
                        border_color: egui::Color32::from_rgb(34, 197, 94),
                        bg_color: egui::Color32::from_rgba_unmultiplied(10, 24, 14, 145),
                        priority: 92,
                    });
                }
            } else {
                if aegis_diff <= 20 {
                    alerts.push(ActiveTimerAlert {
                        title: format!("Aegis врагов спадет через {}с", aegis_diff),
                        subtitle: "Скоро можно безопасно принимать драку!".to_string(),
                        badge_text: format!("{}с", aegis_diff),
                        border_color: egui::Color32::from_rgb(250, 204, 21),
                        bg_color: egui::Color32::from_rgba_unmultiplied(20, 18, 10, 150),
                        priority: 110,
                    });
                } else {
                    alerts.push(ActiveTimerAlert {
                        title: format!("Aegis врагов — {}:{:02}", m, s),
                        subtitle: "Сплитпушьте и избегайте прямых драк 5v5!".to_string(),
                        badge_text: format!("{}:{:02}", m, s),
                        border_color: egui::Color32::from_rgb(248, 113, 113),
                        bg_color: egui::Color32::from_rgba_unmultiplied(24, 10, 10, 145),
                        priority: 93,
                    });
                }
            }
        } else if clock_time >= aegis_expires && clock_time <= aegis_expires + 15 {
            alerts.push(ActiveTimerAlert {
                title: "Aegis спал — силы равны!".to_string(),
                subtitle: "Аегис исчез — можно навязывать драку".to_string(),
                badge_text: "спал".to_string(),
                border_color: egui::Color32::from_rgb(56, 189, 248),
                bg_color: egui::Color32::from_rgba_unmultiplied(10, 20, 30, 150),
                priority: 105,
            });
        }

        // Roshan respawn window alert (from early_respawn - 45s to late_respawn)
        if clock_time >= early_respawn - 45 && clock_time <= late_respawn {
            let early_diff = early_respawn - clock_time;
            let (title, sub) = if early_diff > 0 {
                (
                    format!("Roshan — окно через {}с", early_diff),
                    format!("Логово: {} — займите позицию и поставьте вижн", pit_location),
                )
            } else {
                (
                    "Roshan может заспавниться в любой момент!".to_string(),
                    format!("Окно 8-11 мин активно! Логово: {}", pit_location),
                )
            };
            alerts.push(ActiveTimerAlert {
                title,
                subtitle: sub,
                badge_text: "Roshan".to_string(),
                border_color: egui::Color32::from_rgb(217, 119, 6),
                bg_color: egui::Color32::from_rgba_unmultiplied(20, 14, 10, 150),
                priority: 102,
            });
        }
    }

    // Catapults (every 5 min: 5:00, 10:00, 15:00... 30 sec before spawn)
    if settings.alert_catapults && clock_time >= 0 {
        let interval = 300;
        let target = ((clock_time + 30) / interval) * interval;
        if target >= 300 {
            let diff = target - clock_time;
            if diff >= -5 && diff <= 30 {
                let target_min = target / 60;
                let (title, badge) = if diff > 0 {
                    (
                        format!("Catapult через {diff}с ({target_min}:00)"),
                        format!("через {diff}с"),
                    )
                } else {
                    (
                        format!("Catapult вышла на линию! ({target_min}:00)"),
                        "сейчас!".to_string(),
                    )
                };
                alerts.push(ActiveTimerAlert {
                    title,
                    subtitle: "Осадная волна — готовьтесь пушить вышку или дефать свою".to_string(),
                    badge_text: badge,
                    border_color: egui::Color32::from_rgb(234, 88, 12),
                    bg_color: egui::Color32::from_rgba_unmultiplied(10, 14, 22, 145),
                    priority: 88,
                });
            }
        }
    }

    // Neutrals: T1 from 0:00, T2 at 17:00, T3 at 27:00, T4 at 37:00, T5 at 60:00
    if settings.alert_neutrals {
        for (target, tier) in [(0, 1), (1020, 2), (1620, 3), (2220, 4), (3600, 5)] {
            if target == 0 {
                // T1 alert during pre-game and first 60s
                if clock_time >= -10 && clock_time <= 40 && live_state.my_neutral_item.is_none() {
                    alerts.push(ActiveTimerAlert {
                        title: "Neutral T1 — доступны с 0:00!".to_string(),
                        subtitle: "Выбей нейтральный предмет первого тира в лесу".to_string(),
                        badge_text: "T1".to_string(),
                        border_color: egui::Color32::from_rgb(168, 85, 247),
                        bg_color: egui::Color32::from_rgba_unmultiplied(10, 14, 22, 145),
                        priority: 78,
                    });
                }
            } else if clock_time >= target - 30 && clock_time <= target + 15 {
                let diff = target - clock_time;
                let target_min = target / 60;
                let (title, badge) = if diff > 0 {
                    (
                        format!("Neutral T{tier} через {diff}с ({target_min}:00)"),
                        format!("через {diff}с"),
                    )
                } else {
                    (
                        format!("Neutral T{tier} доступны! ({target_min}:00)"),
                        "сейчас!".to_string(),
                    )
                };
                alerts.push(ActiveTimerAlert {
                    title,
                    subtitle: format!("Освободи слот и выбей предмет Тир-{}", tier),
                    badge_text: badge,
                    border_color: egui::Color32::from_rgb(168, 85, 247),
                    bg_color: egui::Color32::from_rgba_unmultiplied(10, 14, 22, 145),
                    priority: 82,
                });
            }
        }
    }

    // Farm Benchmarks every 5 minutes (5:00, 10:00, 15:00, 20:00)
    if settings.alert_farm_benchmark && clock_time >= 300 {
        for target_min in [5, 10, 15, 20] {
            let target = target_min * 60;
            if clock_time >= target && clock_time <= target + 20 {
                let (expected_lh, role_label) = match pos {
                    PlayerPosition::Pos1Carry => (match target_min { 5 => 25, 10 => 55, 15 => 110, _ => 180 }, "Carry"),
                    PlayerPosition::Pos2Mid => (match target_min { 5 => 28, 10 => 60, 15 => 110, _ => 160 }, "Mid"),
                    PlayerPosition::Pos3Offlane => (match target_min { 5 => 20, 10 => 45, 15 => 80, _ => 120 }, "Offlane"),
                    _ => (0, "Support"),
                };

                if expected_lh > 0 {
                    let diff = live_state.last_hits as i32 - expected_lh as i32;
                    let (title, sub, border) = if diff >= 0 {
                        (
                            format!("Farm {target_min}:00 — {} CS (+{} от нормы)", live_state.last_hits, diff),
                            format!("Отличный темп фарминга для роли {}", role_label),
                            egui::Color32::from_rgb(34, 197, 94),
                        )
                    } else {
                        (
                            format!("Farm {target_min}:00 — {} CS (норма {}+)", live_state.last_hits, expected_lh),
                            "Отставание по крипам — добирай фарм в лесу или свободной линии".to_string(),
                            egui::Color32::from_rgb(239, 68, 68),
                        )
                    };

                    alerts.push(ActiveTimerAlert {
                        title,
                        subtitle: sub,
                        badge_text: format!("{}:00", target_min),
                        border_color: border,
                        bg_color: egui::Color32::from_rgba_unmultiplied(10, 14, 22, 145),
                        priority: 86,
                    });
                }
            }
        }
    }

    // 1. Tormentor at 20:00 (1200s)
    if settings.alert_tormentor && clock_time >= 1155 && clock_time <= 1210 {
        let diff = 1200 - clock_time;
        let (title, badge) = if diff > 0 {
            (
                format!("Tormentor через {diff}с (20:00)"),
                format!("через {diff}с"),
            )
        } else {
            (
                "Tormentor заспавнился! (20:00)".to_string(),
                "сейчас!".to_string(),
            )
        };
        alerts.push(ActiveTimerAlert {
            title,
            subtitle: "Бесплатный Aghanim's Shard для команды".to_string(),
            badge_text: badge,
            border_color: egui::Color32::from_rgb(192, 132, 252),
            bg_color: egui::Color32::from_rgba_unmultiplied(10, 14, 22, 145),
            priority: 100,
        });
    }

    // 2. Wisdom Rune (every 7 minutes: 7:00, 14:00, 21:00...)
    if settings.alert_wisdom_runes && clock_time >= 0 {
        let interval = 420;
        let target = ((clock_time + 35) / interval) * interval;
        if target >= 420 {
            let diff = target - clock_time;
            if diff >= -5 && diff <= 35 {
                let target_min = target / 60;
                let (title, badge) = if diff > 0 {
                    (
                        format!("Wisdom rune через {diff}с ({target_min}:00)"),
                        format!("через {diff}с"),
                    )
                } else {
                    (
                        format!("Wisdom rune! ({target_min}:00)"),
                        "сейчас!".to_string(),
                    )
                };
                alerts.push(ActiveTimerAlert {
                    title,
                    subtitle: "Срочно стянитесь к руне опыта (защита / кража)".to_string(),
                    badge_text: badge,
                    border_color: egui::Color32::from_rgb(250, 204, 21),
                    bg_color: egui::Color32::from_rgba_unmultiplied(10, 14, 22, 145),
                    priority: 95,
                });
            }
        }
    }

    // 3. Water Runes (2:00 = 120s, 4:00 = 240s)
    if settings.alert_water_runes {
        for target in [120, 240] {
            if clock_time >= target - 25 && clock_time <= target + 5 {
                let diff = target - clock_time;
                let target_min = target / 60;
                let (title, badge) = if diff > 0 {
                    (
                        format!("Water runes через {diff}с ({target_min}:00)"),
                        format!("через {diff}с"),
                    )
                } else {
                    (
                        format!("Water runes! ({target_min}:00)"),
                        "сейчас!".to_string(),
                    )
                };
                let sub = if pos == PlayerPosition::Pos2Mid {
                    "Пропушь пачку и заряди боттл (+85 HP/MP)"
                } else {
                    "Помогите мидеру проконтролировать руну"
                };
                alerts.push(ActiveTimerAlert {
                    title,
                    subtitle: sub.to_string(),
                    badge_text: badge,
                    border_color: egui::Color32::from_rgb(56, 189, 248),
                    bg_color: egui::Color32::from_rgba_unmultiplied(10, 14, 22, 145),
                    priority: if pos == PlayerPosition::Pos2Mid { 98 } else { 90 },
                });
            }
        }
    }

    // 4. Power Runes (every even minute starting at 6:00)
    if settings.alert_power_runes && clock_time >= 300 {
        let interval = 120;
        let target = ((clock_time + 25) / interval) * interval;
        if target >= 360 {
            let diff = target - clock_time;
            if diff >= -5 && diff <= 25 {
                let target_min = target / 60;
                let (title, badge) = if diff > 0 {
                    (
                        format!("Power rune через {diff}с ({target_min}:00)"),
                        format!("через {diff}с"),
                    )
                } else {
                    (
                        format!("Power rune! ({target_min}:00)"),
                        "сейчас!".to_string(),
                    )
                };
                alerts.push(ActiveTimerAlert {
                    title,
                    subtitle: "Контроль реки (ДД / Хаста / Инвиз / Реген)".to_string(),
                    badge_text: badge,
                    border_color: egui::Color32::from_rgb(251, 146, 60),
                    bg_color: egui::Color32::from_rgba_unmultiplied(10, 14, 22, 145),
                    priority: 85,
                });
            }
        }
    }

    // 5. Lotus Pools (every 3 minutes: 3, 6, 9, 12...)
    if settings.alert_lotus && clock_time >= 0 {
        let interval = 180;
        let target = ((clock_time + 20) / interval) * interval;
        if target >= 180 {
            let diff = target - clock_time;
            if diff >= -4 && diff <= 20 {
                let target_min = target / 60;
                let (title, badge) = if diff > 0 {
                    (
                        format!("Lotus через {diff}с ({target_min}:00)"),
                        format!("через {diff}с"),
                    )
                } else {
                    (
                        format!("Lotus готов! ({target_min}:00)"),
                        "сейчас!".to_string(),
                    )
                };
                alerts.push(ActiveTimerAlert {
                    title,
                    subtitle: "Соберите лотос (+125 HP / 125 MP мгновенно)".to_string(),
                    badge_text: badge,
                    border_color: egui::Color32::from_rgb(244, 114, 182),
                    bg_color: egui::Color32::from_rgba_unmultiplied(10, 14, 22, 145),
                    priority: 75,
                });
            }
        }
    }

    // 6. Pre-Game Bounty Runes (0:00)
    if settings.alert_bounty_runes && clock_time >= -25 && clock_time <= 5 {
        let (title, badge) = if clock_time < 0 {
            (
                format!("Bounty runes через {}с", -clock_time),
                format!("через {}с", -clock_time),
            )
        } else {
            (
                "Bounty runes появились!".to_string(),
                "сейчас!".to_string(),
            )
        };
        alerts.push(ActiveTimerAlert {
            title,
            subtitle: "Займите точки рун всей командой".to_string(),
            badge_text: badge,
            border_color: egui::Color32::from_rgb(234, 179, 8),
            bg_color: egui::Color32::from_rgba_unmultiplied(10, 14, 22, 145),
            priority: 70,
        });
    }

    alerts.sort_by(|a, b| b.priority.cmp(&a.priority));
    alerts
}

fn render_tactical_alert_banner(
    ctx: &egui::Context,
    alerts: &[ActiveTimerAlert],
    screen_w: f32,
) {
    if alerts.is_empty() {
        return;
    }

    let primary = &alerts[0];
    let banner_w = 340.0;
    let banner_x = ((screen_w - banner_w) / 2.0).max(10.0);
    let banner_y = 44.0;

    egui::Area::new(egui::Id::new("hud_tactical_alert_banner"))
        .fixed_pos(egui::pos2(banner_x, banner_y))
        .show(ctx, |ui| {
            egui::Frame::NONE
                .fill(primary.bg_color)
                .stroke(egui::Stroke::new(1.0, primary.border_color))
                .corner_radius(6)
                .inner_margin(egui::Margin::symmetric(10, 5))
                .show(ui, |ui| {
                    ui.set_width(banner_w);

                    // Row 1: Title + Badge
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(&primary.title)
                                .color(primary.border_color)
                                .strong()
                                .size(12.5),
                        );

                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            egui::Frame::NONE
                                .fill(primary.border_color)
                                .corner_radius(3)
                                .inner_margin(egui::Margin::symmetric(5, 1))
                                .show(ui, |ui| {
                                    ui.label(
                                        egui::RichText::new(&primary.badge_text)
                                            .color(egui::Color32::BLACK)
                                            .strong()
                                            .size(11.0),
                                    );
                                });
                        });
                    });

                    // Row 2: Short 1-line subtitle
                    if !primary.subtitle.is_empty() {
                        ui.add_space(1.0);
                        ui.label(
                            egui::RichText::new(&primary.subtitle)
                                .color(egui::Color32::from_rgb(203, 213, 225))
                                .size(11.0),
                        );
                    }
                });
        });
}

fn is_dota_focused() -> bool {
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.is_null() {
            return false;
        }

        // If overlay window itself is foreground, don't hide
        let our_title = std::ffi::CString::new("LaneTheory").unwrap();
        let our_hwnd = FindWindowA(std::ptr::null(), our_title.as_ptr());
        if hwnd == our_hwnd {
            return true;
        }

        // Check window class name: Source 2 engine window class is "Valve001"
        let mut class_buf = [0u16; 64];
        let class_len = GetClassNameW(hwnd, class_buf.as_mut_ptr(), class_buf.len() as i32);
        if class_len > 0 {
            let class_str = String::from_utf16_lossy(&class_buf[..class_len as usize]);
            if class_str == "Valve001" {
                return true;
            }
        }

        // Check process name: must be dota2.exe
        let mut pid: u32 = 0;
        GetWindowThreadProcessId(hwnd, &mut pid);
        if pid != 0 {
            // PROCESS_QUERY_LIMITED_INFORMATION = 0x1000
            let process = OpenProcess(0x1000, 0, pid);
            if !process.is_null() {
                let mut path_buf = [0u16; 512];
                let mut size = path_buf.len() as u32;
                let success = QueryFullProcessImageNameW(process, 0, path_buf.as_mut_ptr(), &mut size);
                CloseHandle(process);
                if success != 0 && size > 0 {
                    let path_str = String::from_utf16_lossy(&path_buf[..size as usize]).to_lowercase();
                    if path_str.ends_with("dota2.exe") {
                        return true;
                    }
                }
            }
        }

        // Check window title: contains "Dota 2"
        let mut title_buf = [0u16; 64];
        let title_len = GetWindowTextW(hwnd, title_buf.as_mut_ptr(), title_buf.len() as i32);
        if title_len > 0 {
            let title_str = String::from_utf16_lossy(&title_buf[..title_len as usize]);
            if title_str.contains("Dota 2") {
                return true;
            }
        }

        false
    }
}

fn is_dota_running_or_connected(is_connected: bool) -> bool {
    if is_connected {
        return true;
    }
    unsafe {
        let valve_class = std::ffi::CString::new("Valve001").unwrap();
        let hwnd = FindWindowA(valve_class.as_ptr(), std::ptr::null());
        !hwnd.is_null()
    }
}

#[link(name = "user32")]
unsafe extern "system" {
    fn GetAsyncKeyState(v_key: i32) -> i16;
    fn FindWindowA(lp_class_name: *const i8, lp_window_name: *const i8) -> *mut std::ffi::c_void;
    fn SetWindowLongPtrW(h_wnd: *mut std::ffi::c_void, n_index: i32, dw_new_long: isize) -> isize;
    fn GetWindowLongPtrW(h_wnd: *mut std::ffi::c_void, n_index: i32) -> isize;
    fn SetClassLongPtrW(h_wnd: *mut std::ffi::c_void, n_index: i32, dw_new_long: isize) -> isize;
    fn GetForegroundWindow() -> *mut std::ffi::c_void;
    fn SetForegroundWindow(h_wnd: *mut std::ffi::c_void) -> i32;
    fn GetWindowThreadProcessId(h_wnd: *mut std::ffi::c_void, lpdw_process_id: *mut u32) -> u32;
    fn GetClassNameW(h_wnd: *mut std::ffi::c_void, lp_class_name: *mut u16, n_max_count: i32) -> i32;
    fn GetWindowTextW(h_wnd: *mut std::ffi::c_void, lp_string: *mut u16, n_max_count: i32) -> i32;
    fn RegisterHotKey(h_wnd: *mut std::ffi::c_void, id: i32, fs_modifiers: u32, vk: u32) -> i32;
    fn SetWindowsHookExW(
        id_hook: i32,
        callback: Option<unsafe extern "system" fn(i32, usize, isize) -> isize>,
        module: *mut std::ffi::c_void,
        thread_id: u32,
    ) -> *mut std::ffi::c_void;
    fn CallNextHookEx(
        hook: *mut std::ffi::c_void,
        code: i32,
        w_param: usize,
        l_param: isize,
    ) -> isize;
    fn GetCursorPos(point: *mut POINT) -> i32;
    fn ClientToScreen(h_wnd: *mut std::ffi::c_void, point: *mut POINT) -> i32;
    fn GetMessageW(lp_msg: *mut MSG, h_wnd: *mut std::ffi::c_void, w_msg_filter_min: u32, w_msg_filter_max: u32) -> i32;
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn OpenProcess(dw_desired_access: u32, b_inherit_handle: i32, dw_process_id: u32) -> *mut std::ffi::c_void;
    fn CloseHandle(h_object: *mut std::ffi::c_void) -> i32;
    fn QueryFullProcessImageNameW(
        h_process: *mut std::ffi::c_void,
        dw_flags: u32,
        lp_exe_name: *mut u16,
        lpdw_size: *mut u32,
    ) -> i32;
}
