use crate::attach::Active;
use crate::config::{ClaudeThread, Config, Host};
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

/// A random `adjective-noun` name, like `calm-heron`, that `taken` says
/// is free. After 50 tries it adds a number; after 100 it gives up and
/// returns a taken one.
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

/// A random version 4 UUID, as Claude Code's `--session-id` takes one.
pub fn random_uuid() -> String {
    use std::hash::{BuildHasher, Hasher};
    // A fresh RandomState hashes nothing into a random u64.
    let random = || {
        std::collections::hash_map::RandomState::new()
            .build_hasher()
            .finish()
    };
    let high = (random() & !0xf000) | 0x4000;
    let low = (random() & !(0b11 << 62)) | (0b10 << 62);
    format!(
        "{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
        high >> 32,
        (high >> 16) & 0xffff,
        high & 0xffff,
        low >> 48,
        low & 0xffff_ffff_ffff
    )
}

/// Whether `s` is a UUID like `random_uuid`'s, in lowercase hex: all a
/// Claude session ID that lemur types into a shell may be.
pub fn is_uuid(s: &str) -> bool {
    s.len() == 36
        && s.char_indices().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => c == '-',
            _ => matches!(c, '0'..='9' | 'a'..='f'),
        })
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

/// What lemur shows for a session or thread: the name without its unique
/// words. `api/claude--calm-heron` shows as `api/claude`.
pub fn label(name: &str) -> &str {
    match name.rsplit_once(LABEL_END) {
        Some((label, words)) if !words.contains('/') => label,
        _ => name,
    }
}

/// Refuses branch names git wouldn't take, before the host is asked.
pub fn validate_branch(name: &str) -> Result<()> {
    let bad = name.is_empty()
        || name == "@"
        || name.starts_with(['-', '/', '.'])
        || name.ends_with(['/', '.'])
        || name.ends_with(".lock")
        || ["..", "//", "/.", "@{"].iter().any(|s| name.contains(s))
        || name
            .chars()
            .any(|c| c.is_ascii_control() || " ~^:?*[\\".contains(c));
    if bad {
        bail!("{name} can't be a branch name");
    }
    Ok(())
}

/// A shell function that prints the branch new worktrees start from by
/// default: the remote's default branch (`origin/HEAD`, else `origin/main`
/// or `origin/master`), else the branch checked out.
const DEFAULT_BRANCH: &str = r#"default_branch() {
    git symbolic-ref -q --short refs/remotes/origin/HEAD && return
    for b in origin/main origin/master; do
        if git rev-parse -q --verify "refs/remotes/$b" >/dev/null; then echo "$b"; return; fi
    done
    git symbolic-ref -q --short HEAD || echo HEAD
}"#;

/// The branches a new worktree of `base` can start from: the default one
/// first, then the remotes' branches, then the local ones.
pub fn branches(cfg: &Config, base: usize) -> Result<Vec<String>> {
    let b = &cfg.bases[base];
    let r = Remote::new(cfg.host_or_err(&b.host)?);
    let out = r.run(&format!(
        "set -e\ncd {}\n{DEFAULT_BRANCH}\ndefault_branch\ngit for-each-ref --format='%(refname)' refs/remotes refs/heads\n",
        sh_path(&b.path)
    ))?;
    Ok(parse_branches(&out))
}

/// `branches` from the default branch's line, then one full ref name a line.
fn parse_branches(out: &str) -> Vec<String> {
    let mut lines = out.lines();
    let default = lines.next().unwrap_or_default().trim();
    let (mut remote, mut local) = (Vec::new(), Vec::new());
    for name in lines {
        if let Some(branch) = name.strip_prefix("refs/remotes/") {
            // A remote's HEAD only points at its default branch.
            if !branch.ends_with("/HEAD") {
                remote.push(branch);
            }
        } else if let Some(branch) = name.strip_prefix("refs/heads/") {
            local.push(branch);
        }
    }
    std::iter::once(default)
        .chain(remote.into_iter().chain(local).filter(|b| *b != default))
        .filter(|b| !b.is_empty())
        .map(str::to_string)
        .collect()
}

/// A thread just started: its host and session, and for Claude, what
/// resumes it.
#[derive(Debug)]
pub struct Started {
    pub host: String,
    pub session: String,
    pub claude: Option<ClaudeThread>,
}

/// Starts Claude in a new worktree `name` of `base`, in a session for
/// thread `thread`. The worktree checks out a new `branch`, or with none
/// its start, detached. It starts from `from`, or the default branch; a
/// remote's branch is fetched first. Returns the thread, and a note if
/// that fetch failed and the worktree starts where it was last fetched.
pub fn new_worktree_thread(
    cfg: &Config,
    base: usize,
    name: &str,
    thread: &str,
    branch: Option<&str>,
    from: Option<&str>,
) -> Result<(Started, Option<String>)> {
    validate_name(name)?;
    if let Some(branch) = branch {
        validate_branch(branch)?;
    }
    let b = &cfg.bases[base];
    let (from, fetched) = fetch_start(cfg, base, from)?;
    let checkout = match branch {
        Some(branch) => format!("--no-track -b {}", quote(branch)),
        None => "--detach".to_string(),
    };
    let id = random_uuid();
    let started = start_session(cfg, base, thread, Some(&id), |host, tmux, session| {
        let (repo, wt, s) = (sh_path(&b.path), sh_path(&b.worktree_dir), quote(session));
        format!(
            "cd {repo}\nmkdir -p {wt}\ngit worktree add -q {checkout} {wt}/{name} {}\ndir=\"$(cd {wt}/{name} && pwd -P)\"\n{tmux} new-session -d -s {s} -c \"$dir\"\n{}",
            quote(&from),
            launch(tmux, session, &format!("{} --session-id {id}", host.claude)),
        )
    })?;
    let note = (!fetched)
        .then(|| format!("couldn't fetch {from}; the worktree starts where it was last fetched"));
    Ok((started, note))
}

/// What a new worktree of `base` starts from: `from`, or the default
/// branch. A remote's branch is fetched first; false if that failed.
fn fetch_start(cfg: &Config, base: usize, from: Option<&str>) -> Result<(String, bool)> {
    let b = &cfg.bases[base];
    let r = Remote::new(cfg.host_or_err(&b.host)?);
    let from = from.map_or_else(|| "\"$(default_branch)\"".to_string(), quote);
    // No prompts: nobody could answer one.
    let out = r.run(&format!(
        r#"set -e
cd {}
{DEFAULT_BRANCH}
from={from}
fetched=yes
remote=${{from%%/*}}
if [ "$remote" != "$from" ] && git remote | grep -qxF "$remote"; then
    GIT_TERMINAL_PROMPT=0 git fetch -q "$remote" "${{from#*/}}" </dev/null 2>/dev/null || fetched=no
fi
printf '%s\n%s\n' "$from" "$fetched"
"#,
        sh_path(&b.path)
    ))?;
    let mut lines = out.lines();
    let from = lines.next().unwrap_or_default().to_string();
    Ok((from, lines.next() == Some("yes")))
}

/// Starts Claude, or with `shell` just a shell, in an existing worktree, in
/// a session for thread `thread`.
pub fn new_thread_in(
    cfg: &Config,
    base: usize,
    worktree: &str,
    thread: &str,
    shell: bool,
) -> Result<Started> {
    let id = (!shell).then(random_uuid);
    start_session(cfg, base, thread, id.as_deref(), |host, tmux, session| {
        let start = match &id {
            None => tag(tmux, session, "shell"),
            Some(id) => launch(tmux, session, &format!("{} --session-id {id}", host.claude)),
        };
        format!(
            "dir={}\n{tmux} new-session -d -s {} -c \"$dir\"\n{start}",
            quote(worktree),
            quote(session)
        )
    })
}

/// Creates the session for thread `thread` of `base`. `body` gets the host,
/// the tmux command and the session name, and must run `new-session` in
/// `$dir`. With `claude`, the Claude session ID it starts with, the thread
/// comes back with what resumes it.
fn start_session(
    cfg: &Config,
    base: usize,
    thread: &str,
    claude: Option<&str>,
    body: impl FnOnce(&Host, &str, &str) -> String,
) -> Result<Started> {
    validate_name(thread)?;
    let b = &cfg.bases[base];
    let host = cfg.host_or_err(&b.host)?;
    let r = Remote::new(host);
    let tmux = r.tmux();
    let session = b.session_name(thread);
    let out = r.run(&format!(
        "set -e\n{}\n{}\nprintf '%s\\n' \"$dir\"\n",
        refuse_existing(&tmux, &session),
        body(host, &tmux, &session)
    ))?;
    let dir = out.lines().last().unwrap_or_default().to_string();
    Ok(Started {
        host: host.name.clone(),
        session: session.clone(),
        claude: claude.map(|id| ClaudeThread {
            host: host.name.clone(),
            session,
            path: dir,
            claude_session: id.to_string(),
        }),
    })
}

/// Starts a thread's session again, once it's gone, as after its host
/// restarted, and resumes its Claude conversation in it.
pub fn resume_thread(cfg: &Config, thread: &ClaudeThread) -> Result<()> {
    if !is_uuid(&thread.claude_session) {
        bail!("{} isn't a Claude session ID", thread.claude_session);
    }
    let host = cfg.host_or_err(&thread.host)?;
    let r = Remote::new(host);
    let tmux = r.tmux();
    let session = &thread.session;
    let resume = format!("{} --resume {}", host.claude, thread.claude_session);
    r.run(&format!(
        "set -e\n{}\ndir={}\nif [ ! -d \"$dir\" ]; then echo {} >&2; exit 1; fi\n{tmux} new-session -d -s {} -c \"$dir\"\n{}\n",
        refuse_existing(&tmux, session),
        quote(&thread.path),
        quote(&format!("{} is gone", thread.path)),
        quote(session),
        launch(&tmux, session, &resume),
    ))?;
    Ok(())
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
        "{tmux} set-option -t {t}: @lemur_worktree \"$dir\"\n{tmux} set-option -t {t}: @lemur_kind {kind}"
    )
}

/// Records the order threads were dragged into: `@lemur_order` 0, 1, 2, …
pub fn set_order(cfg: &Config, host: &str, sessions: &[String]) -> Result<()> {
    let r = Remote::new(cfg.host_or_err(host)?);
    let tmux = r.tmux();
    let script: String = sessions
        .iter()
        .enumerate()
        .map(|(i, s)| format!("{tmux} set-option -t {}: @lemur_order {i}\n", target(s)))
        .collect();
    r.run(&format!("set -e\n{script}"))?;
    Ok(())
}

/// Takes a linked worktree of `base` out of git; its branch stays, and its
/// folder stays as plain files unless `delete`. The threads running in it
/// close first: those started or filed there, and those with a pane in it.
/// Nothing closes or changes until all that would keep it is ruled out: a
/// thread that isn't one of `sessions` (the error names them all, see
/// `why_kept`), and as git refuses, a lock, and when deleting, uncommitted
/// changes. `force` unlocks it and throws the changes away. Whatever is
/// left of a deleted folder goes too: git can't delete read-only folders.
pub fn remove_worktree(
    cfg: &Config,
    base: usize,
    worktree: &str,
    sessions: &[String],
    force: bool,
    delete: bool,
) -> Result<()> {
    let b = &cfg.bases[base];
    let r = Remote::new(cfg.host_or_err(&b.host)?);
    let tmux = r.tmux();
    let unasked = if sessions.is_empty() {
        "true".to_string()
    } else {
        let names: String = sessions
            .iter()
            .map(|s| format!(" -e {}", quote(s)))
            .collect();
        format!("printf '%s\\n' \"$found\" | grep -vxqF{names}")
    };
    let threads = format!(
        r#"found=$({{ {tmux} list-sessions -F {}; {tmux} list-panes -a -F {}; }} 2>/dev/null | awk -F '[|]::[|]' -v wt="$wt" '{{ for (i = 2; i <= NF; i++) if ($i == wt || index($i, wt "/") == 1) {{ if (!seen[$1]++) print $1; next }} }}')
if [ -n "$found" ] && {unasked}; then
    printf '%s\n%s\n' {} "$found" >&2
    exit 1
fi
"#,
        quote(IN_SESSION),
        quote(IN_PANE),
        quote(STILL_RUNNING),
    );
    let unlock = if force {
        "git worktree unlock \"$wt\" 2>/dev/null || true\n"
    } else {
        ""
    };
    // Git's refusals, checked before any thread closes, in git's words, as
    // `why_kept` reads them. A lock keeps it either way.
    let locked = r#"lock=$(git worktree list --porcelain | awk -v wt="$wt" '/^worktree / { here = (substr($0, 10) == wt) } here && /^locked/ { print "x" substr($0, 8); exit }')
if [ -n "$lock" ]; then
    echo "cannot remove a locked working tree, lock reason: ${lock#x}" >&2
    exit 1
fi
"#;
    // Changes keep it only from deleting, which would lose them; git
    // checks what `status` lists, untracked files included.
    let changes = if delete && !force {
        r#"if [ -d "$wt" ] && [ -n "$(git -C "$wt" status --porcelain --ignore-submodules=none)" ]; then
    echo "$wt contains modified or untracked files, use --force to delete it" >&2
    exit 1
fi
"#
    } else {
        ""
    };
    let close = format!(
        r#"if [ -n "$found" ]; then
    printf '%s\n' "$found" | while IFS= read -r s; do {tmux} kill-session -t "=$s"; done
fi
"#
    );
    let remove = if delete {
        let remove = if force { "remove --force" } else { "remove" };
        format!(
            r#"if ! out=$(LC_ALL=C git worktree {remove} "$wt" 2>&1); then
    # Git refused, unless it let go and just couldn't delete all of the folder.
    if git worktree list --porcelain | grep -qxF "worktree $wt"; then
        printf '%s\n' "$out" >&2
        exit 1
    fi
fi
if [ -e "$wt" ]; then
    chmod -R u+w "$wt"
    rm -rf "$wt"
fi
"#
        )
    } else {
        // Without its .git file, git prunes it and leaves the rest.
        "rm -f \"$wt/.git\"\ngit worktree prune\n".to_string()
    };
    r.run(&format!(
        "set -e\ncd {}\nwt={}\n{threads}{unlock}{locked}{changes}{close}{remove}",
        sh_path(&b.path),
        quote(worktree)
    ))?;
    Ok(())
}

/// Where a session is: where it started, and the worktree lemur filed it
/// under, by its tags from before the renames too.
const IN_SESSION: &str = "#{session_name}|::|#{session_path}|::|#{@lemur_worktree}|::|#{@lemen_worktree}|::|#{@muxbox_worktree}";
/// Where a pane's shell or program is now.
const IN_PANE: &str = "#{session_name}|::|#{pane_current_path}";
/// Starts the refusal that names the threads running in a worktree.
const STILL_RUNNING: &str = "threads still run in the worktree:";

/// Why a worktree was kept, and what removing it anyway takes: closing its
/// threads, or `force`.
#[derive(Debug, PartialEq)]
pub enum Kept {
    /// Threads run in it that weren't to be closed: all of them, by name.
    Threads(Vec<String>),
    Changes,
    /// Locked, with git's reason; Claude Code locks the worktree its
    /// session runs in.
    Locked(String),
}

/// What kept a worktree from being removed, if closing its threads or
/// `force` would help.
pub fn why_kept(e: &anyhow::Error) -> Option<Kept> {
    let text = format!("{e:#}");
    if let Some((_, names)) = text.split_once(STILL_RUNNING) {
        let names = names.lines().filter(|l| !l.is_empty());
        Some(Kept::Threads(names.map(str::to_string).collect()))
    } else if text.contains("locked working tree") {
        let reason = text
            .split_once("lock reason: ")
            .and_then(|(_, rest)| rest.lines().next())
            .unwrap_or_default();
        Some(Kept::Locked(reason.trim().to_string()))
    } else if text.contains("use --force") {
        Some(Kept::Changes)
    } else {
        None
    }
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
        "set -e\n{tmux} set-buffer -b lemur {}\n{tmux} paste-buffer -p -d -b lemur -t {}:\n",
        quote(&paste_line(&paths)),
        target(session)
    ))?;
    Ok(paths)
}

/// The session to send uploads to: explicit flags, else the session lemur
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
        .context("no active session: open one in lemur, or pass --host and --session")?;
    // The last session lemur showed is only the right one on its own host.
    if session.is_none() && host.as_ref().is_some_and(|h| *h != active.host) {
        bail!(
            "pass --session too: lemur last showed a session on {}",
            active.host
        );
    }
    let host = host.unwrap_or(active.host);
    cfg.host_or_err(&host)?;
    Ok((host, session.unwrap_or(active.session)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TempDir, TmuxServer, git_in, wait_until};
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use std::time::Duration;

    /// Removing a worktree closes its threads and then deletes it. A thread
    /// it wasn't asked to close, even one that only has a pane in it, keeps
    /// it and everything in it; so do changes or a lock, until forced.
    #[test]
    fn removing_a_worktree_closes_its_threads() {
        let server = TmuxServer::new("rm-wt");
        let dir = TempDir::new("rm-wt");
        let repo = dir.join("repo");
        std::fs::create_dir(&repo).unwrap();
        let git = |args: &[&str]| git_in(&repo, args);
        git(&["init", "-q"]);
        git(&["commit", "-q", "--allow-empty", "-m", "init"]);
        git(&["worktree", "add", "-q", "-b", "a", "wt/a"]);
        git(&["worktree", "add", "-q", "-b", "b", "wt/b"]);
        let (a, b) = (repo.join("wt/a"), repo.join("wt/b"));
        server.tmux(&[
            "new-session",
            "-d",
            "-s",
            "repo/a1",
            "-c",
            &a.to_string_lossy(),
        ]);
        server.tmux(&[
            "new-session",
            "-d",
            "-s",
            "repo/b1",
            "-c",
            &b.to_string_lossy(),
        ]);
        let mut cfg = server.config();
        cfg.bases
            .push(crate::config::Base::new("local", &repo.to_string_lossy()));
        let alive = |s: &str| {
            server
                .tmux(&["list-sessions", "-F", "#{session_name}"])
                .lines()
                .any(|l| l == s)
        };

        let a_path = a.to_string_lossy().into_owned();
        let err = remove_worktree(&cfg, 0, &a_path, &[], false, true).unwrap_err();
        assert_eq!(
            why_kept(&err),
            Some(Kept::Threads(vec!["repo/a1".into()])),
            "{err:#}"
        );
        assert!(a.exists() && alive("repo/a1"));
        remove_worktree(&cfg, 0, &a_path, &["repo/a1".into()], false, true).unwrap();
        assert!(!a.exists());
        assert!(!alive("repo/a1"));

        // Started in the main checkout, its shell has moved into b.
        let visit = format!("cd {} && exec sleep 600", quote(&b.to_string_lossy()));
        server.tmux(&[
            "new-session",
            "-d",
            "-s",
            "visitor",
            "-c",
            &repo.to_string_lossy(),
            &visit,
        ]);
        let b_path = b.to_string_lossy().into_owned();
        let moved = wait_until(Duration::from_secs(5), || {
            server.tmux(&["display", "-p", "-t", "=visitor:", "#{pane_current_path}"]) == b_path
        });
        assert!(moved, "the visitor's pane never got to {b_path}");
        let err = remove_worktree(&cfg, 0, &b_path, &["repo/b1".into()], false, true).unwrap_err();
        assert_eq!(
            why_kept(&err),
            Some(Kept::Threads(vec!["repo/b1".into(), "visitor".into()])),
            "{err:#}"
        );
        assert!(b.exists() && alive("repo/b1") && alive("visitor"));

        // What git would refuse is asked about before any thread closes.
        std::fs::write(b.join("draft.txt"), "unsaved").unwrap();
        let both = ["repo/b1".to_string(), "visitor".to_string()];
        let err = remove_worktree(&cfg, 0, &b_path, &both, false, true).unwrap_err();
        assert_eq!(why_kept(&err), Some(Kept::Changes), "{err:#}");
        assert!(b.exists() && alive("repo/b1") && alive("visitor"));
        // Locked, as Claude Code locks its worktree: force unlocks it too.
        git(&[
            "worktree",
            "lock",
            "--reason",
            "claude session draft (pid 1)",
            "wt/b",
        ]);
        let err = remove_worktree(&cfg, 0, &b_path, &both, false, true).unwrap_err();
        assert_eq!(
            why_kept(&err),
            Some(Kept::Locked("claude session draft (pid 1)".into())),
            "{err:#}"
        );
        assert!(b.exists() && alive("repo/b1") && alive("visitor"));
        remove_worktree(&cfg, 0, &b_path, &both, true, true).unwrap();
        assert!(!b.exists());
        assert!(!alive("repo/b1") && !alive("visitor"));
    }

    /// The worktrees git lists, by path.
    fn listed(repo: &Path) -> Vec<String> {
        let out = git_out(repo, &["worktree", "list", "--porcelain"]);
        out.lines()
            .filter_map(|l| l.strip_prefix("worktree "))
            .map(str::to_string)
            .collect()
    }

    /// What a git command in `dir` prints; it must succeed.
    fn git_out(dir: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("running git");
        assert!(out.status.success(), "git {args:?}: {out:?}");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// Kept, a removed worktree's folder stays with its files, changes
    /// included, as plain files; a lock still needs force. Deleted, it goes
    /// even when git can't delete all of it.
    #[test]
    fn removing_a_worktree_keeps_its_folder_unless_deleted() {
        let server = TmuxServer::new("keep-wt");
        let dir = TempDir::new("keep-wt");
        let repo = dir.join("repo");
        std::fs::create_dir(&repo).unwrap();
        let git = |args: &[&str]| git_in(&repo, args);
        git(&["init", "-q"]);
        git(&["commit", "-q", "--allow-empty", "-m", "init"]);
        git(&["worktree", "add", "-q", "-b", "a", "wt/a"]);
        git(&["worktree", "add", "-q", "-b", "b", "wt/b"]);
        let (a, b) = (repo.join("wt/a"), repo.join("wt/b"));
        let (a_path, b_path) = (a.to_string_lossy(), b.to_string_lossy());
        server.tmux(&["new-session", "-d", "-s", "repo/a1", "-c", &a_path]);
        // Keeps the tmux server up once repo/a1 is closed.
        server.tmux(&["new-session", "-d", "-s", "other"]);
        let mut cfg = server.config();
        cfg.bases
            .push(crate::config::Base::new("local", &repo.to_string_lossy()));

        std::fs::write(a.join("draft.txt"), "unsaved").unwrap();
        git(&["worktree", "lock", "wt/a"]);
        let err = remove_worktree(&cfg, 0, &a_path, &["repo/a1".into()], false, false).unwrap_err();
        assert_eq!(why_kept(&err), Some(Kept::Locked(String::new())), "{err:#}");
        assert!(listed(&repo).contains(&a_path.to_string()));
        let sessions = server.tmux(&["list-sessions", "-F", "#{session_name}"]);
        assert!(sessions.lines().any(|l| l == "repo/a1"), "{sessions}");
        remove_worktree(&cfg, 0, &a_path, &["repo/a1".into()], true, false).unwrap();
        assert!(!listed(&repo).contains(&a_path.to_string()));
        assert_eq!(
            std::fs::read_to_string(a.join("draft.txt")).unwrap(),
            "unsaved"
        );
        assert!(!a.join(".git").exists());
        assert_eq!(
            server
                .tmux(&["list-sessions", "-F", "#{session_name}"])
                .trim(),
            "other"
        );
        assert_eq!(git_out(&repo, &["branch", "--list", "a"]), "a");

        // A read-only folder, as Go's module cache makes, stops git halfway.
        let cache = b.join("cache/mod");
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(cache.join("go.mod"), "module x").unwrap();
        let read_only = std::fs::Permissions::from_mode(0o555);
        std::fs::set_permissions(&cache, read_only).unwrap();
        remove_worktree(&cfg, 0, &b_path, &[], true, true).unwrap();
        assert!(!b.exists());
        assert!(!listed(&repo).contains(&b_path.to_string()));
    }

    /// A repo cloned from `origin`, which then gets a commit the clone
    /// hasn't fetched. Both are on `main`; the config's one base is the clone.
    fn cloned(tag: &str, server: &TmuxServer) -> (TempDir, Config) {
        let dir = TempDir::new(tag);
        let origin = dir.join("origin");
        std::fs::create_dir(&origin).unwrap();
        git_in(&origin, &["init", "-q", "-b", "main"]);
        git_in(&origin, &["commit", "-q", "--allow-empty", "-m", "one"]);
        git_in(&dir, &["clone", "-q", "origin", "repo"]);
        git_in(&origin, &["commit", "-q", "--allow-empty", "-m", "two"]);
        let mut cfg = server.config();
        // Typed into the new sessions; nothing to start.
        cfg.hosts[0].claude = "true".into();
        cfg.bases.push(crate::config::Base::new(
            "local",
            &dir.join("repo").to_string_lossy(),
        ));
        (dir, cfg)
    }

    #[test]
    fn branches_start_with_the_default_one() {
        let server = TmuxServer::new("branches");
        let (dir, cfg) = cloned("branches", &server);
        git_in(&dir.join("repo"), &["branch", "local-only"]);
        assert_eq!(
            branches(&cfg, 0).unwrap(),
            ["origin/main", "local-only", "main"]
        );
        assert_eq!(
            parse_branches("main\nrefs/heads/main\nrefs/heads/x\n"),
            ["main", "x"]
        );
    }

    /// A new worktree gets the branch it's given, untracked, or is
    /// detached without one; it starts from the default branch as just
    /// fetched, or what it was told. A fetch that fails leaves a note.
    #[test]
    fn a_new_worktree_starts_from_the_fetched_default_branch() {
        let server = TmuxServer::new("new-wt");
        let (dir, cfg) = cloned("new-wt", &server);
        let repo = dir.join("repo");
        let worktree = |name: &str| repo.join(".claude/worktrees").join(name);
        let subject = |name: &str| git_out(&worktree(name), &["log", "-1", "--format=%s"]);

        let (started, note) =
            new_worktree_thread(&cfg, 0, "feat", "feat--a", Some("feature/x"), None).unwrap();
        assert_eq!(note, None);
        assert!(started.session.ends_with("feat--a"), "{}", started.session);
        // Claude resumes in the worktree it starts in.
        let claude = started.claude.expect("a Claude thread");
        assert_eq!(claude.session, started.session);
        assert_eq!(
            claude.path,
            worktree("feat").canonicalize().unwrap().to_string_lossy()
        );
        assert_eq!(
            git_out(&worktree("feat"), &["branch", "--show-current"]),
            "feature/x"
        );
        assert_eq!(subject("feat"), "two");
        let upstream = std::process::Command::new("git")
            .args(["rev-parse", "--abbrev-ref", "feature/x@{upstream}"])
            .current_dir(&repo)
            .output()
            .unwrap();
        assert!(!upstream.status.success(), "feature/x tracks {upstream:?}");

        new_worktree_thread(&cfg, 0, "loose", "loose--a", None, None).unwrap();
        assert_eq!(
            git_out(&worktree("loose"), &["branch", "--show-current"]),
            ""
        );
        assert_eq!(subject("loose"), "two");

        // The local main is still at what was cloned.
        new_worktree_thread(&cfg, 0, "old", "old--a", None, Some("main")).unwrap();
        assert_eq!(subject("old"), "one");

        git_in(&repo, &["remote", "set-url", "origin", "/nonexistent/repo"]);
        let (_, note) = new_worktree_thread(&cfg, 0, "offline", "offline--a", None, None).unwrap();
        assert!(note.is_some_and(|n| n.contains("origin/main")));
        assert_eq!(subject("offline"), "two");

        assert!(new_worktree_thread(&cfg, 0, "bad", "bad--a", Some("a b"), None).is_err());
    }

    /// A Claude thread starts with a session ID of lemur's, and comes back
    /// with what resumes it; once its session is gone, resuming starts it
    /// again in its folder, with that conversation.
    #[test]
    fn claude_threads_start_with_an_id_and_resume_with_it() {
        let server = TmuxServer::new("resume");
        let (dir, mut cfg) = cloned("resume", &server);
        // Prints what Claude would get.
        cfg.hosts[0].claude = "echo claude".into();
        let typed = |session: &str, text: &str| {
            wait_until(Duration::from_secs(5), || {
                server
                    .tmux(&["capture-pane", "-p", "-t", &format!("={session}:")])
                    .contains(text)
            })
        };
        let repo = dir.join("repo").canonicalize().unwrap();
        let path = repo.to_string_lossy().into_owned();
        let started = new_thread_in(&cfg, 0, &path, "claude--a", false).unwrap();
        let claude = started.claude.expect("a Claude thread");
        assert!(is_uuid(&claude.claude_session), "{}", claude.claude_session);
        assert_eq!(
            (claude.host.as_str(), claude.path.as_str()),
            ("local", path.as_str())
        );
        let first = format!("claude --session-id {}", claude.claude_session);
        assert!(typed(&started.session, &first), "{first} never ran");
        assert!(
            new_thread_in(&cfg, 0, &path, "shell--a", true)
                .unwrap()
                .claude
                .is_none()
        );

        // Its session is there: resuming would start a second.
        assert!(resume_thread(&cfg, &claude).is_err());
        server.tmux(&["kill-session", "-t", &format!("={}", started.session)]);
        resume_thread(&cfg, &claude).unwrap();
        let again = format!("claude --resume {}", claude.claude_session);
        assert!(typed(&started.session, &again), "{again} never ran");
        let at = |option: &str| {
            server.tmux(&[
                "display",
                "-p",
                "-t",
                &format!("={}:", started.session),
                option,
            ])
        };
        assert_eq!(at("#{pane_current_path}"), path);
        assert_eq!(at("#{@lemur_kind}"), "claude");

        // Nothing starts where its folder was, or with an ID that isn't one.
        let gone = ClaudeThread {
            session: "repo/gone--a".into(),
            path: dir.join("deleted").to_string_lossy().into_owned(),
            ..claude.clone()
        };
        let err = resume_thread(&cfg, &gone).unwrap_err();
        assert!(format!("{err:#}").contains("is gone"), "{err:#}");
        let odd = ClaudeThread {
            session: "repo/odd--a".into(),
            claude_session: "x; rm -rf ~".into(),
            ..claude
        };
        assert!(resume_thread(&cfg, &odd).is_err());
        let alive = |s: &str| {
            server
                .tmux(&["list-sessions", "-F", "#{session_name}"])
                .lines()
                .any(|l| l == s)
        };
        assert!(alive(&started.session));
        assert!(!alive("repo/gone--a") && !alive("repo/odd--a"));
    }

    #[test]
    fn uuids_are_version_4() {
        let (a, b) = (random_uuid(), random_uuid());
        assert!(is_uuid(&a) && is_uuid(&b), "{a} {b}");
        assert_ne!(a, b);
        assert_eq!(&a[14..15], "4");
        assert!(matches!(&a[19..20], "8" | "9" | "a" | "b"), "{a}");
        assert!(!is_uuid("0FDFABEB-CA80-4916-A103-275BBB5E8614"));
        assert!(!is_uuid("0fdfabeb-ca80-4916-a103-275bbb5e861"));
        assert!(is_uuid("0fdfabeb-ca80-4916-a103-275bbb5e8614"));
    }

    #[test]
    fn branch_names_are_checked() {
        for ok in ["feature/x", "fix-1", "a.b", "release/2.0"] {
            assert!(validate_branch(ok).is_ok(), "{ok}");
        }
        for bad in [
            "", "-x", "a b", "a..b", "a/", "/a", "a.lock", "a~1", "x:y", "@", "a@{b",
        ] {
            assert!(validate_branch(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn why_a_worktree_was_kept() {
        let kept = |text: &str| why_kept(&anyhow::anyhow!("{text}"));
        assert_eq!(
            kept(
                "agent: fatal: cannot remove a locked working tree, lock reason: claude session testing-stuff (pid 1460666 start 8911022)\nuse 'remove -f -f' to override or unlock first"
            ),
            Some(Kept::Locked(
                "claude session testing-stuff (pid 1460666 start 8911022)".into()
            ))
        );
        assert_eq!(
            kept(
                "agent: fatal: cannot remove a locked working tree;\nuse 'remove -f -f' to override or unlock first"
            ),
            Some(Kept::Locked(String::new()))
        );
        assert_eq!(
            kept(
                "agent: fatal: '/w' contains modified or untracked files, use --force to delete it"
            ),
            Some(Kept::Changes)
        );
        assert_eq!(kept("agent: fatal: '/w' is not a working tree"), None);
    }

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
