//! archipel: game servers for several people on one machine, each with their share
//! of it. Runs as one app on facade, game servers being its own processes.

mod api;
mod cgroups;
mod config;
mod console;
mod files;
mod fsops;
mod games;
mod hooks;
mod install;
mod store;
mod supervisor;
mod ui;

use std::sync::Arc;

use anyhow::Context;
use clap::{Parser, Subcommand};
use tracing::info;

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
    #[command(flatten)]
    config: config::Config,
}

#[derive(Subcommand)]
enum Command {
    /// Run archipel (what happens without a command).
    Serve,
    /// For facade's backups of archipel's volume: `before-backup` has every running
    /// server save and pause saving, `after-backup` resumes.
    Hook {
        #[arg(value_parser = ["before-backup", "after-backup"])]
        step: String,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stdout()))
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "archipel=info".into()),
        )
        .init();
    let cli = Cli::parse();
    match cli.command {
        Some(Command::Hook { step }) => {
            hooks::call(step.trim_end_matches("-backup")).await?;
            println!("ok");
            Ok(())
        }
        None | Some(Command::Serve) => serve(cli.config).await,
    }
}

async fn serve(cfg: config::Config) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(&cfg.data_dir)
        .with_context(|| format!("creating {}", cfg.data_dir.display()))?;
    // Servers reach their own directories and the shared game files through it.
    std::fs::set_permissions(&cfg.data_dir, std::fs::Permissions::from_mode(0o755))?;
    cfg.port_range()?;
    let store = Arc::new(store::Store::open(&cfg.data_dir.join("archipel.db"))?);
    for file in ["archipel.db", "archipel.db-wal", "archipel.db-shm"] {
        let path = cfg.data_dir.join(file);
        if path.exists() {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        }
    }
    let games = Arc::new(games::Games::load(&cfg.games_dir())?);
    let cgroups = cgroups::Cgroups::setup().await;
    let supervisor =
        supervisor::Supervisor::new(cfg.clone(), store.clone(), games.clone(), cgroups)?;
    let app = Arc::new(api::App {
        cfg: cfg.clone(),
        store,
        games,
        supervisor: supervisor.clone(),
    });
    api::ensure_admin(&app)?;

    tokio::spawn(supervisor.clone().measure());
    tokio::spawn({
        let supervisor = supervisor.clone();
        async move {
            if let Err(e) = hooks::listen(supervisor).await {
                tracing::warn!("backup hooks unavailable: {e:#}");
            }
        }
    });
    supervisor.start_saved();

    let listener = tokio::net::TcpListener::bind(cfg.listen)
        .await
        .with_context(|| format!("listening on {}", cfg.listen))?;
    info!("archipel ready on http://{}", cfg.listen);
    let server = axum::serve(listener, api::router(app));
    tokio::select! {
        result = server => result.context("serving")?,
        _ = shutdown_signal() => {
            info!("shutting down: stopping servers");
            supervisor.shutdown().await;
        }
    }
    Ok(())
}

async fn shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};
    let mut term = signal(SignalKind::terminate()).expect("SIGTERM handler");
    let mut int = signal(SignalKind::interrupt()).expect("SIGINT handler");
    tokio::select! {
        _ = term.recv() => {}
        _ = int.recv() => {}
    }
}
