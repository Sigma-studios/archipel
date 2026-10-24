//! Running game servers: each one a process of its own user, in its own cgroup,
//! its output in its console, its input from the console's command line. On the
//! desktop, they run as the person using the computer, without limits, each kept to
//! its own files by Landlock, its ports opened on the home router while it runs.
//!
//! A server's life: `installing` (getting or updating its files) → `starting` →
//! `running` (once its log says it is ready) → `stopping` → `stopped`; or `crashed`
//! when it exits on its own or fails to install, after which it is started again
//! unless it keeps crashing.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, anyhow, bail};
use archipel_core::api::{Mode, ServerState, ServerUsage};
use archipel_core::game::GameDef;
use archipel_core::template::{self, Context as Ctx, Value};
use parking_lot::Mutex;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio::sync::{mpsc, watch};
use tracing::{info, warn};

use crate::cgroups::Cgroups;
use crate::config::Config;
use crate::console::Console;
use crate::files;
use crate::fsops;
use crate::games::Games;
use crate::install::{Installed, Installer, RunAs};
use crate::reach::Reacher;
use crate::sandbox::{Allowed, Sandbox};
use crate::store::{ServerRow, Store};

/// Crashes within this long count towards giving up.
const CRASH_WINDOW: Duration = Duration::from_secs(10 * 60);
/// Restarted after a crash, unless it crashed this many times in the window.
const MAX_CRASHES: usize = 3;
/// After its stop timeout, a server gets this long after SIGTERM, then is killed.
const TERM_GRACE: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, PartialEq)]
pub struct Status {
    pub state: ServerState,
    pub message: Option<String>,
    pub started_at: Option<u64>,
}

pub struct Instance {
    pub id: u64,
    pub console: Console,
    status: watch::Sender<Status>,
    stdin: Mutex<Option<mpsc::UnboundedSender<String>>>,
    pid: Mutex<Option<i32>>,
    cgroup: Mutex<Option<PathBuf>>,
    /// Asked to stop: an exit now is not a crash.
    stopping: AtomicBool,
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    usage: Mutex<Option<ServerUsage>>,
    last_cpu: Mutex<Option<(Instant, u64)>>,
    crashes: Mutex<Vec<Instant>>,
    /// Its memory while it runs, for quotas.
    memory_mb: Mutex<u64>,
    /// When its process last started (installs do not count).
    spawned: Mutex<Option<Instant>>,
}

impl Instance {
    pub fn status(&self) -> Status {
        self.status.borrow().clone()
    }

    pub fn usage(&self) -> Option<ServerUsage> {
        self.usage.lock().clone()
    }

    fn set(&self, state: ServerState, message: Option<String>) {
        self.status.send_modify(|s| {
            s.state = state;
            s.message = message;
            match state {
                ServerState::Running if s.started_at.is_none() => {
                    s.started_at = Some(archipel_core::now())
                }
                ServerState::Running => {}
                _ => s.started_at = None,
            }
        });
    }

    fn is_active(&self) -> bool {
        self.status.borrow().state.is_active()
    }

    async fn wait_inactive(&self, timeout: Duration) -> bool {
        let mut rx = self.status.subscribe();
        tokio::time::timeout(timeout, rx.wait_for(|s| !s.state.is_active()))
            .await
            .is_ok()
    }
}

#[derive(Clone)]
pub struct Supervisor(Arc<Inner>);

struct Inner {
    cfg: Config,
    store: Arc<Store>,
    games: Arc<Games>,
    cgroups: Cgroups,
    /// On the desktop: the home router, asked to forward servers' ports.
    reacher: Option<Arc<Reacher>>,
    installer: Installer,
    instances: Mutex<HashMap<u64, Arc<Instance>>>,
    /// Bytes each server's files take, measured now and then.
    disk: Mutex<HashMap<u64, u64>>,
    shutting_down: AtomicBool,
}

impl Supervisor {
    pub fn new(
        cfg: Config,
        store: Arc<Store>,
        games: Arc<Games>,
        cgroups: Cgroups,
    ) -> anyhow::Result<Self> {
        std::fs::create_dir_all(cfg.logs_dir())?;
        let reacher = match cfg.mode() {
            Mode::Desktop => {
                let reacher = Reacher::new(store.clone(), cfg.port_range()?)?;
                tokio::spawn(reacher.clone().renew());
                Some(reacher)
            }
            Mode::Hosted => None,
        };
        Ok(Supervisor(Arc::new(Inner {
            reacher,
            installer: Installer::new(cfg.clone())?,
            cfg,
            store,
            games,
            cgroups,
            instances: Default::default(),
            disk: Default::default(),
            shutting_down: AtomicBool::new(false),
        })))
    }

    pub fn cgroups(&self) -> &Cgroups {
        &self.0.cgroups
    }

    pub fn installer(&self) -> &Installer {
        &self.0.installer
    }

    pub fn instance(&self, id: u64) -> Arc<Instance> {
        self.0
            .instances
            .lock()
            .entry(id)
            .or_insert_with(|| {
                let (status, _) = watch::channel(Status {
                    state: ServerState::Stopped,
                    message: None,
                    started_at: None,
                });
                Arc::new(Instance {
                    id,
                    console: Console::new(self.0.cfg.logs_dir().join(format!("{id}.log"))),
                    status,
                    stdin: Mutex::new(None),
                    pid: Mutex::new(None),
                    cgroup: Mutex::new(None),
                    stopping: AtomicBool::new(false),
                    task: Mutex::new(None),
                    usage: Mutex::new(None),
                    last_cpu: Mutex::new(None),
                    crashes: Mutex::new(Vec::new()),
                    memory_mb: Mutex::new(0),
                    spawned: Mutex::new(None),
                })
            })
            .clone()
    }

    /// Counts bytes just written into a server's files, until it is measured again.
    pub fn grew(&self, id: u64, bytes: u64) {
        if let Some(b) = self.0.disk.lock().get_mut(&id) {
            *b += bytes;
        }
    }

    pub fn disk_bytes(&self, id: u64) -> Option<u64> {
        self.0.disk.lock().get(&id).copied()
    }

    /// Memory held by active servers, by owner, and in all.
    pub fn memory_in_use(&self) -> (HashMap<i64, u64>, u64) {
        let rows = self.0.store.list_servers().unwrap_or_default();
        let mut by_owner = HashMap::new();
        let mut total = 0;
        for row in rows {
            let inst = self.instance(row.id);
            if inst.is_active() {
                let mb = *inst.memory_mb.lock();
                *by_owner.entry(row.owner_id).or_default() += mb;
                total += mb;
            }
        }
        (by_owner, total)
    }

    /// Memory servers may have in all: the container's, less archipel's share.
    pub fn memory_available_mb(&self) -> Option<u64> {
        let (memory, _) = self.0.cgroups.totals();
        memory.map(|b| (b / (1024 * 1024)).saturating_sub(self.0.cfg.reserve_mb))
    }

    /// Starts a server, if its owner's quota and the machine allow.
    pub fn start(&self, id: u64) -> anyhow::Result<()> {
        if self.0.shutting_down.load(Ordering::Relaxed) {
            bail!("archipel is shutting down");
        }
        let hosted = self.hosted();
        if hosted {
            self.0.cgroups.require()?;
        }
        let row = self.0.store.get_server(id)?.context("no such server")?;
        let game = self.game(&row)?;
        let owner = self
            .0
            .store
            .user_by_id(row.owner_id)?
            .context("the server's owner is gone")?;
        let inst = self.instance(id);
        if inst.is_active() {
            bail!("{} is already {}", row.name, inst.status().state);
        }
        game.settings_from(&row.settings).map_err(|e| anyhow!(e))?;
        if row.memory_mb < game.memory.min_mb {
            bail!("{} needs at least {} MB", game.name, game.memory.min_mb);
        }
        if hosted {
            self.admit(&row, &owner)?;
        }
        self.0.store.set_auto_start(id, hosted)?;
        *inst.memory_mb.lock() = row.memory_mb;
        inst.stopping.store(false, Ordering::Relaxed);
        inst.set(ServerState::Installing, None);
        let me = self.clone();
        let task = tokio::spawn({
            let inst = inst.clone();
            async move { me.run(inst).await }
        });
        *inst.task.lock() = Some(task);
        Ok(())
    }

    /// Whether archipel runs hosted (quotas, limits), rather than on a desktop.
    pub fn hosted(&self) -> bool {
        self.0.cfg.mode() == Mode::Hosted
    }

    pub fn reacher(&self) -> Option<&Arc<Reacher>> {
        self.0.reacher.as_ref()
    }

    /// On the desktop, finds out (in the background) whether friends could reach
    /// this computer, with a port of the pool no server has.
    pub fn check_internet(&self) {
        let Some(reacher) = self.0.reacher.clone() else {
            return;
        };
        reacher.checking();
        tokio::spawn(async move { reacher.check().await });
    }

    /// Whether the owner's quota and the machine leave room for the server.
    fn admit(&self, row: &ServerRow, owner: &crate::store::UserRow) -> anyhow::Result<()> {
        let (by_owner, total) = self.memory_in_use();
        let mine = by_owner.get(&owner.id).copied().unwrap_or(0);
        if mine + row.memory_mb > owner.quota.memory_mb {
            bail!(
                "this server needs {} MB, and {} MB of your {} MB are taken by running servers: \
                 stop one, or give this one less memory",
                row.memory_mb,
                mine,
                owner.quota.memory_mb
            );
        }
        if let Some(available) = self.memory_available_mb()
            && total + row.memory_mb > available
        {
            bail!(
                "the machine has {} MB free for servers and this one needs {} MB: try again \
                 when others stop",
                available.saturating_sub(total),
                row.memory_mb
            );
        }
        let disk_mb = self.owner_disk_bytes(owner.id) / (1024 * 1024);
        if disk_mb > owner.quota.disk_mb {
            bail!(
                "your servers' files take {disk_mb} MB, more than your {} MB: delete a server \
                 or ask for more",
                owner.quota.disk_mb
            );
        }
        Ok(())
    }

    /// Whether the owner's disk quota leaves room for `extra` more bytes in the
    /// server's files (hosted: the desktop has no quota).
    pub fn disk_room(&self, row: &ServerRow, extra: u64) -> anyhow::Result<()> {
        if !self.hosted() {
            return Ok(());
        }
        let Some(owner) = self.0.store.user_by_id(row.owner_id)? else {
            return Ok(());
        };
        let used_mb = self.owner_disk_bytes(owner.id) / (1024 * 1024);
        let extra_mb = extra.div_ceil(1024 * 1024);
        if used_mb + extra_mb > owner.quota.disk_mb {
            bail!(
                "your servers' files take {used_mb} MB of your {} MB, and this is {extra_mb} MB: \
                 delete something first, or ask for more",
                owner.quota.disk_mb
            );
        }
        Ok(())
    }

    fn owner_disk_bytes(&self, owner: i64) -> u64 {
        let rows = self.0.store.list_servers().unwrap_or_default();
        let disk = self.0.disk.lock();
        rows.iter()
            .filter(|r| r.owner_id == owner)
            .filter_map(|r| disk.get(&r.id))
            .sum()
    }

    fn game(&self, row: &ServerRow) -> anyhow::Result<GameDef> {
        self.0
            .games
            .get(&row.game)
            .cloned()
            .ok_or_else(|| anyhow!("archipel no longer knows the game {}", row.game))
    }

    /// Runs the server, again after crashes unless they keep coming.
    async fn run(self, inst: Arc<Instance>) {
        loop {
            *inst.spawned.lock() = None;
            let result = self.run_once(&inst).await;
            *inst.pid.lock() = None;
            *inst.stdin.lock() = None;
            *inst.usage.lock() = None;
            let procs = inst.cgroup.lock().take();
            if let Some(procs) = procs {
                self.0.cgroups.kill(&procs);
                self.0.cgroups.remove(&procs).await;
            }
            if let Some(reacher) = &self.0.reacher {
                reacher.close(inst.id).await;
            }
            if inst.stopping.load(Ordering::Relaxed) {
                inst.console.system("stopped".into());
                inst.set(ServerState::Stopped, None);
                return;
            }
            let message = match result {
                // What shells say when a program is missing or cannot start.
                Ok(code) if code == "code 127" => format!(
                    "it exited on its own ({code}): a program it runs is missing or cannot \
                     run on this system, as its console says"
                ),
                Ok(code) => format!("it exited on its own ({code})"),
                Err(e) => format!("{e:#}"),
            };
            inst.console.system(message.clone());
            let crashes = {
                let mut c = inst.crashes.lock();
                c.retain(|t| t.elapsed() < CRASH_WINDOW);
                c.push(Instant::now());
                c.len()
            };
            // A server that fails to install, or exits right after starting, will
            // again.
            let worth_it = inst
                .spawned
                .lock()
                .is_some_and(|t| t.elapsed() > Duration::from_secs(30));
            if !worth_it || crashes >= MAX_CRASHES {
                inst.set(ServerState::Crashed, Some(message));
                return;
            }
            inst.console.system("starting it again in 5 seconds".into());
            inst.set(ServerState::Crashed, Some(message));
            tokio::time::sleep(Duration::from_secs(5)).await;
            if inst.stopping.load(Ordering::Relaxed) {
                inst.set(ServerState::Stopped, None);
                return;
            }
            inst.set(ServerState::Installing, None);
        }
    }

    /// One run: install what is missing, write its files, start it and wait for it
    /// to exit. Its exit status, or why it could not run.
    async fn run_once(&self, inst: &Instance) -> anyhow::Result<String> {
        let id = inst.id;
        let row = self
            .0
            .store
            .get_server(id)?
            .context("the server was deleted")?;
        let game = self.game(&row)?;
        let owner = self
            .0
            .store
            .user_by_id(row.owner_id)?
            .context("the server's owner is gone")?;
        let settings = game.settings_from(&row.settings).map_err(|e| anyhow!(e))?;
        let dir = self.0.cfg.server_dir(id);
        let uid = fsops::uid_of(id);
        fsops::ensure_server_dir(&dir, uid).context("preparing the server's directory")?;
        let run_as = if self.hosted() {
            self.0.cgroups.ensure_user(owner.id, &owner.quota)?;
            let procs = self.0.cgroups.create_server(owner.id, id, row.memory_mb)?;
            *inst.cgroup.lock() = Some(procs.clone());
            RunAs {
                uid,
                cgroup: Some(procs),
                sandbox: None,
            }
        } else {
            RunAs {
                uid,
                cgroup: None,
                sandbox: self.sandbox(&dir, &inst.console)?,
            }
        };

        inst.set(ServerState::Installing, None);
        let installed: Option<Installed> = row
            .installed
            .as_deref()
            .and_then(|i| serde_json::from_str(i).ok());
        let ready = self
            .0
            .installer
            .prepare(
                &game,
                &settings,
                installed.as_ref(),
                &dir,
                &run_as,
                &inst.console,
            )
            .await
            .context("installing")?;
        if ready.installed != installed {
            let json = ready
                .installed
                .as_ref()
                .map(serde_json::to_string)
                .transpose()?;
            self.0.store.set_installed(id, json.as_deref())?;
        }

        let ctx = context(
            &game,
            &row,
            &settings,
            &dir,
            ready.java.as_deref(),
            ready.base.as_deref(),
        );
        files::write_all(&game, &dir, uid, &ctx).await?;
        let words: Vec<String> = game
            .run
            .command
            .iter()
            .map(|w| template::render(w, &ctx))
            .collect::<Result<_, _>>()?;
        let (program, args) = words.split_first().context("no command")?;
        let program = if program.starts_with("./") {
            dir.join(program.trim_start_matches("./"))
        } else {
            PathBuf::from(program)
        };
        if program.starts_with(&dir) && !program.exists() {
            bail!("{}", missing_program(&program, &dir, uid).await);
        }
        let mut env: BTreeMap<String, String> = BTreeMap::new();
        for (k, v) in &game.run.env {
            env.insert(k.clone(), template::render(v, &ctx)?);
        }

        inst.set(ServerState::Starting, None);
        inst.console.system(if self.hosted() {
            format!("starting {} with {} MB of memory", row.name, row.memory_mb)
        } else {
            format!("starting {}", row.name)
        });
        let mut cmd = tokio::process::Command::new(&program);
        cmd.args(args)
            .current_dir(&dir)
            .env_clear()
            .env("PATH", crate::fsops::servers_path())
            .env("HOME", &dir)
            .env("LANG", "C.UTF-8")
            .env("TERM", "dumb")
            .envs(&env)
            .stdin(Stdio::piped());
        let mut child =
            spawn(cmd, &run_as).with_context(|| format!("running {}", program.display()))?;
        *inst.pid.lock() = child.id().map(|p| p as i32);
        *inst.spawned.lock() = Some(Instant::now());
        if let Some(reacher) = self.0.reacher.clone() {
            let ports: Vec<u16> = row.ports.values().copied().collect();
            tokio::spawn(async move { reacher.open(id, ports).await });
        }

        // The console's command line.
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        *inst.stdin.lock() = Some(tx);
        if let Some(mut stdin) = child.stdin.take() {
            tokio::spawn(async move {
                while let Some(line) = rx.recv().await {
                    if stdin
                        .write_all(format!("{line}\n").as_bytes())
                        .await
                        .is_err()
                    {
                        break;
                    }
                    let _ = stdin.flush().await;
                }
            });
        }
        let ready_text = game.run.ready.clone();
        if ready_text.is_none() {
            inst.set(ServerState::Running, None);
        }
        let readers = pipe_output(
            child.stdout.take(),
            child.stderr.take(),
            inst.console.clone(),
            {
                let status = inst.status.clone();
                let console = inst.console.clone();
                move |line: &str| {
                    if let Some(text) = &ready_text
                        && line.contains(text.as_str())
                        && status.borrow().state == ServerState::Starting
                    {
                        status.send_modify(|s| {
                            s.state = ServerState::Running;
                            s.started_at = Some(archipel_core::now());
                        });
                        console.system("ready".into());
                    }
                    true
                }
            },
        );
        let status = child.wait().await?;
        readers.await;
        Ok(match status.code() {
            Some(code) => format!("code {code}"),
            None => "killed".into(),
        })
    }

    /// The desktop's ruleset keeping a server to its own files and the ones archipel
    /// shares; without Landlock, the server runs anyway, and its console says so.
    fn sandbox(&self, dir: &Path, console: &Console) -> anyhow::Result<Option<Arc<Sandbox>>> {
        let read = vec![self.0.cfg.shared_dir(), self.0.cfg.runtimes_dir()];
        for d in &read {
            // Rules hold for what is put there later, but need the directory.
            std::fs::create_dir_all(d)?;
        }
        let allowed = Allowed {
            own: dir.to_path_buf(),
            read,
        };
        Ok(match Sandbox::prepare(&allowed) {
            Ok(sandbox) => Some(Arc::new(sandbox)),
            Err(why) => {
                console.system(format!(
                    "{why}: the server can read your files, not just its own"
                ));
                None
            }
        })
    }

    /// Says why a server could not start.
    pub fn not_started(&self, id: u64, why: &anyhow::Error) {
        let inst = self.instance(id);
        inst.console.system(format!("not started: {why:#}"));
        inst.set(ServerState::Stopped, Some(format!("not started: {why:#}")));
    }

    /// Stops a server cleanly: its stop command or signal, then SIGTERM, then
    /// SIGKILL. `by_owner`: it stays stopped when archipel restarts.
    pub async fn stop(&self, id: u64, by_owner: bool) -> anyhow::Result<()> {
        if by_owner {
            self.0.store.set_auto_start(id, false)?;
        }
        let inst = self.instance(id);
        inst.stopping.store(true, Ordering::Relaxed);
        if !inst.is_active() {
            // A crashed server waiting to start again stays down.
            if inst.status().state == ServerState::Crashed {
                inst.set(ServerState::Stopped, inst.status().message);
            }
            return Ok(());
        }
        let row = self.0.store.get_server(id)?.context("no such server")?;
        let game = self.game(&row)?;
        // Before its log says it is ready, a server may not act on its console yet.
        let ready = game.run.ready.is_none() || inst.status().state == ServerState::Running;
        inst.set(ServerState::Stopping, None);
        let pid = *inst.pid.lock();
        let Some(pid) = pid else {
            // Installing: no server yet, just its install, which goes.
            let task = inst.task.lock().take();
            if let Some(task) = task {
                task.abort();
            }
            let procs = inst.cgroup.lock().take();
            if let Some(procs) = procs {
                self.0.cgroups.kill(&procs);
                self.0.cgroups.remove(&procs).await;
            }
            inst.console.system("install stopped".into());
            inst.set(ServerState::Stopped, None);
            return Ok(());
        };
        inst.console.system("stopping".into());
        match (&game.run.stop_command, &game.run.stop_signal) {
            (Some(_), _) if !ready => signal_group(pid, libc::SIGTERM),
            (Some(command), _) => {
                let tx = inst.stdin.lock().clone();
                if let Some(tx) = tx {
                    let _ = tx.send(command.clone());
                }
            }
            (None, Some(signal)) => signal_group(pid, parse_signal(signal)),
            (None, None) => signal_group(pid, libc::SIGTERM),
        }
        let timeout = Duration::from_secs(u64::from(game.run.stop_timeout_secs));
        if inst.wait_inactive(timeout).await {
            return Ok(());
        }
        inst.console.system(format!(
            "it did not stop within {}s: terminating it",
            timeout.as_secs()
        ));
        signal_group(pid, libc::SIGTERM);
        if inst.wait_inactive(TERM_GRACE).await {
            return Ok(());
        }
        inst.console.system("killing it".into());
        signal_group(pid, libc::SIGKILL);
        let procs = inst.cgroup.lock().clone();
        if let Some(procs) = procs {
            self.0.cgroups.kill(&procs);
        }
        inst.wait_inactive(Duration::from_secs(10)).await;
        Ok(())
    }

    pub async fn restart(&self, id: u64) -> anyhow::Result<()> {
        self.stop(id, false).await?;
        self.start(id)
    }

    /// Types a line into the server's console.
    pub fn send(&self, id: u64, line: &str) -> anyhow::Result<()> {
        let inst = self.instance(id);
        let tx = inst.stdin.lock().clone();
        let Some(tx) = tx else {
            bail!("the server is not running");
        };
        if line.contains(['\n', '\r']) {
            bail!("one line at a time");
        }
        inst.console.line(format!("> {line}"));
        tx.send(line.to_string())
            .map_err(|_| anyhow!("the server is not reading its console"))
    }

    /// Stops a server and deletes its files.
    pub async fn delete(&self, id: u64) -> anyhow::Result<()> {
        self.stop(id, true).await?;
        let dir = self.0.cfg.server_dir(id);
        if dir.exists() {
            // Does not follow links, so nothing outside the directory goes.
            tokio::task::spawn_blocking(move || std::fs::remove_dir_all(dir)).await??;
        }
        let _ = std::fs::remove_file(self.0.cfg.logs_dir().join(format!("{id}.log")));
        let _ = std::fs::remove_file(self.0.cfg.logs_dir().join(format!("{id}.log.1")));
        self.0.instances.lock().remove(&id);
        self.0.disk.lock().remove(&id);
        Ok(())
    }

    /// Forgets what a server's files were installed from: its next start installs
    /// them again (a game update).
    pub fn reinstall(&self, id: u64) -> anyhow::Result<()> {
        if self.instance(id).is_active() {
            bail!("stop the server first");
        }
        self.0.store.set_installed(id, None)?;
        self.instance(id)
            .console
            .system("its game files will be installed again at the next start".into());
        Ok(())
    }

    /// Starts the servers that were running when archipel last stopped.
    pub fn start_saved(&self) {
        for row in self.0.store.list_servers().unwrap_or_default() {
            if row.auto_start
                && let Err(e) = self.start(row.id)
            {
                warn!(server = row.id, "not started again: {e:#}");
                self.instance(row.id)
                    .console
                    .system(format!("not started again: {e:#}"));
            }
        }
    }

    /// Stops every server, for archipel's own shutdown: they start again next time.
    pub async fn shutdown(&self) {
        self.0.shutting_down.store(true, Ordering::Relaxed);
        let ids: Vec<u64> = self
            .0
            .instances
            .lock()
            .values()
            .filter(|i| i.is_active())
            .map(|i| i.id)
            .collect();
        info!("stopping {} servers", ids.len());
        let stops = ids.into_iter().map(|id| {
            let me = self.clone();
            async move {
                if let Err(e) = me.stop(id, false).await {
                    warn!(server = id, "stopping: {e:#}");
                }
            }
        });
        futures_util::future::join_all(stops).await;
        if let Some(reacher) = &self.0.reacher {
            reacher.close_all().await;
        }
    }

    /// How many servers are running, starting or installing.
    pub fn active(&self) -> usize {
        self.0
            .instances
            .lock()
            .values()
            .filter(|i| i.is_active())
            .count()
    }

    /// Measures servers now and then: memory and CPU every few seconds, disk use
    /// every few minutes.
    pub async fn measure(self) {
        let mut tick = tokio::time::interval(Duration::from_secs(5));
        let mut rounds: u64 = 0;
        loop {
            tick.tick().await;
            if rounds.is_multiple_of(120) {
                self.measure_disks().await;
            }
            rounds += 1;
            let instances: Vec<Arc<Instance>> = self.0.instances.lock().values().cloned().collect();
            for inst in instances {
                let Some(pid) = *inst.pid.lock() else {
                    continue;
                };
                let procs = inst.cgroup.lock().clone();
                let stats = match procs {
                    Some(procs) => self.0.cgroups.stats(&procs),
                    None => tokio::task::spawn_blocking(move || tree_stats(pid))
                        .await
                        .ok()
                        .flatten(),
                };
                let Some((memory, cpu_usec)) = stats else {
                    continue;
                };
                let now = Instant::now();
                let cpu_percent = match inst.last_cpu.lock().replace((now, cpu_usec)) {
                    Some((then, before)) => {
                        let wall = now.duration_since(then).as_micros().max(1) as f64;
                        (cpu_usec.saturating_sub(before) as f64 / wall * 100.0) as f32
                    }
                    None => 0.0,
                };
                *inst.usage.lock() = Some(ServerUsage {
                    memory_bytes: memory,
                    cpu_percent,
                });
            }
        }
    }

    async fn measure_disks(&self) {
        let rows = self.0.store.list_servers().unwrap_or_default();
        for row in rows {
            let dir = self.0.cfg.server_dir(row.id);
            if let Ok(bytes) = tokio::task::spawn_blocking(move || fsops::disk_usage(&dir)).await {
                self.0.disk.lock().insert(row.id, bytes);
            }
        }
    }

    /// Makes running servers' files whole for a backup: their game's `before`
    /// commands, waiting for the line saying they are done.
    pub async fn before_backup(&self) -> Vec<String> {
        self.backup_step(true).await
    }

    pub async fn after_backup(&self) -> Vec<String> {
        self.backup_step(false).await
    }

    async fn backup_step(&self, before: bool) -> Vec<String> {
        let mut problems = Vec::new();
        for row in self.0.store.list_servers().unwrap_or_default() {
            let inst = self.instance(row.id);
            if inst.status().state != ServerState::Running {
                continue;
            }
            let Some(backup) = self.game(&row).ok().and_then(|g| g.backup) else {
                continue;
            };
            let commands = if before {
                &backup.before
            } else {
                &backup.after
            };
            if commands.is_empty() {
                continue;
            }
            let mut rx = inst.console.subscribe();
            for command in commands {
                if let Err(e) = self.send(row.id, command) {
                    problems.push(format!("{}: {e:#}", row.name));
                }
            }
            if let (true, Some(text)) = (before, &backup.wait_for) {
                let waited = tokio::time::timeout(Duration::from_secs(120), async {
                    while let Ok(line) = rx.recv().await {
                        if line.contains(text.as_str()) {
                            return true;
                        }
                    }
                    false
                })
                .await;
                if !matches!(waited, Ok(true)) {
                    problems.push(format!("{}: did not confirm it saved", row.name));
                }
            }
        }
        problems
    }
}

/// What a server's templates may use.
pub fn context(
    game: &GameDef,
    row: &ServerRow,
    settings: &BTreeMap<String, Value>,
    dir: &Path,
    java: Option<&Path>,
    base: Option<&Path>,
) -> Ctx {
    let mut ctx = Ctx::new();
    for port in &game.ports {
        if let Some(p) = row.ports.get(&port.name) {
            ctx.insert(format!("port.{}", port.name), Value::Number(i64::from(*p)));
        }
    }
    for (k, v) in settings {
        ctx.insert(format!("setting.{k}"), v.clone());
    }
    let memory = row.memory_mb as i64;
    ctx.insert("memory_mb".into(), Value::Number(memory));
    // The JVM needs room of its own beside its heap.
    ctx.insert(
        "java_memory_mb".into(),
        Value::Number((memory - (memory / 4).max(256)).max(256)),
    );
    ctx.insert("dir".into(), Value::Text(dir.display().to_string()));
    ctx.insert(
        "base".into(),
        Value::Text(base.map(|b| b.display().to_string()).unwrap_or_default()),
    );
    ctx.insert(
        "java".into(),
        Value::Text(
            java.map(|j| j.display().to_string())
                .unwrap_or_else(|| "java".into()),
        ),
    );
    ctx
}

/// Why the game's program is not there, and where files of its name are: what
/// fixes a game definition whose command names the wrong path.
async fn missing_program(program: &Path, dir: &Path, uid: u32) -> String {
    let name = program
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    let root = dir.to_path_buf();
    let found = fsops::as_user(uid, move || {
        let mut found = Vec::new();
        let mut stack = vec![(root.clone(), 0)];
        while let Some((d, depth)) = stack.pop() {
            for entry in std::fs::read_dir(&d)?.flatten() {
                let path = entry.path();
                let kind = entry.file_type()?;
                if kind.is_dir() && depth < 4 {
                    stack.push((path.clone(), depth + 1));
                }
                if entry.file_name() == name {
                    found.push(
                        path.strip_prefix(&root)
                            .unwrap_or(&path)
                            .display()
                            .to_string(),
                    );
                }
            }
        }
        Ok(found)
    })
    .await
    .unwrap_or_default();
    let shown = program
        .strip_prefix(dir)
        .unwrap_or(program)
        .display()
        .to_string();
    if found.is_empty() {
        let root = dir.to_path_buf();
        let mut top = fsops::as_user(uid, move || {
            Ok(std::fs::read_dir(&root)?
                .flatten()
                .map(|e| e.file_name().to_string_lossy().to_string())
                .filter(|n| !n.starts_with('.'))
                .collect::<Vec<_>>())
        })
        .await
        .unwrap_or_default();
        top.sort();
        top.truncate(40);
        format!(
            "the game's program {shown} is not in the server's directory, which holds: {}",
            top.join(", ")
        )
    } else {
        format!(
            "the game's program is not at {shown}; files of that name: {}",
            found.join(", ")
        )
    }
}

/// Spawns `cmd` as the server's user, in its cgroup, in a session of its own
/// (so signals reach everything it starts); on the desktop, kept to its files.
fn spawn(
    mut cmd: tokio::process::Command,
    run_as: &RunAs,
) -> anyhow::Result<tokio::process::Child> {
    let procs = run_as
        .cgroup
        .as_ref()
        .map(|p| std::ffi::CString::new(p.as_os_str().as_encoded_bytes()))
        .transpose()?;
    let uid = run_as.uid;
    let switch_user = !fsops::servers_run_as_self();
    let sandbox = run_as.sandbox.as_ref().map(|s| s.raw_fd());
    cmd.stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    // SAFETY: between fork and exec only async-signal-safe calls are made, on data
    // prepared before the fork.
    unsafe {
        cmd.pre_exec(move || {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            if let Some(procs) = &procs {
                // Writing 0 moves the writer itself.
                let fd = libc::open(procs.as_ptr(), libc::O_WRONLY | libc::O_CLOEXEC);
                if fd < 0 || libc::write(fd, c"0".as_ptr().cast(), 1) != 1 {
                    return Err(std::io::Error::last_os_error());
                }
                libc::close(fd);
            }
            // If memory runs out, servers go before archipel.
            let fd = libc::open(
                c"/proc/self/oom_score_adj".as_ptr(),
                libc::O_WRONLY | libc::O_CLOEXEC,
            );
            if fd >= 0 {
                libc::write(fd, c"500".as_ptr().cast(), 3);
                libc::close(fd);
            }
            // Its own user last: joining the cgroup takes root. (std's `uid` would
            // switch before these closures run.)
            if switch_user
                && (libc::setgroups(0, std::ptr::null()) != 0
                    || libc::setgid(uid) != 0
                    || libc::setuid(uid) != 0)
            {
                return Err(std::io::Error::last_os_error());
            }
            if !switch_user {
                // On the desktop, servers end with the app, even if it is killed.
                // (The signal comes when the thread that started them ends: a
                // runtime worker, which lives as long as the app.)
                #[cfg(target_os = "linux")]
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
            }
            if let Some(fd) = sandbox {
                crate::sandbox::enter(fd)?;
            }
            Ok(())
        });
    }
    let program = PathBuf::from(cmd.as_std().get_program());
    cmd.spawn().map_err(|e| match e.kind() {
        // The file is there, yet "not found": its ELF interpreter is not.
        std::io::ErrorKind::NotFound if program.is_file() => {
            anyhow!("{}", loader_missing(&program))
        }
        _ => e.into(),
    })
}

/// Why `program`, which is there, cannot start: the loader it asks for is not.
fn loader_missing(program: &Path) -> String {
    let loader = elf_interpreter(program);
    let bits32 = loader.as_deref() == Some("/lib/ld-linux.so.2");
    let fix = if bits32 {
        "It is a 32-bit program: install your system's 32-bit libraries (lib32gcc-s1 and \
         lib32stdc++6 on Debian and Ubuntu). NixOS has no 32-bit loader there, even with \
         programs.nix-ld, so 32-bit games do not run on it"
    } else {
        "On NixOS, turn on programs.nix-ld (or open archipel with appimage-run)"
    };
    format!(
        "{} cannot run on this system: it is built for usual Linux distributions, and the \
         loader it needs ({}) is missing. {fix}",
        program.display(),
        loader.unwrap_or_else(|| "/lib64/ld-linux-x86-64.so.2".into()),
    )
}

/// The dynamic loader an ELF program asks for (its `PT_INTERP`), if it says:
/// 64-bit or 32-bit.
fn elf_interpreter(program: &Path) -> Option<String> {
    let bytes = std::fs::read(program).ok()?;
    if bytes.get(..4)? != b"\x7fELF" {
        return None; // a script: its interpreter is on the first line
    }
    let wide = match *bytes.get(4)? {
        1 => false,
        2 => true,
        _ => return None,
    };
    let u16_at = |at: usize| Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?));
    let u32_at = |at: usize| Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?));
    let u64_at = |at: usize| Some(u64::from_le_bytes(bytes.get(at..at + 8)?.try_into().ok()?));
    // Where the program headers are, and where in one its offset and size are.
    let (phoff, phentsize, phnum, field_off, field_size) = if wide {
        (u64_at(0x20)? as usize, u16_at(0x36)?, u16_at(0x38)?, 8, 32)
    } else {
        (u32_at(0x1c)? as usize, u16_at(0x2a)?, u16_at(0x2c)?, 4, 16)
    };
    let word = |at: usize| {
        if wide {
            u64_at(at).map(|v| v as usize)
        } else {
            u32_at(at).map(|v| v as usize)
        }
    };
    (0..phnum as usize).find_map(|i| {
        let h = phoff + i * phentsize as usize;
        if u32_at(h)? != 3 {
            return None;
        }
        let (off, size) = (word(h + field_off)?, word(h + field_size)?);
        let path = bytes.get(off..off + size)?;
        Some(
            String::from_utf8_lossy(path)
                .trim_end_matches('\0')
                .to_string(),
        )
    })
}

/// Copies a child's stdout and stderr into the console, line by line, showing
/// each to `watch`. The returned future ends with them.
fn pipe_output(
    out: Option<tokio::process::ChildStdout>,
    err: Option<tokio::process::ChildStderr>,
    console: Console,
    // Whether to show the line.
    watch: impl Fn(&str) -> bool + Send + Sync + 'static,
) -> impl std::future::Future<Output = ()> + Send + 'static {
    let watch = Arc::new(watch);
    let mut tasks = Vec::new();
    fn show(console: &Console, line: String) {
        // A shell could not start a program, though it is there: say why.
        let hint = line
            .split_once(": cannot execute: required file not found")
            .map(|(before, _)| before.rsplit(": ").next().unwrap_or(before).to_string())
            .map(PathBuf::from)
            .filter(|p| p.is_file())
            .map(|p| loader_missing(&p));
        console.line(line);
        if let Some(hint) = hint {
            console.system(hint);
        }
    }
    if let Some(out) = out {
        let (console, watch) = (console.clone(), watch.clone());
        tasks.push(tokio::spawn(async move {
            let mut lines = tokio::io::BufReader::new(out).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if watch(&line) {
                    show(&console, line);
                }
            }
        }));
    }
    if let Some(err) = err {
        tasks.push(tokio::spawn(async move {
            let mut lines = tokio::io::BufReader::new(err).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if watch(&line) {
                    show(&console, line);
                }
            }
        }));
    }
    async move {
        for t in tasks {
            let _ = t.await;
        }
    }
}

/// Runs an install step's command (SteamCMD) as the server's user, its output in
/// the console. Fails on a non-zero exit.
pub async fn run_logged(
    cmd: tokio::process::Command,
    run_as: &RunAs,
    console: &Console,
) -> anyhow::Result<()> {
    run_logged_with(cmd, run_as, console, |_| true).await
}

/// [`run_logged`], showing only the lines `show` keeps.
pub async fn run_logged_with(
    cmd: tokio::process::Command,
    run_as: &RunAs,
    console: &Console,
    show: impl Fn(&str) -> bool + Send + Sync + 'static,
) -> anyhow::Result<()> {
    let mut child = spawn(cmd, run_as)?;
    let readers = pipe_output(
        child.stdout.take(),
        child.stderr.take(),
        console.clone(),
        show,
    );
    let status = child.wait().await?;
    readers.await;
    if !status.success() {
        bail!("exited with {status}");
    }
    Ok(())
}

/// Memory (resident bytes) and CPU time (microseconds) of a process and all it
/// started: the desktop's servers have no cgroup to read them from.
fn tree_stats(root: i32) -> Option<(u64, u64)> {
    // SAFETY: sysconf has no preconditions.
    let (page, ticks) = unsafe {
        (
            libc::sysconf(libc::_SC_PAGESIZE).max(1) as u64,
            libc::sysconf(libc::_SC_CLK_TCK).max(1) as u64,
        )
    };
    let mut children: HashMap<i32, Vec<i32>> = HashMap::new();
    let mut found = HashMap::new();
    for entry in std::fs::read_dir("/proc").ok()?.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<i32>().ok())
        else {
            continue;
        };
        let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
            continue;
        };
        // After the name, in parentheses and possibly holding spaces: state, ppid…
        let Some((_, rest)) = stat.rsplit_once(')') else {
            continue;
        };
        let fields: Vec<&str> = rest.split_whitespace().collect();
        let field = |n: usize| {
            fields
                .get(n)
                .and_then(|f| f.parse::<u64>().ok())
                .unwrap_or(0)
        };
        let ppid = field(1) as i32;
        // utime and stime, then rss in pages.
        let cpu = field(11) + field(12);
        let rss = field(21);
        children.entry(ppid).or_default().push(pid);
        found.insert(pid, (rss * page, cpu * 1_000_000 / ticks));
    }
    found.get(&root)?;
    let (mut memory, mut cpu) = (0, 0);
    let mut stack = vec![root];
    while let Some(pid) = stack.pop() {
        if let Some((m, c)) = found.get(&pid) {
            memory += m;
            cpu += c;
        }
        stack.extend(children.get(&pid).into_iter().flatten());
    }
    Some((memory, cpu))
}

fn signal_group(pid: i32, signal: i32) {
    // SAFETY: a plain kill(2) to the process group the server leads.
    unsafe {
        libc::kill(-pid, signal);
    }
}

fn parse_signal(name: &str) -> i32 {
    match name.trim_start_matches("SIG") {
        "INT" => libc::SIGINT,
        "QUIT" => libc::SIGQUIT,
        "HUP" => libc::SIGHUP,
        "USR1" => libc::SIGUSR1,
        "USR2" => libc::SIGUSR2,
        "KILL" => libc::SIGKILL,
        _ => libc::SIGTERM,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_programs_loader() {
        let loader = elf_interpreter(Path::new("/bin/ls")).expect("ls has a loader");
        assert!(
            loader.starts_with('/') && loader.contains("ld-"),
            "{loader}"
        );
        assert_eq!(elf_interpreter(Path::new("/etc/hostname")), None);
    }

    #[test]
    fn reads_a_32_bit_programs_loader() {
        // An ELF32 header, one PT_INTERP program header, then the loader's path.
        let interp = b"/lib/ld-linux.so.2\0";
        let mut elf = vec![0u8; 84];
        elf[..6].copy_from_slice(b"\x7fELF\x01\x01");
        elf[0x1c..0x20].copy_from_slice(&52u32.to_le_bytes());
        elf[0x2a..0x2c].copy_from_slice(&32u16.to_le_bytes());
        elf[0x2c..0x2e].copy_from_slice(&1u16.to_le_bytes());
        elf[52..56].copy_from_slice(&3u32.to_le_bytes());
        elf[56..60].copy_from_slice(&84u32.to_le_bytes());
        elf[68..72].copy_from_slice(&(interp.len() as u32).to_le_bytes());
        elf.extend_from_slice(interp);
        let path = std::env::temp_dir().join(format!("archipel-elf32-{}", std::process::id()));
        std::fs::write(&path, elf).unwrap();
        assert_eq!(
            elf_interpreter(&path).as_deref(),
            Some("/lib/ld-linux.so.2")
        );
        assert!(loader_missing(&path).contains("32-bit"));
        let _ = std::fs::remove_file(path);
    }
}
