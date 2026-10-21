//! Attaching to a session, and remembering which one muxbox showed last.

use crate::config::{Host, home};
use crate::remote::{Remote, quote};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::Command;

/// argv for a terminal attached to `session` on `host`.
pub fn attach_argv(host: &Host, session: &str) -> Vec<String> {
    let r = Remote::new(host);
    r.interactive(&format!(
        "unset TMUX; exec {} attach -t ={}",
        r.tmux(),
        quote(session)
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

/// The session muxbox last showed, for `muxbox paste` and `upload`.
#[derive(Serialize, Deserialize)]
pub struct Active {
    pub host: String,
    pub session: String,
}

impl Active {
    fn path() -> Result<PathBuf> {
        Ok(home()?.join(".muxbox/active.toml"))
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::path()?;
        std::fs::create_dir_all(path.parent().expect("has parent"))?;
        std::fs::write(&path, toml::to_string(self)?)?;
        Ok(())
    }

    pub fn load() -> Result<Active> {
        let path = Self::path()?;
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        Ok(toml::from_str(&text)?)
    }
}
