//! Helpers for tests that run real tmux and real processes.

use crate::config::Config;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

/// A tmux server on a private socket. Dropping it kills the server and
/// removes the socket file, so a failing test doesn't leave either behind.
pub struct TmuxServer {
    pub socket: String,
}

impl TmuxServer {
    pub fn new(tag: &str) -> TmuxServer {
        TmuxServer {
            socket: format!("lemen-test-{tag}-{}", std::process::id()),
        }
    }

    pub fn tmux(&self, args: &[&str]) -> String {
        let out = Command::new("tmux")
            .args(["-u", "-L", &self.socket])
            .args(args)
            .output()
            .expect("running tmux");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// A config with one host, `local`, that uses this server.
    pub fn config(&self) -> Config {
        toml::from_str(&format!(
            "[[hosts]]\nname = \"local\"\nssh = \"local\"\ntmux_socket = \"{}\"\n",
            self.socket
        ))
        .expect("test config")
    }
}

impl Drop for TmuxServer {
    fn drop(&mut self) {
        self.tmux(&["kill-server"]);
        // kill-server leaves the socket file; anything that tried to attach
        // created it even without a server.
        let uid = Command::new("id").arg("-u").output().expect("running id");
        let dir = std::env::var("TMUX_TMPDIR").unwrap_or_else(|_| "/tmp".into());
        let uid = String::from_utf8_lossy(&uid.stdout);
        let _ = std::fs::remove_file(format!("{dir}/tmux-{}/{}", uid.trim(), self.socket));
    }
}

/// A fresh folder in the temp dir, removed when dropped, so a failing
/// test doesn't leave it behind. Its path is the real one: macOS's /tmp is
/// a link, and git reports real paths.
pub struct TempDir(PathBuf);

impl TempDir {
    pub fn new(tag: &str) -> TempDir {
        let dir = std::env::temp_dir().join(format!("lemen-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("creating a temp dir");
        TempDir(dir.canonicalize().expect("the temp dir's real path"))
    }
}

impl std::ops::Deref for TempDir {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Runs git in `dir` as a throwaway author; it must succeed.
pub fn git_in(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(args)
        .current_dir(dir)
        .output()
        .expect("running git");
    assert!(out.status.success(), "git {args:?}: {out:?}");
}

/// Polls `done` until it holds or `timeout` passes; returns whether it held.
pub fn wait_until(timeout: Duration, mut done: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if done() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    done()
}
