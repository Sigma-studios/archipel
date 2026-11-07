//! The games archipel knows: the definitions built in (`games/` in the repository)
//! and those dropped into `<data>/games/`, which replace built-in ones of the same id.
//! Likewise games hosted from the game itself, whose ports the desktop opens
//! (`games/hosted/`, and `<data>/games/hosted/`).

use std::collections::BTreeMap;
use std::path::Path;

use archipel_core::game::{GameDef, HostedGame};
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
    ("tf2.toml", include_str!("../../../games/tf2.toml")),
    ("gmod.toml", include_str!("../../../games/gmod.toml")),
    ("fof.toml", include_str!("../../../games/fof.toml")),
];

const HOSTED_BUILT_IN: &[(&str, &str)] = &[(
    "dsp-nebula.toml",
    include_str!("../../../games/hosted/dsp-nebula.toml"),
)];

#[derive(Default)]
pub struct Games(BTreeMap<String, GameDef>, BTreeMap<String, HostedGame>);

impl Games {
    pub fn load(extra: &Path) -> anyhow::Result<Self> {
        let mut games = BTreeMap::new();
        for (file, text) in BUILT_IN {
            let game = parse(file, text)?;
            games.insert(game.id.clone(), game);
        }
        for (name, game) in dropped(extra, parse) {
            info!("game {} from {name}", game.id);
            games.insert(game.id.clone(), game);
        }
        let mut hosted = BTreeMap::new();
        for (file, text) in HOSTED_BUILT_IN {
            let game = parse_hosted(file, text)?;
            hosted.insert(game.id.clone(), game);
        }
        for (name, game) in dropped(&extra.join("hosted"), parse_hosted) {
            info!("hosted game {} from {name}", game.id);
            hosted.insert(game.id.clone(), game);
        }
        Ok(Games(games, hosted))
    }

    /// A game hosted from the game itself, by id.
    pub fn hosted(&self, id: &str) -> Option<&HostedGame> {
        self.1.get(id)
    }

    /// Games hosted from the game itself, by name.
    pub fn list_hosted(&self) -> Vec<HostedGame> {
        let mut all: Vec<HostedGame> = self.1.values().cloned().collect();
        all.sort_by(|a, b| a.name.cmp(&b.name));
        all
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

/// The definitions dropped into `dir`, by file name; those that do not parse are
/// skipped, with a warning.
fn dropped<T>(dir: &Path, parse: fn(&str, &str) -> anyhow::Result<T>) -> Vec<(String, T)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "toml"))
        .filter_map(|path| {
            let name = path.display().to_string();
            std::fs::read_to_string(&path)
                .map_err(anyhow::Error::from)
                .and_then(|text| parse(&name, &text))
                .map_err(|e| warn!("skipping {name}: {e:#}"))
                .ok()
                .map(|game| (name, game))
        })
        .collect()
}

fn parse_hosted(file: &str, text: &str) -> anyhow::Result<HostedGame> {
    let game: HostedGame =
        toml::from_str(text).map_err(|e| anyhow::anyhow!("{file} is not a hosted game: {e}"))?;
    game.validate()
        .map_err(|e| anyhow::anyhow!("{file}: {e}"))?;
    Ok(game)
}

fn parse(file: &str, text: &str) -> anyhow::Result<GameDef> {
    let game: GameDef = toml::from_str(text)
        .map_err(|e| anyhow::anyhow!("{file} is not a game definition: {e}"))?;
    game.validate()
        .map_err(|e| anyhow::anyhow!("{file}: {e}"))?;
    Ok(game)
}
