//! Notes: one Markdown file per worktree, kept on this Mac under
//! `~/.lemen/notes/<host>/<worktree path>.md`.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Where a worktree's notes live under `root`: the host, then the
/// worktree's own path, so the files are easy to find by hand.
pub fn file(root: &Path, host: &str, worktree: &str) -> PathBuf {
    // A host named `.` or `..` mustn't step out of the notes folder.
    let host = match host.replace('/', "-") {
        dots if dots.chars().all(|c| c == '.') => format!("_{dots}"),
        host => host,
    };
    let mut path = root.join(host);
    let parts: Vec<&str> = worktree
        .split('/')
        .filter(|p| !p.is_empty() && *p != "." && *p != "..")
        .collect();
    match parts.split_last() {
        Some((name, dirs)) => {
            path.extend(dirs);
            path.push(format!("{name}.md"));
        }
        None => path.push("root.md"),
    }
    path
}

/// The notes in `path`; none yet reads as empty.
pub fn load(path: &Path) -> Result<String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// Saves notes, creating folders as needed; empty notes that were never
/// saved make no file.
pub fn save(path: &Path, text: &str) -> Result<()> {
    if text.is_empty() && !path.exists() {
        return Ok(());
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    crate::config::write_whole(path, text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_worktree_maps_to_its_own_path() {
        let root = Path::new("/notes");
        assert_eq!(
            file(
                root,
                "agent",
                "/home/agent/projects/api/.claude/worktrees/auth"
            ),
            Path::new("/notes/agent/home/agent/projects/api/.claude/worktrees/auth.md")
        );
        // The main checkout's file sits beside its linked worktrees' folder.
        assert_eq!(
            file(root, "agent", "/home/agent/projects/api/"),
            Path::new("/notes/agent/home/agent/projects/api.md")
        );
        // Nothing climbs out of the notes folder.
        assert_eq!(
            file(root, "a/b", "/x/../../y"),
            Path::new("/notes/a-b/x/y.md")
        );
        assert_eq!(file(root, "..", "/y"), Path::new("/notes/_../y.md"));
    }

    #[test]
    fn notes_round_trip_and_empty_ones_make_no_file() {
        let root = std::env::temp_dir().join(format!("lemen-notes-{}", std::process::id()));
        let path = file(&root, "agent", "/srv/api");
        assert_eq!(load(&path).unwrap(), "");
        save(&path, "").unwrap();
        assert!(!path.exists());
        save(&path, "# API\n\nhttps://example.com").unwrap();
        assert_eq!(load(&path).unwrap(), "# API\n\nhttps://example.com");
        // Clearing saved notes keeps the (now empty) file.
        save(&path, "").unwrap();
        assert_eq!(load(&path).unwrap(), "");
        let _ = std::fs::remove_dir_all(root);
    }
}
