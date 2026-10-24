//! A server's addons: files its owner adds to one directory of it (Paper's
//! plugins, Urban Terror's or a Source game's maps), uploaded or picked from a
//! catalogue (Modrinth, for Paper plugins).
//!
//! Like every file in a server's directory, they are read and written as the
//! server's user (see [`fsops`]).

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, bail};
use archipel_core::api::{AddonFile, AddonResult};
use archipel_core::game::AddonsDef;
use serde::{Deserialize, Serialize};

use crate::console::Console;
use crate::fsops;
use crate::install::{Check, Installer, copy_as_user};

const MODRINTH: &str = "https://api.modrinth.com/v2";
/// Loaders a Paper server runs plugins of.
const PAPER_LOADERS: &[&str] = &["paper", "spigot", "bukkit"];
/// Which catalogue project each file came from, so installing it again replaces
/// it rather than adding a second copy. Kept in the addons' directory.
const RECORD: &str = ".archipel-addons.json";

/// Catalogue projects installed, by id: their file's name.
type Record = BTreeMap<String, String>;

/// The addons there, sorted by name.
pub async fn list(def: &AddonsDef, dir: &Path, uid: u32) -> anyhow::Result<Vec<AddonFile>> {
    let addons = fsops::inside(dir, &def.dir)?;
    let def = def.clone();
    let files = fsops::as_user(uid, move || {
        let entries = match std::fs::read_dir(&addons) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
            Err(e) => return Err(e),
        };
        let mut out = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            // Not followed: a link is not an addon.
            let Ok(meta) = entry.metadata() else { continue };
            if !meta.is_file() || !def.accepts(&name) {
                continue;
            }
            let modified = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_secs());
            let maps = if name.to_ascii_lowercase().ends_with(".pk3") {
                maps_in_pk3(&entry.path()).unwrap_or_default()
            } else {
                vec![]
            };
            out.push(AddonFile {
                name,
                bytes: meta.len(),
                modified,
                maps,
            });
        }
        out.sort_by_key(|f| f.name.to_lowercase());
        Ok(out)
    })
    .await
    .context("listing the addons")?;
    Ok(files)
}

/// The maps among the addons: those in `.pk3` archives, and `.bsp` files.
pub async fn maps(def: &AddonsDef, dir: &Path, uid: u32) -> anyhow::Result<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for file in list(def, dir, uid).await? {
        match file.name.rsplit_once('.') {
            Some((stem, ext)) if ext.eq_ignore_ascii_case("bsp") => out.push(stem.to_string()),
            _ => out.extend(file.maps),
        }
    }
    out.sort();
    out.dedup();
    Ok(out)
}

/// The maps in a Quake 3 archive: its `maps/<name>.bsp`.
fn maps_in_pk3(path: &Path) -> anyhow::Result<Vec<String>> {
    let zip = zip::ZipArchive::new(std::fs::File::open(path)?)?;
    let mut maps: Vec<String> = zip
        .file_names()
        .filter_map(|n| {
            let lower = n.to_ascii_lowercase();
            let name = lower.strip_prefix("maps/")?.strip_suffix(".bsp")?;
            (!name.is_empty() && !name.contains('/')).then(|| name.to_string())
        })
        .take(200)
        .collect();
    maps.sort();
    Ok(maps)
}

/// Adds (or replaces) an addon with `bytes`.
pub async fn save(
    def: &AddonsDef,
    dir: &Path,
    uid: u32,
    name: &str,
    bytes: Vec<u8>,
) -> anyhow::Result<()> {
    if !def.accepts(name) {
        bail!(
            "{} takes .{} files with a plain name",
            def.label,
            def.extensions.join(", .")
        );
    }
    let addons = fsops::inside(dir, &def.dir)?;
    let target = addons.join(name);
    let target_name = name.to_string();
    fsops::as_user(uid, move || {
        std::fs::create_dir_all(&addons)?;
        // Named after it: two uploads at once do not mix.
        let partial = addons.join(format!(".{}.partial", target_name));
        std::fs::write(&partial, bytes)?;
        std::fs::rename(&partial, &target)
    })
    .await
    .context("saving it")
}

/// Removes an addon.
pub async fn remove(def: &AddonsDef, dir: &Path, uid: u32, name: &str) -> anyhow::Result<()> {
    if !def.accepts(name) {
        bail!("no addon named {name}");
    }
    let addons = fsops::inside(dir, &def.dir)?;
    let name = name.to_string();
    fsops::as_user(uid, move || {
        std::fs::remove_file(addons.join(&name))?;
        let mut record = read_record(&addons);
        record.retain(|_, file| *file != name);
        write_record(&addons, &record)
    })
    .await
    .context("removing it")
}

fn read_record(addons: &Path) -> Record {
    std::fs::read_to_string(addons.join(RECORD))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn write_record(addons: &Path, record: &Record) -> std::io::Result<()> {
    std::fs::create_dir_all(addons)?;
    let text = serde_json::to_string_pretty(record).map_err(std::io::Error::other)?;
    std::fs::write(addons.join(RECORD), text)
}

// ---------------------------------------------------------------- Modrinth

/// Paper plugins on Modrinth matching `query`, for Minecraft `version` when known.
pub async fn search(
    installer: &Installer,
    query: &str,
    version: Option<&str>,
) -> anyhow::Result<Vec<AddonResult>> {
    let loaders: Vec<String> = PAPER_LOADERS
        .iter()
        .map(|l| format!("categories:{l}"))
        .collect();
    let mut facets = vec![vec!["project_type:plugin".to_string()], loaders];
    if let Some(v) = version {
        facets.push(vec![format!("versions:{v}")]);
    }
    let facets = serde_json::to_string(&facets)?;
    let url = reqwest::Url::parse_with_params(
        &format!("{MODRINTH}/search"),
        [
            ("query", query),
            ("facets", facets.as_str()),
            ("limit", "20"),
            // Without a query, the most used first.
            (
                "index",
                if query.trim().is_empty() {
                    "downloads"
                } else {
                    "relevance"
                },
            ),
        ],
    )?;
    let found: ModrinthSearch = installer.json(url.as_str()).await?;
    Ok(found
        .hits
        .into_iter()
        .map(|h| AddonResult {
            url: format!("https://modrinth.com/plugin/{}", h.slug),
            id: h.project_id,
            title: h.title,
            description: h.description,
            author: h.author,
            downloads: h.downloads,
            icon_url: h.icon_url.filter(|u| u.starts_with("https://")),
        })
        .collect())
}

/// Installs Modrinth project `id` for Minecraft `version`, and the projects it
/// requires that are not there yet. Says the files it added, in that order.
pub async fn install(
    installer: &Installer,
    def: &AddonsDef,
    dir: &Path,
    uid: u32,
    id: &str,
    version: Option<&str>,
    console: &Console,
) -> anyhow::Result<Vec<String>> {
    let addons = fsops::inside(dir, &def.dir)?;
    let mut record = {
        let addons = addons.clone();
        fsops::as_user(uid, move || Ok(read_record(&addons))).await?
    };
    let mut added = Vec::new();
    let mut queue = vec![(id.to_string(), true)];
    while let Some((project, asked)) = queue.pop() {
        // What it requires is installed once, and kept as it is after.
        if !asked && record.contains_key(&project) {
            continue;
        }
        if added.len() >= 10 {
            bail!("it requires too many other plugins to install them all");
        }
        let chosen = pick_version(installer, &project, version).await?;
        let Some(file) = chosen
            .files
            .iter()
            .find(|f| f.primary)
            .or(chosen.files.first())
        else {
            bail!("{project} has no file to download");
        };
        let name = file.filename.clone();
        if !def.accepts(&name) || !file.url.starts_with("https://") {
            bail!("{project}'s file {name} is not a plugin archipel can install");
        }
        console.system(format!("downloading the plugin {name}"));
        let tmp = installer.tmp_file(&format!("addon-{}", std::process::id()))?;
        let check = Check::Sha1(file.hashes.sha1.clone());
        installer
            .download(&file.url, &tmp, Some(check), console)
            .await?;
        {
            let addons = addons.clone();
            fsops::as_user(uid, move || std::fs::create_dir_all(addons)).await?;
        }
        let copied = copy_as_user(&tmp, addons.join(&name), uid).await;
        let _ = std::fs::remove_file(&tmp);
        copied?;
        // The copy it replaces, under another name (another version).
        if let Some(old) = record.insert(project.clone(), name.clone())
            && old != name
            && def.accepts(&old)
        {
            let old = addons.join(old);
            fsops::as_user(uid, move || match std::fs::remove_file(old) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
                _ => Ok(()),
            })
            .await?;
        }
        added.push(name);
        for dep in &chosen.dependencies {
            if dep.dependency_type != "required" {
                continue;
            }
            let project = match (&dep.project_id, &dep.version_id) {
                (Some(p), _) => p.clone(),
                (None, Some(v)) => {
                    let version: ModrinthVersion = installer
                        .json(&format!("{MODRINTH}/version/{}", encode(v)))
                        .await?;
                    version.project_id
                }
                (None, None) => continue,
            };
            queue.push((project, false));
        }
    }
    {
        let addons = addons.clone();
        fsops::as_user(uid, move || write_record(&addons, &record)).await?;
    }
    console.system(format!(
        "installed {}: restart the server to load {}",
        added.join(", "),
        if added.len() > 1 { "them" } else { "it" }
    ));
    Ok(added)
}

/// The newest release of `project` for Paper and Minecraft `version` (a beta when
/// it has no release).
async fn pick_version(
    installer: &Installer,
    project: &str,
    version: Option<&str>,
) -> anyhow::Result<ModrinthVersion> {
    let loaders = serde_json::to_string(PAPER_LOADERS)?;
    let mut params = vec![("loaders", loaders)];
    if let Some(v) = version {
        params.push(("game_versions", serde_json::to_string(&[v])?));
    }
    let url = reqwest::Url::parse_with_params(
        &format!("{MODRINTH}/project/{}/version", encode(project)),
        params,
    )?;
    let versions: Vec<ModrinthVersion> = installer.json(url.as_str()).await?;
    let newest_release = versions.iter().position(|v| v.version_type == "release");
    match newest_release.or((!versions.is_empty()).then_some(0)) {
        Some(i) => Ok(versions.into_iter().nth(i).expect("in range")),
        None => match version {
            Some(v) => bail!("{project} has no version for Paper on Minecraft {v}"),
            None => bail!("{project} has no version for Paper"),
        },
    }
}

fn encode(segment: &str) -> String {
    segment
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        .collect()
}

#[derive(Deserialize)]
struct ModrinthSearch {
    hits: Vec<ModrinthHit>,
}

#[derive(Deserialize)]
struct ModrinthHit {
    project_id: String,
    slug: String,
    title: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    author: String,
    #[serde(default)]
    downloads: u64,
    #[serde(default)]
    icon_url: Option<String>,
}

#[derive(Deserialize, Serialize)]
struct ModrinthVersion {
    project_id: String,
    #[serde(default)]
    version_type: String,
    files: Vec<ModrinthFile>,
    #[serde(default)]
    dependencies: Vec<ModrinthDependency>,
}

#[derive(Deserialize, Serialize)]
struct ModrinthFile {
    url: String,
    filename: String,
    #[serde(default)]
    primary: bool,
    hashes: ModrinthHashes,
}

#[derive(Deserialize, Serialize)]
struct ModrinthHashes {
    sha1: String,
}

#[derive(Deserialize, Serialize)]
struct ModrinthDependency {
    #[serde(default)]
    project_id: Option<String>,
    #[serde(default)]
    version_id: Option<String>,
    dependency_type: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_maps_in_a_pk3() {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("archipel-pk3-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("ut4_test.pk3");
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
        let opts = zip::write::SimpleFileOptions::default();
        for name in [
            "maps/ut4_Test.bsp",
            "maps/ut4_test.aas",
            "maps/sub/x.bsp",
            "textures/a.jpg",
        ] {
            zip.start_file(name, opts).unwrap();
            zip.write_all(b"x").unwrap();
        }
        zip.finish().unwrap();
        assert_eq!(maps_in_pk3(&path).unwrap(), ["ut4_test"]);
        let _ = std::fs::remove_dir_all(dir);
    }
}
