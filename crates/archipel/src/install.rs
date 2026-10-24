//! Getting a game's files onto the machine.
//!
//! * `minecraft`: the server jar (vanilla from Mojang, or Paper) in the server's
//!   directory, and the Java it needs from Adoptium, shared by every server.
//! * `shared`: an archive downloaded once per game and shared, read-only.
//! * `steamcmd`: a Steam app in the server's directory, downloaded anonymously by
//!   DepotDownloader (shared) run as the server's user.
//!
//! Anything written into a server's directory is written as its user (see
//! [`fsops`]); what is shared belongs to root and is only readable by servers.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, anyhow, bail};
use archipel_core::game::{GameDef, Install};
use archipel_core::template::Value;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use sha1::Digest as _;
use tokio::io::AsyncWriteExt;

use crate::config::Config;
use crate::console::Console;
use crate::fsops;

const MOJANG_MANIFEST: &str = "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json";
const PAPER: &str = "https://fill.papermc.io/v3/projects/paper";
/// Downloads Steam apps (SteamRE/DepotDownloader), pinned with its archives' SHA-256.
const DEPOT_DOWNLOADER_VERSION: &str = "3.4.0";
const DEPOT_DOWNLOADER_X64_SHA256: &str =
    "a999dec66b4850fc961bd50366696d23c2d0fad7b18790e6a5647b2f19097a53";
const DEPOT_DOWNLOADER_ARM64_SHA256: &str =
    "d9fb612ccebc1db8eeea3b4045d2221ec70431381393ce908fb72f01d4f9c812";
/// Steamworks' redistributable files, Steam's client library among them.
const STEAMWORKS_REDIST: u32 = 1007;

/// What a server's files were installed from, stored with it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Installed {
    /// What the settings asked for: `paper latest`. A different one reinstalls.
    pub wanted: String,
    /// What it got: `paper 26.3 build 12`.
    #[serde(default)]
    pub got: String,
    /// The Java major version a Minecraft server needs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub java: Option<u32>,
    /// The game version it got (Minecraft's `1.21.4`), which plugins must suit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// What a server needs to run, once installed.
#[derive(Debug, Default)]
pub struct Ready {
    pub java: Option<PathBuf>,
    pub base: Option<PathBuf>,
    pub installed: Option<Installed>,
}

pub struct Installer {
    cfg: Config,
    http: reqwest::Client,
    /// One download of a shared file at a time.
    locks: parking_lot::Mutex<HashMap<PathBuf, Arc<tokio::sync::Mutex<()>>>>,
    /// Minecraft releases (all, and those Paper builds), newest first, and when
    /// they were asked for.
    versions: tokio::sync::Mutex<Option<Versions>>,
    steam: crate::workshop::Steam,
}

/// When they were asked for, every release, and those Paper builds.
type Versions = (std::time::Instant, Vec<String>, Vec<String>);

/// How long the lists of Minecraft versions are reused.
const VERSIONS_FOR: std::time::Duration = std::time::Duration::from_secs(3600);

/// Where an install's own processes go (the server's cgroup), and as whom.
pub struct RunAs {
    pub uid: u32,
    pub cgroup: Option<PathBuf>,
    /// On the desktop: the ruleset keeping it to its files.
    pub sandbox: Option<std::sync::Arc<crate::sandbox::Sandbox>>,
}

impl Installer {
    pub fn new(cfg: Config) -> anyhow::Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("archipel/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self {
            cfg,
            http,
            locks: Default::default(),
            versions: Default::default(),
            steam: Default::default(),
        })
    }

    pub fn http(&self) -> &reqwest::Client {
        &self.http
    }

    /// What Steam says about Workshop items.
    pub fn workshop(&self) -> &crate::workshop::Steam {
        &self.steam
    }

    /// Minecraft releases, newest first: those Paper builds with `paper`.
    pub async fn minecraft_versions(&self, paper: bool) -> anyhow::Result<Vec<String>> {
        let mut cached = self.versions.lock().await;
        let fresh = cached
            .as_ref()
            .is_some_and(|(at, _, _)| at.elapsed() < VERSIONS_FOR);
        if !fresh {
            let manifest: MojangManifest = self.json(MOJANG_MANIFEST).await?;
            let releases: Vec<String> = manifest
                .versions
                .into_iter()
                .filter(|v| v.kind == "release")
                .map(|v| v.id)
                .collect();
            let builds = self.json::<PaperProject>(PAPER).await?.all();
            let with_paper = releases
                .iter()
                .filter(|r| builds.contains(r))
                .cloned()
                .collect();
            *cached = Some((std::time::Instant::now(), releases, with_paper));
        }
        let (_, all, with_paper) = cached.as_ref().expect("filled above");
        Ok(if paper {
            with_paper.clone()
        } else {
            all.clone()
        })
    }

    fn lock(&self, path: &Path) -> Arc<tokio::sync::Mutex<()>> {
        self.locks
            .lock()
            .entry(path.to_path_buf())
            .or_default()
            .clone()
    }

    /// What the settings ask to install.
    pub fn wanted(game: &GameDef, settings: &std::collections::BTreeMap<String, Value>) -> String {
        let setting = |k: &str| settings.get(k).map(Value::as_text).unwrap_or_default();
        match &game.install {
            Install::Minecraft => format!("{} {}", setting("flavor"), setting("version")),
            Install::Shared { url, .. } => format!("shared {url}"),
            Install::Steamcmd { app_id, beta: None } => format!("steam {app_id}"),
            Install::Steamcmd {
                app_id,
                beta: Some(beta),
            } => format!("steam {app_id} {beta}"),
        }
    }

    /// Installs what the server lacks, and says where its runtime is.
    pub async fn prepare(
        &self,
        game: &GameDef,
        settings: &std::collections::BTreeMap<String, Value>,
        installed: Option<&Installed>,
        dir: &Path,
        run_as: &RunAs,
        console: &Console,
    ) -> anyhow::Result<Ready> {
        let wanted = Self::wanted(game, settings);
        let current = installed.filter(|i| i.wanted == wanted);
        match &game.install {
            Install::Minecraft => {
                let record = match current {
                    Some(i) if dir.join("server.jar").exists() => i.clone(),
                    _ => {
                        self.minecraft(settings, &wanted, dir, run_as.uid, console)
                            .await?
                    }
                };
                let major = record.java.unwrap_or(21);
                let java = self.java(major, console).await?;
                Ok(Ready {
                    java: Some(java),
                    base: None,
                    installed: Some(record),
                })
            }
            Install::Shared { url, subdir } => {
                let base = self
                    .shared(&game.id, url, subdir.as_deref(), console)
                    .await?;
                Ok(Ready {
                    java: None,
                    base: Some(base),
                    installed: Some(Installed {
                        wanted: wanted.clone(),
                        got: wanted,
                        java: None,
                        version: None,
                    }),
                })
            }
            Install::Steamcmd { app_id, beta } => {
                let record = match current {
                    Some(i) => i.clone(),
                    None => {
                        self.steam(*app_id, beta.as_deref(), dir, run_as, console)
                            .await?;
                        Installed {
                            got: wanted.clone(),
                            wanted,
                            java: None,
                            version: None,
                        }
                    }
                };
                Ok(Ready {
                    installed: Some(record),
                    ..Default::default()
                })
            }
        }
    }

    // ---------------------------------------------------------------- Minecraft

    async fn minecraft(
        &self,
        settings: &std::collections::BTreeMap<String, Value>,
        wanted: &str,
        dir: &Path,
        uid: u32,
        console: &Console,
    ) -> anyhow::Result<Installed> {
        let flavor = settings
            .get("flavor")
            .map(Value::as_text)
            .unwrap_or_default();
        let asked = settings
            .get("version")
            .map(Value::as_text)
            .unwrap_or_else(|| "latest".into());
        let asked = asked.trim();
        let manifest: MojangManifest = self.json(MOJANG_MANIFEST).await?;
        let releases: Vec<&MojangVersion> = manifest
            .versions
            .iter()
            .filter(|v| v.kind == "release")
            .collect();

        let (version, jar, check, got) = if flavor == "paper" {
            let paper: PaperProject = self.json(PAPER).await?;
            let available = paper.all();
            let version = if asked == "latest" {
                releases
                    .iter()
                    .map(|v| &v.id)
                    .find(|id| available.contains(id))
                    .context("Paper has no build for any Minecraft release yet")?
                    .clone()
            } else if available.iter().any(|v| v == asked) {
                asked.to_string()
            } else {
                bail!("Paper has no build for Minecraft {asked}: pick another version, or vanilla")
            };
            let build: PaperBuild = self
                .json(&format!("{PAPER}/versions/{version}/builds/latest"))
                .await?;
            let download = build
                .downloads
                .get("server:default")
                .context("Paper's build has no server download")?;
            let got = format!("Paper {version} build {}", build.id);
            (
                version,
                download.url.clone(),
                Check::Sha256(download.checksums.sha256.clone()),
                got,
            )
        } else {
            let version = if asked == "latest" {
                manifest.latest.release.clone()
            } else {
                asked.to_string()
            };
            let entry = manifest
                .versions
                .iter()
                .find(|v| v.id == version)
                .ok_or_else(|| anyhow!("Minecraft has no version {version}"))?;
            let details: MojangDetails = self.json(&entry.url).await?;
            let server = details
                .downloads
                .server
                .context("that version has no server download")?;
            (
                version.clone(),
                server.url,
                Check::Sha1(server.sha1),
                format!("Minecraft {version}"),
            )
        };

        // The Java it needs, from Mojang's description of the version.
        let entry = manifest.versions.iter().find(|v| v.id == version);
        let java = match entry {
            Some(entry) => {
                let details: MojangDetails = self.json(&entry.url).await?;
                details.java_version.map_or(8, |j| j.major_version)
            }
            None => 21,
        };

        console.system(format!("downloading {got}"));
        let tmp = self.tmp_file(&format!("server-{}.jar", std::process::id()))?;
        self.download(&jar, &tmp, Some(check), console).await?;
        let dest = fsops::inside(dir, "server.jar")?;
        copy_as_user(&tmp, dest, uid).await?;
        let _ = std::fs::remove_file(&tmp);
        console.system(format!("installed {got} (Java {java})"));
        Ok(Installed {
            wanted: wanted.to_string(),
            got,
            java: Some(java),
            version: Some(version),
        })
    }

    /// Java `major`, from Adoptium, shared: the path of its `java`.
    async fn java(&self, major: u32, console: &Console) -> anyhow::Result<PathBuf> {
        let dir = self.cfg.runtimes_dir().join(format!("java-{major}"));
        let java = dir.join("bin/java");
        if dir.join(".complete").exists() {
            return Ok(java);
        }
        let lock = self.lock(&dir);
        let _held = lock.lock().await;
        if dir.join(".complete").exists() {
            return Ok(java);
        }
        let arch = match std::env::consts::ARCH {
            "x86_64" => "x64",
            "aarch64" => "aarch64",
            other => bail!("no Java builds for {other}"),
        };
        let url = format!(
            "https://api.adoptium.net/v3/binary/latest/{major}/ga/linux/{arch}/jre/hotspot/normal/eclipse"
        );
        console.system(format!("downloading Java {major}"));
        let tmp = self.tmp_file(&format!("java-{major}.tar.gz"))?;
        self.download(&url, &tmp, None, console).await?;
        let staging = dir.with_extension("partial");
        let _ = std::fs::remove_dir_all(&staging);
        let (from, to) = (tmp.clone(), staging.clone());
        tokio::task::spawn_blocking(move || untar_gz(&from, &to, 1)).await??;
        let _ = std::fs::remove_file(&tmp);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::rename(&staging, &dir)?;
        std::fs::write(dir.join(".complete"), "")?;
        console.system(format!("Java {major} ready"));
        Ok(java)
    }

    // ---------------------------------------------------------------- shared archives

    async fn shared(
        &self,
        game: &str,
        url: &str,
        subdir: Option<&str>,
        console: &Console,
    ) -> anyhow::Result<PathBuf> {
        let key = hex::encode(&sha2::Sha256::digest(url.as_bytes())[..6]);
        let dir = self.cfg.shared_dir().join(game).join(key);
        let base = match subdir {
            Some(sub) => fsops::inside(&dir, sub)?,
            None => dir.clone(),
        };
        if dir.join(".complete").exists() {
            return Ok(base);
        }
        let lock = self.lock(&dir);
        let _held = lock.lock().await;
        if dir.join(".complete").exists() {
            return Ok(base);
        }
        console.system(format!(
            "downloading the game's files, shared by every {game} server: this happens once"
        ));
        let name = url.rsplit('/').next().unwrap_or("download");
        let tmp = self.tmp_file(&format!("{game}-{name}"))?;
        self.download(url, &tmp, None, console).await?;
        console.system("unpacking".into());
        let staging = dir.with_extension("partial");
        let _ = std::fs::remove_dir_all(&staging);
        let (from, to) = (tmp.clone(), staging.clone());
        let lower = name.to_ascii_lowercase();
        tokio::task::spawn_blocking(move || {
            if lower.ends_with(".zip") {
                unzip(&from, &to)
            } else {
                untar_gz(&from, &to, 0)
            }
        })
        .await??;
        let _ = std::fs::remove_file(&tmp);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.parent().unwrap_or(&dir))?;
        std::fs::rename(&staging, &dir)?;
        std::fs::write(dir.join(".complete"), url)?;
        if !base.exists() {
            bail!("the archive has no {}", subdir.unwrap_or_default());
        }
        console.system("game files ready".into());
        Ok(base)
    }

    // ---------------------------------------------------------------- Steam

    /// DepotDownloader, downloaded once and shared: the path of its program.
    async fn depot_downloader(&self, console: &Console) -> anyhow::Result<PathBuf> {
        use std::os::unix::fs::PermissionsExt;
        let dir = self
            .cfg
            .runtimes_dir()
            .join(format!("depotdownloader-{DEPOT_DOWNLOADER_VERSION}"));
        let program = dir.join("DepotDownloader");
        if dir.join(".complete").exists() {
            return Ok(program);
        }
        let lock = self.lock(&dir);
        let _held = lock.lock().await;
        if dir.join(".complete").exists() {
            return Ok(program);
        }
        let (arch, sha256) = match std::env::consts::ARCH {
            "x86_64" => ("x64", DEPOT_DOWNLOADER_X64_SHA256),
            "aarch64" => ("arm64", DEPOT_DOWNLOADER_ARM64_SHA256),
            other => bail!("no DepotDownloader for {other}"),
        };
        let url = format!(
            "https://github.com/SteamRE/DepotDownloader/releases/download/\
             DepotDownloader_{DEPOT_DOWNLOADER_VERSION}/DepotDownloader-linux-{arch}.zip"
        );
        console.system("downloading DepotDownloader, which gets games from Steam".into());
        let tmp = self.tmp_file("depotdownloader.zip")?;
        self.download(&url, &tmp, Some(Check::Sha256(sha256.into())), console)
            .await?;
        let staging = dir.with_extension("partial");
        let _ = std::fs::remove_dir_all(&staging);
        let (from, to) = (tmp.clone(), staging.clone());
        tokio::task::spawn_blocking(move || unzip(&from, &to)).await??;
        let _ = std::fs::remove_file(&tmp);
        // Servers' users run it.
        std::fs::set_permissions(
            staging.join("DepotDownloader"),
            std::fs::Permissions::from_mode(0o755),
        )?;
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(self.cfg.runtimes_dir())?;
        std::fs::rename(&staging, &dir)?;
        std::fs::write(dir.join(".complete"), "")?;
        Ok(program)
    }

    /// Steam app `app_id` (its `beta` branch) in `dir`, downloaded anonymously with
    /// DepotDownloader as the server's user: a 64-bit program, unlike SteamCMD,
    /// which needs 32-bit libraries some systems lack (NixOS).
    async fn steam_app(
        &self,
        app_id: u32,
        beta: Option<&str>,
        dir: &Path,
        run_as: &RunAs,
        console: &Console,
    ) -> anyhow::Result<()> {
        let program = self.depot_downloader(console).await?;
        let mut last = Err(anyhow!("DepotDownloader did not run"));
        // Steam's servers time out now and then: again, keeping what came.
        for attempt in 1..=3 {
            let mut cmd = tokio::process::Command::new(&program);
            cmd.args(["-app", &app_id.to_string()]);
            if let Some(beta) = beta {
                cmd.args(["-beta", beta]);
            }
            cmd.args(["-os", "linux", "-osarch", "64", "-validate", "-dir"])
                .arg(dir)
                .current_dir(dir)
                .env_clear()
                .env("HOME", dir)
                .env("PATH", crate::fsops::servers_path())
                .env("LANG", "C.UTF-8")
                // No ICU needed: it handles no text in other languages.
                .env("DOTNET_SYSTEM_GLOBALIZATION_INVARIANT", "1");
            last =
                crate::supervisor::run_logged_with(cmd, run_as, console, progress(console)).await;
            match &last {
                Ok(()) => break,
                Err(e) => {
                    console.system(format!("DepotDownloader failed (attempt {attempt}): {e:#}"))
                }
            }
        }
        last
    }

    async fn steam(
        &self,
        app_id: u32,
        beta: Option<&str>,
        dir: &Path,
        run_as: &RunAs,
        console: &Console,
    ) -> anyhow::Result<()> {
        console.system(match beta {
            Some(beta) => format!("installing Steam app {app_id} ({beta})"),
            None => format!("installing Steam app {app_id}"),
        });
        self.steam_app(app_id, beta, dir, run_as, console).await?;
        // Steam games look for Steam's client library in their user's home: 64-bit
        // ones in sdk64, 32-bit ones (older Source games) in sdk32. It comes with
        // Steamworks' redistributable files.
        console.system("getting Steam's client library".into());
        let redist = fsops::inside(dir, ".steam/redist")?;
        {
            let redist = redist.clone();
            fsops::as_user(run_as.uid, move || std::fs::create_dir_all(redist)).await?;
        }
        self.steam_app(STEAMWORKS_REDIST, None, &redist, run_as, console)
            .await
            .context("getting Steam's client library")?;
        let root = dir.to_path_buf();
        fsops::as_user(run_as.uid, move || {
            for (from, to) in [
                ("linux64/steamclient.so", "sdk64"),
                ("steamclient.so", "sdk32"),
            ] {
                let client = redist.join(from);
                if client.exists() {
                    let dest = root.join(".steam").join(to);
                    std::fs::create_dir_all(&dest)?;
                    std::fs::rename(&client, dest.join("steamclient.so"))?;
                }
            }
            let _ = std::fs::remove_dir_all(&redist);
            // What SteamCMD left there before archipel used DepotDownloader.
            let _ = std::fs::remove_dir_all(root.join(".steamcmd"));
            // Steam's file lists do not always mark programs executable.
            make_programs_executable(&root)
        })
        .await
        .context("finishing the install")?;
        console.system(format!("Steam app {app_id} installed"));
        Ok(())
    }

    // ---------------------------------------------------------------- helpers

    pub(crate) fn tmp_file(&self, name: &str) -> anyhow::Result<PathBuf> {
        let dir = self.cfg.data_dir.join("tmp");
        std::fs::create_dir_all(&dir)?;
        Ok(dir.join(format!("{}-{name}", archipel_core::now())))
    }

    pub(crate) async fn json<T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
    ) -> anyhow::Result<T> {
        let resp = self
            .http
            .get(url)
            .send()
            .await
            .with_context(|| format!("reaching {url}"))?
            .error_for_status()
            .with_context(|| format!("asking {url}"))?;
        resp.json()
            .await
            .with_context(|| format!("reading the answer of {url}"))
    }

    /// Downloads `url` to `dest`, saying how far along it is.
    pub(crate) async fn download(
        &self,
        url: &str,
        dest: &Path,
        check: Option<Check>,
        console: &Console,
    ) -> anyhow::Result<()> {
        let resp = self
            .http
            .get(url)
            .send()
            .await
            .with_context(|| format!("reaching {url}"))?
            .error_for_status()
            .with_context(|| format!("downloading {url}"))?;
        let total = resp.content_length();
        let mut file = tokio::fs::File::create(dest).await?;
        let mut stream = resp.bytes_stream();
        let mut done: u64 = 0;
        let mut told = 0;
        let mut sha1 = sha1::Sha1::new();
        let mut sha256 = sha2::Sha256::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.context("the download was cut")?;
            file.write_all(&chunk).await?;
            match &check {
                Some(Check::Sha1(_)) => sha1.update(&chunk),
                Some(Check::Sha256(_)) => sha256.update(&chunk),
                None => {}
            }
            done += chunk.len() as u64;
            if let Some(total) = total.filter(|t| *t > 0) {
                let percent = done * 100 / total;
                if percent >= told + 10 {
                    told = percent - percent % 10;
                    console.system(format!("  {told}% of {} MB", total / (1024 * 1024)));
                }
            }
        }
        file.flush().await?;
        let ok = match check {
            Some(Check::Sha1(want)) => hex::encode(sha1.finalize()) == want.to_lowercase(),
            Some(Check::Sha256(want)) => hex::encode(sha256.finalize()) == want.to_lowercase(),
            None => true,
        };
        if !ok {
            let _ = std::fs::remove_file(dest);
            bail!("the download of {url} is corrupt (checksum mismatch)");
        }
        Ok(())
    }
}

pub(crate) enum Check {
    Sha1(String),
    Sha256(String),
}

/// Copies a file archipel downloaded into a server's directory, as its user.
pub(crate) async fn copy_as_user(from: &Path, to: PathBuf, uid: u32) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    // Readable by the server's user for the copy.
    std::fs::set_permissions(from, std::fs::Permissions::from_mode(0o644))?;
    let from = from.to_path_buf();
    fsops::as_user(uid, move || {
        let partial = to.with_extension("partial");
        std::fs::copy(&from, &partial)?;
        std::fs::rename(&partial, &to)
    })
    .await
    .context("copying into the server's directory")
}

/// DepotDownloader prints lines per file, ` 12.34% path`: those become a line
/// every tenth of the way. Its other lines are shown, but those about each file
/// it prepares, and the chunks it fetches again after a timeout.
fn progress(console: &Console) -> impl Fn(&str) -> bool + Send + Sync + 'static {
    let console = console.clone();
    let told = std::sync::atomic::AtomicU32::new(0);
    move |line: &str| {
        if line.starts_with("Pre-allocating ")
            || line.starts_with("Connection timeout downloading chunk")
        {
            return false;
        }
        let Some(percent) = line
            .trim_start()
            .split_once('%')
            .and_then(|(p, _)| p.parse::<f32>().ok())
        else {
            return true;
        };
        let tenth = (percent / 10.0) as u32;
        if tenth > told.load(std::sync::atomic::Ordering::Relaxed) {
            told.store(tenth, std::sync::atomic::Ordering::Relaxed);
            console.system(format!("  {}%", tenth * 10));
        }
        false
    }
}

/// Marks executable the programs and scripts under `root` (ELF files, `#!`
/// scripts) that are not. Links are not followed.
fn make_programs_executable(root: &Path) -> std::io::Result<()> {
    use std::io::Read;
    use std::os::unix::fs::PermissionsExt;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)?.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                stack.push(entry.path());
                continue;
            }
            if !kind.is_file() {
                continue;
            }
            let path = entry.path();
            let Ok(meta) = entry.metadata() else { continue };
            let mode = meta.permissions().mode();
            if mode & 0o100 != 0 {
                continue;
            }
            let mut head = [0u8; 4];
            let Ok(n) = std::fs::File::open(&path).and_then(|mut f| f.read(&mut head)) else {
                continue;
            };
            if head[..n] == *b"\x7fELF" || head[..n.min(2)] == *b"#!" {
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode | 0o111))?;
            }
        }
    }
    Ok(())
}

/// A relative archive path, without its first `strip` components, if it stays
/// inside.
fn clean(path: &Path, strip: usize) -> Option<PathBuf> {
    let parts: Vec<Component> = path.components().collect();
    if parts
        .iter()
        .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
    {
        return None;
    }
    let kept: PathBuf = parts
        .into_iter()
        .filter(|c| matches!(c, Component::Normal(_)))
        .skip(strip)
        .collect();
    (!kept.as_os_str().is_empty()).then_some(kept)
}

fn untar_gz(archive: &Path, dest: &Path, strip: usize) -> anyhow::Result<()> {
    std::fs::create_dir_all(dest)?;
    let file = std::fs::File::open(archive)?;
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(file));
    tar.set_preserve_permissions(true);
    for entry in tar.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        let Some(rel) = clean(&path, strip) else {
            continue;
        };
        // Links pointing out of the archive would let a later file escape.
        if let Some(target) = entry.link_name()?
            && (target.is_absolute()
                || target
                    .components()
                    .any(|c| matches!(c, Component::ParentDir)))
        {
            continue;
        }
        let out = dest.join(&rel);
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)?;
        }
        entry.unpack(&out)?;
    }
    Ok(())
}

fn unzip(archive: &Path, dest: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(dest)?;
    let mut zip = zip::ZipArchive::new(std::fs::File::open(archive)?)?;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i)?;
        let Some(rel) = entry.enclosed_name() else {
            continue;
        };
        let out = dest.join(rel);
        if entry.is_dir() {
            std::fs::create_dir_all(&out)?;
            continue;
        }
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = std::fs::File::create(&out)?;
        let mut buf = vec![0; 1 << 16];
        loop {
            let n = entry.read(&mut buf)?;
            if n == 0 {
                break;
            }
            std::io::Write::write_all(&mut file, &buf[..n])?;
        }
        // Executables stay executable; everything is readable by the servers.
        let mode = entry.unix_mode().unwrap_or(0o644) & 0o755 | 0o444;
        std::fs::set_permissions(&out, std::fs::Permissions::from_mode(mode))?;
    }
    // Directories too.
    let mut stack = vec![dest.to_path_buf()];
    while let Some(d) = stack.pop() {
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o755))?;
        for e in std::fs::read_dir(&d)?.flatten() {
            if e.file_type().is_ok_and(|t| t.is_dir()) {
                stack.push(e.path());
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------- API shapes

#[derive(Deserialize)]
struct MojangManifest {
    latest: MojangLatest,
    versions: Vec<MojangVersion>,
}

#[derive(Deserialize)]
struct MojangLatest {
    release: String,
}

#[derive(Deserialize)]
struct MojangVersion {
    id: String,
    #[serde(rename = "type")]
    kind: String,
    url: String,
}

#[derive(Deserialize)]
struct MojangDetails {
    downloads: MojangDownloads,
    #[serde(rename = "javaVersion")]
    java_version: Option<MojangJava>,
}

#[derive(Deserialize)]
struct MojangDownloads {
    server: Option<MojangFile>,
}

#[derive(Deserialize)]
struct MojangFile {
    url: String,
    sha1: String,
}

#[derive(Deserialize)]
struct MojangJava {
    #[serde(rename = "majorVersion")]
    major_version: u32,
}

#[derive(Deserialize)]
struct PaperProject {
    versions: serde_json::Map<String, serde_json::Value>,
}

impl PaperProject {
    fn all(&self) -> Vec<String> {
        self.versions
            .values()
            .filter_map(|v| v.as_array())
            .flatten()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect()
    }
}

#[derive(Deserialize)]
struct PaperBuild {
    id: u64,
    downloads: HashMap<String, PaperDownload>,
}

#[derive(Deserialize)]
struct PaperDownload {
    url: String,
    checksums: PaperChecksums,
}

#[derive(Deserialize)]
struct PaperChecksums {
    sha256: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn condenses_depot_downloader_progress() {
        let log =
            std::env::temp_dir().join(format!("archipel-progress-{}.log", std::process::id()));
        let console = Console::new(log.clone());
        let keep = progress(&console);
        assert!(keep("Got depot key for 1006 result: OK"));
        assert!(!keep("Pre-allocating /data/servers/1/srcds_run"));
        assert!(!keep("Connection timeout downloading chunk 1535d966"));
        assert!(!keep(" 3.10% srcds_run"));
        assert!(!keep(" 12.00% bin/linux64/srcds"));
        assert!(!keep(" 15.00% garrysmod/cfg/mount.cfg"));
        assert!(!keep("100.00% steamclient.so"));
        assert_eq!(
            console.follow(10).0,
            ["[archipel]   10%", "[archipel]   100%"]
        );
        let _ = std::fs::remove_file(log);
    }

    #[test]
    fn marks_programs_executable() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("archipel-exec-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::write(dir.join("srcds_run"), "#!/bin/sh\n").unwrap();
        std::fs::write(dir.join("bin/srcds"), b"\x7fELF\x02rest").unwrap();
        std::fs::write(dir.join("readme.txt"), "hello").unwrap();
        for f in ["srcds_run", "bin/srcds", "readme.txt"] {
            std::fs::set_permissions(dir.join(f), std::fs::Permissions::from_mode(0o644)).unwrap();
        }
        make_programs_executable(&dir).unwrap();
        let mode = |f: &str| std::fs::metadata(dir.join(f)).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode("srcds_run"), 0o755);
        assert_eq!(mode("bin/srcds"), 0o755);
        assert_eq!(mode("readme.txt"), 0o644);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn cleans_archive_paths() {
        assert_eq!(
            clean(Path::new("jdk-21/bin/java"), 1),
            Some(PathBuf::from("bin/java"))
        );
        assert_eq!(clean(Path::new("./a/b"), 0), Some(PathBuf::from("a/b")));
        assert_eq!(clean(Path::new("../etc/passwd"), 0), None);
        assert_eq!(clean(Path::new("/etc/passwd"), 0), None);
        assert_eq!(clean(Path::new("jdk-21"), 1), None);
    }
}
