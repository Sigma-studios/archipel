//! A server's Steam Workshop items (Garry's Mod): its gamemode's preset and the
//! ones its owner adds. archipel writes the list the server downloads at start
//! (`srcds_workshop_ids.txt`), has players download the items with content, and
//! reads the maps and gamemodes in what the server downloaded.
//!
//! What the owner added is kept in the server's directory, read and written as
//! its user (see [`fsops`]).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use anyhow::Context;
use archipel_core::api::{WorkshopItem, WorkshopKind};
use archipel_core::game::{WorkshopDef, is_workshop_id};
use archipel_core::template::Value;
use serde::{Deserialize, Serialize};

use crate::console::Console;
use crate::fsops;

const STEAM_API: &str = "https://api.steampowered.com/ISteamRemoteStorage";
/// What the owner added, in the server's directory.
const STATE: &str = ".archipel-workshop.json";
/// Items a server may take, collections expanded.
const MAX_ITEMS: usize = 500;

/// Has players download the mounted Workshop items with content of their own:
/// maps they get when the server plays them, gamemodes as the server runs them,
/// and Lua through the game itself.
const LUA: &str = r#"-- Written by archipel at each start: players download the Workshop items
-- with models, materials or sounds. Maps they get when the server plays them.
local dirs = { "models", "materials", "sound", "particles", "resource" }
local count = 0
for _, addon in ipairs(engine.GetAddons()) do
	if addon.mounted and addon.wsid and addon.wsid ~= "" and addon.wsid ~= "0" then
		local maps = file.Find("maps/*.bsp", addon.title)
		if #maps == 0 then
			for _, dir in ipairs(dirs) do
				local files, folders = file.Find(dir .. "/*", addon.title)
				if #files > 0 or #folders > 0 then
					resource.AddWorkshop(addon.wsid)
					count = count + 1
					break
				end
			end
		end
	end
end
print("archipel: players download " .. count .. " Workshop items with content")
"#;

/// What the owner added: items and collections, and what the collections held
/// when last asked (used when Steam does not answer).
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct State {
    #[serde(default)]
    pub items: Vec<String>,
    #[serde(default)]
    pub expanded: BTreeMap<String, Vec<String>>,
}

pub async fn read_state(dir: &Path, uid: u32) -> State {
    let path = dir.join(STATE);
    fsops::as_user(uid, move || {
        Ok(std::fs::read_to_string(path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default())
    })
    .await
    .unwrap_or_default()
}

pub async fn write_state(dir: &Path, uid: u32, state: &State) -> anyhow::Result<()> {
    let path = dir.join(STATE);
    let text = serde_json::to_string_pretty(state)?;
    fsops::as_user(uid, move || std::fs::write(path, text))
        .await
        .context("saving the Workshop items")
}

/// A Workshop item's id, from its id or a link to it (`…?id=123`).
pub fn parse_link(link: &str) -> Option<String> {
    let link = link.trim();
    if is_workshop_id(link) {
        return Some(link.to_string());
    }
    let (_, query) = link.split_once('?')?;
    query
        .split('&')
        .find_map(|kv| kv.strip_prefix("id="))
        .map(|id| id.split(['#', '/']).next().unwrap_or(id).to_string())
        .filter(|id| is_workshop_id(id))
}

// ---------------------------------------------------------------- Steam

/// What Steam says about Workshop items, kept a while.
#[derive(Default)]
pub struct Steam {
    known: parking_lot::Mutex<HashMap<String, (std::time::Instant, WorkshopItem)>>,
}

/// How long what Steam said is reused.
const KNOWN_FOR: std::time::Duration = std::time::Duration::from_secs(600);

impl Steam {
    /// Each item, as Steam describes it: collections with their items, items of
    /// another game or gone with why.
    pub async fn items(
        &self,
        http: &reqwest::Client,
        app_id: u32,
        ids: &[String],
    ) -> anyhow::Result<Vec<WorkshopItem>> {
        let missing: Vec<String> = {
            let known = self.known.lock();
            ids.iter()
                .filter(|id| {
                    known
                        .get(*id)
                        .is_none_or(|(at, _)| at.elapsed() > KNOWN_FOR)
                })
                .cloned()
                .collect()
        };
        for chunk in missing.chunks(100) {
            let found = ask(http, app_id, chunk).await?;
            let mut known = self.known.lock();
            for item in found {
                known.insert(item.id.clone(), (std::time::Instant::now(), item));
            }
        }
        let known = self.known.lock();
        Ok(ids
            .iter()
            .filter_map(|id| known.get(id).map(|(_, item)| item.clone()))
            .collect())
    }

    /// The items of the owner's list, collections replaced by what they hold
    /// (nested ones too). Collections Steam does not answer about keep what they
    /// held last time; `state.expanded` is updated.
    pub async fn expand(
        &self,
        http: &reqwest::Client,
        app_id: u32,
        state: &mut State,
    ) -> Vec<String> {
        let mut out = Vec::new();
        let mut seen = BTreeSet::new();
        let mut queue: Vec<(String, u8)> =
            state.items.iter().rev().map(|i| (i.clone(), 0)).collect();
        while let Some((id, depth)) = queue.pop() {
            if !seen.insert(id.clone()) || out.len() >= MAX_ITEMS {
                continue;
            }
            let item = self.items(http, app_id, std::slice::from_ref(&id)).await;
            let children = match item.as_deref() {
                Ok([item]) if item.kind == WorkshopKind::Collection => {
                    state.expanded.insert(id.clone(), item.children.clone());
                    Some(item.children.clone())
                }
                Ok(_) => None,
                Err(_) => state.expanded.get(&id).cloned(),
            };
            match children {
                Some(children) if depth < 3 => {
                    queue.extend(children.into_iter().rev().map(|c| (c, depth + 1)));
                }
                Some(_) => {}
                None => out.push(id),
            }
        }
        state.expanded.retain(|k, _| state.items.contains(k));
        out
    }
}

#[derive(Deserialize)]
struct Answer<T> {
    response: T,
}

#[derive(Deserialize)]
struct Details {
    #[serde(default)]
    publishedfiledetails: Vec<Detail>,
}

#[derive(Deserialize)]
struct Detail {
    publishedfileid: String,
    result: i32,
    #[serde(default)]
    consumer_app_id: u32,
    #[serde(default)]
    title: String,
    #[serde(default)]
    file_size: serde_json::Value,
    #[serde(default)]
    preview_url: String,
    #[serde(default)]
    banned: serde_json::Value,
    #[serde(default)]
    tags: Vec<Tag>,
}

#[derive(Deserialize)]
struct Tag {
    tag: String,
}

#[derive(Deserialize)]
struct Collections {
    #[serde(default)]
    collectiondetails: Vec<Collection>,
}

#[derive(Deserialize)]
struct Collection {
    publishedfileid: String,
    #[serde(default)]
    children: Vec<Child>,
}

#[derive(Deserialize)]
struct Child {
    publishedfileid: String,
}

/// Steam's public Web API, which answers these without a key.
async fn ask(
    http: &reqwest::Client,
    app_id: u32,
    ids: &[String],
) -> anyhow::Result<Vec<WorkshopItem>> {
    let form = |count: &str| {
        let mut f = vec![(count.to_string(), ids.len().to_string())];
        f.extend(
            ids.iter()
                .enumerate()
                .map(|(i, id)| (format!("publishedfileids[{i}]"), id.clone())),
        );
        f
    };
    let post = |url: String, form: Vec<(String, String)>| async move {
        http.post(url)
            .form(&form)
            .send()
            .await
            .context("reaching Steam")?
            .error_for_status()
            .context("asking Steam")
    };
    let details: Answer<Details> = post(
        format!("{STEAM_API}/GetPublishedFileDetails/v1/"),
        form("itemcount"),
    )
    .await?
    .json()
    .await
    .context("reading Steam's answer")?;
    let collections: Answer<Collections> = post(
        format!("{STEAM_API}/GetCollectionDetails/v1/"),
        form("collectioncount"),
    )
    .await?
    .json()
    .await
    .context("reading Steam's answer")?;
    let children: HashMap<String, Vec<String>> = collections
        .response
        .collectiondetails
        .into_iter()
        .filter(|c| !c.children.is_empty())
        .map(|c| {
            let ids = c
                .children
                .into_iter()
                .map(|ch| ch.publishedfileid)
                .collect();
            (c.publishedfileid, ids)
        })
        .collect();
    Ok(details
        .response
        .publishedfiledetails
        .into_iter()
        .map(|d| {
            let tags: Vec<String> = d.tags.into_iter().map(|t| t.tag).collect();
            let has = |t: &str| tags.iter().any(|x| x.eq_ignore_ascii_case(t));
            let kids = children
                .get(&d.publishedfileid)
                .cloned()
                .unwrap_or_default();
            let kind = if !kids.is_empty() {
                WorkshopKind::Collection
            } else if has("map") {
                WorkshopKind::Map
            } else if has("gamemode") {
                WorkshopKind::Gamemode
            } else {
                WorkshopKind::Addon
            };
            let banned = d.banned.as_bool().unwrap_or(false) || d.banned.as_i64().unwrap_or(0) != 0;
            let problem = if d.result != 1 {
                Some("Steam does not know it, or it is private: make it public or unlisted".into())
            } else if banned {
                Some("Steam has taken it down".into())
            } else if d.consumer_app_id != app_id {
                Some("it is for another game".into())
            } else {
                None
            };
            let bytes = match &d.file_size {
                serde_json::Value::String(s) => s.parse().unwrap_or(0),
                v => v.as_u64().unwrap_or(0),
            };
            WorkshopItem {
                url: format!(
                    "https://steamcommunity.com/sharedfiles/filedetails/?id={}",
                    d.publishedfileid
                ),
                title: if d.title.is_empty() {
                    d.publishedfileid.clone()
                } else {
                    d.title
                },
                id: d.publishedfileid,
                kind,
                bytes,
                children: kids,
                preview_url: Some(d.preview_url).filter(|u| u.starts_with("https://")),
                problem,
                maps: vec![],
                gamemodes: vec![],
                tags,
            }
        })
        .collect())
}

// ---------------------------------------------------------------- at start

/// Gets the Workshop ready for a start: the list of items the server downloads
/// (the gamemode's, then the owner's), the Lua file for players, and the first
/// map, which becomes the gamemode's own when it cannot play the one chosen.
/// Says the console settings of the gamemode (`{workshop.config}`).
pub async fn prepare(
    def: &WorkshopDef,
    steam: &Steam,
    http: &reqwest::Client,
    settings: &mut BTreeMap<String, Value>,
    dir: &Path,
    uid: u32,
    console: &Console,
) -> anyhow::Result<String> {
    let gamemode = settings
        .get(&def.gamemode_setting)
        .map(Value::as_text)
        .unwrap_or_default();
    let preset = def.preset(&gamemode);
    let mut ids: Vec<String> = Vec::new();
    if let Some(p) = preset {
        ids.extend(p.items.iter().cloned());
        ids.extend(p.maps.iter().map(|m| m.id.clone()));
        let map = settings
            .get(&def.map_setting)
            .map(Value::as_text)
            .unwrap_or_default();
        if !p.plays(&map)
            && let Some(first) = p.maps.first()
        {
            console.system(format!(
                "{gamemode} does not play {map}: starting on {} instead (Settings, First map)",
                first.map
            ));
            settings.insert(def.map_setting.clone(), Value::Text(first.map.clone()));
        }
    }
    let mut state = read_state(dir, uid).await;
    if !state.items.is_empty() {
        let before = state.expanded.clone();
        ids.extend(steam.expand(http, def.app_id, &mut state).await);
        if state.expanded != before {
            write_state(dir, uid, &state).await?;
        }
    }
    let mut seen = BTreeSet::new();
    ids.retain(|id| seen.insert(id.clone()));
    ids.truncate(MAX_ITEMS);
    if !ids.is_empty() {
        console.system(format!(
            "the server downloads {} Workshop items as it starts, or updates them",
            ids.len()
        ));
    }
    let mut list = String::from(
        "// Written by archipel at each start: the gamemode's Workshop items, then\n// the ones added in the Workshop tab.\n\"WorkshopItems\"\n{\n",
    );
    for (i, id) in ids.iter().enumerate() {
        list.push_str(&format!("\t\"{}\"\t{{ \"wsid\"\t\"{id}\" }}\n", i + 1));
    }
    list.push_str("}\n");
    let mut files = vec![
        (fsops::inside(dir, &def.ids_file)?, list),
        (fsops::inside(dir, &def.lua_file)?, LUA.to_string()),
    ];
    // The maps it plays in turn: its own, then those downloaded before.
    if let Some(p) = preset
        && let Some(path) = &p.map_list_file
    {
        let mut maps: Vec<String> = p.maps.iter().map(|m| m.map.clone()).collect();
        let among = ids.iter().cloned().collect();
        let mut found: Vec<String> = downloaded_among(def, dir, uid, Some(among))
            .await
            .into_values()
            .flat_map(|c| c.maps)
            .filter(|m| p.plays(m) && !maps.contains(m))
            .collect();
        found.sort();
        found.dedup();
        maps.extend(found);
        files.push((fsops::inside(dir, path)?, maps.join("\n") + "\n"));
    }
    fsops::as_user(uid, move || {
        for (path, text) in files {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(path, text)?;
        }
        Ok(())
    })
    .await
    .context("writing the Workshop items")?;
    Ok(preset.map(|p| p.config.clone()).unwrap_or_default())
}

// ---------------------------------------------------------------- what came

/// The maps and gamemodes in an item the server downloaded.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Contents {
    pub maps: Vec<String>,
    pub gamemodes: Vec<String>,
}

/// Gamemodes others build on, which cannot be played themselves.
const BASE_GAMEMODES: &[&str] = &["base", "fretta", "fretta13"];

/// What the server downloaded, by item, among those of its list (what it keeps
/// of items no longer there is not mounted): read from the `.gma` archives'
/// headers.
pub async fn downloaded(def: &WorkshopDef, dir: &Path, uid: u32) -> HashMap<String, Contents> {
    downloaded_among(def, dir, uid, None).await
}

/// [`downloaded`], among the items `among`, or those of the list the server
/// last started with.
async fn downloaded_among(
    def: &WorkshopDef,
    dir: &Path,
    uid: u32,
    among: Option<BTreeSet<String>>,
) -> HashMap<String, Contents> {
    let roots: Vec<PathBuf> = def
        .cache_dirs
        .iter()
        .filter_map(|d| fsops::inside(dir, d).ok())
        .collect();
    let Ok(ids_file) = fsops::inside(dir, &def.ids_file) else {
        return HashMap::new();
    };
    fsops::as_user(uid, move || {
        let listed = among
            .unwrap_or_else(|| listed_ids(&std::fs::read_to_string(ids_file).unwrap_or_default()));
        let mut out = HashMap::new();
        for root in roots {
            let mut stack = vec![(root, 0)];
            while let Some((d, depth)) = stack.pop() {
                let Ok(entries) = std::fs::read_dir(&d) else {
                    continue;
                };
                for entry in entries.flatten() {
                    let path = entry.path();
                    let Ok(kind) = entry.file_type() else {
                        continue;
                    };
                    if kind.is_dir() && depth < 4 {
                        stack.push((path, depth + 1));
                    } else if kind.is_file()
                        && path.extension().is_some_and(|e| e == "gma")
                        && let Some(id) = item_of(&path)
                        && listed.contains(&id)
                        && let Ok(contents) = read_gma(&path)
                    {
                        out.insert(id, contents);
                    }
                }
            }
        }
        Ok(out)
    })
    .await
    .unwrap_or_default()
}

/// The items a list written by [`prepare`] names (`"wsid" "123"`).
fn listed_ids(list: &str) -> BTreeSet<String> {
    list.lines()
        .filter_map(|l| l.split_once("\"wsid\""))
        .filter_map(|(_, rest)| {
            let id = rest.trim().trim_start_matches('"');
            let id = &id[..id.find('"')?];
            is_workshop_id(id).then(|| id.to_string())
        })
        .collect()
}

/// The item a downloaded archive is: its name (`123.gma`), or the folder it is
/// in (`…/123/x.gma`).
fn item_of(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_str()?;
    if is_workshop_id(stem) {
        return Some(stem.to_string());
    }
    let parent = path.parent()?.file_name()?.to_str()?;
    is_workshop_id(parent).then(|| parent.to_string())
}

/// The maps (`maps/x.bsp`) and gamemodes (`gamemodes/x/…`) a `.gma` holds, from
/// its file list.
fn read_gma(path: &Path) -> anyhow::Result<Contents> {
    use std::io::Read;
    // The file list comes first: its start is enough.
    let mut head = Vec::new();
    std::fs::File::open(path)?
        .take(4 * 1024 * 1024)
        .read_to_end(&mut head)?;
    parse_gma(&head)
}

fn parse_gma(bytes: &[u8]) -> anyhow::Result<Contents> {
    let bad = || anyhow::anyhow!("not a .gma archive");
    if bytes.get(..4) != Some(b"GMAD") {
        return Err(bad());
    }
    let version = *bytes.get(4).ok_or_else(bad)?;
    // Version, SteamID, timestamp.
    let mut at = 5 + 8 + 8;
    let cstr = |at: &mut usize| -> anyhow::Result<String> {
        let rest = bytes.get(*at..).ok_or_else(bad)?;
        let end = rest.iter().position(|b| *b == 0).ok_or_else(bad)?;
        let s = String::from_utf8_lossy(&rest[..end]).into_owned();
        *at += end + 1;
        Ok(s)
    };
    if version > 1 {
        while !cstr(&mut at)?.is_empty() {}
    }
    // Name, description, author, then its version.
    for _ in 0..3 {
        cstr(&mut at)?;
    }
    at += 4;
    let mut maps = BTreeSet::new();
    let mut gamemodes = BTreeSet::new();
    loop {
        let number = u32::from_le_bytes(bytes.get(at..at + 4).ok_or_else(bad)?.try_into()?);
        at += 4;
        if number == 0 {
            break;
        }
        let name = cstr(&mut at)?.to_lowercase();
        // Its size and CRC.
        at += 8 + 4;
        if let Some(map) = name
            .strip_prefix("maps/")
            .and_then(|n| n.strip_suffix(".bsp"))
            && !map.contains('/')
        {
            maps.insert(map.to_string());
        }
        if let Some(rest) = name.strip_prefix("gamemodes/")
            && let Some((gm, file)) = rest.split_once('/')
            && file == format!("{gm}.txt")
            && !BASE_GAMEMODES.contains(&gm)
        {
            gamemodes.insert(gm.to_string());
        }
    }
    Ok(Contents {
        maps: maps.into_iter().collect(),
        gamemodes: gamemodes.into_iter().collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_links() {
        assert_eq!(parse_link("159321088").as_deref(), Some("159321088"));
        assert_eq!(
            parse_link(
                "https://steamcommunity.com/sharedfiles/filedetails/?id=203105131&searchtext="
            )
            .as_deref(),
            Some("203105131")
        );
        assert_eq!(
            parse_link("https://steamcommunity.com/workshop/filedetails/?l=french&id=42")
                .as_deref(),
            Some("42")
        );
        assert_eq!(parse_link("ttt_minecraft_b5"), None);
        assert_eq!(parse_link("https://example.com/?id=abc"), None);
    }

    #[test]
    fn reads_gma_file_lists() {
        let mut gma = b"GMAD\x03".to_vec();
        gma.extend([0u8; 16]);
        gma.extend(b"\0"); // no required content
        gma.extend(b"My addon\0{}\0Author\0");
        gma.extend(1i32.to_le_bytes());
        for (i, name) in [
            "maps/ttt_Minecraft_b5.bsp",
            "maps/graphs/ttt_minecraft_b5.ain",
            "gamemodes/murder/murder.txt",
            "gamemodes/murder/gamemode/init.lua",
            "materials/x.vmt",
        ]
        .iter()
        .enumerate()
        {
            gma.extend((i as u32 + 1).to_le_bytes());
            gma.extend(name.as_bytes());
            gma.push(0);
            gma.extend(10i64.to_le_bytes());
            gma.extend(0u32.to_le_bytes());
        }
        gma.extend(0u32.to_le_bytes());
        let c = parse_gma(&gma).unwrap();
        assert_eq!(c.maps, ["ttt_minecraft_b5"]);
        assert_eq!(c.gamemodes, ["murder"]);
        assert!(parse_gma(b"nope").is_err());
    }

    #[test]
    fn reads_its_own_lists() {
        let list = "// Written by archipel\n\"WorkshopItems\"\n{\n\t\"1\"\t{ \"wsid\"\t\"151583504\" }\n\t\"2\"\t{ \"wsid\"\t\"159321088\" }\n}\n";
        assert_eq!(
            listed_ids(list).into_iter().collect::<Vec<_>>(),
            ["151583504", "159321088"]
        );
    }

    #[test]
    fn names_items_from_their_path() {
        assert_eq!(
            item_of(Path::new("/s/garrysmod/cache/srcds/159321088.gma")).as_deref(),
            Some("159321088")
        );
        assert_eq!(
            item_of(Path::new("/s/steam_cache/content/4000/42/x.gma")).as_deref(),
            Some("42")
        );
        assert_eq!(item_of(Path::new("/s/addons/thing.gma")), None);
    }
}
