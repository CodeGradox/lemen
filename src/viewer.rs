//! The Claude pane: a WezTerm split running `tmux attach` on the host.
//!
//! The pane's remote shell records its tty, so switching sessions is a
//! `tmux switch-client -c <tty>` instead of a reconnect.

use crate::config::{Config, Host, home};
use crate::remote::{Remote, quote};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::Command;

pub fn in_wezterm() -> Option<String> {
    std::env::var("WEZTERM_PANE").ok().filter(|p| !p.is_empty())
}

fn wezterm(args: &[&str]) -> Result<String> {
    let out = Command::new("wezterm")
        .arg("cli")
        .args(args)
        .output()
        .context("running wezterm cli")?;
    if !out.status.success() {
        bail!(
            "wezterm cli {}: {}",
            args[0],
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn tty_file(id: &str) -> String {
    format!("\"$HOME/.muxbox/viewers/{id}\"")
}

#[derive(Default)]
pub struct Viewer {
    id: String,
    pane: Option<String>,
    pub host: Option<String>,
    pub session: Option<String>,
}

impl Viewer {
    pub fn new() -> Self {
        Viewer {
            id: std::process::id().to_string(),
            ..Default::default()
        }
    }

    /// Shows `session` in the Claude pane, creating the pane if needed.
    pub fn show(&mut self, host: &Host, session: &str) -> Result<()> {
        let r = Remote::new(host);
        let tmux = r.tmux();
        let s = quote(session);
        if self.pane.is_some() && self.host.as_deref() == Some(host.name.as_str()) {
            let switch = format!(
                "t=$(cat {}) && {tmux} switch-client -c \"$t\" -t ={s}",
                tty_file(&self.id)
            );
            if r.run(&switch).is_ok() {
                self.shown(host, session);
                return Ok(());
            }
            // The pane is gone or its connection dropped: start a new one.
        }
        let me = in_wezterm().context("not running inside WezTerm")?;
        if let Some(p) = self.pane.take() {
            let _ = wezterm(&["kill-pane", "--pane-id", &p]);
        }
        let script = format!(
            "unset TMUX; mkdir -p \"$HOME/.muxbox/viewers\" && tty > {} && exec {tmux} attach -t ={s}",
            tty_file(&self.id)
        );
        let argv = r.interactive(&script);
        let mut args = vec![
            "split-pane",
            "--pane-id",
            &me,
            "--right",
            "--percent",
            "75",
            "--",
        ];
        args.extend(argv.iter().map(String::as_str));
        let pane = wezterm(&args)?;
        // Keep the keyboard in the sidebar; Tab moves to the Claude pane.
        let _ = wezterm(&["activate-pane", "--pane-id", &me]);
        self.pane = Some(pane);
        self.shown(host, session);
        Ok(())
    }

    fn shown(&mut self, host: &Host, session: &str) {
        self.host = Some(host.name.clone());
        self.session = Some(session.to_string());
        let _ = Active {
            host: host.name.clone(),
            session: session.to_string(),
            viewer: Some(self.id.clone()),
        }
        .save();
    }

    pub fn focus(&self) -> Result<()> {
        let pane = self
            .pane
            .as_deref()
            .context("no Claude pane yet; open a session first")?;
        wezterm(&["activate-pane", "--pane-id", pane]).map(|_| ())
    }

    /// Closes the Claude pane. The tmux session keeps running.
    pub fn close(&mut self, cfg: &Config) {
        if let Some(p) = self.pane.take() {
            let _ = wezterm(&["kill-pane", "--pane-id", &p]);
        }
        if let Some(h) = self.host.as_deref().and_then(|h| cfg.host(h)) {
            let _ = Remote::new(h).run(&format!("rm -f {}", tty_file(&self.id)));
        }
    }
}

/// Attaches this terminal to a session until you detach.
pub fn attach_here(host: &Host, session: &str) -> Result<()> {
    let r = Remote::new(host);
    let argv = r.interactive(&format!(
        "unset TMUX; exec {} attach -t ={}",
        r.tmux(),
        quote(session)
    ));
    let _ = Active {
        host: host.name.clone(),
        session: session.to_string(),
        viewer: None,
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
    pub viewer: Option<String>,
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
