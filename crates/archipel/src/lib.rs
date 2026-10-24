//! archipel: game servers for several people on one machine, each with their share
//! of it, or on one's own desktop. Hosted, it runs as one app on facade; on a
//! desktop, inside the desktop app. Game servers are its own processes either way.

pub mod addons;
pub mod api;
pub mod cgroups;
pub mod config;
pub mod console;
pub mod desktop;
pub mod files;
pub mod fsops;
pub mod games;
pub mod hooks;
pub mod install;
pub mod probe;
pub mod reach;
pub mod sandbox;
pub mod store;
pub mod supervisor;
pub mod ui;
pub mod workshop;

use std::sync::Arc;

use anyhow::Context;
use archipel_core::api::Mode;
use tracing::info;

/// What runs archipel: its API (and UI) and the servers it supervises.
pub struct Built {
    pub app: Arc<api::App>,
    pub supervisor: supervisor::Supervisor,
}

/// Opens the data directory and gets everything ready, nothing started yet.
pub async fn build(cfg: config::Config) -> anyhow::Result<Built> {
    use std::os::unix::fs::PermissionsExt;
    let mode = cfg.mode();
    std::fs::create_dir_all(&cfg.data_dir)
        .with_context(|| format!("creating {}", cfg.data_dir.display()))?;
    if mode == Mode::Hosted {
        // Servers reach their own directories and the shared game files through it.
        std::fs::set_permissions(&cfg.data_dir, std::fs::Permissions::from_mode(0o755))?;
    } else {
        // Every server runs as the desktop's own user.
        let allowed = sandbox::Allowed {
            own: cfg.servers_dir(),
            read: vec![
                cfg.shared_dir(),
                cfg.runtimes_dir(),
                cfg.data_dir.join("tmp"),
            ],
        };
        for dir in std::iter::once(&allowed.own).chain(&allowed.read) {
            std::fs::create_dir_all(dir)?;
        }
        let file_work = sandbox::Sandbox::prepare(&allowed)
            .map_err(|why| tracing::warn!("{why}: servers are not kept to their files"))
            .ok();
        fsops::run_servers_as_self(file_work);
    }
    cfg.port_range()?;
    let store = Arc::new(store::Store::open(&cfg.data_dir.join("archipel.db"))?);
    for file in ["archipel.db", "archipel.db-wal", "archipel.db-shm"] {
        let path = cfg.data_dir.join(file);
        if path.exists() {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        }
    }
    let games = Arc::new(games::Games::load(&cfg.games_dir())?);
    let cgroups = match mode {
        Mode::Hosted => cgroups::Cgroups::setup().await,
        Mode::Desktop => cgroups::Cgroups::none("the desktop app sets no limits"),
    };
    let supervisor =
        supervisor::Supervisor::new(cfg.clone(), store.clone(), games.clone(), cgroups)?;
    let app = Arc::new(api::App {
        cfg: cfg.clone(),
        store,
        games,
        supervisor: supervisor.clone(),
        prober: (mode == Mode::Hosted && cfg.probe).then(probe::Prober::default),
    });
    match mode {
        Mode::Hosted => api::ensure_admin(&app)?,
        Mode::Desktop => api::ensure_owner(&app)?,
    }
    tokio::spawn(supervisor.clone().measure());
    supervisor.check_internet();
    Ok(Built { app, supervisor })
}

/// Hosted archipel: the API and UI over HTTP, backup hooks, servers that were
/// running started again, until SIGTERM.
pub async fn serve(cfg: config::Config) -> anyhow::Result<()> {
    let listen = cfg.listen;
    let Built { app, supervisor } = build(cfg).await?;
    if app.cfg.mode() == Mode::Hosted {
        tokio::spawn({
            let supervisor = supervisor.clone();
            async move {
                if let Err(e) = hooks::listen(supervisor).await {
                    tracing::warn!("backup hooks unavailable: {e:#}");
                }
            }
        });
        supervisor.start_saved();
    }
    let listener = tokio::net::TcpListener::bind(listen)
        .await
        .with_context(|| format!("listening on {listen}"))?;
    info!("archipel ready on http://{listen}");
    if app.prober.is_some() {
        info!("desktop apps may ask to be checked from outside (ARCHIPEL_PROBE)");
    }
    // The peer's address, for probes from outside.
    let server = axum::serve(
        listener,
        api::router(app).into_make_service_with_connect_info::<std::net::SocketAddr>(),
    );
    tokio::select! {
        result = server => result.context("serving")?,
        _ = shutdown_signal() => {
            info!("shutting down: stopping servers");
            supervisor.shutdown().await;
        }
    }
    Ok(())
}

pub async fn shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};
    let mut term = signal(SignalKind::terminate()).expect("SIGTERM handler");
    let mut int = signal(SignalKind::interrupt()).expect("SIGINT handler");
    tokio::select! {
        _ = term.recv() => {}
        _ = int.recv() => {}
    }
}
