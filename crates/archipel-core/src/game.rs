//! Game definitions: everything archipel knows about a game, in one TOML file
//! (`games/minecraft.toml`). Adding a game is writing one.
//!
//! Templates in a definition use these placeholders:
//!
//! * `{port.<name>}`: the ports given to the server, one per `[[ports]]`;
//! * `{setting.<key>}`: what the server's owner chose, one per `[[settings]]`;
//! * `{memory_mb}`: the server's memory, and `{java_memory_mb}`, the part of it a JVM
//!   heap may take (the rest is the JVM's own);
//! * `{dir}`: the server's directory, `{base}`: the game's shared files (`shared`
//!   installs), `{java}`: the Java the server needs (`minecraft` installs);
//! * `{host}` (in `connect` only): the address players use.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::template::{self, Context, Value};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GameDef {
    /// `minecraft`: lowercase letters, digits and dashes.
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// A short mark for the game's card: one or two characters, an emoji.
    #[serde(default)]
    pub badge: String,
    pub memory: MemoryDef,
    /// Ports the server listens on, given consecutively from archipel's pool, the
    /// same inside and outside, in this order.
    #[serde(default)]
    pub ports: Vec<PortDef>,
    pub install: Install,
    pub run: RunDef,
    #[serde(default)]
    pub settings: Vec<SettingDef>,
    #[serde(default)]
    pub files: Vec<FileDef>,
    /// How players connect, shown on the server's page: `{host}:{port.game}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connect: Option<String>,
    /// Console commands making the files whole while a backup copies them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backup: Option<BackupDef>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryDef {
    /// Less and the server does not start.
    pub min_mb: u64,
    /// What a new server gets.
    pub default_mb: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    Tcp,
    Udp,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PortDef {
    pub name: String,
    pub protocol: Protocol,
    /// What it is for, for people: "Game", "Queries".
    #[serde(default)]
    pub label: String,
}

/// How a game's files get onto the node.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Install {
    /// Minecraft's server jar, vanilla or Paper (settings `version` and `flavor`),
    /// in each server's directory, and the Java it needs, shared.
    Minecraft,
    /// An archive (`.zip`, `.tar.gz`) downloaded once and shared, read-only, by every
    /// server of the game: `{base}`, the directory `subdir` of the archive.
    Shared {
        url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subdir: Option<String>,
    },
    /// A Steam dedicated server, logged in anonymously, in each server's directory.
    /// Updated when the server is reinstalled.
    Steamcmd { app_id: u32 },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunDef {
    /// The program and its arguments (templates), run in the server's directory.
    pub command: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Typed into the console to stop the server cleanly: `stop`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_command: Option<String>,
    /// Or the signal asking it to: `SIGINT`. `SIGTERM` when neither is given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_signal: Option<String>,
    /// How long it may take to stop before it is killed.
    #[serde(default = "default_stop_timeout")]
    pub stop_timeout_secs: u32,
    /// Text of a log line telling the server is up: `Done (`. Without it, a server
    /// counts as running once started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ready: Option<String>,
}

fn default_stop_timeout() -> u32 {
    30
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SettingDef {
    /// `motd`: used as `{setting.motd}`.
    pub key: String,
    pub label: String,
    #[serde(default)]
    pub help: String,
    /// A toggle that must be on (an EULA), or text that must not be empty.
    #[serde(default)]
    pub required: bool,
    /// A page to read about it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
    /// Changing it reinstalls the server's files (a game version).
    #[serde(default)]
    pub reinstall: bool,
    #[serde(flatten)]
    pub kind: SettingKind,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SettingKind {
    Text {
        #[serde(default)]
        default: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_len: Option<usize>,
    },
    /// Text shown as dots: a password.
    Secret {
        #[serde(default)]
        default: String,
    },
    Number {
        default: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        min: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max: Option<i64>,
    },
    Toggle {
        #[serde(default)]
        default: bool,
    },
    Choice {
        default: String,
        options: Vec<ChoiceOption>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChoiceOption {
    pub value: String,
    pub label: String,
}

impl SettingDef {
    pub fn default_value(&self) -> Value {
        match &self.kind {
            SettingKind::Text { default, .. } | SettingKind::Secret { default } => {
                Value::Text(default.clone())
            }
            SettingKind::Number { default, .. } => Value::Number(*default),
            SettingKind::Toggle { default } => Value::Bool(*default),
            SettingKind::Choice { default, .. } => Value::Text(default.clone()),
        }
    }

    /// `value` as this setting takes it, or why it cannot.
    pub fn check(&self, value: &Value) -> Result<Value, String> {
        let label = &self.label;
        let value = match (&self.kind, value) {
            (SettingKind::Text { max_len, .. }, v) => {
                let text = v.as_text();
                if max_len.is_some_and(|max| text.chars().count() > max) {
                    return Err(format!("{label} is too long"));
                }
                if text.contains(['\n', '\r', '\0']) {
                    return Err(format!("{label} must fit on one line"));
                }
                Value::Text(text)
            }
            (SettingKind::Secret { .. }, v) => {
                let text = v.as_text();
                if text.contains(['\n', '\r', '\0', '"']) {
                    return Err(format!("{label} cannot contain quotes or line breaks"));
                }
                Value::Text(text)
            }
            (SettingKind::Number { min, max, .. }, v) => {
                let n = match v {
                    Value::Number(n) => *n,
                    other => other
                        .as_text()
                        .trim()
                        .parse()
                        .map_err(|_| format!("{label} must be a number"))?,
                };
                if min.is_some_and(|m| n < m) || max.is_some_and(|m| n > m) {
                    return Err(format!(
                        "{label} must be between {} and {}",
                        min.map_or("…".into(), |m| m.to_string()),
                        max.map_or("…".into(), |m| m.to_string())
                    ));
                }
                Value::Number(n)
            }
            (SettingKind::Toggle { .. }, v) => Value::Bool(v.as_bool()),
            (SettingKind::Choice { options, .. }, v) => {
                let text = v.as_text();
                if !options.iter().any(|o| o.value == text) {
                    return Err(format!("{label}: `{text}` is not one of the choices"));
                }
                Value::Text(text)
            }
        };
        if self.required {
            match &value {
                Value::Bool(false) => return Err(format!("“{label}” must be checked")),
                Value::Text(t) if t.trim().is_empty() => {
                    return Err(format!("{label} is required"));
                }
                _ => {}
            }
        }
        Ok(value)
    }
}

/// A file archipel writes in the server's directory before each start.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileDef {
    /// Relative to the server's directory.
    pub path: String,
    pub format: FileFormat,
    /// For people, in the Files tab.
    #[serde(default)]
    pub label: String,
    /// Keys set at each start (`properties`, `json`), others being left as they are.
    #[serde(default)]
    pub set: BTreeMap<String, String>,
    /// The whole file, written at each start (`text`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// Written as is (no placeholders) when the file is missing: a starting point
    /// to edit, which `set` then applies to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial: Option<String>,
    /// The server's owner may edit it in the Files tab. Keys in `set` still win.
    #[serde(default)]
    pub editable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FileFormat {
    /// `key=value` lines (Java's `.properties`).
    Properties,
    Json,
    Text,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BackupDef {
    /// Typed into the console before the copy: `save-off`, `save-all flush`.
    #[serde(default)]
    pub before: Vec<String>,
    /// A log line telling the `before` commands are done: `Saved the game`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait_for: Option<String>,
    /// Typed in after the copy: `save-on`.
    #[serde(default)]
    pub after: Vec<String>,
}

impl GameDef {
    pub fn setting(&self, key: &str) -> Option<&SettingDef> {
        self.settings.iter().find(|s| s.key == key)
    }

    /// The settings `given`, checked, with defaults for the ones left out.
    pub fn settings_from(
        &self,
        given: &BTreeMap<String, Value>,
    ) -> Result<BTreeMap<String, Value>, String> {
        if let Some(unknown) = given.keys().find(|k| self.setting(k).is_none()) {
            return Err(format!("{} has no setting `{unknown}`", self.name));
        }
        self.settings
            .iter()
            .map(|s| {
                let value = given
                    .get(&s.key)
                    .cloned()
                    .unwrap_or_else(|| s.default_value());
                Ok((s.key.clone(), s.check(&value)?))
            })
            .collect()
    }

    /// The placeholders any server of this game can use, with sample values.
    fn sample_context(&self) -> Context {
        let mut ctx = Context::new();
        for (i, p) in self.ports.iter().enumerate() {
            ctx.insert(format!("port.{}", p.name), Value::Number(30000 + i as i64));
        }
        for s in &self.settings {
            ctx.insert(format!("setting.{}", s.key), s.default_value());
        }
        for (key, value) in [
            ("memory_mb", Value::Number(1024)),
            ("java_memory_mb", Value::Number(768)),
            ("dir", Value::Text("/data/servers/1".into())),
            ("base", Value::Text("/data/shared/game".into())),
            ("java", Value::Text("/data/runtimes/java/bin/java".into())),
        ] {
            ctx.insert(key.into(), value);
        }
        ctx
    }

    /// Whether the definition holds together: names, and templates using only
    /// placeholders that exist.
    pub fn validate(&self) -> Result<(), String> {
        let word = |s: &str| {
            !s.is_empty()
                && s.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
        };
        if !word(&self.id) {
            return Err(format!(
                "game id `{}` must be lowercase letters, digits and dashes",
                self.id
            ));
        }
        let at = |e: String| format!("{}: {e}", self.id);
        if self.memory.min_mb == 0 || self.memory.default_mb < self.memory.min_mb {
            return Err(at(
                "memory.default_mb must be at least memory.min_mb, above 0".into(),
            ));
        }
        for (i, p) in self.ports.iter().enumerate() {
            if !word(&p.name) || self.ports[..i].iter().any(|q| q.name == p.name) {
                return Err(at(format!("port name `{}` is invalid or repeated", p.name)));
            }
        }
        for (i, s) in self.settings.iter().enumerate() {
            if !word(&s.key) || self.settings[..i].iter().any(|t| t.key == s.key) {
                return Err(at(format!(
                    "setting key `{}` is invalid or repeated",
                    s.key
                )));
            }
            if let SettingKind::Choice { default, options } = &s.kind
                && !options.iter().any(|o| &o.value == default)
            {
                return Err(at(format!(
                    "setting {}: the default is not a choice",
                    s.key
                )));
            }
        }
        if self.run.command.is_empty() {
            return Err(at("run.command is empty".into()));
        }
        if matches!(self.install, Install::Minecraft) {
            for key in ["version", "flavor"] {
                if self.setting(key).is_none() {
                    return Err(at(format!("minecraft installs need a `{key}` setting")));
                }
            }
        }
        let ctx = self.sample_context();
        let check = |what: &str, t: &str| {
            template::render(t, &ctx)
                .map(|_| ())
                .map_err(|e| at(format!("{what}: {e}")))
        };
        for part in &self.run.command {
            check("run.command", part)?;
        }
        for (k, v) in &self.run.env {
            check(&format!("run.env.{k}"), v)?;
        }
        for file in &self.files {
            let path = std::path::Path::new(&file.path);
            if file.path.is_empty()
                || path.is_absolute()
                || path
                    .components()
                    .any(|c| !matches!(c, std::path::Component::Normal(_)))
            {
                return Err(at(format!(
                    "file `{}` must be a plain relative path",
                    file.path
                )));
            }
            for v in file.set.values() {
                check(&file.path, v)?;
            }
            if let Some(t) = &file.content {
                check(&file.path, t)?;
            }
            if let (FileFormat::Json, Some(initial)) = (file.format, &file.initial)
                && serde_json::from_str::<serde_json::Value>(initial).is_err()
            {
                return Err(at(format!("{}: `initial` is not valid JSON", file.path)));
            }
            if file.format != FileFormat::Text && file.content.is_some() {
                return Err(at(format!("{}: `content` is for text files", file.path)));
            }
        }
        if let Some(connect) = &self.connect {
            let mut ctx = ctx.clone();
            ctx.insert("host".into(), Value::Text("example.com".into()));
            template::render(connect, &ctx).map_err(|e| at(format!("connect: {e}")))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
id = "sample"
name = "Sample"
memory = { min_mb = 256, default_mb = 512 }
ports = [{ name = "game", protocol = "udp" }]
install = { kind = "shared", url = "https://example.com/game.zip" }
connect = "{host}:{port.game}"

[run]
command = ["{base}/server", "--port", "{port.game}", "--name", "{setting.name}"]
stop_signal = "SIGINT"

[[settings]]
key = "name"
label = "Server name"
type = "text"
default = "My server"

[[settings]]
key = "slots"
label = "Players"
type = "number"
default = 8
min = 1
max = 32

[[files]]
path = "config.json"
format = "json"
set = { port = "{port.game}", slots = "{setting.slots}" }
"#;

    #[test]
    fn parses_and_checks() {
        let game: GameDef = toml::from_str(SAMPLE).unwrap();
        game.validate().unwrap();
        assert_eq!(game.run.stop_timeout_secs, 30);
        let settings = game
            .settings_from(&BTreeMap::from([(
                "slots".into(),
                Value::Text("12".into()),
            )]))
            .unwrap();
        assert_eq!(settings["slots"], Value::Number(12));
        assert_eq!(settings["name"], Value::Text("My server".into()));
        assert!(
            game.settings_from(&BTreeMap::from([("slots".into(), Value::Number(99))]))
                .is_err()
        );
        assert!(
            game.settings_from(&BTreeMap::from([("nope".into(), Value::Number(1))]))
                .is_err()
        );

        let mut broken = game.clone();
        broken.run.command.push("{setting.missing}".into());
        assert!(broken.validate().is_err());
    }
}

#[cfg(test)]
mod shipped {
    use super::*;

    /// The definitions shipped in `games/` parse and hold together.
    #[test]
    fn shipped_games_are_valid() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../games");
        let mut count = 0;
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|e| e == "toml") {
                let text = std::fs::read_to_string(&path).unwrap();
                let game: GameDef =
                    toml::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
                game.validate()
                    .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
                count += 1;
            }
        }
        assert!(count >= 3);
    }
}
