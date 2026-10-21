use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::path::PathBuf;

/// Where `claude -w` puts worktrees; bases using it let Claude create them.
pub const CLAUDE_WORKTREES: &str = ".claude/worktrees";

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub theme: ThemeChoice,
    #[serde(default)]
    pub hosts: Vec<Host>,
    #[serde(default)]
    pub bases: Vec<Base>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeChoice {
    /// Follow macOS light/dark.
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Host {
    pub name: String,
    /// SSH destination (`user@host` or a `~/.ssh/config` alias), or "local" to run on this machine.
    pub ssh: String,
    #[serde(default = "default_upload_dir")]
    pub upload_dir: String,
    /// tmux socket name (`tmux -L`); unset uses the default server.
    pub tmux_socket: Option<String>,
    /// Command that starts Claude Code in a new session.
    #[serde(default = "default_claude")]
    pub claude: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Base {
    pub host: String,
    pub path: String,
    /// Display name and session-name prefix; defaults to the last path component.
    pub name: Option<String>,
    /// Where new worktrees go: relative to the repo, absolute, or `~/...`.
    #[serde(default = "default_worktree_dir")]
    pub worktree_dir: String,
}

fn default_upload_dir() -> String {
    "~/.muxbox/uploads".into()
}

fn default_claude() -> String {
    "claude".into()
}

fn default_worktree_dir() -> String {
    CLAUDE_WORKTREES.into()
}

impl Base {
    pub fn name(&self) -> String {
        let name = match &self.name {
            Some(n) => n.clone(),
            None => self
                .path
                .trim_end_matches('/')
                .rsplit('/')
                .next()
                .unwrap_or_default()
                .to_string(),
        };
        // tmux reserves `.` and `:` in target names.
        name.replace(['.', ':'], "-")
    }

    pub fn uses_claude_worktrees(&self) -> bool {
        self.worktree_dir.trim_end_matches('/') == CLAUDE_WORKTREES
    }
}

impl Config {
    pub fn path() -> Result<PathBuf> {
        if let Some(p) = std::env::var_os("MUXBOX_CONFIG") {
            return Ok(p.into());
        }
        Ok(home()?.join(".config/muxbox/config.toml"))
    }

    pub fn load() -> Result<Config> {
        let path = Self::path()?;
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        let cfg: Config =
            toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        for b in &cfg.bases {
            if cfg.host(&b.host).is_none() {
                bail!("base {} refers to unknown host {}", b.path, b.host);
            }
        }
        Ok(cfg)
    }

    pub fn host(&self, name: &str) -> Option<&Host> {
        self.hosts.iter().find(|h| h.name == name)
    }

    pub fn host_or_err(&self, name: &str) -> Result<&Host> {
        self.host(name)
            .with_context(|| format!("unknown host {name}"))
    }

    pub fn base_by_name(&self, name: &str) -> Option<usize> {
        self.bases.iter().position(|b| b.name() == name)
    }
}

pub fn home() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME is not set")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_defaults_to_system() {
        let cfg: Config = toml::from_str("").unwrap();
        assert_eq!(cfg.theme, ThemeChoice::System);
        let cfg: Config = toml::from_str("theme = \"light\"").unwrap();
        assert_eq!(cfg.theme, ThemeChoice::Light);
        assert!(toml::from_str::<Config>("theme = \"blue\"").is_err());
    }
}
