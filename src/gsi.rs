use crate::models::{steamid64_to_account_id, LiveGameState};
use axum::{extract::State, routing::post, Json, Router};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

pub struct GsiServer;

impl GsiServer {
    pub fn start(
        shared_state: Arc<Mutex<LiveGameState>>,
        api_client: Arc<tokio::sync::Mutex<crate::api::DotaApiClient>>,
        hero_names_by_id: Arc<HashMap<u32, String>>,
    ) {
        // A bot lobby can return to the menu without a final GSI payload.
        // The configured five-second heartbeat lets this watchdog provide a
        // safe session boundary instead of retaining stale Live Match data.
        let watchdog_state = shared_state.clone();
        tokio::spawn(async move {
            const STALE_AFTER_SECONDS: u64 = 12;
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                let Ok(mut state) = watchdog_state.lock() else {
                    continue;
                };
                let has_active_game_state = matches!(
                    state.game_state.as_str(),
                    "DOTA_GAMERULES_STATE_HERO_SELECTION"
                        | "DOTA_GAMERULES_STATE_STRATEGY_TIME"
                        | "DOTA_GAMERULES_STATE_PRE_GAME"
                        | "DOTA_GAMERULES_STATE_GAME_IN_PROGRESS"
                );
                if state.is_connected
                    && has_active_game_state
                    && now.saturating_sub(state.last_update_sec) > STALE_AFTER_SECONDS
                {
                    state.reset_to_menu();
                    state.is_connected = false;
                    state.gsi_listener_status = "GSI не присылал обновлений более 12 с; состояние матча сброшено".to_string();
                    println!("GSI: нет heartbeat более {STALE_AFTER_SECONDS} с, состояние матча сброшено");
                }
            }
        });
        tokio::spawn(async move {
            let app_state = (shared_state, api_client, hero_names_by_id);
            let listener_state = app_state.0.clone();
            let app = Router::new()
                .route("/", post(handle_gsi_payload))
                .with_state(app_state);

            let addr = "127.0.0.1:3000";
            match tokio::net::TcpListener::bind(addr).await {
                Ok(listener) => {
                    if let Ok(mut state) = listener_state.lock() {
                        state.gsi_listener_status = format!("GSI слушает {addr}; ждём первый payload");
                    }
                    println!("GSI HTTP сервер слушает на {addr}");
                    let _ = axum::serve(listener, app).await;
                }
                Err(error) => {
                    if let Ok(mut state) = listener_state.lock() {
                        state.gsi_listener_status = format!("GSI не слушает {addr}: {error}");
                    }
                    eprintln!("Не удалось забиндить GSI порт {addr}: {error}. Вероятно, порт занят другим приложением.");
                }
            }
        });
    }
}

type SharedContext = (
    Arc<Mutex<LiveGameState>>,
    Arc<tokio::sync::Mutex<crate::api::DotaApiClient>>,
    Arc<HashMap<u32, String>>,
);

async fn handle_gsi_payload(
    State((state_mutex, api_mutex, hero_names_by_id)): State<SharedContext>,
    Json(payload): Json<Value>,
) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let mut state = state_mutex.lock().unwrap();
    state.is_connected = true;
    state.last_update_sec = now;

    if let Some(map) = payload.get("map") {
        if let Some(match_id) = map.get("matchid").and_then(|value| {
            value.as_str()
                .map(str::to_owned)
                .or_else(|| value.as_u64().map(|id| id.to_string()))
        }).filter(|id| !id.is_empty() && id != "0") {
            if state.match_id.as_deref() != Some(&match_id) {
                let had_previous_match = state.match_id.is_some();
                state.reset_to_menu();
                state.match_id = Some(match_id);
                if had_previous_match {
                    println!("GSI: обнаружен новый matchid, состояние прошлого матча сброшено");
                }
            }
        }
        if let Some(gs) = map.get("game_state").and_then(|v| v.as_str()) {
            if matches!(gs, "DOTA_GAMERULES_STATE_POST_GAME" | "DOTA_GAMERULES_STATE_DISCONNECT") {
                state.reset_to_menu();
                state.is_connected = false;
                return;
            }
            state.game_state = gs.to_string();
        }
        if let Some(ct) = map.get("clock_time").and_then(|v| v.as_i64()) {
            state.clock_time = ct as i32;
        }
        if let Some(day) = map.get("daytime").and_then(|v| v.as_bool()) {
            state.is_day = day;
        }
    }

    let mut need_fetch_account_id = None;
    if let Some(player) = payload.get("player") {
        if let Some(act) = player.get("activity").and_then(|v| v.as_str()) {
            if act == "menu" {
                state.reset_to_menu();
            }
        }

        if let Some(steamid) = player.get("steamid").and_then(|v| v.as_str()) {
            if state.player_steam_id.as_deref() != Some(steamid) {
                state.player_steam_id = Some(steamid.to_string());
                if let Some(acc_id) = steamid64_to_account_id(steamid) {
                    if state.player_profile.is_none() {
                        need_fetch_account_id = Some(acc_id);
                    }
                }
            }
        }

        if let Some(team_name) = player.get("team_name").and_then(|v| v.as_str()) {
            state.my_team = Some(team_name.to_lowercase());
        }

        if let Some(lh) = player.get("last_hits").and_then(|v| v.as_u64()) {
            state.last_hits = lh as u32;
        }
        if let Some(dn) = player.get("denies").and_then(|v| v.as_u64()) {
            state.denies = dn as u32;
        }
        if let Some(nw) = player.get("net_worth").and_then(|v| v.as_u64()) {
            state.net_worth = nw as u32;
        }
        if let Some(g) = player.get("gold").and_then(|v| v.as_u64()) {
            state.gold = g as u32;
        }
        if let Some(gr) = player.get("gold_reliable").and_then(|v| v.as_u64()) {
            state.gold_reliable = gr as u32;
        }
        if let Some(gu) = player.get("gold_unreliable").and_then(|v| v.as_u64()) {
            state.gold_unreliable = gu as u32;
        }
        if let Some(gpm) = player.get("gpm").and_then(|v| v.as_u64()) {
            state.gpm = gpm as u32;
        }
        if let Some(xpm) = player.get("xpm").and_then(|v| v.as_u64()) {
            state.xpm = xpm as u32;
        }
    }

    if let Some(hero) = payload.get("hero") {
        if let Some(hname) = hero.get("name").and_then(|v| v.as_str()) {
            state.my_hero_name = Some(hname.to_string());
        }
        if let Some(lvl) = hero.get("level").and_then(|v| v.as_u64()) {
            state.hero_level = lvl as u32;
        }
        if let Some(alive) = hero.get("alive").and_then(|v| v.as_bool()) {
            state.is_alive = alive;
        }
        if let Some(respawn) = hero.get("respawn_seconds").and_then(|v| v.as_u64()) {
            state.respawn_seconds = respawn as u32;
        }
        if let Some(bb_cost) = hero.get("buyback_cost").and_then(|v| v.as_u64()) {
            state.buyback_cost = bb_cost as u32;
        }
        if let Some(bb_cd) = hero.get("buyback_cooldown").and_then(|v| v.as_u64()) {
            state.buyback_cooldown = bb_cd as u32;
        }
    }

    if let Some(items) = payload.get("items") {
        let mut my_items = Vec::new();
        for i in 0..6 {
            let slot_key = format!("slot{i}");
            if let Some(item_obj) = items.get(&slot_key) {
                if let Some(name) = item_obj.get("name").and_then(|v| v.as_str()) {
                    if name != "empty" {
                        my_items.push(name.to_string());
                    }
                }
            }
        }
        state.my_items = my_items;

        if let Some(neutral_obj) = items.get("neutral0") {
            if let Some(nname) = neutral_obj.get("name").and_then(|v| v.as_str()) {
                if nname != "empty" {
                    state.my_neutral_item = Some(nname.to_string());
                } else {
                    state.my_neutral_item = None;
                }
            }
        }
    }

    // Parse draft picks if available
    if let Some(draft) = payload.get("draft") {
        let hero_names = hero_names_by_id.as_ref();

        let mut rad_heroes = Vec::new();
        let mut dire_heroes = Vec::new();
        let mut unassigned_heroes = Vec::new();

        let team2 = draft.get("team2").or_else(|| draft.get("radiant"));
        if let Some(t2) = team2 {
            for i in 0..5 {
                if let Some(h) = extract_hero_from_pick(t2, i, hero_names) {
                    rad_heroes.push(h);
                }
            }
        }

        let team3 = draft.get("team3").or_else(|| draft.get("dire"));
        if let Some(t3) = team3 {
            for i in 0..5 {
                if let Some(h) = extract_hero_from_pick(t3, i, hero_names) {
                    dire_heroes.push(h);
                }
            }
        }

        // The GSI draft payload differs between normal matchmaking and lobby/spectator
        // modes. In normal matchmaking it is commonly flat: pick0 + pick0_team rather
        // than nested team2/team3 objects. Support both forms.
        for i in 0..10 {
            let Some(hero) = extract_hero_from_pick(draft, i, hero_names) else {
                continue;
            };
            if rad_heroes.contains(&hero) || dire_heroes.contains(&hero) {
                continue;
            }
            let team_key = format!("pick{i}_team");
            let team = draft.get(&team_key).and_then(|value| {
                value.as_str().map(str::to_owned)
                    .or_else(|| value.as_i64().map(|id| id.to_string()))
            });
            match team.as_deref().map(str::to_ascii_lowercase).as_deref() {
                Some("2") | Some("radiant") | Some("team2") => rad_heroes.push(hero),
                Some("3") | Some("dire") | Some("team3") => dire_heroes.push(hero),
                _ => unassigned_heroes.push(hero),
            }
        }

        if !rad_heroes.is_empty() { state.radiant_heroes = rad_heroes; }
        if !dire_heroes.is_empty() { state.dire_heroes = dire_heroes; }

        let is_radiant = state.my_team.as_deref().unwrap_or("radiant") == "radiant";
        if is_radiant {
            state.ally_heroes = state.radiant_heroes.clone();
            state.enemy_heroes = state.dire_heroes.clone();
        } else {
            state.ally_heroes = state.dire_heroes.clone();
            state.enemy_heroes = state.radiant_heroes.clone();
        }

        // Some clients omit pick team data. Showing a labelled best-effort counter analysis
        // is still more useful than a blank draft panel; never include the local hero.
        if state.enemy_heroes.is_empty() && !unassigned_heroes.is_empty() {
            state.enemy_heroes = unassigned_heroes
                .into_iter()
                .filter(|hero| state.my_hero_name.as_deref() != Some(hero))
                .collect();
        }

        let status = if state.enemy_heroes.is_empty() {
            "GSI draft получен, но пиков противника в нём нет".to_string()
        } else {
            format!("GSI draft: {} вражеских пиков", state.enemy_heroes.len())
        };
        if state.draft_status != status {
            println!("{status}");
            state.draft_status = status;
        }
    }

    // Parse allplayers if available (custom lobby, spectator, or coach mode)
    if let Some(allplayers) = payload.get("allplayers") {
        let hero_names = hero_names_by_id.as_ref();

        let mut rad_heroes = Vec::new();
        let mut dire_heroes = Vec::new();
        let mut observed_levels = std::collections::HashMap::new();

        // Check nested team2/team3 or radiant/dire
        for (team_key, is_rad) in [("team2", true), ("radiant", true), ("team3", false), ("dire", false)] {
            if let Some(team_obj) = allplayers.get(team_key).and_then(|v| v.as_object()) {
                for (_pk, pval) in team_obj {
                    if let Some(h) = extract_hero_from_player_val(pval, hero_names) {
                        if let Some(level) = pval.get("level")
                            .or_else(|| pval.pointer("/hero/level"))
                            .and_then(|value| value.as_u64())
                        {
                            observed_levels.insert(h.clone(), level as u32);
                        }
                        if is_rad {
                            if !rad_heroes.contains(&h) { rad_heroes.push(h); }
                        } else {
                            if !dire_heroes.contains(&h) { dire_heroes.push(h); }
                        }
                    }
                }
            }
        }

        // Check flat player0..player9
        if let Some(flat_obj) = allplayers.as_object() {
            for (_pk, pval) in flat_obj {
                if let Some(team_name) = pval.get("team_name").or_else(|| pval.get("team")).and_then(|v| v.as_str()) {
                    let is_rad = team_name.to_lowercase() == "radiant" || team_name == "2";
                    if let Some(h) = extract_hero_from_player_val(pval, hero_names) {
                        if let Some(level) = pval.get("level")
                            .or_else(|| pval.pointer("/hero/level"))
                            .and_then(|value| value.as_u64())
                        {
                            observed_levels.insert(h.clone(), level as u32);
                        }
                        if is_rad {
                            if !rad_heroes.contains(&h) { rad_heroes.push(h); }
                        } else {
                            if !dire_heroes.contains(&h) { dire_heroes.push(h); }
                        }
                    }
                }
            }
        }

        if !rad_heroes.is_empty() || !dire_heroes.is_empty() {
            if !rad_heroes.is_empty() { state.radiant_heroes = rad_heroes; }
            if !dire_heroes.is_empty() { state.dire_heroes = dire_heroes; }
            let is_radiant = state.my_team.as_deref().unwrap_or("radiant") == "radiant";
            if is_radiant {
                state.ally_heroes = state.radiant_heroes.clone();
                state.enemy_heroes = state.dire_heroes.clone();
            } else {
                state.ally_heroes = state.dire_heroes.clone();
                state.enemy_heroes = state.radiant_heroes.clone();
            }
            state.enemy_levels = state.enemy_heroes.iter()
                .filter_map(|hero| observed_levels.get(hero).map(|level| (hero.clone(), *level)))
                .collect();
            let status = format!("GSI allplayers: {} вражеских героев", state.enemy_heroes.len());
            if state.draft_status != status {
                println!("{status}");
                state.draft_status = status;
            }
        }
    }

    drop(state);

    let profile_context = if need_fetch_account_id.is_some() {
        api_mutex
            .try_lock()
            .ok()
            .map(|api| api.profile_fetch_context())
    } else {
        None
    };

    if let (Some(acc_id), Some((client, heroes))) = (need_fetch_account_id, profile_context) {
        let state_clone = state_mutex.clone();
        tokio::spawn(async move {
            if let Some(profile) = crate::api::DotaApiClient::fetch_player_profile(client, heroes, acc_id).await {
                let mut st = state_clone.lock().unwrap();
                st.player_profile = Some(profile);
                println!("Профиль игрока успешно загружен!");
            }
        });
    }
}

fn extract_hero_from_pick(
    team: &serde_json::Value,
    i: usize,
    hero_names: &HashMap<u32, String>,
) -> Option<String> {
    // 1. Try pick{i}_class: e.g. "npc_dota_hero_antimage"
    let class_key = format!("pick{i}_class");
    if let Some(h) = team.get(&class_key).and_then(|v| v.as_str()) {
        if !h.is_empty() {
            return Some(h.to_string());
        }
    }

    // 2. Try pick{i}: e.g. "npc_dota_hero_juggernaut"
    let pick_key = format!("pick{i}");
    if let Some(h) = team.get(&pick_key).and_then(|v| v.as_str()) {
        if !h.is_empty() {
            return Some(h.to_string());
        }
    }

    // 3. Try pick{i}_id: numeric or string hero ID
    let id_key = format!("pick{i}_id");
    if let Some(val) = team.get(&id_key) {
        let hid_opt = if let Some(n) = val.as_u64() {
            Some(n as u32)
        } else if let Some(s) = val.as_str() {
            s.parse::<u32>().ok()
        } else {
            None
        };
        if let Some(hid) = hid_opt {
            if hid > 0 {
                if let Some(name) = hero_names.get(&hid) {
                    return Some(name.clone());
                }
            }
        }
    }

    None
}

fn extract_hero_from_player_val(
    pval: &serde_json::Value,
    hero_names: &HashMap<u32, String>,
) -> Option<String> {
    if let Some(h) = pval.get("hero_name").or_else(|| pval.get("hero")).and_then(|v| v.as_str()) {
        if !h.is_empty() {
            return Some(h.to_string());
        }
    }
    let hid_opt = pval.get("hero_id").and_then(|v| v.as_u64()).map(|n| n as u32)
        .or_else(|| pval.get("hero_id").and_then(|v| v.as_str()).and_then(|s| s.parse::<u32>().ok()));
    if let Some(hid) = hid_opt {
        if hid > 0 {
            if let Some(name) = hero_names.get(&hid) {
                return Some(name.clone());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_numeric_draft_pick_without_api_lock() {
        let mut heroes = HashMap::new();
        heroes.insert(1, "npc_dota_hero_antimage".to_string());
        let payload = serde_json::json!({ "pick0_id": 1 });

        assert_eq!(
            extract_hero_from_pick(&payload, 0, &heroes),
            Some("npc_dota_hero_antimage".to_string())
        );
    }
}
