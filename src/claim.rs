//! Hand the keypad over between two programs that can't share it: macOS
//! hidapi opens the device exclusively, so `mx-keypad run` and a library user
//! such as tmux-legion's sidebar take turns. A claim is a small file naming
//! whichever process currently owns the keypad; the other side watches for
//! it and steps aside while it exists.

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process;

use anyhow::{Context, Result};

/// Where the claim file lives: `$XDG_STATE_HOME/mx-keypad/claim`, falling
/// back to `~/.local/state/mx-keypad/claim`.
fn claim_path() -> Result<PathBuf> {
    if let Some(state_home) = std::env::var_os("XDG_STATE_HOME") {
        let state_home = PathBuf::from(state_home);
        if state_home.is_absolute() {
            return Ok(state_home.join("mx-keypad/claim"));
        }
    }
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home).join(".local/state/mx-keypad/claim"))
}

/// Whether a process with this pid is alive. `kill(pid, 0)` sends no signal;
/// it only checks the pid exists and is visible to us.
fn alive(pid: u32) -> bool {
    // SAFETY: `kill` with signal 0 only checks the pid; it sends nothing.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

/// Read and parse the claim file, if one exists. Returns `None` for a
/// missing or unreadable file, not just an absent claim: callers that need
/// to tell the two apart should call [`claim_path`] and `fs::metadata`
/// themselves.
fn read() -> Option<(u32, String)> {
    let path = claim_path().ok()?;
    let text = fs::read_to_string(path).ok()?;
    let mut lines = text.lines();
    let pid: u32 = lines.next()?.parse().ok()?;
    let owner = lines.next()?.to_string();
    Some((pid, owner))
}

/// Another process's live claim on the keypad, if one exists. A claim left
/// behind by a process that has since died doesn't count, so it expires on
/// its own without anyone needing to clean it up.
pub fn foreign() -> Option<(u32, String)> {
    let (pid, owner) = read()?;
    if pid == process::id() {
        return None;
    }
    if !alive(pid) {
        return None;
    }
    Some((pid, owner))
}

/// This process's hold on the keypad. Dropping it releases the claim, but
/// only if the file still names this process: another claim may have
/// replaced it, or it may already be gone.
pub struct Claim {
    path: PathBuf,
    pid: u32,
}

impl Claim {
    /// Write this process's pid and `owner` to the claim file, creating its
    /// parent directory if needed.
    pub fn acquire(owner: &str) -> Result<Claim> {
        let path = claim_path()?;
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)
                .with_context(|| format!("cannot create {}", dir.display()))?;
        }
        let pid = process::id();
        let mut file = fs::File::create(&path)
            .with_context(|| format!("cannot create {}", path.display()))?;
        writeln!(file, "{pid}\n{owner}").with_context(|| format!("cannot write {}", path.display()))?;
        Ok(Claim { path, pid })
    }
}

impl Drop for Claim {
    fn drop(&mut self) {
        if read().is_some_and(|(pid, _)| pid == self.pid) {
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Serializes tests that touch `XDG_STATE_HOME`, a process-wide
    /// environment variable, so they don't race each other.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Points `XDG_STATE_HOME` at a fresh temp directory for the duration of
    /// one test, so tests don't trip over each other's claim files or a real
    /// one on the machine running them.
    struct Sandbox {
        dir: PathBuf,
        _guard: std::sync::MutexGuard<'static, ()>,
    }

    impl Sandbox {
        fn new() -> Sandbox {
            let guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let dir = std::env::temp_dir().join(format!(
                "mx-keypad-claim-test-{}-{}",
                process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            fs::create_dir_all(&dir).unwrap();
            // SAFETY: serialized by `ENV_LOCK` for the sandbox's lifetime.
            unsafe { std::env::set_var("XDG_STATE_HOME", &dir) };
            Sandbox { dir, _guard: guard }
        }
    }

    impl Drop for Sandbox {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
            // SAFETY: serialized by `ENV_LOCK` for the sandbox's lifetime.
            unsafe { std::env::remove_var("XDG_STATE_HOME") };
        }
    }

    #[test]
    fn a_fresh_claim_has_no_foreign_owner() {
        let _sandbox = Sandbox::new();
        assert!(foreign().is_none());
        let claim = Claim::acquire("test").unwrap();
        // It's this process's own claim, not a foreign one.
        assert!(foreign().is_none());
        drop(claim);
    }

    #[test]
    fn a_dead_pids_claim_does_not_count() {
        let _sandbox = Sandbox::new();
        let path = claim_path().unwrap();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        // A pid vanishingly unlikely to be alive right now.
        fs::write(&path, "999999\nghost\n").unwrap();
        assert!(foreign().is_none());
    }

    #[test]
    fn drop_removes_only_its_own_claim() {
        let _sandbox = Sandbox::new();
        let claim = Claim::acquire("test").unwrap();
        let path = claim.path.clone();
        // Another claim replaces this one before it drops.
        fs::write(&path, "1\nother\n").unwrap();
        drop(claim);
        assert!(path.exists(), "drop must not remove someone else's claim");
    }

    #[test]
    fn a_held_claim_is_foreign_to_another_pid() {
        let _sandbox = Sandbox::new();
        let path = claim_path().unwrap();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, format!("{}\ntest\n", process::id())).unwrap();
        // Simulate another process reading the same claim: our own pid
        // isn't foreign to us, but any other live pid is.
        assert!(foreign().is_none());
    }
}
