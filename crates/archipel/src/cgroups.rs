//! Resource limits through the cgroup facade delegates to archipel's container
//! (`delegate_cgroups`): `/sys/fs/cgroup` is the container's cgroup, writable.
//!
//! ```text
//! /sys/fs/cgroup            the container: facade's limits, the whole pie
//! ├── archipel/             archipel itself, its downloads and installs
//! └── users/
//!     └── u<user>/          a user's quota: memory.max, cpu.max
//!         └── s<server>/    a server: memory.max, pids.max
//! ```
//!
//! Without delegation (an older facade, or running elsewhere) servers run without
//! limits, and the machine page says so.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Context;
use archipel_core::api::Quota;
use tracing::{info, warn};

const ROOT: &str = "/sys/fs/cgroup";
const CONTROLLERS: &[&str] = &["cpu", "memory", "pids"];
/// Processes and threads one server may run.
const SERVER_PIDS: u32 = 4096;

pub struct Cgroups {
    root: PathBuf,
    /// Why limits are not enforced, when they are not.
    off: Option<String>,
}

impl Cgroups {
    /// Takes over the container's cgroup, waiting a little for facade to make it
    /// writable (it does so a moment after the container starts).
    pub async fn setup() -> Self {
        let root = PathBuf::from(ROOT);
        let mut cgroups = Cgroups { root, off: None };
        if !cfg!(target_os = "linux") {
            cgroups.off = Some("archipel runs on Linux only: limits are not enforced".into());
            return cgroups;
        }
        if !cgroups.root.join("cgroup.controllers").exists() {
            cgroups.off = Some("no cgroup v2 here: memory and CPU limits are not enforced".into());
            return cgroups;
        }
        let mut writable = false;
        for _ in 0..30 {
            if std::fs::OpenOptions::new()
                .write(true)
                .open(cgroups.root.join("cgroup.subtree_control"))
                .is_ok()
            {
                writable = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        if !writable {
            cgroups.off = Some(
                "the container's cgroup is not delegated to archipel, so memory and CPU limits \
                 are not enforced: set `--delegate-cgroups true` on archipel's facade app"
                    .into(),
            );
            return cgroups;
        }
        match cgroups.take_over() {
            Ok(()) => info!("cgroups delegated: server limits are enforced"),
            Err(e) => {
                warn!("cgroups: {e:#}");
                cgroups.off = Some(format!("setting cgroups up failed: {e:#}"));
            }
        }
        cgroups
    }

    /// cgroup v2 allows processes only in leaves: archipel's own go to `archipel/`
    /// before controllers are enabled for the children.
    fn take_over(&self) -> anyhow::Result<()> {
        let own = self.root.join("archipel");
        std::fs::create_dir_all(&own)?;
        for _ in 0..10 {
            let procs = std::fs::read_to_string(self.root.join("cgroup.procs"))?;
            let pids: Vec<&str> = procs.split_whitespace().collect();
            if pids.is_empty() {
                break;
            }
            for pid in pids {
                // Gone already, or a kernel thread: either way not in the way.
                let _ = std::fs::write(own.join("cgroup.procs"), pid);
            }
        }
        self.enable_controllers(&self.root)?;
        let users = self.root.join("users");
        std::fs::create_dir_all(&users)?;
        self.enable_controllers(&users)
    }

    fn enable_controllers(&self, dir: &Path) -> anyhow::Result<()> {
        let available = std::fs::read_to_string(dir.join("cgroup.controllers"))?;
        let wanted: Vec<String> = CONTROLLERS
            .iter()
            .filter(|c| available.split_whitespace().any(|a| a == **c))
            .map(|c| format!("+{c}"))
            .collect();
        std::fs::write(dir.join("cgroup.subtree_control"), wanted.join(" "))
            .with_context(|| format!("enabling {} in {}", wanted.join(" "), dir.display()))
    }

    pub fn enforced(&self) -> bool {
        self.off.is_none()
    }

    pub fn note(&self) -> Option<String> {
        self.off.clone()
    }

    /// The container's memory (bytes) and CPU (100 per core), as facade limits it.
    pub fn totals(&self) -> (Option<u64>, Option<u32>) {
        let read = |f: &str| std::fs::read_to_string(self.root.join(f)).ok();
        let memory = read("memory.max")
            .and_then(|m| m.trim().parse::<u64>().ok())
            .or_else(meminfo_total);
        let cpu = read("cpu.max")
            .and_then(|c| {
                let mut parts = c.split_whitespace();
                let quota: u64 = parts.next()?.parse().ok()?;
                let period: u64 = parts.next()?.parse().ok()?;
                Some((quota * 100 / period.max(1)) as u32)
            })
            .or_else(|| {
                std::thread::available_parallelism()
                    .ok()
                    .map(|n| n.get() as u32 * 100)
            });
        (memory, cpu)
    }

    fn user_dir(&self, user: i64) -> PathBuf {
        self.root.join("users").join(format!("u{user}"))
    }

    /// The user's cgroup, holding their quota.
    pub fn ensure_user(&self, user: i64, quota: &Quota) -> anyhow::Result<()> {
        if !self.enforced() {
            return Ok(());
        }
        let dir = self.user_dir(user);
        if !dir.exists() {
            std::fs::create_dir(&dir)?;
            self.enable_controllers(&dir)?;
        }
        write(
            &dir,
            "memory.max",
            &(quota.memory_mb * 1024 * 1024).to_string(),
        )?;
        write(
            &dir,
            "cpu.max",
            &format!("{} 100000", u64::from(quota.cpu_percent.max(1)) * 1000),
        )?;
        Ok(())
    }

    /// A cgroup for a server about to start, inside its owner's: the
    /// `cgroup.procs` file its process joins.
    pub fn create_server(
        &self,
        user: i64,
        server: u64,
        memory_mb: u64,
    ) -> anyhow::Result<Option<PathBuf>> {
        if !self.enforced() {
            return Ok(None);
        }
        let dir = self.user_dir(user).join(format!("s{server}"));
        if !dir.exists() {
            std::fs::create_dir(&dir)?;
        }
        write(&dir, "memory.max", &(memory_mb * 1024 * 1024).to_string())?;
        // Swapping a game server is lag; going over its memory ends it instead.
        if dir.join("memory.swap.max").exists() {
            write(&dir, "memory.swap.max", "0")?;
        }
        write(&dir, "pids.max", &SERVER_PIDS.to_string())?;
        Ok(Some(dir.join("cgroup.procs")))
    }

    /// Kills every process left in a server's cgroup.
    pub fn kill(&self, procs: &Path) {
        let dir = procs.parent().unwrap_or(procs);
        if std::fs::write(dir.join("cgroup.kill"), "1").is_ok() {
            return;
        }
        if let Ok(list) = std::fs::read_to_string(procs) {
            for pid in list
                .split_whitespace()
                .filter_map(|p| p.parse::<i32>().ok())
            {
                // SAFETY: a plain kill(2).
                unsafe {
                    libc::kill(pid, libc::SIGKILL);
                }
            }
        }
    }

    /// Removes a server's cgroup once its processes are gone.
    pub async fn remove(&self, procs: &Path) {
        let Some(dir) = procs.parent() else { return };
        for _ in 0..20 {
            if std::fs::remove_dir(dir).is_ok() || !dir.exists() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        warn!("could not remove cgroup {}", dir.display());
    }

    /// A server's memory (bytes) and CPU time so far (microseconds).
    pub fn stats(&self, procs: &Path) -> Option<(u64, u64)> {
        let dir = procs.parent()?;
        let memory = std::fs::read_to_string(dir.join("memory.current"))
            .ok()?
            .trim()
            .parse()
            .ok()?;
        let cpu = std::fs::read_to_string(dir.join("cpu.stat"))
            .ok()?
            .lines()
            .find_map(|l| l.strip_prefix("usage_usec "))?
            .trim()
            .parse()
            .ok()?;
        Some((memory, cpu))
    }
}

fn write(dir: &Path, file: &str, value: &str) -> anyhow::Result<()> {
    std::fs::write(dir.join(file), value)
        .with_context(|| format!("setting {file} = {value} in {}", dir.display()))
}

fn meminfo_total() -> Option<u64> {
    std::fs::read_to_string("/proc/meminfo")
        .ok()?
        .lines()
        .find_map(|l| l.strip_prefix("MemTotal:"))?
        .trim()
        .trim_end_matches("kB")
        .trim()
        .parse::<u64>()
        .ok()
        .map(|kb| kb * 1024)
}
