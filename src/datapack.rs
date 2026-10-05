use crate::api::{AbilityData, HeroBracketWinrates};
use crate::models::{HeroData, ItemData};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

pub const DATA_PACK_FILE: &str = "LaneTheory.ypk";
const DATA_PACK_FORMAT: &str = "lanetheory.ypk";
const DATA_PACK_SCHEMA: u32 = 1;

/// A versioned, local data snapshot kept beside the executable.  `.ypk` is a
/// LaneTheory container, not an executable or an archive supplied by a server.
/// It is deliberately human-inspectable JSON during the first schema version.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataPack {
    format: String,
    schema_version: u32,
    generated_at_unix: u64,
    heroes: Vec<HeroData>,
    items: Vec<ItemData>,
    bracket_winrates: Vec<HeroBracketWinrates>,
    abilities: HashMap<String, AbilityData>,
    hero_ultimate_abilities: HashMap<String, String>,
    #[serde(default)]
    hero_abilities: HashMap<String, Vec<String>>,
}

impl DataPack {
    pub fn exists() -> bool {
        pack_path().is_file()
    }

    pub fn load() -> Result<Self, String> {
        let path = pack_path();
        let raw = std::fs::read_to_string(&path)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        let pack: Self = serde_json::from_str(&raw)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        if pack.format != DATA_PACK_FORMAT || pack.schema_version != DATA_PACK_SCHEMA {
            return Err(format!(
                "{} имеет неподдерживаемую схему {}",
                path.display(),
                pack.schema_version
            ));
        }
        if pack.heroes.len() < 100 || pack.items.len() < 300 {
            return Err(format!("{} неполный", path.display()));
        }
        Ok(pack)
    }

    pub fn write(
        heroes: Vec<HeroData>,
        items: Vec<ItemData>,
        bracket_winrates: Vec<HeroBracketWinrates>,
        abilities: HashMap<String, AbilityData>,
        hero_ultimate_abilities: HashMap<String, String>,
        hero_abilities: HashMap<String, Vec<String>>,
    ) -> Result<PathBuf, String> {
        let generated_at_unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let pack = Self {
            format: DATA_PACK_FORMAT.to_string(),
            schema_version: DATA_PACK_SCHEMA,
            generated_at_unix,
            heroes,
            items,
            bracket_winrates,
            abilities,
            hero_ultimate_abilities,
            hero_abilities,
        };
        let path = pack_path();
        let encoded = serde_json::to_vec_pretty(&pack).map_err(|error| error.to_string())?;
        let temporary = path.with_extension("ypk.tmp");
        std::fs::write(&temporary, encoded).map_err(|error| error.to_string())?;
        // Windows does not replace an existing destination with `rename`.
        // Move the old version aside first and restore it on a failed swap.
        let backup = path.with_extension("ypk.bak");
        if backup.exists() {
            let _ = std::fs::remove_file(&backup);
        }
        if path.exists() {
            std::fs::rename(&path, &backup).map_err(|error| error.to_string())?;
        }
        if let Err(error) = std::fs::rename(&temporary, &path) {
            if backup.exists() {
                let _ = std::fs::rename(&backup, &path);
            }
            return Err(error.to_string());
        }
        if backup.exists() {
            let _ = std::fs::remove_file(&backup);
        }
        Ok(path)
    }

    pub fn heroes(&self) -> &[HeroData] {
        &self.heroes
    }

    pub fn items(&self) -> &[ItemData] {
        &self.items
    }

    pub fn bracket_winrates(&self) -> &[HeroBracketWinrates] {
        &self.bracket_winrates
    }

    pub fn abilities(&self) -> &HashMap<String, AbilityData> {
        &self.abilities
    }

    pub fn hero_ultimate_abilities(&self) -> &HashMap<String, String> {
        &self.hero_ultimate_abilities
    }

    pub fn hero_abilities(&self) -> &HashMap<String, Vec<String>> {
        &self.hero_abilities
    }
}

fn pack_path() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(|parent| parent.join(DATA_PACK_FILE)))
        .unwrap_or_else(|| PathBuf::from(DATA_PACK_FILE))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_a_complete_current_schema() {
        let pack = DataPack {
            format: DATA_PACK_FORMAT.to_string(),
            schema_version: DATA_PACK_SCHEMA,
            generated_at_unix: 0,
            heroes: (0..100).map(|id| HeroData {
                id,
                name: format!("npc_dota_hero_{id}"),
                localized_name: format!("Hero {id}"),
                primary_attr: "int".to_string(),
                roles: Vec::new(),
                attack_type: String::new(),
            }).collect(),
            items: (0..300).map(|id| ItemData {
                id,
                name: format!("item_{id}"),
                localized_name: format!("Item {id}"),
                cost: Some(0),
            }).collect(),
            bracket_winrates: Vec::new(),
            abilities: HashMap::new(),
            hero_ultimate_abilities: HashMap::new(),
            hero_abilities: HashMap::new(),
        };
        let json = serde_json::to_string(&pack).unwrap();
        let decoded: DataPack = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.schema_version, DATA_PACK_SCHEMA);
        assert_eq!(decoded.heroes().len(), 100);
        assert_eq!(decoded.items().len(), 300);
    }
}
