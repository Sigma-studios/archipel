//! File work in a server's directory, done as the server's own user.
//!
//! archipel runs as root, and a server's files belong to a program that may be
//! hostile (plugins and mods are code): a symlink planted there must not make
//! archipel read or write anything else. So file work happens on a thread whose
//! filesystem identity (`setfsuid`) is the server's user, which drops root's
//! file powers for that thread: the kernel checks every path as that user.

use std::io;
use std::path::{Path, PathBuf};

/// First uid (and gid) given to servers: server `n` runs as `UID_BASE + n`.
pub const UID_BASE: u32 = 20000;

pub fn uid_of(server: u64) -> u32 {
    UID_BASE + server as u32
}

/// Runs `work` with the filesystem identity of `uid`.
pub async fn as_user<T, F>(uid: u32, work: F) -> io::Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> io::Result<T> + Send + 'static,
{
    tokio::task::spawn_blocking(move || {
        // A thread of its own: its identity changes for good.
        std::thread::spawn(move || {
            become_user(uid)?;
            work()
        })
        .join()
        .map_err(|_| io::Error::other("file work panicked"))?
    })
    .await
    .map_err(io::Error::other)?
}

#[cfg(target_os = "linux")]
fn become_user(uid: u32) -> io::Result<()> {
    // SAFETY: plain system calls affecting this thread only.
    unsafe {
        libc::setfsgid(uid);
        libc::setfsuid(uid);
        // Both return the previous id: asking again tells whether it took.
        if libc::setfsuid(uid) as u32 != uid || libc::setfsgid(uid) as u32 != uid {
            return Err(io::Error::other("cannot switch to the server's user"));
        }
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn become_user(_: u32) -> io::Result<()> {
    // Development elsewhere: no isolation.
    Ok(())
}

/// The server's directory, made (and owned by its user) if needed.
pub fn ensure_server_dir(dir: &Path, uid: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    if let Some(parent) = dir.parent() {
        std::fs::create_dir_all(parent)?;
        // Others may enter servers/ to reach their own directory, not list it.
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o711))?;
    }
    if !dir.exists() {
        std::fs::create_dir(dir)?;
    }
    chown(dir, uid)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
}

fn chown(path: &Path, uid: u32) -> io::Result<()> {
    std::os::unix::fs::chown(path, Some(uid), Some(uid))
}

/// `relative` under `dir`, refusing paths that leave it.
pub fn inside(dir: &Path, relative: &str) -> io::Result<PathBuf> {
    let rel = Path::new(relative);
    if relative.is_empty()
        || rel.is_absolute()
        || rel
            .components()
            .any(|c| !matches!(c, std::path::Component::Normal(_)))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("`{relative}` is not a path inside the server"),
        ));
    }
    Ok(dir.join(rel))
}

/// Bytes the files under `dir` take on disk, links not followed.
pub fn disk_usage(dir: &Path) -> u64 {
    use std::os::unix::fs::MetadataExt;
    let mut total = 0;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(path) = stack.pop() {
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        total += meta.blocks() * 512;
        if meta.is_dir()
            && let Ok(entries) = std::fs::read_dir(&path)
        {
            stack.extend(entries.flatten().map(|e| e.path()));
        }
    }
    total
}
