//! Types shared by the archipel server and its web UI: game definitions, the
//! templates they use, and the HTTP API's bodies.

pub mod api;
pub mod game;
pub mod template;

/// Seconds since the Unix epoch.
pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
