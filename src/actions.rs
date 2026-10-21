use crate::attach::Active;
use crate::config::{CLAUDE_WORKTREES, Config, Host};
use crate::remote::{Remote, quote, sh_path};
use anyhow::{Context, Result, bail};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

pub fn validate_name(name: &str) -> Result<()> {
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        bail!("names may only use letters, digits, - and _");
    }
    Ok(())
}

const ADJECTIVES: &[&str] = &[
    "amber", "bold", "brave", "bright", "calm", "clever", "cosmic", "crisp", "daring", "eager",
    "gentle", "glad", "golden", "happy", "hidden", "jolly", "keen", "kind", "lively", "lucky",
    "merry", "mighty", "misty", "nimble", "noble", "plucky", "proud", "quiet", "rapid", "rustic",
    "shiny", "silent", "silver", "snowy", "sunny", "swift", "tidy", "witty", "wild", "zesty",
];

const NOUNS: &[&str] = &[
    "badger", "beaver", "bison", "cedar", "comet", "crane", "falcon", "fern", "finch", "fjord",
    "fox", "glacier", "harbor", "heron", "lynx", "maple", "meadow", "moose", "nebula", "orca",
    "otter", "owl", "panda", "pebble", "pine", "puffin", "quasar", "raven", "reef", "river",
    "robin", "salmon", "spruce", "summit", "thistle", "tiger", "walrus", "willow", "wren", "yak",
];

/// A random `adjective-noun` name, like `calm-heron`, that `taken` accepts.
pub fn random_name(taken: impl Fn(&str) -> bool) -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut name = String::new();
    for attempt in 0..100 {
        // A fresh RandomState hashes nothing into a random u64.
        let n = std::collections::hash_map::RandomState::new()
            .build_hasher()
            .finish() as usize;
        name = format!(
            "{}-{}",
            ADJECTIVES[n % ADJECTIVES.len()],
            NOUNS[(n >> 32) % NOUNS.len()]
        );
        if attempt >= 50 {
            name = format!("{name}-{}", n % 100);
        }
        if !taken(&name) {
            break;
        }
    }
    name
}

/// Starts Claude in a new worktree of `base`. Returns (host, session).
pub fn new_worktree_thread(cfg: &Config, base: usize, name: &str) -> Result<(String, String)> {
    validate_name(name)?;
    let b = &cfg.bases[base];
    let host = cfg.host_or_err(&b.host)?;
    let r = Remote::new(host);
    let tmux = r.tmux();
    let session = format!("{}/{name}", b.name());
    let s = quote(&session);
    let start = if b.uses_claude_worktrees() {
        // Claude creates the worktree itself; the session starts in the repo.
        format!(
            "cd {repo}\ndir=\"$(pwd -P)/{CLAUDE_WORKTREES}/{name}\"\n{tmux} new-session -d -s {s} -c \"$(pwd -P)\"\n{launch}",
            repo = sh_path(&b.path),
            launch = launch(&tmux, &s, &format!("{} -w {name}", host.claude)),
        )
    } else {
        let wt = sh_path(&b.worktree_dir);
        format!(
            "cd {repo}\nmkdir -p {wt}\ngit worktree add -q -b {branch} {wt}/{name}\ndir=\"$(cd {wt}/{name} && pwd -P)\"\n{tmux} new-session -d -s {s} -c \"$dir\"\n{launch}",
            repo = sh_path(&b.path),
            branch = quote(&format!("worktree-{name}")),
            launch = launch(&tmux, &s, &host.claude),
        )
    };
    r.run(&format!(
        "set -e\n{}\n{start}\n",
        refuse_existing(&tmux, &s, &session)
    ))?;
    Ok((host.name.clone(), session))
}

/// Starts Claude in an existing worktree. Returns (host, session).
pub fn new_thread_in(
    cfg: &Config,
    base: usize,
    worktree: &str,
    name: &str,
) -> Result<(String, String)> {
    validate_name(name)?;
    let b = &cfg.bases[base];
    let host = cfg.host_or_err(&b.host)?;
    let r = Remote::new(host);
    let tmux = r.tmux();
    let session = format!("{}/{name}", b.name());
    let s = quote(&session);
    r.run(&format!(
        "set -e\n{}\ndir={}\n{tmux} new-session -d -s {s} -c \"$dir\"\n{}\n",
        refuse_existing(&tmux, &s, &session),
        quote(worktree),
        launch(&tmux, &s, &host.claude),
    ))?;
    Ok((host.name.clone(), session))
}

fn refuse_existing(tmux: &str, s: &str, session: &str) -> String {
    format!(
        "if {tmux} has-session -t ={s} 2>/dev/null; then echo {} >&2; exit 1; fi",
        quote(&format!("session {session} already exists"))
    )
}

/// Tags the session with its worktree (`$dir`) and types the Claude command
/// into its shell, so a login shell has set up PATH first.
fn launch(tmux: &str, s: &str, command: &str) -> String {
    format!(
        "{tmux} set-option -t ={s}: @muxbox_worktree \"$dir\"\n{tmux} send-keys -t ={s}: -l {}\n{tmux} send-keys -t ={s}: Enter",
        quote(command)
    )
}

pub fn kill_session(cfg: &Config, host: &str, session: &str) -> Result<()> {
    let r = Remote::new(cfg.host_or_err(host)?);
    r.run(&format!("{} kill-session -t ={}", r.tmux(), quote(session)))?;
    Ok(())
}

/// The session name for a thread renamed to `name`. The `base/` prefix
/// stays, so the thread stays under its base.
pub fn renamed(session: &str, name: &str) -> String {
    match session.rsplit_once('/') {
        Some((prefix, _)) => format!("{prefix}/{name}"),
        None => name.to_string(),
    }
}

/// Renames a thread (its tmux session); returns the new session name.
/// Claude keeps running and attached clients stay attached.
pub fn rename_session(cfg: &Config, host: &str, session: &str, name: &str) -> Result<String> {
    validate_name(name)?;
    let r = Remote::new(cfg.host_or_err(host)?);
    let tmux = r.tmux();
    let new = renamed(session, name);
    if new == session {
        return Ok(new);
    }
    let s = quote(&new);
    r.run(&format!(
        "set -e\n{}\n{tmux} rename-session -t ={} {s}\n",
        refuse_existing(&tmux, &s, &new),
        quote(session),
    ))?;
    Ok(new)
}

/// Copies files into the host's upload dir; returns their remote paths.
pub fn upload(host: &Host, files: &[PathBuf]) -> Result<Vec<String>> {
    let r = Remote::new(host);
    let dir = sh_path(&host.upload_dir);
    let dir = r
        .run(&format!("set -e\nmkdir -p {dir}\ncd {dir}\npwd\n"))?
        .trim()
        .to_string();
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    files
        .iter()
        .enumerate()
        .map(|(i, f)| {
            let name = f
                .file_name()
                .with_context(|| format!("{} is not a file", f.display()))?
                .to_string_lossy();
            let safe: String = name
                .chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() || "._-".contains(c) {
                        c
                    } else {
                        '-'
                    }
                })
                .collect();
            let path = format!("{dir}/{stamp}-{i}-{safe}");
            r.copy_to(f, &path)?;
            Ok(path)
        })
        .collect()
}

/// Uploads files and pastes their remote paths into a session's prompt,
/// as a bracketed paste when the app asked for one (like a real drop).
pub fn upload_and_type(
    cfg: &Config,
    host: &str,
    session: &str,
    files: &[PathBuf],
) -> Result<Vec<String>> {
    let h = cfg.host_or_err(host)?;
    let paths = upload(h, files)?;
    let r = Remote::new(h);
    let tmux = r.tmux();
    r.run(&format!(
        "set -e\n{tmux} set-buffer -b muxbox {}\n{tmux} paste-buffer -p -d -b muxbox -t ={}:\n",
        quote(&format!("{} ", paths.join(" "))),
        quote(session)
    ))?;
    Ok(paths)
}

/// The session to send uploads to: explicit flags, else the session muxbox
/// showed last.
pub fn resolve_target(
    cfg: &Config,
    host: Option<String>,
    session: Option<String>,
) -> Result<(String, String)> {
    if let (Some(h), Some(s)) = (&host, &session) {
        cfg.host_or_err(h)?;
        return Ok((h.clone(), s.clone()));
    }
    let active = Active::load()
        .context("no active session: open one in muxbox, or pass --host and --session")?;
    let host = host.unwrap_or(active.host);
    cfg.host_or_err(&host)?;
    Ok((host, session.unwrap_or(active.session)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_restricted() {
        assert!(validate_name("auth-ui_2").is_ok());
        assert!(validate_name("").is_err());
        assert!(validate_name("a b").is_err());
        assert!(validate_name("x;rm").is_err());
    }

    #[test]
    fn renaming_keeps_the_base_prefix() {
        assert_eq!(renamed("api/calm-heron", "auth-ui"), "api/auth-ui");
        assert_eq!(renamed("scratch", "notes"), "notes");
    }

    #[test]
    fn random_names_are_valid_and_avoid_taken_ones() {
        for _ in 0..200 {
            let name = random_name(|n| n.starts_with('a'));
            assert!(validate_name(&name).is_ok(), "{name}");
            assert!(!name.starts_with('a'), "{name}");
        }
        let names: std::collections::HashSet<String> =
            (0..50).map(|_| random_name(|_| false)).collect();
        assert!(names.len() > 25, "names barely vary: {names:?}");
    }
}
