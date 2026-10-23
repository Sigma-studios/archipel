//! The games archipel knows: the definitions built in (`games/` in the repository)
//! and those dropped into `<data>/games/`, which replace built-in ones of the same id.

use std::collections::BTreeMap;
use std::path::Path;

use archipel_core::game::GameDef;
use tracing::{info, warn};

const BUILT_IN: &[(&str, &str)] = &[
    (
        "minecraft.toml",
        include_str!("../../../games/minecraft.toml"),
    ),
    (
        "urbanterror.toml",
        include_str!("../../../games/urbanterror.toml"),
    ),
    ("puck.toml", include_str!("../../../games/puck.toml")),
];

#[derive(Default)]
pub struct Games(BTreeMap<String, GameDef>);

impl Games {
    pub fn load(extra: &Path) -> anyhow::Result<Self> {
        let mut games = BTreeMap::new();
        for (file, text) in BUILT_IN {
            let game = parse(file, text)?;
            games.insert(game.id.clone(), game);
        }
        if let Ok(entries) = std::fs::read_dir(extra) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_none_or(|e| e != "toml") {
                    continue;
                }
                let name = path.display().to_string();
                match std::fs::read_to_string(&path)
                    .map_err(anyhow::Error::from)
                    .and_then(|text| parse(&name, &text))
                {
                    Ok(game) => {
                        info!("game {} from {name}", game.id);
                        games.insert(game.id.clone(), game);
                    }
                    Err(e) => warn!("skipping {name}: {e:#}"),
                }
            }
        }
        Ok(Games(games))
    }

    pub fn get(&self, id: &str) -> Option<&GameDef> {
        self.0.get(id)
    }

    /// By name.
    pub fn list(&self) -> Vec<GameDef> {
        let mut all: Vec<GameDef> = self.0.values().cloned().collect();
        all.sort_by(|a, b| a.name.cmp(&b.name));
        all
    }
}

fn parse(file: &str, text: &str) -> anyhow::Result<GameDef> {
    let game: GameDef = toml::from_str(text)
        .map_err(|e| anyhow::anyhow!("{file} is not a game definition: {e}"))?;
    game.validate()
        .map_err(|e| anyhow::anyhow!("{file}: {e}"))?;
    Ok(game)
}
