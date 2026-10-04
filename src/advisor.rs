use crate::api::DotaApiClient;
use crate::models::{HeroData, PlayerPosition, PopularItemEntry, RecommendedHero, SituationalItem};
use std::collections::HashSet;

pub struct Advisor;

impl Advisor {
    pub async fn recommend_counter_picks(
        api: &mut DotaApiClient,
        enemy_hero_names: &[String],
        ally_hero_names: &[String],
        rank_bracket: crate::models::RankBracket,
    ) -> Vec<RecommendedHero> {
        let mut enemy_ids = Vec::new();
        for name in enemy_hero_names {
            if let Some(h) = api.find_hero(name) {
                enemy_ids.push(h.id);
            }
        }

        let mut picked_ids = HashSet::new();
        for id in &enemy_ids {
            picked_ids.insert(*id);
        }
        for name in ally_hero_names {
            if let Some(h) = api.find_hero(name) {
                picked_ids.insert(h.id);
            }
        }

        // Enemy matchups remain the main signal. Hero-versus-hero win rates
        // are adversarial data, so using them as "synergy" would be a serious
        // modelling error. Allies instead contribute a small composition-fit
        // signal based on roles that are missing from the current team.
        let ally_role_coverage = ally_hero_names.iter()
            .filter_map(|name| api.find_hero(name))
            .flat_map(|hero| hero.roles.iter().cloned())
            .collect::<HashSet<_>>();

        if enemy_ids.is_empty() {
            return Vec::new();
        }

        // OpenDota's endpoint is indexed by the hero whose matchup table is being
        // requested.  The old implementation fetched that table for every possible
        // recommendation (~127 serial requests).  Invert the relation: fetch each
        // selected enemy once (at most five requests), then score its opponents locally.
        let mut candidate_scores: std::collections::HashMap<u32, (f32, usize, usize, u32, Vec<String>)> =
            std::collections::HashMap::new();
        for enemy_id in &enemy_ids {
            let Some(enemy_hero) = api.heroes.get(enemy_id).cloned() else {
                continue;
            };
            for matchup in api.get_matchups(*enemy_id).await {
                if picked_ids.contains(&matchup.hero_id) || matchup.games_played == 0 {
                    continue;
                }
                // `wins` belongs to the queried enemy hero, therefore reverse it for
                // the prospective counter-pick.
                let candidate_wr = 100.0 - matchup.winrate();
                let advantage = candidate_wr - 50.0;
                let entry = candidate_scores
                    .entry(matchup.hero_id)
                    .or_insert_with(|| (0.0, 0, 0, 0, Vec::new()));
                entry.0 += advantage;
                entry.1 += 1;
                entry.3 += matchup.games_played;
                if advantage >= 1.5 {
                    entry.2 += 1;
                    entry.4.push(format!(
                        "+{}% против {}",
                        format_adv(advantage),
                        enemy_hero.localized_name
                    ));
                }
            }
        }

        let mut scored_heroes: Vec<RecommendedHero> = Vec::new();
        for (candidate_id, (mut total_advantage, count, coverage, evidence_games, mut reasons)) in candidate_scores {
            let Some(candidate) = api.heroes.get(&candidate_id).cloned() else {
                continue;
            };
            if count == 0 {
                continue;
            }

            if !ally_role_coverage.is_empty() {
                let missing_roles = candidate.roles.iter()
                    .filter(|role| !ally_role_coverage.contains(*role))
                    .count();
                // At most +1.4 pp-equivalent: enough to break ties in favour
                // of a balanced lineup, never enough to override a counter.
                let draft_fit = (missing_roles as f32 * 0.35).min(1.4);
                if draft_fit > 0.0 {
                    total_advantage += draft_fit;
                    reasons.push(format!("+{draft_fit:.1}% баланс с союзниками"));
                }
            }

            // Factor in bracket-specific meta winrate if specified
            if rank_bracket != crate::models::RankBracket::All {
                if let Some(rank_wr) = api.get_hero_winrate_for_bracket(candidate.id, rank_bracket) {
                    let bracket_adv = (rank_wr - 50.0) * 0.5;
                    total_advantage += bracket_adv;
                    if rank_wr >= 52.0 {
                    reasons.push(format!("{:.1}% винрейт ({})", rank_wr, rank_bracket.title_ru()));
                    }
                }
            }

            let avg_adv = total_advantage / count as f32;
            let coverage_ratio = coverage as f32 / enemy_ids.len() as f32;
            let advantage_component = (avg_adv.max(0.0) / 10.0).min(1.0);
            let evidence_component = ((evidence_games as f32 + 1.0).ln() / 9.0).min(1.0);
            let priority_score = (100.0
                * (0.60 * coverage_ratio + 0.28 * advantage_component + 0.12 * evidence_component))
                .round()
                .clamp(0.0, 100.0) as u8;

            scored_heroes.push(RecommendedHero {
                hero: candidate,
                // A counter card is read as an advantage against one opposing
                // hero. Showing the aggregate across four/five opponents made
                // a routine +4–5 pp matchup look like a fictitious +20 pp.
                advantage: avg_adv,
                avg_winrate: 50.0 + avg_adv,
                countered_enemies: coverage,
                evaluated_enemies: count,
                evidence_games,
                priority_score,
                reasons,
            });
        }

        scored_heroes.sort_by(|a, b| b.priority_score.cmp(&a.priority_score)
            .then_with(|| b.advantage.partial_cmp(&a.advantage).unwrap_or(std::cmp::Ordering::Equal)));
        scored_heroes.truncate(30);

        scored_heroes
    }

    pub fn filter_counters_by_position(
        counters: &[RecommendedHero],
        pos: PlayerPosition,
    ) -> Vec<RecommendedHero> {
        let mut matching: Vec<(RecommendedHero, f32)> = counters
            .iter()
            .filter_map(|rec| {
                let role_confidence = Self::position_confidence(&rec.hero, pos);
                // Do not fill the list with a high-matchup hero who is only a
                // theoretical fit for the requested lane. A short, credible
                // list is better than seven misleading "counters".
                (role_confidence >= 0.45).then(|| (rec.clone(), role_confidence))
            })
            .collect();
        matching.sort_by(|(left, left_fit), (right, right_fit)| {
            let left_score = left.priority_score as f32 * (0.55 + 0.45 * left_fit);
            let right_score = right.priority_score as f32 * (0.55 + 0.45 * right_fit);
            right_score.partial_cmp(&left_score).unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| right_fit.partial_cmp(left_fit).unwrap_or(std::cmp::Ordering::Equal))
                .then_with(|| right.priority_score.cmp(&left.priority_score))
        });
        matching.into_iter().take(7).map(|(rec, _)| rec).collect()
    }

    pub fn analyze_draft_weaknesses(api: &DotaApiClient, enemy_hero_names: &[String]) -> Vec<String> {
        let mut warnings = Vec::new();
        if enemy_hero_names.len() < 2 {
            return warnings;
        }

        let mut disabler_count = 0;
        let mut melee_count = 0;
        let mut nuker_count = 0;
        let total = enemy_hero_names.len();

        for name in enemy_hero_names {
            if let Some(h) = api.find_hero(name) {
                if h.roles.iter().any(|r| r == "Disabler") {
                    disabler_count += 1;
                }
                if h.roles.iter().any(|r| r == "Nuker") {
                    nuker_count += 1;
                }
                let s = h.short_name();
                let is_melee = [
                    "antimage", "axe", "bloodseeker", "earthshaker", "juggernaut",
                    "phantom_lancer", "pudge", "sand_king", "sven", "tiny",
                    "slardar", "tidehunter", "riki", "beastmaster", "faceless_void",
                    "skeleton_king", "phantom_assassin", "dragon_knight", "clockwerk",
                    "life_stealer", "huskar", "night_stalker", "broodmother", "bounty_hunter",
                    "spirit_breaker", "alchemist", "brewmaster", "chaos_knight", "meepo",
                    "treant", "ogre_magi", "undying", "nyx_assassin", "naga_siren",
                    "slark", "troll_warlord", "centaur", "magnataur", "timbersaw",
                    "bristleback", "tusk", "abaddon", "elder_titan", "legion_commander",
                    "earth_spirit", "abyssal_underlord", "terrorblade", "monkey_king",
                    "pangolier", "mars", "dawnbreaker", "marci", "primal_beast", "kez", "largo",
                ].contains(&s);
                if is_melee {
                    melee_count += 1;
                }
            }
        }

        if total >= 3 && disabler_count <= 1 {
            warnings.push("[Weakness] Мало станов и контроля — пикайте Storm, Weaver, Puck, Slark".to_string());
        }
        if total >= 3 && melee_count >= 3 {
            warnings.push(format!("[Weakness] {} милишника — топ пик Monkey King, Underlord, Timbersaw", melee_count));
        }
        if total >= 3 && nuker_count <= 1 {
            warnings.push("[Weakness] Почти нет магического урона — броня (Axe, Sven, Кираса) чувствует себя комфортнее".to_string());
        }

        warnings
    }

    pub fn get_situational_items(api: &DotaApiClient, enemy_hero_names: &[String]) -> Vec<SituationalItem> {
        let enemy_shorts: Vec<String> = enemy_hero_names
            .iter()
            .map(|n| n.strip_prefix("npc_dota_hero_").unwrap_or(n).to_string())
            .collect();

        let mut items = Vec::new();
        let mut seen = HashSet::new();

        let mut add_item = |name: &str, reason: &str, prio: u8| {
            if seen.insert(name.to_string()) {
                let loc_name = api
                    .items_by_name
                    .get(name)
                    .map(|i| i.localized_name.as_str())
                    .unwrap_or(name);

                let img = format!(
                    "https://cdn.cloudflare.steamstatic.com/apps/dota2/images/dota_react/items/{}.png",
                    name
                );

                items.push(SituationalItem {
                    item_name: name.to_string(),
                    localized_name: loc_name.to_string(),
                    image_url: img,
                    reason: reason.to_string(),
                    priority: prio,
                });
            }
        };

        // 0. Intelligent Stick Analysis
        let hard_spammers = ["batrider", "bristleback", "phantom_assassin", "skywrath_mage", "zuus", "undying", "shadow_demon", "tidehunter", "rattletrap", "pugna"];
        let moderate_spammers = ["lion", "vengefulspirit", "skeleton_king", "sven", "ogre_magi", "crystal_maiden", "rubick", "lina"];

        let matched_hard: Vec<&str> = enemy_shorts.iter().filter(|s| hard_spammers.contains(&s.as_str())).map(|s| s.as_str()).collect();
        let matched_mod: Vec<&str> = enemy_shorts.iter().filter(|s| moderate_spammers.contains(&s.as_str())).map(|s| s.as_str()).collect();
        // Never freeze a gold difference in a human-written string. Item
        // costs arrive with the current item schema/cache and can change on a
        // patch without requiring an application release.
        let wand_extra_cost = api.items_by_name.get("magic_wand").and_then(|wand| wand.cost)
            .zip(api.items_by_name.get("magic_stick").and_then(|stick| stick.cost))
            .and_then(|(wand, stick)| wand.checked_sub(stick));

        if !matched_hard.is_empty() {
            add_item(
                "magic_wand",
                &format!("[ЗАКУП] Враги спамят скиллами! Обязательно апгрейди Magic Wand: {}", matched_hard.join(", ")),
                6,
            );
        } else if !matched_mod.is_empty() {
            let cost_note = wand_extra_cost
                .map(|extra| format!("; разница сейчас {extra}g"))
                .unwrap_or_else(|| "; цена сверяется с актуальным schema".to_string());
            add_item(
                "magic_stick",
                &format!("[ЗАКУП] Достаточно базового Magic Stick, не переплачивай за Wand{cost_note}: {}", matched_mod.join(", ")),
                3,
            );
        }

        // 1. Evasion / True Strike: Bloodthorn & Monkey King Bar
        let evasion = ["phantom_assassin", "windrunner", "brewmaster", "riki"];
        let matched_evasion: Vec<&str> = enemy_shorts.iter().filter(|s| evasion.contains(&s.as_str())).map(|s| s.as_str()).collect();
        if !matched_evasion.is_empty() {
            add_item(
                "bloodthorn",
                &format!("True Strike (Soul Rend) + Сайленс + криты. Топ замена MKB для иллюзионистов (PL, Naga) и кастеров против: {}", matched_evasion.join(", ")),
                5,
            );
            add_item(
                "monkey_king_bar",
                &format!("80% True Strike против уклонений {} (для обычных керри)", matched_evasion.join(", ")),
                4,
            );
        }

        // 2. Dispels / Saves (Ghost, Aeon Disk, Wind Waker, Force Staff, Buffs)
        let save_buffs = ["necrolyte", "omniknight", "pugna", "dazzle", "windrunner", "tinker"];
        let matched_saves: Vec<&str> = enemy_shorts.iter().filter(|s| save_buffs.contains(&s.as_str())).map(|s| s.as_str()).collect();
        if !matched_saves.is_empty() {
            add_item(
                "nullifier",
                &format!("Непрерывное развеивание (Mute Ghost/Aeon/Force/Buffs): {}", matched_saves.join(", ")),
                5,
            );
        }

        // 3. Break Passives: Silver Edge (Highest priority counter for Bristleback, PA, Spectre)
        let passives = ["bristleback", "phantom_assassin", "spectre", "huskar", "viper", "tidehunter", "dragon_knight", "mars", "timbersaw"];
        let matched_passives: Vec<&str> = enemy_shorts.iter().filter(|s| passives.contains(&s.as_str())).map(|s| s.as_str()).collect();
        if !matched_passives.is_empty() {
            let reason = if matched_passives.contains(&"bristleback") {
                format!("Истощение (Break на 5 сек): выключает спину и Warpath у Bristleback! ({})", matched_passives.join(", "))
            } else {
                format!("Отключить пассивки (Break на 5 сек): {}", matched_passives.join(", "))
            };
            add_item(
                "silver_edge",
                &reason,
                6,
            );
        }

        // 4. Healing / High HP Regen / Spell Lifesteal: Spirit Vessel, Shiva, Skadi
        let high_heal = ["morphling", "alchemist", "necrolyte", "slark", "huskar", "life_stealer", "abaddon", "dazzle", "witch_doctor", "bristleback"];
        let matched_heal: Vec<&str> = enemy_shorts.iter().filter(|s| high_heal.contains(&s.as_str())).map(|s| s.as_str()).collect();
        if !matched_heal.is_empty() {
            let vessel_reason = if matched_heal.contains(&"bristleback") {
                format!("Режет Bloodstone/вампиризм на 45% + процентный урон: {}", matched_heal.join(", "))
            } else {
                format!("Режет лечение на 45% + процентный урон: {}", matched_heal.join(", "))
            };
            add_item(
                "spirit_vessel",
                &vessel_reason,
                5,
            );
            add_item(
                "shivas_guard",
                "Аура снижения исцеления на 25% + броня против физ. урона",
                4,
            );
            add_item(
                "skadi",
                "Атаки режут лечение и вампиризм на 40%",
                4,
            );
        }

        // 5. Illusions: Mjollnir, Radiance, Shiva's Guard
        let illusions = ["phantom_lancer", "naga_siren", "chaos_knight", "broodmother", "meepo", "terrorblade"];
        let matched_illu: Vec<&str> = enemy_shorts.iter().filter(|s| illusions.contains(&s.as_str())).map(|s| s.as_str()).collect();
        if !matched_illu.is_empty() {
            add_item(
                "mjollnir",
                &format!("Сплеш для дальников: {}", matched_illu.join(", ")),
                5,
            );
            add_item("radiance", "Фарм, сжигание иллюзий и 20% промахов", 4);
            add_item("shivas_guard", "AoE урон и раскрытие иллюзий", 4);
        }

        // 6. Mobile / Escape: Bloodthorn, Gleipnir, Rod of Atos, Scythe of Vyse, Orchid
        let mobile = ["storm_spirit", "antimage", "queenofpain", "puck", "weaver", "void_spirit", "ember_spirit", "mirana"];
        let matched_mobile: Vec<&str> = enemy_shorts.iter().filter(|s| mobile.contains(&s.as_str())).map(|s| s.as_str()).collect();
        if !matched_mobile.is_empty() {
            add_item(
                "bloodthorn",
                &format!("Сайленс + криты по мобильным целям: {}", matched_mobile.join(", ")),
                5,
            );
            add_item(
                "gungir",
                &format!("AoE Root (2 сек) + молнии. Ловит мобильных: {}", matched_mobile.join(", ")),
                5,
            );
            add_item(
                "rod_of_atos",
                &format!("Дальний Root (1100 range) против эскейпов: {}", matched_mobile.join(", ")),
                4,
            );
            add_item("sheepstick", "Мгновенный контроль (Хекс 2.8 сек)", 5);
            add_item(
                "orchid",
                &format!("Сайленс против побега: {}", matched_mobile.join(", ")),
                4,
            );
        }

        // 7. Kite / Long Range (Sniper, Drow Ranger, Viper): Harpoon & Disperser
        let kiting = ["sniper", "drow_ranger", "viper", "venomancer"];
        let matched_kiting: Vec<&str> = enemy_shorts.iter().filter(|s| kiting.contains(&s.as_str())).map(|s| s.as_str()).collect();
        if !matched_kiting.is_empty() {
            add_item(
                "harpoon",
                &format!("Гарпун — для сокращения дистанции (хорошо при замедлениях): {}", matched_kiting.join(", ")),
                4,
            );
            add_item(
                "disperser",
                &format!("Снятие замедлений + максимальная скорость против {}", matched_kiting.join(", ")),
                4,
            );
        }

        // 8. Magic Burst: BKB, Mage Slayer, Pipe
        let magic_burst = ["lina", "lion", "leshrac", "zuus", "storm_spirit", "skywrath_mage", "tinker", "queenofpain", "puck"];
        let matched_magic: Vec<&str> = enemy_shorts.iter().filter(|s| magic_burst.contains(&s.as_str())).map(|s| s.as_str()).collect();
        if matched_magic.len() >= 2 {
            add_item("black_king_bar", "Иммунитет к магии против сильного прокаста", 5);
            add_item("mage_slayer", "Удары снижают маг. урон врагов на 35%", 4);
            add_item("pipe", "Магический барьер для команды", 4);
        }

        // 9. Physical Burst: Halberd, Crimson Guard, Ghost
        let phys_burst = ["ursa", "sven", "templar_assassin", "clinkz", "troll_warlord", "phantom_assassin"];
        let matched_phys: Vec<&str> = enemy_shorts.iter().filter(|s| phys_burst.contains(&s.as_str())).map(|s| s.as_str()).collect();
        if !matched_phys.is_empty() {
            add_item(
                "heavens_halberd",
                &format!("Безоружность (Дизарм 3-5 сек): {}", matched_phys.join(", ")),
                4,
            );
            add_item("crimson_guard", "Блок физического урона всей команде", 4);
            add_item("ghost", "Физическая неуязвимость на 4 сек", 3);
        }

        items.sort_by(|a, b| b.priority.cmp(&a.priority));
        items.truncate(10);

        items
    }

    pub fn get_popular_items_stage(
        api: &DotaApiClient,
        items_map: &std::collections::HashMap<String, u32>,
        stage: &str, // "start", "early", "mid", "late"
    ) -> Vec<PopularItemEntry> {
        const RAW_COMPONENTS: &[&str] = &[
            // Basic attributes
            "branches", "gauntlets", "slippers", "mantle", "circlet",
            "belt_of_strength", "boots_of_elves", "robe", "crown", "diadem",
            "ogre_axe", "blade_of_alacrity", "staff_of_wizardry",
            // Secret shop raw
            "point_booster", "vitality_booster", "energy_booster",
            "mystic_staff", "reaver", "eagle", "ultimate_orb",
            "hyperstone", "demon_edge", "relic", "platemail",
            "talisman_of_evasion", "ring_of_tarrasque", "tiara_of_selemene",
            "cornucopia", "void_stone", "ring_of_health",
            // Basic components
            "blades_of_attack", "broadsword", "claymore", "javelin",
            "mithril_hammer", "blitz_knuckles", "quarterstaff", "helm_of_iron_will",
            "ring_of_protection", "chainmail", "fluffy_hat", "wind_lace",
            "cloak", "shadow_amulet", "gloves", "blight_stone",
            "voodoo_mask", "morbid_mask", "sobi_mask",
            // Intermediate swords (prefer completed SnY, KnS, Halberd, Silver Edge)
            "sange", "yasha", "kaya",
            // Consumables / Wards (not in early/mid/late builds)
            "tpscroll", "clarity", "faerie_fire", "tango", "flask",
            "ward_observer", "ward_sentry", "smoke_of_veil", "dust",
        ];

        const VALID_START_ITEMS: &[&str] = &[
            "tango", "clarity", "flask", "faerie_fire", "enchanted_mango",
            "blood_grenade", "quelling_blade", "branches", "circlet",
            "slippers", "gauntlets", "mantle", "magic_stick", "ring_of_protection",
            "ward_observer", "ward_sentry", "wind_lace", "blight_stone", "orb_of_venom",
            "infused_raindrop",
        ];

        let mut list: Vec<(u32, u32)> = items_map
            .iter()
            .filter_map(|(id_str, count)| {
                let id = id_str.parse::<u32>().ok()?;
                Some((id, *count))
            })
            .collect();

        list.sort_by(|a, b| b.1.cmp(&a.1));

        let max_count = list.first().map(|x| x.1).unwrap_or(1).max(1) as f32;

        list.into_iter()
            .filter_map(|(id, count)| {
                let item = api.items_by_id.get(&id)?;
                let clean = item.clean_name();
                let cost = item.cost.unwrap_or(0);

                if clean.starts_with("recipe_") {
                    return None;
                }

                match stage {
                    "start" => {
                        // Starting items (600 starting gold)
                        if !VALID_START_ITEMS.contains(&clean) {
                            return None;
                        }
                    }
                    "early" => {
                        // Early game items: completed early items only (boots, wand, bracer, etc.)
                        if RAW_COMPONENTS.contains(&clean) {
                            return None;
                        }
                        if cost > 3200 {
                            return None;
                        }
                    }
                    "mid" => {
                        // Mid-game items (BKB, Manta, Blink, Orchid, etc.)
                        if RAW_COMPONENTS.contains(&clean) {
                            return None;
                        }
                        if cost < 1500 {
                            return None;
                        }
                    }
                    "late" => {
                        // Late-game luxury items (Butterfly, Hex, Skadi, Satanic, Shiva, etc.)
                        if RAW_COMPONENTS.contains(&clean) {
                            return None;
                        }
                        if cost < 2500 {
                            return None;
                        }
                        if matches!(clean, "boots" | "bottle" | "magic_wand" | "power_treads" | "arcane_boots" | "phase_boots" | "null_talisman" | "wraith_band" | "bracer" | "soul_ring") {
                            return None;
                        }
                    }
                    _ => {}
                }

                let pct = (count as f32 / max_count) * 100.0;
                Some(PopularItemEntry {
                    item_name: clean.to_string(),
                    localized_name: item.localized_name.clone(),
                    image_url: item.image_url(),
                    count,
                    percentage: pct,
                })
            })
            .take(5)
            .collect()
    }

    pub fn get_hero_build(
        api: &DotaApiClient,
        _hero_name: &str,
        pop: &crate::models::ItemPopularity,
    ) -> (
        Vec<PopularItemEntry>,
        Vec<PopularItemEntry>,
        Vec<PopularItemEntry>,
        Vec<PopularItemEntry>,
    ) {
        // A build must come from current population data.  Inventing a static fallback
        // makes the UI look complete, but turns every fallback into a false 100% pick.
        let start = Self::get_popular_items_stage(api, &pop.start_game_items, "start");
        let early = Self::get_popular_items_stage(api, &pop.early_game_items, "early");
        let mid = Self::get_popular_items_stage(api, &pop.mid_game_items, "mid");
        let late = Self::get_popular_items_stage(api, &pop.late_game_items, "late");

        (start, early, mid, late)
    }

    /// Estimate a role from live hero attributes supplied by the API.  This is deliberately
    /// feature based rather than a stale hero-name table: new heroes and role changes are
    /// incorporated as soon as OpenDota updates their role tags.
    pub fn position_score(hero: &HeroData, pos: PlayerPosition) -> f32 {
        let has = |role: &str| hero.roles.iter().any(|r| r.eq_ignore_ascii_case(role));
        let ranged = hero.attack_type.eq_ignore_ascii_case("ranged");
        let attr = hero.primary_attr.as_str();

        match pos {
            PlayerPosition::Pos1Carry => {
                6.0 * has("Carry") as u8 as f32 + 1.5 * has("Escape") as u8 as f32
                    + 1.0 * (attr == "agi") as u8 as f32 - 4.0 * has("Support") as u8 as f32
            }
            PlayerPosition::Pos2Mid => {
                3.5 * has("Nuker") as u8 as f32 + 2.5 * has("Escape") as u8 as f32
                    + 1.5 * has("Carry") as u8 as f32 + 1.0 * ranged as u8 as f32
                    - 3.0 * has("Support") as u8 as f32
            }
            PlayerPosition::Pos3Offlane => {
                5.0 * has("Durable") as u8 as f32 + 4.0 * has("Initiator") as u8 as f32
                    + 1.5 * has("Disabler") as u8 as f32 - 2.0 * has("Support") as u8 as f32
            }
            PlayerPosition::Pos4SoftSupport => {
                4.0 * has("Support") as u8 as f32 + 3.5 * has("Disabler") as u8 as f32
                    + 2.0 * has("Nuker") as u8 as f32 + 1.5 * has("Initiator") as u8 as f32
            }
            PlayerPosition::Pos5HardSupport => {
                5.0 * has("Support") as u8 as f32 + 3.0 * has("Disabler") as u8 as f32
                    + 1.5 * has("Nuker") as u8 as f32 - 2.0 * has("Carry") as u8 as f32
            }
        }
    }

    /// Combines absolute evidence for a role with separation from the hero's
    /// best role. This keeps versatile heroes available while pushing a rare,
    /// weak fit below real lane candidates.
    pub fn position_confidence(hero: &HeroData, pos: PlayerPosition) -> f32 {
        let target = Self::position_score(hero, pos).max(0.0);
        let best = [
            PlayerPosition::Pos1Carry,
            PlayerPosition::Pos2Mid,
            PlayerPosition::Pos3Offlane,
            PlayerPosition::Pos4SoftSupport,
            PlayerPosition::Pos5HardSupport,
        ].into_iter()
            .map(|candidate| Self::position_score(hero, candidate).max(0.0))
            .fold(0.0_f32, f32::max);
        if best <= 0.0 { return 0.0; }
        let absolute = (target / 6.0).clamp(0.0, 1.0);
        absolute * (target / best)
    }

    pub fn infer_primary_position(hero: &HeroData) -> PlayerPosition {
        [
            PlayerPosition::Pos1Carry,
            PlayerPosition::Pos2Mid,
            PlayerPosition::Pos3Offlane,
            PlayerPosition::Pos4SoftSupport,
            PlayerPosition::Pos5HardSupport,
        ]
        .into_iter()
        .max_by(|a, b| Self::position_score(hero, *a).partial_cmp(&Self::position_score(hero, *b)).unwrap_or(std::cmp::Ordering::Equal))
        .unwrap_or(PlayerPosition::Pos1Carry)
    }
}

fn format_adv(adv: f32) -> String {
    format!("{:.1}", adv)
}

pub fn hero_cdn_name(name: &str) -> &str {
    let clean = name.strip_prefix("npc_dota_hero_").unwrap_or(name);
    match clean {
        "zeus" => "zuus",
        "wraith_king" => "skeleton_king",
        "windranger" => "windrunner",
        "necrophos" => "necrolyte",
        "queen_of_pain" => "queenofpain",
        "shadow_fiend" => "nevermore",
        "underlord" => "abyssal_underlord",
        "io" => "wisp",
        "timbersaw" => "shredder",
        "clockwerk" => "rattletrap",
        "nature_s_prophet" | "natures_prophet" => "furion",
        "outworld_destroyer" | "od" => "obsidian_destroyer",
        "treant_protector" => "treant",
        "magnus" => "magnataur",
        "doom" => "doom_bringer",
        "vengeful_spirit" => "vengefulspirit",
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hero(roles: &[&str]) -> HeroData {
        HeroData {
            id: 1,
            name: "npc_dota_hero_test".into(),
            localized_name: "Test".into(),
            primary_attr: "str".into(),
            roles: roles.iter().map(|role| (*role).to_string()).collect(),
            attack_type: "Melee".into(),
        }
    }

    #[test]
    fn role_based_position_inference_is_not_hero_name_based() {
        let initiator = hero(&["Durable", "Initiator", "Disabler"]);
        let support = hero(&["Support", "Disabler", "Nuker"]);
        assert_eq!(Advisor::infer_primary_position(&initiator), PlayerPosition::Pos3Offlane);
        assert_eq!(Advisor::infer_primary_position(&support), PlayerPosition::Pos5HardSupport);
    }

    #[test]
    fn position_confidence_penalizes_rare_role_fit() {
        let support = hero(&["Support", "Disabler", "Nuker"]);
        assert!(
            Advisor::position_confidence(&support, PlayerPosition::Pos5HardSupport)
                > Advisor::position_confidence(&support, PlayerPosition::Pos2Mid)
        );
    }
}


