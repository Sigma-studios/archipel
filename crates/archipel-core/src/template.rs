//! Placeholders in game definitions: `{port.game}`, `{setting.motd}`, `{memory_mb}`.
//! `{{` and `}}` stand for braces.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// A value a placeholder stands for. Settings keep their type, so a JSON file
/// gets `"maxPlayers": 10` rather than `"10"`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Value {
    Bool(bool),
    Number(i64),
    Text(String),
}

impl Value {
    pub fn as_text(&self) -> String {
        match self {
            Value::Bool(b) => b.to_string(),
            Value::Number(n) => n.to_string(),
            Value::Text(s) => s.clone(),
        }
    }

    pub fn as_bool(&self) -> bool {
        match self {
            Value::Bool(b) => *b,
            Value::Number(n) => *n != 0,
            Value::Text(s) => matches!(s.as_str(), "true" | "yes" | "1" | "on"),
        }
    }
}

/// What placeholders stand for, by name (`port.game`).
pub type Context = BTreeMap<String, Value>;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TemplateError {
    #[error("unknown placeholder {{{0}}}")]
    Unknown(String),
    #[error("unclosed {{ in `{0}`")]
    Unclosed(String),
}

/// The placeholders a template uses.
pub fn placeholders(template: &str) -> Result<Vec<String>, TemplateError> {
    let mut out = Vec::new();
    walk(template, |piece| {
        if let Piece::Placeholder(name) = piece {
            out.push(name.to_string());
        }
        Ok(())
    })?;
    Ok(out)
}

enum Piece<'a> {
    Text(&'a str),
    Brace(char),
    Placeholder(&'a str),
}

fn walk<'a>(
    template: &'a str,
    mut each: impl FnMut(Piece<'a>) -> Result<(), TemplateError>,
) -> Result<(), TemplateError> {
    let mut rest = template;
    while let Some(i) = rest.find(['{', '}']) {
        each(Piece::Text(&rest[..i]))?;
        let tail = &rest[i..];
        if let Some(after) = tail.strip_prefix("{{") {
            each(Piece::Brace('{'))?;
            rest = after;
        } else if let Some(after) = tail.strip_prefix("}}") {
            each(Piece::Brace('}'))?;
            rest = after;
        } else if let Some(after) = tail.strip_prefix('}') {
            each(Piece::Brace('}'))?;
            rest = after;
        } else {
            let end = tail
                .find('}')
                .ok_or_else(|| TemplateError::Unclosed(template.to_string()))?;
            each(Piece::Placeholder(tail[1..end].trim()))?;
            rest = &tail[end + 1..];
        }
    }
    each(Piece::Text(rest))
}

/// The template with its placeholders replaced.
pub fn render(template: &str, context: &Context) -> Result<String, TemplateError> {
    let mut out = String::with_capacity(template.len());
    walk(template, |piece| {
        match piece {
            Piece::Text(t) => out.push_str(t),
            Piece::Brace(c) => out.push(c),
            Piece::Placeholder(name) => {
                let value = context
                    .get(name)
                    .ok_or_else(|| TemplateError::Unknown(name.to_string()))?;
                out.push_str(&value.as_text());
            }
        }
        Ok(())
    })?;
    Ok(out)
}

/// Like [`render`], but a template that is one placeholder and nothing else keeps
/// the placeholder's type.
pub fn render_value(template: &str, context: &Context) -> Result<Value, TemplateError> {
    let trimmed = template.trim();
    if let Some(name) = trimmed
        .strip_prefix('{')
        .and_then(|t| t.strip_suffix('}'))
        .filter(|n| !n.contains(['{', '}']))
    {
        return context
            .get(name.trim())
            .cloned()
            .ok_or_else(|| TemplateError::Unknown(name.trim().to_string()));
    }
    render(template, context).map(Value::Text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> Context {
        Context::from([
            ("port.game".into(), Value::Number(25565)),
            ("setting.motd".into(), Value::Text("Hi".into())),
            ("setting.pvp".into(), Value::Bool(true)),
        ])
    }

    #[test]
    fn renders() {
        assert_eq!(
            render("{setting.motd} on {port.game} {{x}}", &ctx()).unwrap(),
            "Hi on 25565 {x}"
        );
        assert_eq!(
            render("{nope}", &ctx()),
            Err(TemplateError::Unknown("nope".into()))
        );
        assert!(render("{open", &ctx()).is_err());
        assert_eq!(
            render_value("{port.game}", &ctx()).unwrap(),
            Value::Number(25565)
        );
        assert_eq!(
            render_value("{setting.pvp}", &ctx()).unwrap(),
            Value::Bool(true)
        );
        assert_eq!(
            render_value("port {port.game}", &ctx()).unwrap(),
            Value::Text("port 25565".into())
        );
        assert_eq!(
            placeholders("{a} {{b}} {c.d}").unwrap(),
            ["a".to_string(), "c.d".to_string()]
        );
    }
}
