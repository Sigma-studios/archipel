//! Settings, from flags or environment variables (how facade passes them).

use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{Context, bail};
use archipel_core::api::Mode;

#[derive(Debug, Clone, clap::Args)]
pub struct Config {
    /// Where everything is kept: the database, servers' files, downloads.
    #[arg(long, env = "ARCHIPEL_DATA", default_value = "/data")]
    pub data_dir: PathBuf,
    /// The web UI and API (plain HTTP: facade's entry point adds HTTPS).
    #[arg(long, env = "ARCHIPEL_LISTEN", default_value = "0.0.0.0:7810")]
    pub listen: SocketAddr,
    /// Ports servers get, published by facade on the same numbers, TCP and UDP:
    /// `30000-30019`.
    #[arg(long, env = "ARCHIPEL_PORTS", default_value = "30000-30019")]
    pub ports: String,
    /// The address players connect to. Without it, the address the web UI is
    /// browsed at is shown.
    #[arg(long, env = "ARCHIPEL_PUBLIC_HOST")]
    pub public_host: Option<String>,
    /// Memory kept for archipel itself, out of its container's.
    #[arg(long, env = "ARCHIPEL_RESERVE_MB", default_value_t = 256)]
    pub reserve_mb: u64,
    /// The first admin's password, on the first start only (otherwise a random one
    /// is printed and saved in <data>/initial-admin-password).
    #[arg(long, env = "ARCHIPEL_ADMIN_PASSWORD", hide_env_values = true)]
    pub admin_password: Option<String>,
    /// Desktop mode: one owner, no login, no limits, ports opened on the router.
    /// The desktop app sets it; on its own it serves over HTTP, for development
    /// only (keep `--listen` on 127.0.0.1: there is no login).
    #[arg(long, env = "ARCHIPEL_DESKTOP", hide = true)]
    pub desktop: bool,
    /// Lets desktop apps ask this archipel to check, from the internet, that their
    /// ports reach them (`POST /api/probe`): `on` or `off`.
    #[arg(long, env = "ARCHIPEL_PROBE", value_parser = clap::builder::BoolishValueParser::new())]
    pub probe: bool,
}

impl Config {
    pub fn mode(&self) -> Mode {
        if self.desktop {
            Mode::Desktop
        } else {
            Mode::Hosted
        }
    }

    /// The desktop app's settings: its data in `data_dir`, the usual port pool.
    pub fn desktop(data_dir: PathBuf) -> Self {
        Config {
            data_dir,
            listen: "127.0.0.1:7810".parse().expect("an address"),
            ports: "30000-30019".into(),
            public_host: None,
            reserve_mb: 0,
            admin_password: None,
            desktop: true,
            probe: false,
        }
    }

    /// The port pool, first and last.
    pub fn port_range(&self) -> anyhow::Result<(u16, u16)> {
        let (a, b) = self
            .ports
            .split_once('-')
            .unwrap_or((&self.ports, &self.ports));
        let parse = |p: &str| {
            p.trim()
                .parse::<u16>()
                .with_context(|| format!("ARCHIPEL_PORTS: `{p}` is not a port"))
        };
        let (first, last) = (parse(a)?, parse(b)?);
        if first == 0 || last < first {
            bail!("ARCHIPEL_PORTS must be a range like 30000-30019");
        }
        Ok((first, last))
    }

    pub fn servers_dir(&self) -> PathBuf {
        self.data_dir.join("servers")
    }

    pub fn server_dir(&self, id: u64) -> PathBuf {
        self.servers_dir().join(id.to_string())
    }

    /// Downloads shared by every server of a game.
    pub fn shared_dir(&self) -> PathBuf {
        self.data_dir.join("shared")
    }

    /// Java, SteamCMD.
    pub fn runtimes_dir(&self) -> PathBuf {
        self.data_dir.join("runtimes")
    }

    /// Game definitions added to the built-in ones.
    pub fn games_dir(&self) -> PathBuf {
        self.data_dir.join("games")
    }

    pub fn logs_dir(&self) -> PathBuf {
        self.data_dir.join("logs")
    }
}
