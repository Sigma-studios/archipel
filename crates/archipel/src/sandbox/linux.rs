use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use super::Allowed;

const SYS_CREATE_RULESET: libc::c_long = 444;
const SYS_ADD_RULE: libc::c_long = 445;
const SYS_RESTRICT_SELF: libc::c_long = 446;
const CREATE_RULESET_VERSION: u32 = 1;
const RULE_PATH_BENEATH: libc::c_int = 1;

const EXECUTE: u64 = 1 << 0;
const WRITE_FILE: u64 = 1 << 1;
const READ_FILE: u64 = 1 << 2;
const READ_DIR: u64 = 1 << 3;
/// Every access right of the first ABI: removing, making files, links, devices…
const ABI1_ALL: u64 = (1 << 13) - 1;
const REFER: u64 = 1 << 13;
const TRUNCATE: u64 = 1 << 14;
const IOCTL_DEV: u64 = 1 << 15;

const READ: u64 = EXECUTE | READ_FILE | READ_DIR;

/// What any program needs to read and run.
const SYSTEM: &[&str] = &[
    "/usr", "/lib", "/lib32", "/lib64", "/bin", "/sbin", "/etc", "/opt", "/nix", "/run", "/proc",
    "/sys", "/snap",
];

#[repr(C)]
struct RulesetAttr {
    handled_access_fs: u64,
}

#[repr(C, packed)]
struct PathBeneathAttr {
    allowed_access: u64,
    parent_fd: i32,
}

/// A ruleset ready for a server's process to apply to itself.
pub struct Sandbox {
    fd: OwnedFd,
}

impl Sandbox {
    /// The ruleset for `allowed`, or why Landlock cannot be used here (a kernel
    /// older than 5.13, or Landlock turned off).
    pub fn prepare(allowed: &Allowed) -> Result<Sandbox, String> {
        // SAFETY: plain system calls; the attributes outlive them.
        let abi = unsafe {
            libc::syscall(
                SYS_CREATE_RULESET,
                std::ptr::null::<RulesetAttr>(),
                0usize,
                CREATE_RULESET_VERSION,
            )
        };
        if abi < 1 {
            return Err(match io::Error::last_os_error().raw_os_error() {
                Some(libc::ENOSYS) => "this kernel has no Landlock (it needs Linux 5.13)".into(),
                Some(libc::EOPNOTSUPP) => "Landlock is turned off on this computer".into(),
                _ => format!("Landlock is unavailable: {}", io::Error::last_os_error()),
            });
        }
        let handled = ABI1_ALL
            | if abi >= 2 { REFER } else { 0 }
            | if abi >= 3 { TRUNCATE } else { 0 }
            | if abi >= 5 { IOCTL_DEV } else { 0 };
        let attr = RulesetAttr {
            handled_access_fs: handled,
        };
        // SAFETY: as above.
        let fd = unsafe {
            libc::syscall(
                SYS_CREATE_RULESET,
                &attr as *const RulesetAttr,
                std::mem::size_of::<RulesetAttr>(),
                0u32,
            )
        };
        if fd < 0 {
            return Err(format!("Landlock: {}", io::Error::last_os_error()));
        }
        // SAFETY: a new file descriptor, ours alone.
        let sandbox = Sandbox {
            fd: unsafe { OwnedFd::from_raw_fd(fd as i32) },
        };
        let devices = READ_FILE | WRITE_FILE | READ_DIR | (handled & IOCTL_DEV);
        sandbox.allow(&allowed.own, handled)?;
        sandbox.allow(Path::new("/tmp"), handled)?;
        sandbox.allow(Path::new("/dev"), devices)?;
        for path in SYSTEM
            .iter()
            .map(Path::new)
            .chain(allowed.read.iter().map(PathBuf::as_path))
        {
            sandbox.allow(path, READ)?;
        }
        Ok(sandbox)
    }

    /// Lets the server reach `path` (if it exists) with `access`.
    fn allow(&self, path: &Path, access: u64) -> Result<(), String> {
        let Ok(dir) = std::fs::File::options()
            .read(true)
            .custom_flags(libc::O_PATH | libc::O_CLOEXEC)
            .open(path)
        else {
            return Ok(());
        };
        let rule = PathBeneathAttr {
            allowed_access: access,
            parent_fd: dir.as_raw_fd(),
        };
        // SAFETY: plain system call; the rule and the descriptor outlive it.
        let done = unsafe {
            libc::syscall(
                SYS_ADD_RULE,
                self.fd.as_raw_fd(),
                RULE_PATH_BENEATH,
                &rule as *const PathBeneathAttr,
                0u32,
            )
        };
        if done != 0 {
            return Err(format!(
                "Landlock refused a rule for {}: {}",
                path.display(),
                io::Error::last_os_error()
            ));
        }
        Ok(())
    }

    pub fn raw_fd(&self) -> i32 {
        self.fd.as_raw_fd()
    }
}

/// Applies the ruleset `fd` to the calling process, for good. Made of system calls
/// only: for `pre_exec`, between `fork` and `exec`.
///
/// # Safety
/// `fd` must be a Landlock ruleset descriptor.
pub unsafe fn enter(fd: i32) -> io::Result<()> {
    // SAFETY: plain system calls, async-signal-safe.
    unsafe {
        if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
            return Err(io::Error::last_os_error());
        }
        if libc::syscall(SYS_RESTRICT_SELF, fd, 0u32) != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::CommandExt;
    use std::process::Command;

    #[test]
    fn keeps_a_program_to_its_files() {
        // Not under /tmp, which servers may use.
        let base = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target")
            .join(format!("sandbox-test-{}", std::process::id()));
        let own = base.join("own");
        std::fs::create_dir_all(&own).unwrap();
        std::fs::write(own.join("mine.txt"), "mine").unwrap();
        std::fs::write(base.join("theirs.txt"), "theirs").unwrap();
        let sandbox = match Sandbox::prepare(&Allowed {
            own: own.clone(),
            read: vec![],
        }) {
            Ok(sandbox) => sandbox,
            Err(why) => {
                eprintln!("not tested: {why}");
                return;
            }
        };
        let run = |script: &str| {
            let fd = sandbox.raw_fd();
            let mut cmd = Command::new("/bin/sh");
            cmd.args(["-c", script]).current_dir(&base);
            // SAFETY: system calls only, on a ruleset descriptor.
            unsafe { cmd.pre_exec(move || enter(fd)) };
            cmd.status().unwrap().success()
        };
        assert!(run("cat own/mine.txt && echo more > own/new.txt"));
        assert!(!run("cat theirs.txt"));
        assert!(!run("echo changed > theirs.txt"));
        assert!(!run("ln -s ../theirs.txt own/link && cat own/link"));
        assert_eq!(
            std::fs::read_to_string(base.join("theirs.txt")).unwrap(),
            "theirs"
        );
        std::fs::remove_dir_all(&base).unwrap();
    }
}
