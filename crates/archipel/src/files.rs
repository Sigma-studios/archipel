//! A game's files, written in a server's directory before each start from its
//! settings, and the ones its owner may edit.

use std::path::Path;

use anyhow::Context;
use archipel_core::api::FileView;
use archipel_core::game::{FileDef, FileFormat, GameDef};
use archipel_core::template::{self, Context as Ctx, Value};

use crate::fsops;

/// Writes every file of the game, as the server's user.
pub async fn write_all(game: &GameDef, dir: &Path, uid: u32, ctx: &Ctx) -> anyhow::Result<()> {
    for file in &game.files {
        let rendered = render(file, ctx)?;
        let path = fsops::inside(dir, &file.path)?;
        let name = file.path.clone();
        let file = file.clone();
        fsops::as_user(uid, move || {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let current = std::fs::read_to_string(&path).ok();
            let out = apply(&file, current.as_deref(), &rendered)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
            if current.as_deref() != Some(out.as_str()) {
                std::fs::write(&path, out)?;
            }
            Ok(())
        })
        .await
        .with_context(|| format!("writing {name}"))?;
    }
    Ok(())
}

/// What a file's templates give for this server.
enum Rendered {
    Content(String),
    Keys(Vec<(String, Value)>),
    Nothing,
}

fn render(file: &FileDef, ctx: &Ctx) -> anyhow::Result<Rendered> {
    let at = |e: template::TemplateError| anyhow::anyhow!("{}: {e}", file.path);
    Ok(match (&file.content, file.set.is_empty()) {
        (Some(content), _) => Rendered::Content(template::render(content, ctx).map_err(at)?),
        (None, false) => Rendered::Keys(
            file.set
                .iter()
                .map(|(k, t)| Ok((k.clone(), template::render_value(t, ctx).map_err(at)?)))
                .collect::<anyhow::Result<_>>()?,
        ),
        (None, true) => Rendered::Nothing,
    })
}

/// The file's new content, from what it holds now (if it exists).
fn apply(file: &FileDef, current: Option<&str>, rendered: &Rendered) -> Result<String, String> {
    let base = current.or(file.initial.as_deref()).unwrap_or_default();
    match rendered {
        Rendered::Content(content) => Ok(content.clone()),
        Rendered::Nothing => Ok(base.to_string()),
        Rendered::Keys(keys) => match file.format {
            FileFormat::Properties => Ok(set_properties(base, keys)),
            FileFormat::Json => set_json(base, keys),
            FileFormat::Text => Ok(base.to_string()),
        },
    }
}

/// `key=value` lines: the keys given are replaced in place (or added), the rest
/// and comments kept.
fn set_properties(text: &str, keys: &[(String, Value)]) -> String {
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    for (key, value) in keys {
        let line = format!("{key}={}", escape_property(&value.as_text()));
        let existing = lines.iter().position(|l| {
            let l = l.trim_start();
            !l.starts_with('#')
                && !l.starts_with('!')
                && l.split_once(['=', ':'])
                    .is_some_and(|(k, _)| k.trim() == key)
        });
        match existing {
            Some(i) => lines[i] = line,
            None => lines.push(line),
        }
    }
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

fn escape_property(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c if (c as u32) > 0x7e => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

fn set_json(text: &str, keys: &[(String, Value)]) -> Result<String, String> {
    let mut doc: serde_json::Value = if text.trim().is_empty() {
        serde_json::Value::Object(Default::default())
    } else {
        serde_json::from_str(text).map_err(|e| format!("not valid JSON: {e}"))?
    };
    let obj = doc.as_object_mut().ok_or("the file is not a JSON object")?;
    for (key, value) in keys {
        obj.insert(
            key.clone(),
            match value {
                Value::Bool(b) => serde_json::Value::Bool(*b),
                Value::Number(n) => serde_json::Value::from(*n),
                Value::Text(t) => serde_json::Value::String(t.clone()),
            },
        );
    }
    let mut out = serde_json::to_string_pretty(&doc).map_err(|e| e.to_string())?;
    out.push('\n');
    Ok(out)
}

/// The files the server's owner may edit, as they are now.
pub async fn editable(game: &GameDef, dir: &Path, uid: u32) -> anyhow::Result<Vec<FileView>> {
    let mut out = Vec::new();
    for file in game.files.iter().filter(|f| f.editable) {
        let path = fsops::inside(dir, &file.path)?;
        let content = fsops::as_user(uid, move || match std::fs::read_to_string(&path) {
            Ok(c) => Ok(c),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
            Err(e) => Err(e),
        })
        .await?;
        out.push(FileView {
            path: file.path.clone(),
            label: if file.label.is_empty() {
                file.path.clone()
            } else {
                file.label.clone()
            },
            content: if content.is_empty() {
                file.initial.clone().unwrap_or_default()
            } else {
                content
            },
            managed_keys: file.set.keys().cloned().collect(),
        });
    }
    Ok(out)
}

/// Saves one of the editable files.
pub async fn save(
    game: &GameDef,
    dir: &Path,
    uid: u32,
    path: &str,
    content: String,
) -> anyhow::Result<()> {
    let file = game
        .files
        .iter()
        .find(|f| f.editable && f.path == path)
        .with_context(|| format!("{path} is not a file you can edit"))?;
    if file.format == FileFormat::Json {
        serde_json::from_str::<serde_json::Value>(&content)
            .map_err(|e| anyhow::anyhow!("that is not valid JSON: {e}"))?;
    }
    if content.len() > 1024 * 1024 {
        anyhow::bail!("that file is too large to edit here");
    }
    let target = fsops::inside(dir, &file.path)?;
    fsops::as_user(uid, move || {
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&target, content)
    })
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sets_properties_in_place() {
        let text = "#Minecraft server properties\nmotd=old\nserver-port = 1\nlevel-name=world\n";
        let out = set_properties(
            text,
            &[
                ("motd".into(), Value::Text("Hé".into())),
                ("server-port".into(), Value::Number(25565)),
                ("pvp".into(), Value::Bool(true)),
            ],
        );
        assert_eq!(
            out,
            "#Minecraft server properties\nmotd=H\\u00e9\nserver-port=25565\nlevel-name=world\npvp=true\n"
        );
    }

    #[test]
    fn sets_json_keys_with_their_types() {
        let out = set_json(
            "{\"port\": 1, \"name\": \"x\", \"mods\": []}",
            &[
                ("port".into(), Value::Number(30001)),
                ("isPublic".into(), Value::Bool(false)),
            ],
        )
        .unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["port"], 30001);
        assert_eq!(v["isPublic"], false);
        assert_eq!(v["name"], "x");
        // Key order is kept.
        assert!(out.find("\"port\"").unwrap() < out.find("\"mods\"").unwrap());
        assert!(set_json("[1]", &[]).is_err());
    }
}
