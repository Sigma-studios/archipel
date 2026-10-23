//! archipel inside the desktop app: no network listener, no login, no limits.
//!
//! The app's window loads the UI from a URL scheme of its own (`archipel://`), and
//! hands every request made there to [`Desktop::handle`], which answers from the
//! same API the hosted archipel serves over HTTP. Nothing listens on a port, so
//! nothing else on the computer or the network can reach it. Servers start only
//! when asked and stop when the app closes.

use std::path::PathBuf;

use anyhow::Context;
use axum::body::Body;
use tower::ServiceExt;

use crate::config::Config;
use crate::supervisor::Supervisor;
use crate::{Built, api, build};

pub struct Desktop {
    router: axum::Router,
    supervisor: Supervisor,
}

impl Desktop {
    /// archipel with its data in `data_dir` ([`data_dir`] by default).
    pub async fn start(data_dir: PathBuf) -> anyhow::Result<Self> {
        let Built { app, supervisor } = build(Config::desktop(data_dir)).await?;
        Ok(Desktop {
            router: api::router(app),
            supervisor,
        })
    }

    /// Answers a request from the window, as archipel's HTTP API would.
    pub async fn handle(&self, req: http::Request<Vec<u8>>) -> http::Response<Vec<u8>> {
        let response = match self.router.clone().oneshot(req.map(Body::from)).await {
            Ok(response) => response,
            Err(never) => match never {},
        };
        let (parts, body) = response.into_parts();
        match axum::body::to_bytes(body, usize::MAX).await {
            Ok(bytes) => http::Response::from_parts(parts, bytes.to_vec()),
            Err(e) => http::Response::builder()
                .status(http::StatusCode::INTERNAL_SERVER_ERROR)
                .body(format!("{e}").into_bytes())
                .expect("a response"),
        }
    }

    /// How many servers are running (or starting, or installing).
    pub fn running(&self) -> usize {
        self.supervisor.active()
    }

    /// Stops every server cleanly and takes their ports back from the router.
    pub async fn shutdown(&self) {
        self.supervisor.shutdown().await;
    }
}

/// Where the desktop app keeps its servers: `$XDG_DATA_HOME/archipel`, or
/// `~/.local/share/archipel`.
pub fn data_dir() -> anyhow::Result<PathBuf> {
    if let Some(dir) = std::env::var_os("XDG_DATA_HOME").filter(|d| !d.is_empty()) {
        return Ok(PathBuf::from(dir).join("archipel"));
    }
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home).join(".local/share/archipel"))
}
