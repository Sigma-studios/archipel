//! Keeping a desktop game server to its own files, with Landlock.
//!
//! On the desktop every server runs as the person using the computer, so without
//! more a server (its plugins, its mods) could read their home folder. Landlock lets
//! an unprivileged program give up access to the filesystem, for good, before it
//! starts another: the server gets its own directory, the game files archipel
//! shares, and what any program needs to run (system libraries, `/tmp`, `/dev`,
//! `/proc`); everything else, the home folder and the other servers included, is out
//! of its reach. The network is not restricted.
//!
//! The ruleset is built in archipel, before the server is started; the server's
//! process applies it to itself between `fork` and `exec` ([`enter`]), with system
//! calls only, as must be done there.

use std::path::PathBuf;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::{Sandbox, enter};

/// What a server may reach.
pub struct Allowed {
    /// Everything there: its own directory.
    pub own: PathBuf,
    /// Read and run: the game files and runtimes archipel shares.
    pub read: Vec<PathBuf>,
}

#[cfg(not(target_os = "linux"))]
pub struct Sandbox;

#[cfg(not(target_os = "linux"))]
impl Sandbox {
    pub fn prepare(_: &Allowed) -> Result<Sandbox, String> {
        Err("Landlock is Linux's".into())
    }

    pub fn raw_fd(&self) -> i32 {
        -1
    }
}

/// # Safety
/// Nothing to do outside Linux.
#[cfg(not(target_os = "linux"))]
pub unsafe fn enter(_: i32) -> std::io::Result<()> {
    Ok(())
}
