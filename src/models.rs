use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeroData {
    pub id: u32,
    pub name: String,
    pub localized_name: String,
    pub primary_attr: String,
    pub roles: Vec<String>,
    #[serde(default)]
    pub attack_type: String,
}

impl HeroData {
    pub fn short_name(&self) -> &str {
        self.name.strip_prefix("npc_dota_hero_").unwrap_or(&self.name)
    }

    pub fn image_url(&self) -> String {
        let cdn_name = crate::advisor::hero_cdn_name(self.short_name());
        format!(
            "https://cdn.cloudflare.steamstatic.com/apps/dota2/images/dota_react/heroes/{}.png",
            cdn_name
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ItemData {
    pub id: u32,
    pub name: String,
    pub localized_name: String,
    pub cost: Option<u32>,
}

impl ItemData {
    pub fn clean_name(&self) -> &str {
        self.name.strip_prefix("item_").unwrap_or(&self.name)
    }

    pub fn image_url(&self) -> String {
        format!(
            "https://cdn.cloudflare.steamstatic.com/apps/dota2/images/dota_react/items/{}.png",
            self.clean_name()
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeroMatchup {
    pub hero_id: u32,
    pub games_played: u32,
    pub wins: u32,
}

impl HeroMatchup {
    pub fn winrate(&self) -> f32 {
        if self.games_played == 0 {
            50.0
        } else {
            (self.wins as f32 / self.games_played as f32) * 100.0
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ItemPopularity {
    pub start_game_items: HashMap<String, u32>,
    pub early_game_items: HashMap<String, u32>,
    pub mid_game_items: HashMap<String, u32>,
    pub late_game_items: HashMap<String, u32>,
}

#[derive(Debug, Clone)]
pub struct RecommendedHero {
    pub hero: HeroData,
    pub advantage: f32,
    pub avg_winrate: f32,
    /// Number of drafted enemies this hero has a materially favourable matchup against.
    pub countered_enemies: usize,
    /// Number of enemy matchups with usable data (not merely a guessed counter).
    pub evaluated_enemies: usize,
    pub evidence_games: u32,
    /// 0..100 confidence score: coverage is weighted above winrate and sample size.
    pub priority_score: u8,
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct SituationalItem {
    pub item_name: String,
    pub localized_name: String,
    pub image_url: String,
    pub reason: String,
    pub priority: u8,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct PopularItemEntry {
    pub item_name: String,
    pub localized_name: String,
    pub image_url: String,
    pub count: u32,
    pub percentage: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum RankBracket {
    #[default]
    All,
    Herald,
    Guardian,
    Crusader,
    Archon,
    Legend,
    Ancient,
    Divine,
    Immortal,
}

impl RankBracket {
    pub fn title_ru(&self) -> &'static str {
        match self {
            Self::All => "Все ранги (Общий)",
            Self::Herald => "Рекрут (Herald)",
            Self::Guardian => "Страж (Guardian)",
            Self::Crusader => "Рыцарь (Crusader)",
            Self::Archon => "Герой (Archon)",
            Self::Legend => "Легенда (Legend)",
            Self::Ancient => "Властелин (Ancient)",
            Self::Divine => "Божество (Divine)",
            Self::Immortal => "Титан (Immortal)",
        }
    }

    pub fn title_en(&self) -> &'static str {
        match self {
            Self::All => "All Ranks",
            Self::Herald => "Herald",
            Self::Guardian => "Guardian",
            Self::Crusader => "Crusader",
            Self::Archon => "Archon",
            Self::Legend => "Legend",
            Self::Ancient => "Ancient",
            Self::Divine => "Divine",
            Self::Immortal => "Immortal",
        }
    }

    #[allow(dead_code)]
    pub fn next(&self) -> Self {
        match self {
            Self::All => Self::Herald,
            Self::Herald => Self::Guardian,
            Self::Guardian => Self::Crusader,
            Self::Crusader => Self::Archon,
            Self::Archon => Self::Legend,
            Self::Legend => Self::Ancient,
            Self::Ancient => Self::Divine,
            Self::Divine => Self::Immortal,
            Self::Immortal => Self::All,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RoshanState {
    pub is_tracking: bool,
    pub kill_clock_time: i32,
    pub is_ally: bool,
}

impl RoshanState {
    pub fn aegis_expires_at(&self) -> i32 {
        self.kill_clock_time + 300 // 5:00
    }

    pub fn respawn_early_at(&self) -> i32 {
        self.kill_clock_time + 480 // 8:00
    }

    pub fn respawn_late_at(&self) -> i32 {
        self.kill_clock_time + 660 // 11:00
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrackedSpell {
    pub hero_name: String,
    pub spell_name: String,
    pub localized_spell: String,
    pub ability_key: String,
    pub base_cd: i32,
    pub cooldowns: Vec<i32>,
    pub ability_image: String,
    /// Actual ultimate tier (1..=3). This is distinct from the hero level:
    /// GSI may expose neither during a normal player match.
    pub ultimate_level: Option<u8>,
    pub enemy_level: Option<u32>,
    pub on_cooldown_until: Option<i32>,
}

#[derive(Debug, Clone)]
pub struct LiveGameState {
    pub is_connected: bool,
    /// Local listener health, shown when Dota has not connected yet.
    pub gsi_listener_status: String,
    /// Startup and data-refresh state. Unlike GSI, this must never prevent
    /// the overlay from appearing.
    pub analytics_status: String,
    pub game_state: String,
    /// Dota's stable identifier for the current game. It is the only
    /// reliable boundary when GSI stops sending payloads after a match and
    /// resumes directly at the following draft without a menu event.
    pub match_id: Option<String>,
    pub clock_time: i32,
    pub is_day: bool,
    pub my_hero_name: Option<String>,
    pub my_team: Option<String>,
    pub radiant_heroes: Vec<String>,
    pub dire_heroes: Vec<String>,
    pub enemy_heroes: Vec<String>,
    pub ally_heroes: Vec<String>,
    pub enemy_levels: HashMap<String, u32>,
    pub draft_status: String,
    pub last_update_sec: u64,
    pub player_steam_id: Option<String>,
    pub player_profile: Option<PlayerProfile>,

    // GSI Player & Hero stats
    pub last_hits: u32,
    pub denies: u32,
    pub net_worth: u32,
    pub gold: u32,
    pub gold_reliable: u32,
    pub gold_unreliable: u32,
    pub gpm: u32,
    pub xpm: u32,
    pub hero_level: u32,
    pub is_alive: bool,
    pub respawn_seconds: u32,
    pub buyback_cost: u32,
    pub buyback_cooldown: u32,
    pub my_items: Vec<String>,
    pub my_neutral_item: Option<String>,

    // Roshan & Ultimates
    pub roshan: RoshanState,
    pub tracked_spells: Vec<TrackedSpell>,
}

impl Default for LiveGameState {
    fn default() -> Self {
        Self {
            is_connected: false,
            gsi_listener_status: "GSI listener запускается".to_string(),
            analytics_status: "Данные: локальный пакет готов".to_string(),
            game_state: "menu".to_string(),
            match_id: None,
            clock_time: -999,
            is_day: true,
            my_hero_name: None,
            my_team: None,
            radiant_heroes: Vec::new(),
            dire_heroes: Vec::new(),
            enemy_heroes: Vec::new(),
            ally_heroes: Vec::new(),
            enemy_levels: HashMap::new(),
            draft_status: "GSI: draft payload ещё не получен".to_string(),
            last_update_sec: 0,
            player_steam_id: None,
            player_profile: None,

            last_hits: 0,
            denies: 0,
            net_worth: 0,
            gold: 0,
            gold_reliable: 0,
            gold_unreliable: 0,
            gpm: 0,
            xpm: 0,
            hero_level: 1,
            is_alive: true,
            respawn_seconds: 0,
            buyback_cost: 0,
            buyback_cooldown: 0,
            my_items: Vec::new(),
            my_neutral_item: None,

            roshan: RoshanState::default(),
            tracked_spells: Vec::new(),
        }
    }
}

impl LiveGameState {
    pub fn reset_to_menu(&mut self) {
        self.game_state = "menu".to_string();
        self.match_id = None;
        self.clock_time = -999;
        self.is_day = true;
        self.my_hero_name = None;
        self.my_team = None;
        self.radiant_heroes.clear();
        self.dire_heroes.clear();
        self.enemy_heroes.clear();
        self.ally_heroes.clear();
        self.enemy_levels.clear();
        self.draft_status = "GSI: draft payload ещё не получен".to_string();
        self.last_hits = 0;
        self.denies = 0;
        self.net_worth = 0;
        self.gold = 0;
        self.gold_reliable = 0;
        self.gold_unreliable = 0;
        self.gpm = 0;
        self.xpm = 0;
        self.hero_level = 1;
        self.is_alive = true;
        self.respawn_seconds = 0;
        self.buyback_cost = 0;
        self.buyback_cooldown = 0;
        self.my_items.clear();
        self.my_neutral_item = None;
        self.roshan = RoshanState::default();
        self.tracked_spells.clear();
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PlayerProfile {
    pub account_id: u32,
    pub personaname: String,
    pub avatar_url: String,
    pub rank_tier: Option<u32>,
    pub leaderboard_rank: Option<u32>,
    pub rank_label: String,
    pub wins: u32,
    pub losses: u32,
    pub winrate: f32,
    pub top_heroes: Vec<(String, u32, f32)>,
    pub dominant_position: Option<PlayerPosition>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum PlayerPosition {
    #[default]
    Pos1Carry,
    Pos2Mid,
    Pos3Offlane,
    Pos4SoftSupport,
    Pos5HardSupport,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum BuildSource {
    #[default]
    OpenDotaAggregate,
    Dota2ProTracker,
}

impl BuildSource {
    pub fn title(self) -> &'static str {
        match self {
            Self::OpenDotaAggregate => "OpenDota · все роли",
            Self::Dota2ProTracker => "D2PT · роль / 7000+ MMR",
        }
    }
}

impl PlayerPosition {
    #[allow(dead_code)]
    pub fn title_ru(&self) -> &'static str {
        match self {
            Self::Pos1Carry => "Поз 1 (Керри)",
            Self::Pos2Mid => "Поз 2 (Мид)",
            Self::Pos3Offlane => "Поз 3 (Оффлейн)",
            Self::Pos4SoftSupport => "Поз 4 (Семи-сап)",
            Self::Pos5HardSupport => "Поз 5 (Фулл-сап)",
        }
    }

    pub fn title_en(&self) -> &'static str {
        match self {
            Self::Pos1Carry => "Pos 1 Carry",
            Self::Pos2Mid => "Pos 2 Mid",
            Self::Pos3Offlane => "Pos 3 Offlane",
            Self::Pos4SoftSupport => "Pos 4 Support",
            Self::Pos5HardSupport => "Pos 5 Hard Support",
        }
    }

    #[allow(dead_code)]
    pub fn short_name(&self) -> &'static str {
        match self {
            Self::Pos1Carry => "Pos 1",
            Self::Pos2Mid => "Pos 2",
            Self::Pos3Offlane => "Pos 3",
            Self::Pos4SoftSupport => "Pos 4",
            Self::Pos5HardSupport => "Pos 5",
        }
    }

    pub fn d2pt_role(&self) -> &'static str {
        match self {
            Self::Pos1Carry => "carry",
            Self::Pos2Mid => "mid",
            Self::Pos3Offlane => "offlane",
            Self::Pos4SoftSupport => "support",
            Self::Pos5HardSupport => "hard_support",
        }
    }

    pub fn next(&self) -> Self {
        match self {
            Self::Pos1Carry => Self::Pos2Mid,
            Self::Pos2Mid => Self::Pos3Offlane,
            Self::Pos3Offlane => Self::Pos4SoftSupport,
            Self::Pos4SoftSupport => Self::Pos5HardSupport,
            Self::Pos5HardSupport => Self::Pos1Carry,
        }
    }
}

pub fn steamid64_to_account_id(steamid64_str: &str) -> Option<u32> {
    let id64 = steamid64_str.parse::<u64>().ok()?;
    const BASE: u64 = 76561197960265728;
    if id64 > BASE {
        Some((id64 - BASE) as u32)
    } else {
        None
    }
}

pub fn parse_rank_tier(tier: Option<u32>, leaderboard: Option<u32>) -> String {
    let t = match tier {
        Some(val) if val > 0 => val,
        _ => return "Без ранга".to_string(),
    };
    if t >= 80 {
        if let Some(rank) = leaderboard {
            return format!("Титан #{}", rank);
        } else {
            return "Титан".to_string();
        }
    }
    let medal = match t / 10 {
        1 => "Рекрут",
        2 => "Страж",
        3 => "Рыцарь",
        4 => "Герой",
        5 => "Легенда",
        6 => "Властелин",
        7 => "Божество",
        _ => "Неизвестно",
    };
    let stars = t % 10;
    format!("{} [★ {}]", medal, stars)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct OverlaySettings {
    // --- Notification types ---
    pub alert_power_runes: bool,
    pub alert_water_runes: bool,
    pub alert_bounty_runes: bool,
    pub alert_wisdom_runes: bool,
    pub alert_tormentor: bool,
    pub alert_lotus: bool,
    pub alert_stacks: bool,
    pub alert_pulls: bool,
    pub alert_catapults: bool,
    pub alert_neutrals: bool,
    pub alert_farm_benchmark: bool,
    pub alert_roshan: bool,
    pub alert_ultimates: bool,

    // --- Audio / TTS ---
    pub enable_tts: bool,

    // --- Rank Bracket Filter ---
    pub enable_rank_filter: bool,
    pub selected_rank: RankBracket,
    pub build_source: BuildSource,

    // --- Match setup ---
    /// Saved position applied at the start of every new match. F5 is only a
    /// per-match override, so an old switch never leaks into the next queue.
    pub preferred_position: PlayerPosition,

    // --- UI elements ---
    pub show_top_bar: bool,
    pub show_top_timers: bool,
    pub show_left_panel: bool,
    pub show_right_panel: bool,
    pub show_center_alerts: bool,
    pub show_camp_pill: bool,
    pub show_roshan_panel: bool,
    pub show_ultimates_panel: bool,
}

impl Default for OverlaySettings {
    fn default() -> Self {
        Self {
            alert_power_runes: true,
            alert_water_runes: true,
            alert_bounty_runes: true,
            alert_wisdom_runes: true,
            alert_tormentor: true,
            alert_lotus: true,
            alert_stacks: true,
            alert_pulls: true,
            alert_catapults: true,
            alert_neutrals: true,
            alert_farm_benchmark: true,
            alert_roshan: true,
            alert_ultimates: true,

            enable_tts: false,

            enable_rank_filter: true,
            selected_rank: RankBracket::All,
            build_source: BuildSource::OpenDotaAggregate,
            preferred_position: PlayerPosition::Pos1Carry,

            show_top_bar: true,
            show_top_timers: true,
            show_left_panel: true,
            show_right_panel: true,
            show_center_alerts: true,
            show_camp_pill: true,
            show_roshan_panel: true,
            show_ultimates_panel: true,
        }
    }
}

impl OverlaySettings {
    pub fn load() -> Self {
        if let Ok(data) = std::fs::read_to_string("settings.json") {
            if let Ok(settings) = serde_json::from_str(&data) {
                return settings;
            }
        }
        Self::default()
    }

    pub fn save(&self) {
        if let Ok(data) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write("settings.json", data);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returning_to_menu_clears_match_only_state() {
        let mut state = LiveGameState {
            game_state: "DOTA_GAMERULES_STATE_GAME_IN_PROGRESS".into(),
            match_id: Some("12345".into()),
            clock_time: 900,
            my_hero_name: Some("npc_dota_hero_huskar".into()),
            enemy_heroes: vec!["npc_dota_hero_axe".into()],
            my_items: vec!["item_armlet".into()],
            tracked_spells: vec![TrackedSpell {
                hero_name: "Enigma".into(), spell_name: "Black Hole".into(),
                localized_spell: "Black Hole".into(), ability_key: "enigma_black_hole".into(),
                base_cd: 180, cooldowns: vec![180, 170, 160], ability_image: String::new(),
                ultimate_level: Some(1), enemy_level: Some(6), on_cooldown_until: Some(1080),
            }],
            ..Default::default()
        };

        state.reset_to_menu();

        assert_eq!(state.game_state, "menu");
        assert!(state.match_id.is_none());
        assert_eq!(state.clock_time, -999);
        assert!(state.my_hero_name.is_none());
        assert!(state.enemy_heroes.is_empty());
        assert!(state.my_items.is_empty());
        assert!(state.tracked_spells.is_empty());
    }

    #[test]
    fn old_settings_file_uses_carry_as_saved_preferred_position() {
        let settings: OverlaySettings = serde_json::from_str("{\"enable_tts\": true}")
            .expect("settings from a previous version should remain readable");

        assert_eq!(settings.preferred_position, PlayerPosition::Pos1Carry);
        assert!(settings.enable_tts);
    }
}
