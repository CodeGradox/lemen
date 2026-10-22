//! Attaching to a session, and remembering which one lemur showed last.

use crate::config::{Host, home, read_toml};
use crate::remote::{Remote, target};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;

/// argv for a terminal attached to `session` on `host`.
pub fn attach_argv(host: &Host, session: &str) -> Vec<String> {
    let r = Remote::new(host);
    r.interactive(&format!(
        "unset TMUX; exec {} attach -t {}",
        r.tmux(),
        target(session)
    ))
}

/// Attaches this terminal to a session until you detach.
pub fn attach_here(host: &Host, session: &str) -> Result<()> {
    let argv = attach_argv(host, session);
    let _ = Active {
        host: host.name.clone(),
        session: session.to_string(),
    }
    .save();
    let status = Command::new(&argv[0])
        .args(&argv[1..])
        .status()
        .context("starting attach")?;
    if !status.success() {
        bail!("attach to {session} exited with {status}");
    }
    Ok(())
}

/// The session lemur last showed, for `lemur paste` and `upload`.
#[derive(Serialize, Deserialize)]
pub struct Active {
    pub host: String,
    pub session: String,
}

impl Active {
    /// Where lemur remembers it.
    pub fn path() -> Result<PathBuf> {
        Ok(home()?.join(".lemur/active.toml"))
    }

    pub fn save(&self) -> Result<()> {
        self.save_to(&Self::path()?)
    }

    pub fn save_to(&self, path: &Path) -> Result<()> {
        let dir = path.parent().expect("has parent");
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        std::fs::write(path, toml::to_string(self)?)
            .with_context(|| format!("writing {}", path.display()))
    }

    pub fn load() -> Result<Active> {
        Self::load_from(&Self::path()?)
    }

    pub fn load_from(path: &Path) -> Result<Active> {
        read_toml(path)
    }
}
