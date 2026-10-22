//! Finding folders to add: host aliases from the SSH config, and the
//! folders on a host, listed over the connection lemen already uses.

use crate::config::Host;
use crate::remote::{Remote, sh_path};
use anyhow::Result;
use std::path::{Path, PathBuf};

/// The `Host` aliases in the SSH config at `path` and the files it
/// includes, in order. Patterns (`*`, `?`, `!`) aren't hosts you can
/// connect to, so they're left out.
pub fn ssh_aliases(path: &Path) -> Vec<String> {
    let mut aliases = vec![];
    collect_aliases(path, &mut aliases, 0);
    let mut seen = std::collections::HashSet::new();
    aliases.retain(|a| seen.insert(a.clone()));
    aliases
}

fn collect_aliases(path: &Path, aliases: &mut Vec<String>, depth: usize) {
    // Include loops end here.
    if depth > 8 {
        return;
    }
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    for line in text.lines() {
        let line = line.trim();
        let Some((keyword, rest)) = line.split_once(|c: char| c.is_whitespace() || c == '=') else {
            continue;
        };
        let words = words(rest.trim_start_matches(|c: char| c.is_whitespace() || c == '='));
        match keyword.to_ascii_lowercase().as_str() {
            "host" => aliases.extend(words.into_iter().filter(|w| !w.contains(['*', '?', '!']))),
            "include" => {
                for word in words {
                    for file in included(&word) {
                        collect_aliases(&file, aliases, depth + 1);
                    }
                }
            }
            _ => {}
        }
    }
}

/// Splits on whitespace, keeping "quoted words" whole.
fn words(s: &str) -> Vec<String> {
    let mut words = vec![];
    let mut word = String::new();
    let mut quoted = false;
    for c in s.chars() {
        match c {
            '"' => quoted = !quoted,
            c if c.is_whitespace() && !quoted => {
                if !word.is_empty() {
                    words.push(std::mem::take(&mut word));
                }
            }
            c => word.push(c),
        }
    }
    if !word.is_empty() {
        words.push(word);
    }
    words
}

/// The files an `Include` names: `~/` is home, a relative path is under
/// `~/.ssh`, and a `*` in the file name matches like a shell glob.
fn included(pattern: &str) -> Vec<PathBuf> {
    let Ok(home) = crate::config::home() else {
        return vec![];
    };
    let path = match pattern.strip_prefix("~/") {
        Some(rest) => home.join(rest),
        None if pattern.starts_with('/') => PathBuf::from(pattern),
        None => home.join(".ssh").join(pattern),
    };
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let Some((before, after)) = name.split_once('*') else {
        return vec![path];
    };
    let Some(dir) = path.parent() else {
        return vec![];
    };
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name().is_some_and(|n| {
                let n = n.to_string_lossy();
                n.len() >= before.len() + after.len() && n.starts_with(before) && n.ends_with(after)
            })
        })
        .collect();
    files.sort();
    files
}

#[derive(Debug, Clone, PartialEq)]
pub struct Folder {
    pub name: String,
    /// A git repository's top folder.
    pub repo: bool,
}

/// The folders in `dir` on `host` (`~` and `~/...` work), hidden ones
/// included, sorted by name.
pub fn list_folders(host: &Host, dir: &str) -> Result<Vec<Folder>> {
    let dir = if dir.is_empty() { "~" } else { dir };
    let out = Remote::new(host).run(&format!(
        "cd {} || exit 1\nfor d in .[!.]* *; do [ -d \"$d\" ] || continue; if [ -e \"$d/.git\" ]; then echo \"g $d\"; else echo \"d $d\"; fi; done\n",
        sh_path(dir)
    ))?;
    let mut folders: Vec<Folder> = out
        .lines()
        .filter_map(|line| {
            let (kind, name) = line.split_once(' ')?;
            Some(Folder {
                name: name.to_string(),
                repo: kind == "g",
            })
        })
        .collect();
    folders.sort_by_key(|f| f.name.to_lowercase());
    Ok(folders)
}

/// The folder to add for `path` on `host`: the top folder of the git
/// repository it's in (absolute), or `path` itself when that's the top
/// folder or it's in no repository.
pub fn folder_to_add(host: &Host, path: &str) -> Result<String> {
    let out = Remote::new(host).run(&format!(
        "cd {} || exit 1\ntop=$(git rev-parse --show-toplevel 2>/dev/null) || {{ echo '@@here'; exit 0; }}\nif [ \"$(pwd -P)\" = \"$top\" ]; then echo '@@here'; else echo \"$top\"; fi\n",
        sh_path(path)
    ))?;
    match out.trim() {
        "@@here" => Ok(path.to_string()),
        top => Ok(top.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TempDir, git_in};

    #[test]
    fn aliases_come_from_the_config_and_its_includes() {
        let dir = TempDir::new("ssh-config");
        std::fs::create_dir(dir.join("conf.d")).unwrap();
        std::fs::write(
            dir.join("conf.d/work.conf"),
            "Host build\n  HostName 10.0.0.2\n",
        )
        .unwrap();
        std::fs::write(dir.join("conf.d/notes.txt"), "Host skipped\n").unwrap();
        std::fs::write(dir.join("extra config"), "Host=lab\n").unwrap();
        let main = dir.join("config");
        std::fs::write(
            &main,
            format!(
                "Include \"{}\"\nInclude {}/conf.d/*.conf\n\nHost *\n  ServerAliveInterval 30\nHost agent\n  HostName 1.2.3.4\nhost db database !nope\nHost web-?\nHost agent\n",
                dir.join("extra config").display(),
                dir.display()
            ),
        )
        .unwrap();
        assert_eq!(
            ssh_aliases(&main),
            ["lab", "build", "agent", "db", "database"]
        );
        assert!(ssh_aliases(&dir.join("missing")).is_empty());
    }

    #[test]
    fn folders_are_listed_and_repos_found() {
        let dir = TempDir::new("browse");
        for d in ["web", "api/src", ".hidden", "Docs"] {
            std::fs::create_dir_all(dir.join(d)).unwrap();
        }
        std::fs::write(dir.join("file.txt"), "").unwrap();
        git_in(&dir.join("api"), &["init", "-q"]);
        let local = Host::new("local", "local");
        let base = dir.to_string_lossy().into_owned();

        let folders = list_folders(&local, &base).unwrap();
        let names: Vec<(&str, bool)> = folders.iter().map(|f| (f.name.as_str(), f.repo)).collect();
        assert_eq!(
            names,
            [
                (".hidden", false),
                ("api", true),
                ("Docs", false),
                ("web", false)
            ]
        );
        assert!(list_folders(&local, &format!("{base}/nope")).is_err());

        let api = format!("{base}/api");
        assert_eq!(folder_to_add(&local, &api).unwrap(), api);
        assert_eq!(folder_to_add(&local, &format!("{api}/src")).unwrap(), api);
        let web = format!("{base}/web");
        assert_eq!(folder_to_add(&local, &web).unwrap(), web);
        assert!(folder_to_add(&local, &format!("{base}/nope")).is_err());
    }
}
