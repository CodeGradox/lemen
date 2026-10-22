use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};

/// Where `claude -w` puts worktrees; bases using it let Claude create them.
pub const CLAUDE_WORKTREES: &str = ".claude/worktrees";

/// Terminal font sizes lemen accepts, in points.
pub const FONT_SIZES: RangeInclusive<f64> = 6.0..=72.0;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub theme: ThemeChoice,
    #[serde(default)]
    pub terminal: Terminal,
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

impl ThemeChoice {
    pub const ALL: [ThemeChoice; 3] = [ThemeChoice::System, ThemeChoice::Light, ThemeChoice::Dark];

    /// The name the config file uses.
    pub fn as_str(self) -> &'static str {
        match self {
            ThemeChoice::System => "system",
            ThemeChoice::Light => "light",
            ThemeChoice::Dark => "dark",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Terminal {
    pub font_family: String,
    /// In points.
    pub font_size: f64,
}

impl Default for Terminal {
    fn default() -> Self {
        Terminal {
            font_family: "Menlo".into(),
            font_size: 13.0,
        }
    }
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
    "~/.lemen/uploads".into()
}

fn default_claude() -> String {
    "claude".into()
}

fn default_worktree_dir() -> String {
    CLAUDE_WORKTREES.into()
}

impl Base {
    pub fn name(&self) -> String {
        let name = self.name.as_deref().unwrap_or_else(|| leaf(&self.path));
        // tmux reserves `.` and `:` in target names.
        name.replace(['.', ':'], "-")
    }

    /// The tmux session for thread `thread` of this base: `<base>/<thread>`.
    pub fn session_name(&self, thread: &str) -> String {
        format!("{}/{thread}", self.name())
    }

    pub fn uses_claude_worktrees(&self) -> bool {
        self.worktree_dir.trim_end_matches('/') == CLAUDE_WORKTREES
    }
}

impl Config {
    pub fn path() -> Result<PathBuf> {
        if let Some(p) = std::env::var_os("LEMEN_CONFIG") {
            return Ok(p.into());
        }
        Ok(home()?.join(".config/lemen/config.toml"))
    }

    pub fn load() -> Result<Config> {
        let cfg: Config = read_toml(&Self::path()?)?;
        for b in &cfg.bases {
            if cfg.host(&b.host).is_none() {
                bail!("base {} refers to unknown host {}", b.path, b.host);
            }
        }
        if !FONT_SIZES.contains(&cfg.terminal.font_size) {
            bail!(
                "terminal font_size {} is outside {}-{}",
                cfg.terminal.font_size,
                FONT_SIZES.start(),
                FONT_SIZES.end()
            );
        }
        Ok(cfg)
    }

    /// Writes the settings the Settings window changes into the config file
    /// at `path`, leaving everything else in it, comments included, as is.
    pub fn save_settings(path: &Path, theme: ThemeChoice, terminal: &Terminal) -> Result<()> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let mut doc: toml_edit::DocumentMut = text
            .parse()
            .with_context(|| format!("parsing {}", path.display()))?;
        doc["theme"] = toml_edit::value(theme.as_str());
        let table = doc
            .entry("terminal")
            .or_insert_with(toml_edit::table)
            .as_table_like_mut()
            .context("terminal in the config is not a table")?;
        table.insert("font_family", toml_edit::value(&terminal.font_family));
        // Whole sizes read best without the ".0".
        let size = terminal.font_size;
        let size = if size.fract() == 0.0 {
            toml_edit::value(size as i64)
        } else {
            toml_edit::value(size)
        };
        table.insert("font_size", size);
        std::fs::write(path, doc.to_string()).with_context(|| format!("writing {}", path.display()))
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

/// The last component of a path: `api` for `~/code/api/`.
pub fn leaf(path: &str) -> &str {
    path.trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or_default()
}

pub fn read_toml<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
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

    #[test]
    fn terminal_font_defaults_and_whole_sizes() {
        let cfg: Config = toml::from_str("").unwrap();
        assert_eq!(cfg.terminal, Terminal::default());
        let cfg: Config = toml::from_str("[terminal]\nfont_size = 15").unwrap();
        assert_eq!(cfg.terminal.font_size, 15.0);
        assert_eq!(cfg.terminal.font_family, "Menlo");
    }

    #[test]
    fn saving_settings_keeps_the_rest_of_the_file() {
        let path = std::env::temp_dir().join(format!("lemen-save-{}.toml", std::process::id()));
        let original = "\
# my hosts
[[hosts]]
name = \"agent\"   # the box
ssh = \"agent\"

[[bases]]
host = \"agent\"
path = \"~/projects/api\"
";
        std::fs::write(&path, original).unwrap();
        let terminal = Terminal {
            font_family: "JetBrains Mono".into(),
            font_size: 14.0,
        };
        Config::save_settings(&path, ThemeChoice::Dark, &terminal).unwrap();
        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(
            saved.contains("# my hosts") && saved.contains("# the box"),
            "{saved}"
        );
        assert!(saved.contains("font_size = 14\n"), "{saved}");
        let cfg: Config = toml::from_str(&saved).unwrap();
        assert_eq!(cfg.theme, ThemeChoice::Dark);
        assert_eq!(cfg.terminal, terminal);
        assert_eq!(cfg.bases[0].path, "~/projects/api");

        // Saving again updates the same keys rather than adding new ones.
        let terminal = Terminal {
            font_size: 12.5,
            ..terminal
        };
        Config::save_settings(&path, ThemeChoice::System, &terminal).unwrap();
        let saved = std::fs::read_to_string(&path).unwrap();
        assert_eq!(saved.matches("font_size").count(), 1, "{saved}");
        let cfg: Config = toml::from_str(&saved).unwrap();
        assert_eq!(cfg.theme, ThemeChoice::System);
        assert_eq!(cfg.terminal.font_size, 12.5);
        let _ = std::fs::remove_file(path);
    }
}
