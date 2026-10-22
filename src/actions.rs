use crate::attach::Active;
use crate::config::{CLAUDE_WORKTREES, Config, Host};
use crate::remote::{Remote, quote, sh_path, target};
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

/// Separates a thread's label from the random words that keep its tmux name
/// unique, so several threads can share a label: `claude--calm-heron`.
const LABEL_END: &str = "--";

/// A thread name for `label` that `taken` doesn't know yet:
/// `claude` becomes `claude--calm-heron`.
pub fn unique_thread(label: &str, taken: impl Fn(&str) -> bool) -> String {
    let words = random_name(|w| taken(&format!("{label}{LABEL_END}{w}")));
    format!("{label}{LABEL_END}{words}")
}

/// What lemen shows for a session or thread: the name without its unique
/// words. `api/claude--calm-heron` shows as `api/claude`.
pub fn label(name: &str) -> &str {
    match name.rsplit_once(LABEL_END) {
        Some((label, words)) if !words.contains('/') => label,
        _ => name,
    }
}

/// Starts Claude in a new worktree `worktree` of `base`, in a session for
/// thread `thread`. Returns (host, session).
pub fn new_worktree_thread(
    cfg: &Config,
    base: usize,
    worktree: &str,
    thread: &str,
) -> Result<(String, String)> {
    validate_name(worktree)?;
    let b = &cfg.bases[base];
    let name = worktree;
    start_session(cfg, base, thread, |host, tmux, session| {
        let (repo, s) = (sh_path(&b.path), quote(session));
        if b.uses_claude_worktrees() {
            // Claude creates the worktree itself; the session starts in the repo.
            format!(
                "cd {repo}\ndir=\"$(pwd -P)/{CLAUDE_WORKTREES}/{name}\"\n{tmux} new-session -d -s {s} -c \"$(pwd -P)\"\n{}",
                launch(tmux, session, &format!("{} -w {name}", host.claude)),
            )
        } else {
            let wt = sh_path(&b.worktree_dir);
            format!(
                "cd {repo}\nmkdir -p {wt}\ngit worktree add -q -b {branch} {wt}/{name}\ndir=\"$(cd {wt}/{name} && pwd -P)\"\n{tmux} new-session -d -s {s} -c \"$dir\"\n{}",
                launch(tmux, session, &host.claude),
                branch = quote(&format!("worktree-{name}")),
            )
        }
    })
}

/// Starts Claude, or with `shell` just a shell, in an existing worktree, in
/// a session for thread `thread`. Returns (host, session).
pub fn new_thread_in(
    cfg: &Config,
    base: usize,
    worktree: &str,
    thread: &str,
    shell: bool,
) -> Result<(String, String)> {
    start_session(cfg, base, thread, |host, tmux, session| {
        let start = if shell {
            tag(tmux, session, "shell")
        } else {
            launch(tmux, session, &host.claude)
        };
        format!(
            "dir={}\n{tmux} new-session -d -s {} -c \"$dir\"\n{start}",
            quote(worktree),
            quote(session)
        )
    })
}

/// Creates the session for thread `thread` of `base`. `body` gets the host,
/// the tmux command and the session name, and must run `new-session`.
/// Returns (host, session).
fn start_session(
    cfg: &Config,
    base: usize,
    thread: &str,
    body: impl FnOnce(&Host, &str, &str) -> String,
) -> Result<(String, String)> {
    validate_name(thread)?;
    let b = &cfg.bases[base];
    let host = cfg.host_or_err(&b.host)?;
    let r = Remote::new(host);
    let tmux = r.tmux();
    let session = b.session_name(thread);
    r.run(&format!(
        "set -e\n{}\n{}\n",
        refuse_existing(&tmux, &session),
        body(host, &tmux, &session)
    ))?;
    Ok((host.name.clone(), session))
}

fn refuse_existing(tmux: &str, session: &str) -> String {
    format!(
        "if {tmux} has-session -t {} 2>/dev/null; then echo {} >&2; exit 1; fi",
        target(session),
        quote(&format!("session {session} already exists"))
    )
}

/// Tags the session as Claude's and types the Claude command into its shell,
/// so a login shell has set up PATH first.
fn launch(tmux: &str, session: &str, command: &str) -> String {
    let t = target(session);
    format!(
        "{}\n{tmux} send-keys -t {t}: -l {}\n{tmux} send-keys -t {t}: Enter",
        tag(tmux, session, "claude"),
        quote(command)
    )
}

/// Records the session's worktree (`$dir`), so the sidebar files it there,
/// and what it was started for (`claude` or `shell`), for its icon.
fn tag(tmux: &str, session: &str, kind: &str) -> String {
    let t = target(session);
    format!(
        "{tmux} set-option -t {t}: @lemen_worktree \"$dir\"\n{tmux} set-option -t {t}: @lemen_kind {kind}"
    )
}

/// Records the order threads were dragged into: `@lemen_order` 0, 1, 2, …
pub fn set_order(cfg: &Config, host: &str, sessions: &[String]) -> Result<()> {
    let r = Remote::new(cfg.host_or_err(host)?);
    let tmux = r.tmux();
    let script: String = sessions
        .iter()
        .enumerate()
        .map(|(i, s)| format!("{tmux} set-option -t {}: @lemen_order {i}\n", target(s)))
        .collect();
    r.run(&script)?;
    Ok(())
}

pub fn kill_session(cfg: &Config, host: &str, session: &str) -> Result<()> {
    let r = Remote::new(cfg.host_or_err(host)?);
    r.run(&format!("{} kill-session -t {}", r.tmux(), target(session)))?;
    Ok(())
}

/// The session name for a thread relabeled `name`. The `base/` prefix and
/// the unique words stay: `api/claude--calm-heron` becomes
/// `api/review--calm-heron`.
pub fn renamed(session: &str, name: &str) -> String {
    let (prefix, thread) = match session.rsplit_once('/') {
        Some((prefix, thread)) => (format!("{prefix}/"), thread),
        None => (String::new(), session),
    };
    match thread.rsplit_once(LABEL_END) {
        Some((_, words)) => format!("{prefix}{name}{LABEL_END}{words}"),
        None => format!("{prefix}{name}"),
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
    r.run(&format!(
        "set -e\n{}\n{tmux} rename-session -t {} {}\n",
        refuse_existing(&tmux, &new),
        target(session),
        quote(&new),
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

/// What a session gets typed after an upload: the remote paths, then a
/// space so you can keep writing.
pub fn paste_line(paths: &[String]) -> String {
    format!("{} ", paths.join(" "))
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
        "set -e\n{tmux} set-buffer -b lemen {}\n{tmux} paste-buffer -p -d -b lemen -t {}:\n",
        quote(&paste_line(&paths)),
        target(session)
    ))?;
    Ok(paths)
}

/// The session to send uploads to: explicit flags, else the session lemen
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
        .context("no active session: open one in lemen, or pass --host and --session")?;
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
    fn renaming_keeps_the_base_prefix_and_unique_words() {
        assert_eq!(renamed("api/calm-heron", "auth-ui"), "api/auth-ui");
        assert_eq!(
            renamed("api/claude--calm-heron", "review"),
            "api/review--calm-heron"
        );
        assert_eq!(renamed("scratch", "notes"), "notes");
    }

    #[test]
    fn labels_hide_the_unique_words() {
        let thread = unique_thread("claude", |_| false);
        assert!(
            thread.starts_with("claude--") && validate_name(&thread).is_ok(),
            "{thread}"
        );
        assert_eq!(label(&thread), "claude");
        assert_eq!(label("api/claude--calm-heron"), "api/claude");
        assert_eq!(label("api/calm-heron"), "api/calm-heron");
        // Only the last part counts: `--` in a base name isn't a label end.
        assert_eq!(label("my--repo/claude"), "my--repo/claude");
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
