//! Backup hooks: `archipel hook before-backup` and `after-backup`, which facade
//! runs in archipel's container around a backup of its volume. They ask the
//! running archipel, over a socket only root can reach, to have each running server
//! save its files and pause saving (`save-off`), then resume.

use std::path::Path;

use anyhow::{Context, bail};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tracing::{info, warn};

use crate::supervisor::Supervisor;

pub const SOCKET: &str = "/run/archipel/hooks.sock";

pub async fn listen(supervisor: Supervisor) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let path = Path::new(SOCKET);
    let dir = path.parent().unwrap_or(path);
    std::fs::create_dir_all(dir)?;
    // Servers run as other users: they cannot reach it.
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    let _ = std::fs::remove_file(path);
    let listener = UnixListener::bind(path).with_context(|| format!("listening on {SOCKET}"))?;
    loop {
        let (stream, _) = listener.accept().await?;
        let supervisor = supervisor.clone();
        tokio::spawn(async move {
            if let Err(e) = answer(stream, supervisor).await {
                warn!("backup hook: {e:#}");
            }
        });
    }
}

async fn answer(stream: UnixStream, supervisor: Supervisor) -> anyhow::Result<()> {
    let (read, mut write) = stream.into_split();
    let mut line = String::new();
    BufReader::new(read).read_line(&mut line).await?;
    let problems = match line.trim() {
        "before" => supervisor.before_backup().await,
        "after" => supervisor.after_backup().await,
        other => vec![format!("unknown hook `{other}`")],
    };
    info!("backup hook `{}`: {} problems", line.trim(), problems.len());
    let reply = if problems.is_empty() {
        "ok\n".to_string()
    } else {
        format!("error {}\n", problems.join("; "))
    };
    write.write_all(reply.as_bytes()).await?;
    Ok(())
}

/// The client side, for `archipel hook <step>`.
pub async fn call(step: &str) -> anyhow::Result<()> {
    let mut stream = UnixStream::connect(SOCKET)
        .await
        .context("archipel is not running here")?;
    stream.write_all(format!("{step}\n").as_bytes()).await?;
    let mut reply = String::new();
    BufReader::new(stream).read_line(&mut reply).await?;
    match reply.trim().strip_prefix("error ") {
        None if reply.trim() == "ok" => Ok(()),
        Some(problems) => bail!("{problems}"),
        None => bail!("unexpected answer: {}", reply.trim()),
    }
}
