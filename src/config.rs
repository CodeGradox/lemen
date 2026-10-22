use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use std::collections::BTreeMap;
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};

/// Where `claude -w` puts worktrees; bases using it let Claude create them.
pub const CLAUDE_WORKTREES: &str = ".claude/worktrees";

/// The colors a worktree's folder icon can have, by the name the config
/// uses, as RGB. Mid shades, so they read on light and dark alike.
pub const COLORS: [(&str, u32); 10] = [
    ("red", 0xef4444),
    ("orange", 0xf97316),
    ("yellow", 0xeab308),
    ("green", 0x22c55e),
    ("teal", 0x14b8a6),
    ("cyan", 0x06b6d4),
    ("blue", 0x3b82f6),
    ("purple", 0xa855f7),
    ("pink", 0xec4899),
    ("gray", 0x6b7280),
];

/// Terminal font sizes lemur accepts, in points.
pub const FONT_SIZES: RangeInclusive<f64> = 6.0..=72.0;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub theme: ThemeChoice,
    /// The sidebar's width in pixels, as last dragged; unset is lemur's own.
    #[serde(default)]
    pub sidebar_width: Option<f32>,
    /// The notes panel's width in pixels, as last dragged.
    #[serde(default)]
    pub notes_width: Option<f32>,
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
    /// Colors over lemur's own for a dark app, then a light one.
    pub dark: TerminalColors,
    pub light: TerminalColors,
}

impl Default for Terminal {
    fn default() -> Self {
        Terminal {
            font_family: "Menlo".into(),
            font_size: 13.0,
            dark: TerminalColors::default(),
            light: TerminalColors::default(),
        }
    }
}

/// Terminal colors as WezTerm names them, each `#rrggbb`; what's left out
/// is lemur's own.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct TerminalColors {
    pub foreground: Option<String>,
    pub background: Option<String>,
    pub cursor: Option<String>,
    /// Black, red, green, yellow, blue, magenta, cyan, white.
    pub ansi: Option<Vec<String>>,
    /// The same eight, bright.
    pub brights: Option<Vec<String>>,
}

impl TerminalColors {
    pub fn is_empty(&self) -> bool {
        *self == TerminalColors::default()
    }

    /// Why one of its colors can't be used, if one can't.
    fn problem(&self) -> Option<String> {
        let single = [&self.foreground, &self.background, &self.cursor];
        let all = single
            .into_iter()
            .flatten()
            .chain(self.ansi.iter().flatten())
            .chain(self.brights.iter().flatten());
        for hex in all {
            if parse_hex(hex).is_none() {
                return Some(format!("{hex} isn't a #rrggbb color"));
            }
        }
        for (name, list) in [("ansi", &self.ansi), ("brights", &self.brights)] {
            if list.as_ref().is_some_and(|l| l.len() != 8) {
                return Some(format!("{name} needs 8 colors"));
            }
        }
        None
    }
}

/// `#rrggbb` as `0xrrggbb`.
pub fn parse_hex(s: &str) -> Option<u32> {
    let digits = s.strip_prefix('#')?;
    if digits.len() != 6 {
        return None;
    }
    u32::from_str_radix(digits, 16).ok()
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
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
    /// Folded away in the sidebar.
    #[serde(default)]
    pub collapsed: bool,
    /// Its worktrees' paths in the order they were dragged into; others
    /// follow in git's order.
    #[serde(default)]
    pub worktree_order: Vec<String>,
    /// Folder icon colors, one of `COLORS` by worktree path.
    #[serde(default)]
    pub worktree_colors: BTreeMap<String, String>,
}

fn default_upload_dir() -> String {
    "~/.lemur/uploads".into()
}

fn default_claude() -> String {
    "claude".into()
}

fn default_worktree_dir() -> String {
    CLAUDE_WORKTREES.into()
}

impl Host {
    /// A host reached at `ssh` (or "local"), with the default settings.
    pub fn new(name: &str, ssh: &str) -> Host {
        Host {
            name: name.into(),
            ssh: ssh.into(),
            upload_dir: default_upload_dir(),
            tmux_socket: None,
            claude: default_claude(),
        }
    }
}

impl Base {
    /// The repo at `path` on `host`, with the default settings.
    pub fn new(host: &str, path: &str) -> Base {
        Base {
            host: host.into(),
            path: path.into(),
            name: None,
            worktree_dir: default_worktree_dir(),
            collapsed: false,
            worktree_order: vec![],
            worktree_colors: BTreeMap::new(),
        }
    }

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
        // `LEMEN_CONFIG` from before the rename still works.
        if let Some(p) =
            std::env::var_os("LEMUR_CONFIG").or_else(|| std::env::var_os("LEMEN_CONFIG"))
        {
            return Ok(p.into());
        }
        Ok(home()?.join(".config/lemur/config.toml"))
    }

    pub fn load() -> Result<Config> {
        let cfg: Config = read_toml(&Self::path()?)?;
        for b in &cfg.bases {
            if cfg.host(&b.host).is_none() {
                bail!("base {} refers to unknown host {}", b.path, b.host);
            }
            for color in b.worktree_colors.values() {
                if !COLORS.iter().any(|(name, _)| name == color) {
                    let names: Vec<&str> = COLORS.iter().map(|(name, _)| *name).collect();
                    bail!(
                        "base {} has worktree color {color}; the colors are {}",
                        b.path,
                        names.join(", ")
                    );
                }
            }
        }
        for (name, colors) in [("dark", &cfg.terminal.dark), ("light", &cfg.terminal.light)] {
            if let Some(problem) = colors.problem() {
                bail!("[terminal.{name}]: {problem}");
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
        edit(path, |doc| {
            doc["theme"] = toml_edit::value(theme.as_str());
            let table = doc
                .entry("terminal")
                .or_insert_with(toml_edit::table)
                .as_table_mut()
                .context("terminal in the config is not a [terminal] table")?;
            table.insert("font_family", toml_edit::value(&terminal.font_family));
            // Whole sizes read best without the ".0".
            let size = terminal.font_size;
            let size = if size.fract() == 0.0 {
                toml_edit::value(size as i64)
            } else {
                toml_edit::value(size)
            };
            table.insert("font_size", size);
            for (name, colors) in [("dark", &terminal.dark), ("light", &terminal.light)] {
                if colors.is_empty() {
                    table.remove(name);
                    continue;
                }
                let mut sub = toml_edit::Table::new();
                let single = [
                    ("foreground", &colors.foreground),
                    ("background", &colors.background),
                    ("cursor", &colors.cursor),
                ];
                for (key, hex) in single {
                    if let Some(hex) = hex {
                        sub.insert(key, toml_edit::value(hex));
                    }
                }
                for (key, list) in [("ansi", &colors.ansi), ("brights", &colors.brights)] {
                    if let Some(list) = list {
                        let array: toml_edit::Array = list.iter().collect();
                        sub.insert(key, toml_edit::value(array));
                    }
                }
                table.insert(name, toml_edit::Item::Table(sub));
            }
            Ok(())
        })
    }

    /// Adds a folder (a base) to the config file at `path`, and its host
    /// when `new_host` says the file doesn't have it yet.
    pub fn save_folder(path: &Path, new_host: Option<&Host>, base: &Base) -> Result<()> {
        edit(path, |doc| {
            if let Some(h) = new_host {
                push_table(doc, "hosts", &[("name", &h.name), ("ssh", &h.ssh)])?;
            }
            push_table(doc, "bases", &[("host", &base.host), ("path", &base.path)])
        })
    }

    /// Saves how a project shows in the sidebar (whether it's folded, its
    /// worktrees' order and colors) to its entry in the config file at
    /// `path`. Defaults are left out of the file.
    pub fn save_project_look(path: &Path, base: &Base) -> Result<()> {
        edit(path, |doc| {
            let Some(table) = bases_mut(doc)?.and_then(|b| b.iter_mut().find(|t| is_base(t, base)))
            else {
                return Ok(());
            };
            if base.collapsed {
                table.insert("collapsed", toml_edit::value(true));
            } else {
                table.remove("collapsed");
            }
            if base.worktree_order.is_empty() {
                table.remove("worktree_order");
            } else {
                let order: toml_edit::Array = base.worktree_order.iter().collect();
                table.insert("worktree_order", toml_edit::value(order));
            }
            if base.worktree_colors.is_empty() {
                table.remove("worktree_colors");
            } else {
                let mut colors = toml_edit::InlineTable::new();
                for (path, color) in &base.worktree_colors {
                    colors.insert(path, color.as_str().into());
                }
                table.insert("worktree_colors", toml_edit::value(colors));
            }
            Ok(())
        })
    }

    /// Saves the side panels' widths, as dragged, to the config file at
    /// `path`. A panel at lemur's own width is left out of the file.
    pub fn save_panel_widths(path: &Path, sidebar: Option<f32>, notes: Option<f32>) -> Result<()> {
        edit(path, |doc| {
            for (key, width) in [("sidebar_width", sidebar), ("notes_width", notes)] {
                match width {
                    Some(width) => doc[key] = toml_edit::value(width.round() as i64),
                    None => {
                        doc.remove(key);
                    }
                }
            }
            Ok(())
        })
    }

    /// Takes a folder (a base) out of the config file at `path`; its host
    /// stays.
    pub fn remove_folder(path: &Path, base: &Base) -> Result<()> {
        edit(path, |doc| {
            if let Some(bases) = bases_mut(doc)? {
                bases.retain(|t| !is_base(t, base));
            }
            Ok(())
        })
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

/// Changes the config file at `path` in place: what `change` doesn't touch,
/// comments included, stays as it is.
fn edit(path: &Path, change: impl FnOnce(&mut toml_edit::DocumentMut) -> Result<()>) -> Result<()> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let mut doc: toml_edit::DocumentMut = text
        .parse()
        .with_context(|| format!("parsing {}", path.display()))?;
    change(&mut doc)?;
    write_whole(path, &doc.to_string())
}

/// The `[[bases]]` tables, if the file has any.
fn bases_mut(doc: &mut toml_edit::DocumentMut) -> Result<Option<&mut toml_edit::ArrayOfTables>> {
    match doc.get_mut("bases") {
        None => Ok(None),
        Some(bases) => bases
            .as_array_of_tables_mut()
            .map(Some)
            .context("bases in the config is not a list of [[bases]] tables"),
    }
}

/// Whether a `[[bases]]` table is `base`'s.
fn is_base(table: &toml_edit::Table, base: &Base) -> bool {
    let field = |key| table.get(key).and_then(|v| v.as_str());
    field("host") == Some(base.host.as_str()) && field("path") == Some(base.path.as_str())
}

/// Writes `contents` to `path` whole or not at all: it goes to a file
/// beside it first, then takes its place, so a crash midway leaves the old
/// file. A symlinked file (dotfiles) stays a link, to the new contents.
pub fn write_whole(path: &Path, contents: &str) -> Result<()> {
    let target = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let name = target
        .file_name()
        .with_context(|| format!("{} has no file name", target.display()))?;
    let temporary = target.with_file_name(format!(".{}.lemur-new", name.to_string_lossy()));
    std::fs::write(&temporary, contents)
        .with_context(|| format!("writing {}", temporary.display()))?;
    if let Ok(meta) = std::fs::metadata(&target) {
        let _ = std::fs::set_permissions(&temporary, meta.permissions());
    }
    std::fs::rename(&temporary, &target).with_context(|| format!("writing {}", path.display()))
}

/// Appends a `[[key]]` table with `fields`.
fn push_table(doc: &mut toml_edit::DocumentMut, key: &str, fields: &[(&str, &str)]) -> Result<()> {
    let tables = doc
        .entry(key)
        .or_insert_with(|| toml_edit::Item::ArrayOfTables(Default::default()))
        .as_array_of_tables_mut()
        .with_context(|| format!("{key} in the config is not a list of [[{key}]] tables"))?;
    let mut table = toml_edit::Table::new();
    for (k, v) in fields {
        table.insert(k, toml_edit::value(*v));
    }
    tables.push(table);
    Ok(())
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
    fn terminal_colors_are_checked() {
        let colors = |toml: &str| toml::from_str::<Config>(toml).unwrap().terminal.dark;
        assert_eq!(colors("").problem(), None);
        let ok = colors(
            "[terminal.dark]\nbackground = \"#282c34\"\nansi = [\"#000000\", \"#111111\", \"#222222\", \"#333333\", \"#444444\", \"#555555\", \"#666666\", \"#777777\"]",
        );
        assert_eq!(ok.problem(), None);
        assert!(
            colors("[terminal.dark]\nbackground = \"red\"")
                .problem()
                .is_some()
        );
        assert!(
            colors("[terminal.dark]\nansi = [\"#000000\"]")
                .problem()
                .is_some()
        );
        assert_eq!(parse_hex("#dcdfe4"), Some(0xdcdfe4));
        assert_eq!(parse_hex("dcdfe4"), None);
    }

    #[test]
    fn saving_settings_keeps_the_rest_of_the_file() {
        let path = std::env::temp_dir().join(format!("lemur-save-{}.toml", std::process::id()));
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
            dark: TerminalColors {
                background: Some("#000000".into()),
                ..Default::default()
            },
            ..Default::default()
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
        assert_eq!(cfg.terminal.dark.background.as_deref(), Some("#000000"));
        assert!(saved.contains("[terminal.dark]"), "{saved}");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn saving_panel_widths_keeps_them_before_the_tables() {
        let path = std::env::temp_dir().join(format!("lemur-widths-{}.toml", std::process::id()));
        std::fs::write(&path, "[[hosts]]\nname = \"agent\"\nssh = \"agent\"\n").unwrap();
        Config::save_panel_widths(&path, Some(300.4), None).unwrap();
        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(saved.contains("sidebar_width = 300\n"), "{saved}");
        let cfg: Config = toml::from_str(&saved).unwrap();
        assert_eq!((cfg.sidebar_width, cfg.notes_width), (Some(300.0), None));
        assert_eq!(cfg.hosts[0].name, "agent");

        // Back at lemur's own width, a panel leaves the file.
        Config::save_panel_widths(&path, None, Some(420.0)).unwrap();
        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(!saved.contains("sidebar_width"), "{saved}");
        let cfg: Config = toml::from_str(&saved).unwrap();
        assert_eq!((cfg.sidebar_width, cfg.notes_width), (None, Some(420.0)));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn saving_a_folder_adds_it_and_a_new_host() {
        let path = std::env::temp_dir().join(format!("lemur-folder-{}.toml", std::process::id()));
        let original = "\
theme = \"dark\"   # keep me

[[hosts]]
name = \"agent\"
ssh = \"agent\"

[[bases]]
host = \"agent\"
path = \"~/projects/api\"
";
        std::fs::write(&path, original).unwrap();
        // A folder on a host the file already has, then one on a new host.
        Config::save_folder(&path, None, &Base::new("agent", "~/projects/web")).unwrap();
        let app = Host::new("app", "app");
        Config::save_folder(&path, Some(&app), &Base::new("app", "/srv/site")).unwrap();
        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(saved.contains("# keep me"), "{saved}");
        let cfg: Config = toml::from_str(&saved).unwrap();
        let hosts: Vec<&str> = cfg.hosts.iter().map(|h| h.name.as_str()).collect();
        assert_eq!(hosts, ["agent", "app"]);
        let bases: Vec<(&str, &str)> = cfg
            .bases
            .iter()
            .map(|b| (b.host.as_str(), b.path.as_str()))
            .collect();
        assert_eq!(
            bases,
            [
                ("agent", "~/projects/api"),
                ("agent", "~/projects/web"),
                ("app", "/srv/site")
            ]
        );
        assert_eq!(cfg.bases[2].worktree_dir, CLAUDE_WORKTREES);

        // Removing one takes out only its entry.
        Config::remove_folder(&path, &Base::new("agent", "~/projects/web")).unwrap();
        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(saved.contains("# keep me"), "{saved}");
        let cfg: Config = toml::from_str(&saved).unwrap();
        let bases: Vec<&str> = cfg.bases.iter().map(|b| b.path.as_str()).collect();
        assert_eq!(bases, ["~/projects/api", "/srv/site"]);
        assert_eq!(cfg.hosts.len(), 2);

        // A project's look goes in its entry, and its defaults come out.
        let mut site = cfg.bases[1].clone();
        site.collapsed = true;
        site.worktree_colors
            .insert("/srv/site/.worktrees/b".into(), "blue".into());
        site.worktree_order = vec!["/srv/site/.worktrees/b".into(), "/srv/site".into()];
        Config::save_project_look(&path, &site).unwrap();
        let cfg: Config = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(cfg.bases[1].worktree_colors, site.worktree_colors);
        assert!(cfg.bases[1].collapsed);
        assert_eq!(cfg.bases[1].worktree_order, site.worktree_order);
        assert!(cfg.bases[0].worktree_colors.is_empty() && !cfg.bases[0].collapsed);
        site.worktree_colors.clear();
        site.collapsed = false;
        site.worktree_order.clear();
        Config::save_project_look(&path, &site).unwrap();
        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(
            !saved.contains("color") && !saved.contains("collapsed") && !saved.contains("order"),
            "{saved}"
        );

        // A symlinked config stays a link.
        let real = path.with_extension("real.toml");
        std::fs::rename(&path, &real).unwrap();
        std::os::unix::fs::symlink(&real, &path).unwrap();
        Config::save_project_look(&path, &site).unwrap();
        assert!(
            std::fs::symlink_metadata(&path)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(
            std::fs::read_to_string(&real)
                .unwrap()
                .contains("# keep me")
        );
        std::fs::remove_file(&path).unwrap();
        std::fs::rename(&real, &path).unwrap();

        // A file with no hosts or bases yet gets its first ones.
        std::fs::write(&path, "# empty\n").unwrap();
        Config::save_folder(&path, Some(&app), &Base::new("app", "/srv/site")).unwrap();
        let cfg: Config = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!((cfg.hosts.len(), cfg.bases.len()), (1, 1));
        let _ = std::fs::remove_file(path);
    }
}
