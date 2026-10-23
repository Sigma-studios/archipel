//! HTTP API bodies, shared by the server and the web UI.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub use crate::template::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// Manages users and every server.
    Admin,
    User,
}

/// A user's share of the machine archipel runs on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Quota {
    /// Memory all their running servers may take together.
    pub memory_mb: u64,
    /// CPU all their running servers may use together: 100 is one core.
    pub cpu_percent: u32,
    /// Servers they may have, running or not.
    pub max_servers: u32,
    /// Disk their servers' files may take.
    pub disk_mb: u64,
}

impl Default for Quota {
    fn default() -> Self {
        Quota {
            memory_mb: 4096,
            cpu_percent: 200,
            max_servers: 3,
            disk_mb: 20 * 1024,
        }
    }
}

/// What a user's servers take now.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    /// Memory reserved by their running servers.
    pub memory_mb: u64,
    pub servers: u32,
    pub running: u32,
    pub disk_mb: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UserView {
    pub username: String,
    pub role: Role,
    pub quota: Quota,
    pub usage: Usage,
    pub created_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoginResponse {
    pub token: String,
    pub user: UserView,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateUser {
    pub username: String,
    pub password: String,
    pub role: Role,
    pub quota: Quota,
}

/// Fields left out stay as they are.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UpdateUser {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<Role>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quota: Option<Quota>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangePassword {
    pub current: String,
    pub new: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ServerState {
    Stopped,
    /// Downloading or updating its files.
    Installing,
    /// Started, not ready yet.
    Starting,
    Running,
    Stopping,
    /// Exited on its own, or failed to start or install.
    Crashed,
}

impl ServerState {
    /// Holds memory: counts against quotas.
    pub fn is_active(self) -> bool {
        matches!(
            self,
            ServerState::Installing
                | ServerState::Starting
                | ServerState::Running
                | ServerState::Stopping
        )
    }
}

impl std::fmt::Display for ServerState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            ServerState::Stopped => "stopped",
            ServerState::Installing => "installing",
            ServerState::Starting => "starting",
            ServerState::Running => "running",
            ServerState::Stopping => "stopping",
            ServerState::Crashed => "crashed",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServerView {
    pub id: u64,
    pub name: String,
    pub owner: String,
    /// Its game's id, and name.
    pub game: String,
    pub game_name: String,
    pub memory_mb: u64,
    pub settings: BTreeMap<String, Value>,
    /// Port by name (`game`), the same inside and outside.
    pub ports: BTreeMap<String, u16>,
    pub state: ServerState,
    /// Why it crashed, what it is installing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<u64>,
    /// Measured while it runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<ServerUsage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk_bytes: Option<u64>,
    /// How players connect, with `{host}` left for the page to fill in with the
    /// address it is browsed at, unless archipel knows its public address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connect: Option<String>,
    /// Started again when archipel starts (it was running).
    pub auto_start: bool,
    pub created_at: u64,
    /// Whether players outside can reach it (the desktop app, which opens its
    /// ports on the home router).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reach: Option<Reach>,
}

/// How archipel runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// On a server (facade): accounts, quotas, limits, servers restarted on boot.
    Hosted,
    /// The desktop app: one owner, no login, no limits, ports opened on the
    /// home router.
    Desktop,
}

/// `GET /api/about`, asked before logging in.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct About {
    pub mode: Mode,
    pub version: String,
}

/// Whether this computer can receive games from the internet (the desktop app):
/// checked when it opens by asking the router for a test forwarding, and again
/// as servers start.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Internet {
    pub state: ReachState,
    /// This computer's address on the home network.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_ip: Option<String>,
    /// Where the internet sees it, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_ip: Option<String>,
    /// The port pool, to forward by hand when the router will not: `30000-30019`.
    pub ports: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// A firewall on this computer that may turn players away, and how to open
    /// the ports in it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub firewall: Option<String>,
}

/// Whether a running server can be reached from the internet.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Reach {
    pub state: ReachState,
    /// How players at home connect: `192.168.1.20:30000`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local: Option<String>,
    /// How players anywhere connect, when they can: `86.245.1.2:30000`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public: Option<String>,
    /// The ports to forward by hand on the router, when it would not do it:
    /// `UDP 30000`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub forward: Vec<String>,
    /// What went wrong, in a few words: "the router has UPnP turned off".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// As [`Internet::firewall`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub firewall: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReachState {
    /// Asking the router.
    Checking,
    /// Friends can join from anywhere.
    Internet,
    /// The router did not open the ports: only the home network can join.
    LocalOnly,
    /// The connection has no public address of its own (carrier-grade NAT):
    /// only the home network can join, whatever the router does.
    NoPublicAddress,
}

/// `GET /api/servers/{id}/console/lines?after=N&wait=S`: the console's lines after
/// line `after`, waiting up to `wait` seconds for some.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConsoleLines {
    pub lines: Vec<String>,
    /// What to ask `after` next time.
    pub next: u64,
    /// The lines asked for are gone (too old, or the console restarted): these
    /// replace what was shown.
    pub reset: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServerUsage {
    pub memory_bytes: u64,
    /// 100 is one core.
    pub cpu_percent: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateServer {
    pub name: String,
    pub game: String,
    pub memory_mb: u64,
    #[serde(default)]
    pub settings: BTreeMap<String, Value>,
    /// Start it right away.
    #[serde(default)]
    pub start: bool,
    /// Its owner, for admins creating a server for someone else.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
}

/// Fields left out stay as they are. Changes apply at the next start.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UpdateServer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_mb: Option<u64>,
    /// Merged over the current ones.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settings: Option<BTreeMap<String, Value>>,
}

/// A line typed into a server's console.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandRequest {
    pub line: String,
}

/// One of the files a server's owner may edit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileView {
    pub path: String,
    pub label: String,
    /// Empty when the file does not exist yet.
    pub content: String,
    /// Keys archipel sets at each start: editing them does nothing.
    pub managed_keys: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SaveFile {
    pub content: String,
}

/// The machine, as archipel sees it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Overview {
    /// What archipel may give its servers: its container's limits, less what it
    /// keeps for itself.
    pub memory_mb: Option<u64>,
    pub cpu_percent: Option<u32>,
    /// Memory reserved by running servers.
    pub memory_used_mb: u64,
    pub disk_total_bytes: Option<u64>,
    pub disk_used_bytes: Option<u64>,
    /// Whether servers can start: archipel holds each one to its memory and CPU
    /// (facade delegates cgroups to it), and if not, why.
    pub limits_enforced: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limits_note: Option<String>,
    pub ports_first: u16,
    pub ports_last: u16,
    pub ports_used: u32,
    /// The address players use, if set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_host: Option<String>,
    pub servers: u32,
    pub running: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorBody {
    pub error: String,
}
