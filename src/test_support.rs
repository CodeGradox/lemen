//! Helpers for tests that run real tmux and real processes.

use crate::config::Config;
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
            socket: format!("muxbox-test-{tag}-{}", std::process::id()),
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
