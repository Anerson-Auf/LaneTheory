use crate::models::{HeroData, HeroMatchup, ItemData, ItemPopularity};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;

const CACHE_DIR: &str = "cache";
static EMBEDDED_HEROES_JSON: &str = include_str!("../cache/heroes.json");
static EMBEDDED_ITEMS_JSON: &str = include_str!("../cache/items.json");
// A last-known-good rank snapshot is shipped with the app.  Network data can
// replace it, but an OpenDota outage must not turn every recommendation into 50%.
static EMBEDDED_HERO_STATS_JSON: &str = include_str!("../cache/hero_stats.json");

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HeroBracketWinrates {
    pub hero_id: u32,
    pub herald_wr: f32,
    pub guardian_wr: f32,
    pub crusader_wr: f32,
    pub archon_wr: f32,
    pub legend_wr: f32,
    pub ancient_wr: f32,
    pub divine_wr: f32,
    pub immortal_wr: f32,
    pub overall_wr: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AbilityData {
    pub cooldowns: Vec<i32>,
    pub image_url: String,
    pub localized_name: String,
}

impl HeroBracketWinrates {
    pub fn get_winrate(&self, bracket: crate::models::RankBracket) -> f32 {
        let wr = match bracket {
            crate::models::RankBracket::All => self.overall_wr,
            crate::models::RankBracket::Herald => self.herald_wr,
            crate::models::RankBracket::Guardian => self.guardian_wr,
            crate::models::RankBracket::Crusader => self.crusader_wr,
            crate::models::RankBracket::Archon => self.archon_wr,
            crate::models::RankBracket::Legend => self.legend_wr,
            crate::models::RankBracket::Ancient => self.ancient_wr,
            crate::models::RankBracket::Divine => self.divine_wr,
            crate::models::RankBracket::Immortal => self.immortal_wr,
        };
        if wr > 0.0 {
            wr
        } else {
            self.overall_wr.max(50.0)
        }
    }
}

pub struct DotaApiClient {
    client: reqwest::Client,
    pub heroes: HashMap<u32, HeroData>,
    pub heroes_by_name: HashMap<String, HeroData>,
    pub items_by_id: HashMap<u32, ItemData>,
    pub items_by_name: HashMap<String, ItemData>,
    pub matchups_cache: HashMap<u32, Vec<HeroMatchup>>,
    pub popularity_cache: HashMap<u32, ItemPopularity>,
    pub bracket_winrates: HashMap<u32, HeroBracketWinrates>,
    pub abilities: HashMap<String, AbilityData>,
    /// Hero internal name -> current ultimate ability. Derived from OpenDota's
    /// hero_abilities constants, so a patch can update it without code changes.
    pub hero_ultimate_abilities: HashMap<String, String>,
}

impl DotaApiClient {
    pub async fn new() -> Self {
        let client = reqwest::Client::builder()
            // OpenDota constants are a large compressed document; 8 seconds is
            // routinely too short over a VPN.  HTTP/1.1 also behaves more
            // predictably through the local CONNECT proxy used on this machine.
            .connect_timeout(std::time::Duration::from_secs(12))
            .timeout(std::time::Duration::from_secs(45))
            .http1_only()
            .gzip(true)
            .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) DotaAssistant/1.0")
            .build()
            .unwrap_or_default();

        let _ = fs::create_dir_all(format!("{CACHE_DIR}/matchups"));
        let _ = fs::create_dir_all(format!("{CACHE_DIR}/popularity"));

        let mut instance = Self {
            client,
            heroes: HashMap::new(),
            heroes_by_name: HashMap::new(),
            items_by_id: HashMap::new(),
            items_by_name: HashMap::new(),
            matchups_cache: HashMap::new(),
            popularity_cache: HashMap::new(),
            bracket_winrates: HashMap::new(),
            abilities: HashMap::new(),
            hero_ultimate_abilities: HashMap::new(),
        };

        // Instantly load embedded constants (127 heroes, 501 items)
        instance.load_embedded_data();
        instance.load_data_pack();

        instance.load_or_fetch_heroes().await;
        instance.load_or_fetch_items().await;
        instance.load_or_fetch_hero_stats().await;
        instance.load_or_fetch_abilities().await;
        instance.load_or_fetch_hero_abilities().await;
        instance.write_data_pack();

        instance
    }

    fn load_data_pack(&mut self) {
        if !crate::datapack::DataPack::exists() {
            return;
        }
        match crate::datapack::DataPack::load() {
            Ok(pack) => {
                self.replace_heroes(pack.heroes().to_vec());
                self.replace_items(pack.items().to_vec());
                if !pack.bracket_winrates().is_empty() {
                    self.bracket_winrates = pack.bracket_winrates()
                        .iter()
                        .cloned()
                        .map(|entry| (entry.hero_id, entry))
                        .collect();
                }
                if !pack.abilities().is_empty() {
                    self.abilities = pack.abilities().clone();
                }
                if !pack.hero_ultimate_abilities().is_empty() {
                    self.hero_ultimate_abilities = pack.hero_ultimate_abilities().clone();
                }
                println!(
                    "YPK загружен: {} героев, {} предметов, {} способностей",
                    self.heroes.len(),
                    self.items_by_id.len(),
                    self.abilities.len()
                );
            }
            Err(error) => {
                eprintln!("YPK не загружен; использую bootstrap-кэш: {error}");
            }
        }
    }

    fn write_data_pack(&self) {
        let mut heroes = self.heroes.values().cloned().collect::<Vec<_>>();
        heroes.sort_by_key(|hero| hero.id);
        let mut items = self.items_by_id.values().cloned().collect::<Vec<_>>();
        items.sort_by_key(|item| item.id);
        let mut bracket_winrates = self.bracket_winrates.values().cloned().collect::<Vec<_>>();
        bracket_winrates.sort_by_key(|entry| entry.hero_id);
        match crate::datapack::DataPack::write(
            heroes,
            items,
            bracket_winrates,
            self.abilities.clone(),
            self.hero_ultimate_abilities.clone(),
        ) {
            Ok(path) => println!("YPK обновлён: {}", path.display()),
            Err(error) => eprintln!("Не удалось обновить YPK: {error}"),
        }
    }

    /// A CI data refresh must fail closed when a public source did not
    /// provide the patch-sensitive datasets. The ordinary overlay remains
    /// intentionally usable with its embedded fallback instead.
    pub fn has_publishable_data_pack(&self) -> bool {
        self.heroes.len() >= 100
            && self.items_by_id.len() >= 300
            // OpenDota can legitimately omit brackets for a portion of the
            // roster (the public CI response currently carries 94 entries),
            // so require broad coverage rather than an impossible 100%.
            && self.bracket_winrates.len() >= 80
            && self.abilities.len() >= 1_000
            && self.hero_ultimate_abilities.len() >= 100
    }

    async fn load_or_fetch_heroes(&mut self) {
        let cache_file = format!("{CACHE_DIR}/heroes.json");

        if let Ok(data) = fs::read_to_string(&cache_file) {
            if let Ok(heroes) = serde_json::from_str::<Vec<HeroData>>(&data) {
                if !heroes.is_empty() {
                    for hero in heroes {
                        self.heroes_by_name.insert(hero.name.clone(), hero.clone());
                        self.heroes.insert(hero.id, hero);
                    }
                }
            }
        }

        if self.heroes.len() < 100 {
            println!("Загрузка списка героев из OpenDota API...");
            match self.client.get("https://api.opendota.com/api/heroes").send().await {
                Ok(resp) => {
                    if let Ok(heroes) = resp.json::<Vec<HeroData>>().await {
                        let _ = fs::write(&cache_file, serde_json::to_string(&heroes).unwrap_or_default());
                        for hero in heroes {
                            self.heroes_by_name.insert(hero.name.clone(), hero.clone());
                            self.heroes.insert(hero.id, hero);
                        }
                    }
                }
                Err(e) => eprintln!("Не удалось скачать героев с OpenDota: {e}"),
            }
        }

        println!("Героев загружено: {}", self.heroes.len());
    }

    fn replace_heroes(&mut self, heroes: Vec<HeroData>) {
        self.heroes.clear();
        self.heroes_by_name.clear();
        for hero in heroes {
            self.heroes_by_name.insert(hero.name.clone(), hero.clone());
            self.heroes.insert(hero.id, hero);
        }
    }

    async fn load_or_fetch_items(&mut self) {
        let cache_file = format!("{CACHE_DIR}/items.json");

        if let Ok(data) = fs::read_to_string(&cache_file) {
            if let Ok(items) = serde_json::from_str::<Vec<ItemData>>(&data) {
                if !items.is_empty() {
                    self.replace_items(items);
                }
            }
        }

        // Items are patch-sensitive: a count check leaves an old price table
        // looking valid forever. Query once per application start; cached and
        // embedded data still keep the overlay functional offline.
        println!("Проверка актуальности item schema из OpenDota...");
        match self.client.get("https://api.opendota.com/api/constants/items")
            // Some VPN/proxy paths serve a malformed compressed response for
            // this endpoint.  Asking for identity avoids a decode failure and
            // costs very little for an item schema requested once at startup.
            .header(reqwest::header::ACCEPT_ENCODING, "identity")
            .timeout(std::time::Duration::from_secs(8))
            .send().await
        {
            Ok(resp) if resp.status().is_success() => match resp.text().await {
                Ok(body) => match serde_json::from_str::<HashMap<String, serde_json::Value>>(&body) {
                Ok(raw_items) => {
                    let item_list = Self::parse_item_schema(raw_items);
                    if item_list.len() >= 300 {
                        let _ = fs::write(&cache_file, serde_json::to_string(&item_list).unwrap_or_default());
                        self.replace_items(item_list);
                        println!("Item schema обновлён: {} предметов", self.items_by_id.len());
                    } else {
                        eprintln!("OpenDota вернул неполный item schema; оставляю кэш");
                    }
                }
                Err(error) => eprintln!("Не удалось разобрать item schema; оставляю кэш: {error}"),
                },
                Err(error) => eprintln!("Не удалось прочитать item schema; оставляю кэш: {error}"),
            },
            Ok(resp) => eprintln!("OpenDota item schema вернул HTTP {}; оставляю кэш", resp.status()),
            Err(error) => eprintln!("Не удалось обновить item schema за 8с; работаю с кэшем: {error}"),
        }

        println!("Предметов загружено: {}", self.items_by_id.len());
    }

    fn replace_items(&mut self, items: Vec<ItemData>) {
        self.items_by_id.clear();
        self.items_by_name.clear();
        for item in items {
            self.items_by_name.insert(item.clean_name().to_string(), item.clone());
            self.items_by_id.insert(item.id, item);
        }
    }

    fn parse_item_schema(raw_items: HashMap<String, serde_json::Value>) -> Vec<ItemData> {
        raw_items.into_iter().filter_map(|(raw_name, val)| {
            let id = val.get("id").and_then(|id| id.as_u64())? as u32;
            Some(ItemData {
                id,
                localized_name: val.get("dname").and_then(|name| name.as_str()).unwrap_or(&raw_name).to_string(),
                cost: val.get("cost").and_then(|cost| cost.as_u64()).map(|cost| cost as u32),
                name: raw_name,
            })
        }).collect()
    }

    async fn load_or_fetch_hero_stats(&mut self) {
        let cache_file = format!("{CACHE_DIR}/hero_stats.json");
        let mut loaded = !self.bracket_winrates.is_empty();

        // Keep genuine rank statistics available even before the first
        // successful network request (or while OpenDota is unreachable).
        if let Ok(raw_stats) = serde_json::from_str::<Vec<serde_json::Value>>(EMBEDDED_HERO_STATS_JSON) {
            self.parse_and_store_hero_stats(&raw_stats);
        }

        if let Ok(data) = fs::read_to_string(&cache_file) {
            if let Ok(raw_stats) = serde_json::from_str::<Vec<serde_json::Value>>(&data) {
                self.parse_and_store_hero_stats(&raw_stats);
                loaded = !self.bracket_winrates.is_empty();
            } else {
                eprintln!("Кэш hero_stats.json повреждён; запрашиваю свежую rank-статистику");
            }
        }

        if !loaded {
            println!("Загрузка статистики по рангам из OpenDota API...");
            match self.client
                .get("https://api.opendota.com/api/heroStats")
                .send()
                .await
            {
                Ok(resp) if !resp.status().is_success() => {
                    eprintln!("OpenDota heroStats вернул HTTP {}", resp.status());
                }
                Ok(resp) => match resp.text().await {
                    Ok(body) => match serde_json::from_str::<Vec<serde_json::Value>>(&body) {
                    Ok(raw_stats) if raw_stats.is_empty() => eprintln!("OpenDota heroStats вернул пустой набор данных"),
                    Ok(raw_stats) => {
                        let _ = fs::write(&cache_file, serde_json::to_string(&raw_stats).unwrap_or_default());
                        self.parse_and_store_hero_stats(&raw_stats);
                    }
                    Err(error) => eprintln!("Не удалось разобрать ответ OpenDota heroStats: {error}"),
                    },
                    Err(error) => eprintln!("Не удалось прочитать ответ OpenDota heroStats: {error}"),
                }
                Err(e) => eprintln!("Не удалось скачать heroStats: {e}"),
            }
        }

        println!("Ранговой статистики героев загружено: {}", self.bracket_winrates.len());
    }

    fn parse_and_store_hero_stats(&mut self, list: &[serde_json::Value]) {
        for obj in list {
            if let Some(id) = obj.get("id").and_then(|v| v.as_u64()).map(|v| v as u32) {
                let calc_wr = |pick_key: &str, win_key: &str| -> f32 {
                    let pick = obj.get(pick_key).and_then(|v| v.as_u64()).unwrap_or(0);
                    let win = obj.get(win_key).and_then(|v| v.as_u64()).unwrap_or(0);
                    if pick > 0 {
                        (win as f32 / pick as f32) * 100.0
                    } else {
                        0.0
                    }
                };

                let pub_wr = calc_wr("pub_pick", "pub_win");
                let mut stats = HeroBracketWinrates {
                    hero_id: id,
                    herald_wr: calc_wr("1_pick", "1_win"),
                    guardian_wr: calc_wr("2_pick", "2_win"),
                    crusader_wr: calc_wr("3_pick", "3_win"),
                    archon_wr: calc_wr("4_pick", "4_win"),
                    legend_wr: calc_wr("5_pick", "5_win"),
                    ancient_wr: calc_wr("6_pick", "6_win"),
                    divine_wr: calc_wr("7_pick", "7_win"),
                    immortal_wr: calc_wr("8_pick", "8_win"),
                    overall_wr: if pub_wr > 0.0 { pub_wr } else { 50.0 },
                };

                if stats.immortal_wr == 0.0 {
                    stats.immortal_wr = if stats.divine_wr > 0.0 { stats.divine_wr } else { stats.overall_wr };
                }

                self.bracket_winrates.insert(id, stats);
            }
        }
    }

    async fn load_or_fetch_abilities(&mut self) {
        if !self.abilities.is_empty() {
            println!("Способностей из YPK загружено: {}", self.abilities.len());
            return;
        }
        let cache_file = format!("{CACHE_DIR}/abilities.json");
        let raw = match fs::read_to_string(&cache_file) {
            Ok(cached) => serde_json::from_str::<HashMap<String, serde_json::Value>>(&cached).ok(),
            Err(_) => None,
        };
        let raw = match raw {
            Some(raw) if !raw.is_empty() => raw,
            _ => match self.client.get("https://api.opendota.com/api/constants/abilities")
                .send().await
            {
                Ok(response) if response.status().is_success() => match response.text().await
                    .ok().and_then(|text| serde_json::from_str::<HashMap<String, serde_json::Value>>(&text).ok()) {
                    Some(raw) => {
                        let _ = fs::write(&cache_file, serde_json::to_string(&raw).unwrap_or_default());
                        raw
                    }
                    None => {
                        eprintln!("Не удалось разобрать OpenDota ability constants");
                        return;
                    }
                },
                Ok(response) => {
                    eprintln!("OpenDota abilities вернул HTTP {}", response.status());
                    return;
                }
                Err(error) => {
                    eprintln!("Не удалось загрузить OpenDota abilities: {error}");
                    return;
                }
            },
        };

        for (key, value) in raw {
            let cooldowns = value.get("cd").and_then(|cd| cd.as_array())
                .map(|items| items.iter().filter_map(|value| {
                    value.as_i64().map(|number| number as i32)
                        .or_else(|| value.as_str().and_then(|text| text.parse::<f32>().ok()).map(|number| number.round() as i32))
                }).collect())
                .unwrap_or_default();
            let image_url = value.get("img").and_then(|value| value.as_str())
                .map(|path| format!("https://cdn.cloudflare.steamstatic.com{path}"))
                .unwrap_or_default();
            let localized_name = value.get("dname").and_then(|value| value.as_str())
                .unwrap_or(&key)
                .to_string();
            self.abilities.insert(key, AbilityData { cooldowns, image_url, localized_name });
        }
        println!("Способностей с cooldown-данными загружено: {}", self.abilities.len());
    }

    async fn load_or_fetch_hero_abilities(&mut self) {
        if !self.hero_ultimate_abilities.is_empty() {
            println!("Ультимейтов героев из YPK загружено: {}", self.hero_ultimate_abilities.len());
            return;
        }
        let cache_file = format!("{CACHE_DIR}/hero_abilities.json");
        let raw = match fs::read_to_string(&cache_file) {
            Ok(cached) => serde_json::from_str::<HashMap<String, serde_json::Value>>(&cached).ok(),
            Err(_) => None,
        };
        let raw = match raw {
            Some(raw) if !raw.is_empty() => raw,
            _ => match self.client.get("https://api.opendota.com/api/constants/hero_abilities").send().await {
                Ok(response) if response.status().is_success() => match response.text().await
                    .ok().and_then(|text| serde_json::from_str::<HashMap<String, serde_json::Value>>(&text).ok()) {
                    Some(raw) => {
                        let _ = fs::write(&cache_file, serde_json::to_string(&raw).unwrap_or_default());
                        raw
                    }
                    None => {
                        eprintln!("Не удалось разобрать OpenDota hero_abilities");
                        return;
                    }
                },
                Ok(response) => {
                    eprintln!("OpenDota hero_abilities вернул HTTP {}", response.status());
                    return;
                }
                Err(error) => {
                    eprintln!("Не удалось загрузить OpenDota hero_abilities: {error}");
                    return;
                }
            },
        };

        for (hero_name, value) in raw {
            // The data describes ability slots in the same order as the hero HUD.
            // Index 5 is the hero's ultimate slot; the elements before/after it
            // include basic abilities, hidden placeholders, innates and facets.
            let ultimate = value.get("abilities")
                .and_then(|abilities| abilities.as_array())
                .and_then(|abilities| abilities.get(5))
                .and_then(|ability| ability.as_str())
                .filter(|ability| !ability.is_empty() && *ability != "generic_hidden");
            if let Some(ultimate) = ultimate {
                self.hero_ultimate_abilities.insert(hero_name, ultimate.to_string());
            }
        }
        println!("Ультимейтов героев загружено: {}", self.hero_ultimate_abilities.len());
    }

    pub fn ultimates_for_enemies(&self, enemy_hero_names: &[String]) -> Vec<crate::models::TrackedSpell> {
        enemy_hero_names.iter().filter_map(|enemy_name| {
            let hero_key = if enemy_name.starts_with("npc_dota_hero_") {
                enemy_name.clone()
            } else {
                format!("npc_dota_hero_{enemy_name}")
            };
            let ability_key = self.hero_ultimate_abilities.get(&hero_key)?;
            let ability = self.abilities.get(ability_key)?;
            let hero_name = self.find_hero(&hero_key)
                .map(|hero| hero.localized_name.clone())
                .unwrap_or_else(|| enemy_name.strip_prefix("npc_dota_hero_").unwrap_or(enemy_name).replace('_', " "));
            Some(crate::models::TrackedSpell {
                hero_name,
                spell_name: ability_key.clone(),
                localized_spell: ability.localized_name.clone(),
                ability_key: ability_key.clone(),
                base_cd: ability.cooldowns.first().copied().unwrap_or(0),
                cooldowns: ability.cooldowns.clone(),
                ability_image: ability.image_url.clone(),
                ultimate_level: None,
                enemy_level: None,
                on_cooldown_until: None,
            })
        }).collect()
    }

    /// `None` means the source has no measurement for this hero.  It is never
    /// rendered as a made-up 50% win rate.
    pub fn get_hero_winrate_for_bracket(
        &self,
        hero_id: u32,
        bracket: crate::models::RankBracket,
    ) -> Option<f32> {
        self.bracket_winrates
            .get(&hero_id)
            .map(|stats| stats.get_winrate(bracket))
    }

    /// Current meta is calculated from the downloaded rank bracket data, not a snapshot
    /// committed into the binary. Position suitability is inferred from OpenDota's role tags.
    pub fn meta_heroes_for_position(
        &self,
        position: crate::models::PlayerPosition,
        bracket: crate::models::RankBracket,
    ) -> Vec<(String, String, f32)> {
        let mut heroes: Vec<_> = self.heroes.values()
            .filter(|hero| crate::advisor::Advisor::position_score(hero, position) > 0.0)
            .filter_map(|hero| {
                let winrate = self.get_hero_winrate_for_bracket(hero.id, bracket)?;
                Some((
                    hero.short_name().to_string(),
                    hero.localized_name.clone(),
                    winrate,
                    crate::advisor::Advisor::position_score(hero, position),
                ))
            })
            .collect();

        // Require meaningful role evidence first, then rank by real win rate. The affinity
        // only breaks ties so a generic role heuristic cannot overrule live statistics.
        heroes.sort_by(|a, b| b.2.partial_cmp(&a.2)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.3.partial_cmp(&a.3).unwrap_or(std::cmp::Ordering::Equal)));
        heroes.into_iter().take(7).map(|(id, name, wr, _)| (id, name, wr)).collect()
    }

    pub async fn get_matchups(&mut self, hero_id: u32) -> Vec<HeroMatchup> {
        if let Some(cached) = self.matchups_cache.get(&hero_id) {
            return cached.clone();
        }

        let cache_file = format!("{CACHE_DIR}/matchups/{hero_id}.json");
        if let Ok(data) = fs::read_to_string(&cache_file) {
            if let Ok(matchups) = serde_json::from_str::<Vec<HeroMatchup>>(&data) {
                self.matchups_cache.insert(hero_id, matchups.clone());
                return matchups;
            }
        }

        let url = format!("https://api.opendota.com/api/heroes/{hero_id}/matchups");
        if let Ok(resp) = self.client.get(&url).send().await {
            if let Ok(matchups) = resp.json::<Vec<HeroMatchup>>().await {
                let _ = fs::write(&cache_file, serde_json::to_string(&matchups).unwrap_or_default());
                self.matchups_cache.insert(hero_id, matchups.clone());
                return matchups;
            }
        }

        Vec::new()
    }

    pub async fn get_item_popularity(&mut self, hero_id: u32) -> ItemPopularity {
        if let Some(cached) = self.popularity_cache.get(&hero_id) {
            return cached.clone();
        }

        let cache_file = format!("{CACHE_DIR}/popularity/{hero_id}.json");
        if let Ok(data) = fs::read_to_string(&cache_file) {
            if let Ok(pop) = serde_json::from_str::<ItemPopularity>(&data) {
                self.popularity_cache.insert(hero_id, pop.clone());
                return pop;
            }
        }

        let url = format!("https://api.opendota.com/api/heroes/{hero_id}/itemPopularity");
        if let Ok(resp) = self.client.get(&url).send().await {
            if let Ok(pop) = resp.json::<ItemPopularity>().await {
                let _ = fs::write(&cache_file, serde_json::to_string(&pop).unwrap_or_default());
                self.popularity_cache.insert(hero_id, pop.clone());
                return pop;
            }
        }

        ItemPopularity::default()
    }

    pub fn find_hero(&self, name_or_sub: &str) -> Option<&HeroData> {
        if let Some(h) = self.heroes_by_name.get(name_or_sub) {
            return Some(h);
        }
        let clean = name_or_sub.strip_prefix("npc_dota_hero_").unwrap_or(name_or_sub);
        if let Some(h) = self.heroes.values().find(|h| h.short_name() == clean) {
            return Some(h);
        }
        let cdn_alias = crate::advisor::hero_cdn_name(clean);
        if let Some(h) = self.heroes.values().find(|h| h.short_name() == cdn_alias) {
            return Some(h);
        }
        self.heroes.values().find(|h| h.localized_name.eq_ignore_ascii_case(clean))
    }

    /// Runs without holding the shared analytics cache lock.  Profile enrichment is
    /// optional and must never delay draft analysis.
    pub async fn fetch_player_profile(
        client: reqwest::Client,
        heroes: HashMap<u32, HeroData>,
        account_id: u32,
    ) -> Option<crate::models::PlayerProfile> {
        let url = format!("https://api.opendota.com/api/players/{}", account_id);
        let resp = client.get(&url).send().await.ok()?;
        let val: serde_json::Value = resp.json().await.ok()?;

        let profile = val.get("profile")?;
        let personaname = profile.get("personaname")?.as_str()?.to_string();
        let avatar_url = profile
            .get("avatarfull")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let rank_tier = val.get("rank_tier").and_then(|v| v.as_u64()).map(|v| v as u32);
        let leaderboard_rank = val.get("leaderboard_rank").and_then(|v| v.as_u64()).map(|v| v as u32);
        let rank_label = crate::models::parse_rank_tier(rank_tier, leaderboard_rank);

        let mut wins = 0;
        let mut losses = 0;
        let mut winrate = 50.0;
        let wl_url = format!("https://api.opendota.com/api/players/{}/wl", account_id);
        if let Ok(wl_resp) = client.get(&wl_url).send().await {
            if let Ok(wl_val) = wl_resp.json::<serde_json::Value>().await {
                wins = wl_val.get("win").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                losses = wl_val.get("lose").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                let total = wins + losses;
                if total > 0 {
                    winrate = (wins as f32 / total as f32) * 100.0;
                }
            }
        }

        let mut top_heroes = Vec::new();
        let heroes_url = format!("https://api.opendota.com/api/players/{}/heroes", account_id);
        if let Ok(h_resp) = client.get(&heroes_url).send().await {
            if let Ok(h_arr) = h_resp.json::<Vec<serde_json::Value>>().await {
                for item in h_arr.into_iter().take(3) {
                    if let Some(h_id_str) = item.get("hero_id").and_then(|v| v.as_str()) {
                        if let Ok(h_id) = h_id_str.parse::<u32>() {
                            let games = item.get("games").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                            let h_win = item.get("win").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                            let h_wr = if games > 0 { (h_win as f32 / games as f32) * 100.0 } else { 0.0 };
                            if let Some(h) = heroes.get(&h_id) {
                                top_heroes.push((h.localized_name.clone(), games, h_wr));
                            }
                        }
                    }
                }
            }
        }

        let mut dominant_position = None;
        let recent_url = format!("https://api.opendota.com/api/players/{}/recentMatches", account_id);
        if let Ok(rec_resp) = client.get(&recent_url).send().await {
            if let Ok(rec_matches) = rec_resp.json::<Vec<serde_json::Value>>().await {
                let mut mid_count = 0;
                let mut off_count = 0;
                let mut safe_carry = 0;
                let mut safe_sup = 0;

                for m in rec_matches.iter().take(15) {
                    let lane_role = m.get("lane_role").and_then(|v| v.as_u64()).unwrap_or(0);
                    let h_id = m.get("hero_id").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                    let hero_pos = heroes.get(&h_id).map(crate::advisor::Advisor::infer_primary_position);

                    match lane_role {
                        2 => mid_count += 1,
                        3 => off_count += 1,
                        1 => {
                            if let Some(crate::models::PlayerPosition::Pos5HardSupport) = hero_pos {
                                safe_sup += 1;
                            } else {
                                safe_carry += 1;
                            }
                        }
                        _ => {
                            if let Some(pos) = hero_pos {
                                match pos {
                                    crate::models::PlayerPosition::Pos1Carry => safe_carry += 1,
                                    crate::models::PlayerPosition::Pos2Mid => mid_count += 1,
                                    crate::models::PlayerPosition::Pos3Offlane => off_count += 1,
                                    crate::models::PlayerPosition::Pos4SoftSupport => off_count += 1,
                                    crate::models::PlayerPosition::Pos5HardSupport => safe_sup += 1,
                                }
                            }
                        }
                    }
                }

                let max_votes = mid_count.max(off_count).max(safe_carry).max(safe_sup);
                if max_votes > 0 {
                    if max_votes == mid_count {
                        dominant_position = Some(crate::models::PlayerPosition::Pos2Mid);
                    } else if max_votes == off_count {
                        dominant_position = Some(crate::models::PlayerPosition::Pos3Offlane);
                    } else if max_votes == safe_carry {
                        dominant_position = Some(crate::models::PlayerPosition::Pos1Carry);
                    } else {
                        dominant_position = Some(crate::models::PlayerPosition::Pos5HardSupport);
                    }
                }
            }
        }

        Some(crate::models::PlayerProfile {
            account_id,
            personaname,
            avatar_url,
            rank_tier,
            leaderboard_rank,
            rank_label,
            wins,
            losses,
            winrate,
            top_heroes,
            dominant_position,
        })
    }

    pub fn profile_fetch_context(&self) -> (reqwest::Client, HashMap<u32, HeroData>) {
        (self.client.clone(), self.heroes.clone())
    }

    fn load_embedded_data(&mut self) {
        if let Ok(heroes) = serde_json::from_str::<Vec<HeroData>>(EMBEDDED_HEROES_JSON) {
            for hero in heroes {
                self.heroes_by_name.insert(hero.name.clone(), hero.clone());
                self.heroes.insert(hero.id, hero);
            }
        }
        if let Ok(items) = serde_json::from_str::<Vec<ItemData>>(EMBEDDED_ITEMS_JSON) {
            for item in items {
                self.items_by_name.insert(item.clean_name().to_string(), item.clone());
                self.items_by_id.insert(item.id, item);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::RankBracket;

    #[test]
    fn bracket_stat_uses_selected_rank_and_falls_back_to_overall() {
        let stats = HeroBracketWinrates {
            herald_wr: 55.5,
            immortal_wr: 0.0,
            overall_wr: 51.0,
            ..Default::default()
        };
        assert_eq!(stats.get_winrate(RankBracket::Herald), 55.5);
        assert_eq!(stats.get_winrate(RankBracket::Immortal), 51.0);
    }

    #[test]
    fn parses_opendota_object_item_schema() {
        let raw = serde_json::from_str::<HashMap<String, serde_json::Value>>(r#"{
            "blink": {"id": 1, "dname": "Blink Dagger", "cost": 2250},
            "broken": {"dname": "Broken"}
        }"#).unwrap();
        let items = DotaApiClient::parse_item_schema(raw);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "blink");
        assert_eq!(items[0].localized_name, "Blink Dagger");
        assert_eq!(items[0].cost, Some(2250));
    }
}
