//! SQLite persistence: users, sessions, servers, the desktop's settings. One mutex-guarded connection:
//! queries are tiny.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, bail};
use archipel_core::api::{Quota, Role, Value};
use parking_lot::Mutex;
use rusqlite::{Connection, OptionalExtension, params};

pub struct Store(Mutex<Connection>);

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS users (
    id INTEGER PRIMARY KEY,
    username TEXT NOT NULL UNIQUE,
    password_hash TEXT NOT NULL,
    role TEXT NOT NULL,
    memory_mb INTEGER NOT NULL,
    cpu_percent INTEGER NOT NULL,
    max_servers INTEGER NOT NULL,
    disk_mb INTEGER NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS sessions (
    token_hash TEXT PRIMARY KEY,
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    expires_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS servers (
    id INTEGER PRIMARY KEY,
    owner_id INTEGER NOT NULL REFERENCES users(id),
    name TEXT NOT NULL,
    game TEXT NOT NULL,
    memory_mb INTEGER NOT NULL,
    settings TEXT NOT NULL,
    ports TEXT NOT NULL,
    auto_start INTEGER NOT NULL DEFAULT 0,
    installed TEXT,
    created_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS settings (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
"#;

#[derive(Debug, Clone)]
pub struct UserRow {
    pub id: i64,
    pub username: String,
    pub password_hash: String,
    pub role: Role,
    pub quota: Quota,
    pub created_at: u64,
}

#[derive(Debug, Clone)]
pub struct ServerRow {
    pub id: u64,
    pub owner_id: i64,
    pub name: String,
    pub game: String,
    pub memory_mb: u64,
    pub settings: BTreeMap<String, Value>,
    pub ports: BTreeMap<String, u16>,
    pub auto_start: bool,
    /// What its files were installed from (`paper 26.3`), so a change of version
    /// reinstalls them.
    pub installed: Option<String>,
    pub created_at: u64,
}

/// What a server is created with.
#[derive(Clone, Copy)]
pub struct NewServer<'a> {
    pub owner_id: i64,
    pub name: &'a str,
    pub game: &'a str,
    pub memory_mb: u64,
    pub settings: &'a BTreeMap<String, Value>,
    pub port_names: &'a [String],
}

fn role_text(role: Role) -> &'static str {
    match role {
        Role::Admin => "admin",
        Role::User => "user",
    }
}

const USER_COLUMNS: &str =
    "id, username, password_hash, role, memory_mb, cpu_percent, max_servers, disk_mb, created_at";
const SERVER_COLUMNS: &str =
    "id, owner_id, name, game, memory_mb, settings, ports, auto_start, installed, created_at";

impl Store {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        let conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL; PRAGMA foreign_keys = ON; PRAGMA busy_timeout = 5000;",
        )?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self(Mutex::new(conn)))
    }

    // ---- users

    fn user_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<UserRow> {
        let role: String = r.get(3)?;
        Ok(UserRow {
            id: r.get(0)?,
            username: r.get(1)?,
            password_hash: r.get(2)?,
            role: if role == "admin" {
                Role::Admin
            } else {
                Role::User
            },
            quota: Quota {
                memory_mb: r.get(4)?,
                cpu_percent: r.get(5)?,
                max_servers: r.get(6)?,
                disk_mb: r.get(7)?,
            },
            created_at: r.get(8)?,
        })
    }

    pub fn count_users(&self) -> anyhow::Result<u64> {
        Ok(self
            .0
            .lock()
            .query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0))?)
    }

    pub fn create_user(
        &self,
        username: &str,
        password_hash: &str,
        role: Role,
        quota: &Quota,
    ) -> anyhow::Result<()> {
        let result = self.0.lock().execute(
            "INSERT INTO users (username, password_hash, role, memory_mb, cpu_percent, max_servers,
             disk_mb, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                username,
                password_hash,
                role_text(role),
                quota.memory_mb,
                quota.cpu_percent,
                quota.max_servers,
                quota.disk_mb,
                archipel_core::now()
            ],
        );
        match result {
            Ok(_) => Ok(()),
            Err(rusqlite::Error::SqliteFailure(e, _))
                if e.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                bail!("there is already a user named {username}")
            }
            Err(e) => Err(e.into()),
        }
    }

    pub fn list_users(&self) -> anyhow::Result<Vec<UserRow>> {
        let conn = self.0.lock();
        let mut stmt = conn.prepare(&format!(
            "SELECT {USER_COLUMNS} FROM users ORDER BY username"
        ))?;
        let rows = stmt
            .query_map([], Self::user_row)?
            .collect::<Result<_, _>>()?;
        Ok(rows)
    }

    pub fn user_by_name(&self, username: &str) -> anyhow::Result<Option<UserRow>> {
        Ok(self
            .0
            .lock()
            .query_row(
                &format!("SELECT {USER_COLUMNS} FROM users WHERE username = ?1"),
                [username],
                Self::user_row,
            )
            .optional()?)
    }

    pub fn user_by_id(&self, id: i64) -> anyhow::Result<Option<UserRow>> {
        Ok(self
            .0
            .lock()
            .query_row(
                &format!("SELECT {USER_COLUMNS} FROM users WHERE id = ?1"),
                [id],
                Self::user_row,
            )
            .optional()?)
    }

    pub fn update_user(&self, user: &UserRow) -> anyhow::Result<()> {
        self.0.lock().execute(
            "UPDATE users SET password_hash = ?2, role = ?3, memory_mb = ?4, cpu_percent = ?5,
             max_servers = ?6, disk_mb = ?7 WHERE id = ?1",
            params![
                user.id,
                user.password_hash,
                role_text(user.role),
                user.quota.memory_mb,
                user.quota.cpu_percent,
                user.quota.max_servers,
                user.quota.disk_mb
            ],
        )?;
        Ok(())
    }

    pub fn delete_user(&self, id: i64) -> anyhow::Result<()> {
        self.0
            .lock()
            .execute("DELETE FROM users WHERE id = ?1", [id])?;
        Ok(())
    }

    // ---- sessions

    pub fn create_session(
        &self,
        token_hash: &str,
        user_id: i64,
        expires_at: u64,
    ) -> anyhow::Result<()> {
        let conn = self.0.lock();
        conn.execute(
            "DELETE FROM sessions WHERE expires_at < ?1",
            [archipel_core::now()],
        )?;
        conn.execute(
            "INSERT INTO sessions (token_hash, user_id, expires_at) VALUES (?1, ?2, ?3)",
            params![token_hash, user_id, expires_at],
        )?;
        Ok(())
    }

    pub fn session_user(&self, token_hash: &str) -> anyhow::Result<Option<UserRow>> {
        Ok(self
            .0
            .lock()
            .query_row(
                &format!(
                    "SELECT {} FROM sessions s JOIN users u ON u.id = s.user_id
                     WHERE s.token_hash = ?1 AND s.expires_at > ?2",
                    USER_COLUMNS
                        .split(", ")
                        .map(|c| format!("u.{c}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                params![token_hash, archipel_core::now()],
                Self::user_row,
            )
            .optional()?)
    }

    pub fn delete_session(&self, token_hash: &str) -> anyhow::Result<()> {
        self.0
            .lock()
            .execute("DELETE FROM sessions WHERE token_hash = ?1", [token_hash])?;
        Ok(())
    }

    /// Everywhere a user is logged in, but `keep`.
    pub fn delete_sessions_of(&self, user_id: i64, keep: Option<&str>) -> anyhow::Result<()> {
        self.0.lock().execute(
            "DELETE FROM sessions WHERE user_id = ?1 AND token_hash IS NOT ?2",
            params![user_id, keep],
        )?;
        Ok(())
    }

    // ---- servers

    fn server_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<ServerRow> {
        let settings: String = r.get(5)?;
        let ports: String = r.get(6)?;
        Ok(ServerRow {
            id: r.get(0)?,
            owner_id: r.get(1)?,
            name: r.get(2)?,
            game: r.get(3)?,
            memory_mb: r.get(4)?,
            settings: serde_json::from_str(&settings).unwrap_or_default(),
            ports: serde_json::from_str(&ports).unwrap_or_default(),
            auto_start: r.get(7)?,
            installed: r.get(8)?,
            created_at: r.get(9)?,
        })
    }

    /// Stores a new server, giving it ports from the pool `first..=last` (one
    /// block of consecutive ports, one per name), and returns it.
    pub fn create_server(
        &self,
        new: &NewServer<'_>,
        (first, last): (u16, u16),
    ) -> anyhow::Result<ServerRow> {
        let NewServer {
            owner_id,
            name,
            game,
            memory_mb,
            settings,
            port_names,
        } = *new;
        let mut conn = self.0.lock();
        let tx = conn.transaction()?;
        let used: Vec<u16> = {
            let mut stmt = tx.prepare("SELECT ports FROM servers")?;
            stmt.query_map([], |r| r.get::<_, String>(0))?
                .flatten()
                .flat_map(|p| {
                    serde_json::from_str::<BTreeMap<String, u16>>(&p)
                        .unwrap_or_default()
                        .into_values()
                })
                .collect()
        };
        let needed = port_names.len() as u32;
        let start = (u32::from(first)..=u32::from(last) + 1 - needed.max(1))
            .find(|&p| (p..p + needed).all(|q| !used.contains(&(q as u16))));
        let ports: BTreeMap<String, u16> = match (needed, start) {
            (0, _) => BTreeMap::new(),
            (_, Some(start)) => port_names
                .iter()
                .enumerate()
                .map(|(i, n)| (n.clone(), (start + i as u32) as u16))
                .collect(),
            (_, None) => bail!(
                "no {needed} free consecutive ports left in {first}-{last}: delete a server, or \
                 ask the admin for a larger pool"
            ),
        };
        tx.execute(
            "INSERT INTO servers (owner_id, name, game, memory_mb, settings, ports, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                owner_id,
                name,
                game,
                memory_mb,
                serde_json::to_string(settings)?,
                serde_json::to_string(&ports)?,
                archipel_core::now()
            ],
        )?;
        let id = tx.last_insert_rowid();
        let row = tx.query_row(
            &format!("SELECT {SERVER_COLUMNS} FROM servers WHERE id = ?1"),
            [id],
            Self::server_row,
        )?;
        tx.commit()?;
        Ok(row)
    }

    pub fn get_server(&self, id: u64) -> anyhow::Result<Option<ServerRow>> {
        Ok(self
            .0
            .lock()
            .query_row(
                &format!("SELECT {SERVER_COLUMNS} FROM servers WHERE id = ?1"),
                [id],
                Self::server_row,
            )
            .optional()?)
    }

    pub fn list_servers(&self) -> anyhow::Result<Vec<ServerRow>> {
        let conn = self.0.lock();
        let mut stmt =
            conn.prepare(&format!("SELECT {SERVER_COLUMNS} FROM servers ORDER BY id"))?;
        let rows = stmt
            .query_map([], Self::server_row)?
            .collect::<Result<_, _>>()?;
        Ok(rows)
    }

    pub fn update_server(&self, s: &ServerRow) -> anyhow::Result<()> {
        self.0.lock().execute(
            "UPDATE servers SET name = ?2, memory_mb = ?3, settings = ?4, auto_start = ?5,
             installed = ?6 WHERE id = ?1",
            params![
                s.id,
                s.name,
                s.memory_mb,
                serde_json::to_string(&s.settings)?,
                s.auto_start,
                s.installed
            ],
        )?;
        Ok(())
    }

    pub fn set_auto_start(&self, id: u64, on: bool) -> anyhow::Result<()> {
        self.0.lock().execute(
            "UPDATE servers SET auto_start = ?2 WHERE id = ?1",
            params![id, on],
        )?;
        Ok(())
    }

    pub fn set_installed(&self, id: u64, installed: Option<&str>) -> anyhow::Result<()> {
        self.0.lock().execute(
            "UPDATE servers SET installed = ?2 WHERE id = ?1",
            params![id, installed],
        )?;
        Ok(())
    }

    pub fn delete_server(&self, id: u64) -> anyhow::Result<()> {
        self.0
            .lock()
            .execute("DELETE FROM servers WHERE id = ?1", [id])?;
        Ok(())
    }

    // ---- settings

    /// A setting, if set.
    pub fn setting(&self, key: &str) -> anyhow::Result<Option<String>> {
        Ok(self
            .0
            .lock()
            .query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| {
                r.get(0)
            })
            .optional()?)
    }

    /// Sets a setting, or unsets it (`None`): back to its default.
    pub fn set_setting(&self, key: &str, value: Option<&str>) -> anyhow::Result<()> {
        let conn = self.0.lock();
        match value {
            Some(value) => conn.execute(
                "INSERT INTO settings (key, value) VALUES (?1, ?2) \
                 ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                params![key, value],
            )?,
            None => conn.execute("DELETE FROM settings WHERE key = ?1", [key])?,
        };
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server<'a>(
        owner_id: i64,
        name: &'a str,
        game: &'a str,
        settings: &'a BTreeMap<String, Value>,
        port_names: &'a [String],
    ) -> NewServer<'a> {
        NewServer {
            owner_id,
            name,
            game,
            memory_mb: 1024,
            settings,
            port_names,
        }
    }

    #[test]
    fn hands_out_consecutive_ports() {
        let dir = std::env::temp_dir().join(format!("archipel-store-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(&dir.join("test.db")).unwrap();
        store
            .create_user("alice", "x", Role::User, &Quota::default())
            .unwrap();
        let alice = store.user_by_name("alice").unwrap().unwrap();
        let names = |n: &[&str]| n.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let pool = (30000, 30004);
        let empty = BTreeMap::new();
        let (one, two) = (names(&["game"]), names(&["game", "ping"]));
        let new =
            |owner_id, name, game, port_names| server(owner_id, name, game, &empty, port_names);
        let a = store
            .create_server(&new(alice.id, "a", "minecraft", &one), pool)
            .unwrap();
        assert_eq!(a.ports["game"], 30000);
        let b = store
            .create_server(&new(alice.id, "b", "puck", &two), pool)
            .unwrap();
        assert_eq!((b.ports["game"], b.ports["ping"]), (30001, 30002));
        store.delete_server(a.id).unwrap();
        // The freed port is reused; a pair needs two in a row.
        let c = store
            .create_server(&new(alice.id, "c", "puck", &two), pool)
            .unwrap();
        assert_eq!(c.ports["game"], 30003);
        assert!(
            store
                .create_server(&new(alice.id, "d", "puck", &two), pool)
                .is_err()
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn keeps_settings_until_unset() {
        let dir = std::env::temp_dir().join(format!("archipel-settings-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(&dir.join("test.db")).unwrap();
        assert_eq!(store.setting("public_host").unwrap(), None);
        store
            .set_setting("public_host", Some("a.ddns.net"))
            .unwrap();
        store
            .set_setting("public_host", Some("b.ddns.net"))
            .unwrap();
        assert_eq!(
            store.setting("public_host").unwrap().as_deref(),
            Some("b.ddns.net")
        );
        store.set_setting("probe_url", Some("")).unwrap();
        assert_eq!(store.setting("probe_url").unwrap().as_deref(), Some(""));
        store.set_setting("public_host", None).unwrap();
        assert_eq!(store.setting("public_host").unwrap(), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
