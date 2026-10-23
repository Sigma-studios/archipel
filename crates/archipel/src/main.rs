//! `archipel`, hosted: the API and web UI over HTTP, for facade (see the library).

use archipel::{config, hooks};
use clap::{Parser, Subcommand};

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
        None | Some(Command::Serve) => archipel::serve(cli.config).await,
    }
}
