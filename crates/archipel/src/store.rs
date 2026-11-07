//! SQLite persistence: users, sessions, servers, the desktop's openings and settings. One mutex-guarded connection:
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
CREATE TABLE IF NOT EXISTS openings (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    ports TEXT NOT NULL,
    custom INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    preset TEXT,
    target INTEGER
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

/// Ports the desktop opens on the router for a game hosted from the game itself
/// (a mod's multiplayer, a game's own "host" button): no server, only ports.
#[derive(Debug, Clone)]
pub struct OpeningRow {
    pub id: u64,
    pub name: String,
    pub ports: Vec<u16>,
    /// A port outside the pool forwarded as it is (made before relays): this
    /// computer's firewall must let it in, and the check from outside does not
    /// stand for it.
    pub custom: bool,
    pub created_at: u64,
    /// The hosted game it was made for (`dsp-nebula`), if picked from the list.
    pub preset: Option<String>,
    /// The game's own port on this computer, outside the pool, that its port
    /// passes connections on to (see [`crate::relay`]).
    pub target: Option<u16>,
}

/// What an opening is created with.
#[derive(Debug, Clone, Copy)]
pub enum Wanted {
    /// This many consecutive ports from the pool.
    Pool(u16),
    /// The game's own port: itself if it is in the pool, otherwise a port of the
    /// pool that passes connections on to it.
    Port(u16),
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
        // Openings made before presets, or relays, have no column for them.
        for (column, kind) in [("preset", "TEXT"), ("target", "INTEGER")] {
            let has = conn
                .prepare("SELECT 1 FROM pragma_table_info('openings') WHERE name = ?1")?
                .exists([column])?;
            if !has {
                conn.execute_batch(&format!("ALTER TABLE openings ADD COLUMN {column} {kind}"))?;
            }
        }
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
        let used = used_ports(&tx)?;
        let needed = port_names.len() as u32;
        let start = free_block(&used, (first, last), needed);
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

    /// Every port a server or an opening has.
    pub fn used_ports(&self) -> anyhow::Result<std::collections::HashSet<u16>> {
        used_ports(&self.0.lock())
    }

    // ---- openings

    fn opening_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<OpeningRow> {
        let ports: String = r.get(2)?;
        Ok(OpeningRow {
            id: r.get(0)?,
            name: r.get(1)?,
            ports: serde_json::from_str(&ports).unwrap_or_default(),
            custom: r.get(3)?,
            created_at: r.get(4)?,
            preset: r.get(5)?,
            target: r.get(6)?,
        })
    }

    /// Stores a new opening with the ports it wants: consecutive ones from the
    /// pool `first..=last`; or the game's own, if in the pool and no server or
    /// opening has it, or else one of the pool to relay to it.
    pub fn create_opening(
        &self,
        name: &str,
        preset: Option<&str>,
        wanted: Wanted,
        (first, last): (u16, u16),
    ) -> anyhow::Result<OpeningRow> {
        let mut conn = self.0.lock();
        let tx = conn.transaction()?;
        let used = used_ports(&tx)?;
        let block = |n: u16| match free_block(&used, (first, last), n.into()) {
            Some(start) => Ok((start..start + u32::from(n)).map(|p| p as u16).collect()),
            None => Err(anyhow::anyhow!(
                "no {n} free consecutive ports left in {first}-{last}: delete a server or \
                 an opening"
            )),
        };
        let relayed: Vec<u16> = {
            let mut stmt = tx.prepare("SELECT target FROM openings WHERE target IS NOT NULL")?;
            stmt.query_map([], |r| r.get(0))?.flatten().collect()
        };
        let (ports, target): (Vec<u16>, Option<u16>) = match wanted {
            Wanted::Pool(n) => (block(n)?, None),
            Wanted::Port(port) if used.contains(&port) || relayed.contains(&port) => {
                bail!("port {port} is already a server's or an opening's")
            }
            Wanted::Port(port) if (first..=last).contains(&port) => (vec![port], None),
            Wanted::Port(port) => (block(1)?, Some(port)),
        };
        tx.execute(
            "INSERT INTO openings (name, ports, custom, created_at, preset, target) \
             VALUES (?1, ?2, 0, ?3, ?4, ?5)",
            params![
                name,
                serde_json::to_string(&ports)?,
                archipel_core::now(),
                preset,
                target
            ],
        )?;
        let id = tx.last_insert_rowid();
        let row = tx.query_row(
            "SELECT id, name, ports, custom, created_at, preset, target FROM openings WHERE id = ?1",
            [id],
            Self::opening_row,
        )?;
        tx.commit()?;
        Ok(row)
    }

    pub fn get_opening(&self, id: u64) -> anyhow::Result<Option<OpeningRow>> {
        Ok(self
            .0
            .lock()
            .query_row(
                "SELECT id, name, ports, custom, created_at, preset, target FROM openings WHERE id = ?1",
                [id],
                Self::opening_row,
            )
            .optional()?)
    }

    pub fn list_openings(&self) -> anyhow::Result<Vec<OpeningRow>> {
        let conn = self.0.lock();
        let mut stmt = conn.prepare(
            "SELECT id, name, ports, custom, created_at, preset, target FROM openings ORDER BY id",
        )?;
        let rows = stmt
            .query_map([], Self::opening_row)?
            .collect::<Result<_, _>>()?;
        Ok(rows)
    }

    pub fn delete_opening(&self, id: u64) -> anyhow::Result<()> {
        self.0
            .lock()
            .execute("DELETE FROM openings WHERE id = ?1", [id])?;
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

/// Every port a server or an opening has.
fn used_ports(conn: &Connection) -> anyhow::Result<std::collections::HashSet<u16>> {
    let mut used = std::collections::HashSet::new();
    let mut stmt = conn.prepare("SELECT ports FROM servers")?;
    for ports in stmt.query_map([], |r| r.get::<_, String>(0))?.flatten() {
        used.extend(
            serde_json::from_str::<BTreeMap<String, u16>>(&ports)
                .unwrap_or_default()
                .into_values(),
        );
    }
    let mut stmt = conn.prepare("SELECT ports FROM openings")?;
    for ports in stmt.query_map([], |r| r.get::<_, String>(0))?.flatten() {
        used.extend(serde_json::from_str::<Vec<u16>>(&ports).unwrap_or_default());
    }
    Ok(used)
}

/// The first of `needed` consecutive ports of the pool none of which is used.
fn free_block(
    used: &std::collections::HashSet<u16>,
    (first, last): (u16, u16),
    needed: u32,
) -> Option<u32> {
    (u32::from(first)..=(u32::from(last) + 1).checked_sub(needed.max(1))?)
        .find(|&p| (p..p + needed).all(|q| !used.contains(&(q as u16))))
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
    fn openings_and_servers_share_the_pool() {
        let dir = std::env::temp_dir().join(format!("archipel-openings-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(&dir.join("test.db")).unwrap();
        store
            .create_user("me", "x", Role::User, &Quota::default())
            .unwrap();
        let me = store.user_by_name("me").unwrap().unwrap();
        let pool = (30000, 30003);
        let game = vec!["game".to_string()];
        let empty = BTreeMap::new();
        let a = store
            .create_server(&server(me.id, "a", "minecraft", &empty, &game), pool)
            .unwrap();
        assert_eq!(a.ports["game"], 30000);
        let dsp = store
            .create_opening("dsp", Some("dsp-nebula"), Wanted::Pool(2), pool)
            .unwrap();
        assert_eq!((dsp.ports.clone(), dsp.target), (vec![30001, 30002], None));
        assert_eq!(dsp.preset.as_deref(), Some("dsp-nebula"));
        // A server does not get an opening's port.
        let b = store
            .create_server(&server(me.id, "b", "minecraft", &empty, &game), pool)
            .unwrap();
        assert_eq!(b.ports["game"], 30003);
        assert!(
            store
                .create_opening("more", None, Wanted::Pool(1), pool)
                .is_err()
        );
        assert!(
            store
                .create_opening("taken", None, Wanted::Port(30000), pool)
                .is_err()
        );
        store.delete_opening(dsp.id).unwrap();
        assert!(!store.used_ports().unwrap().contains(&30001));
        // The game's own port, in the pool: itself.
        let own = store
            .create_opening("own", None, Wanted::Port(30002), pool)
            .unwrap();
        assert_eq!((own.ports.clone(), own.target), (vec![30002], None));
        // Outside it: a port of the pool, relayed to it; once.
        let nebula = store
            .create_opening("nebula", None, Wanted::Port(8469), pool)
            .unwrap();
        assert_eq!(
            (nebula.ports.clone(), nebula.target),
            (vec![30001], Some(8469))
        );
        assert!(!nebula.custom);
        assert!(
            store
                .create_opening("again", None, Wanted::Port(8469), pool)
                .is_err()
        );
        assert_eq!(store.list_openings().unwrap().len(), 2);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn adds_the_preset_column_to_older_openings() {
        let dir = std::env::temp_dir().join(format!("archipel-migrate-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.db");
        Connection::open(&path)
            .unwrap()
            .execute_batch(
                "CREATE TABLE openings (id INTEGER PRIMARY KEY, name TEXT NOT NULL, \
                 ports TEXT NOT NULL, custom INTEGER NOT NULL, created_at INTEGER NOT NULL);
                 INSERT INTO openings (name, ports, custom, created_at) \
                 VALUES ('old', '[30000]', 0, 1);",
            )
            .unwrap();
        let store = Store::open(&path).unwrap();
        let old = &store.list_openings().unwrap()[0];
        assert_eq!(
            (old.name.as_str(), old.preset.clone(), old.target),
            ("old", None, None)
        );
        // And again, once it is there.
        drop(store);
        Store::open(&path).unwrap();
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
