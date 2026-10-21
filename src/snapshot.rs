//! One round trip per host: worktrees of every base plus all tmux sessions.

use crate::config::{Config, Host};
use crate::remote::{Remote, quote, sh_path};

#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub hosts: Vec<HostView>,
}

#[derive(Debug, Clone)]
pub struct HostView {
    pub name: String,
    pub error: Option<String>,
    pub bases: Vec<BaseView>,
    /// Sessions that don't belong to any base's worktrees.
    pub other: Vec<Session>,
}

#[derive(Debug, Clone)]
pub struct BaseView {
    /// Index into `Config::bases`.
    pub index: usize,
    pub error: Option<String>,
    /// The main checkout comes first, as `git worktree list` prints it.
    pub worktrees: Vec<Worktree>,
}

#[derive(Debug, Clone)]
pub struct Worktree {
    pub path: String,
    pub branch: Option<String>,
    pub sessions: Vec<Session>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Session {
    pub name: String,
    /// The worktree it was started for, or its start directory.
    pub path: String,
    pub attached: bool,
    pub idle_secs: u64,
    /// Set by Claude Code hooks through the `@muxbox_state` tmux option.
    pub state: Option<String>,
}

/// tmux prints control characters such as tabs as `_`, and session names
/// can't contain `:`, so fields are split on a printable marker instead.
const SEP: &str = "|::|";
const FIELDS: &str = "#{session_name}|::|#{session_path}|::|#{session_attached}|::|#{session_activity}|::|#{@muxbox_state}|::|#{@muxbox_worktree}";

pub fn fetch(cfg: &Config) -> Snapshot {
    Snapshot {
        hosts: cfg.hosts.iter().map(|h| fetch_host(cfg, h)).collect(),
    }
}

fn fetch_host(cfg: &Config, host: &Host) -> HostView {
    let remote = Remote::new(host);
    match remote.run(&script(cfg, host, &remote)) {
        Ok(out) => parse(cfg, host, &out),
        Err(e) => HostView {
            name: host.name.clone(),
            error: Some(format!("{e:#}")),
            bases: vec![],
            other: vec![],
        },
    }
}

fn script(cfg: &Config, host: &Host, remote: &Remote) -> String {
    let mut s = String::new();
    for (i, b) in cfg
        .bases
        .iter()
        .enumerate()
        .filter(|(_, b)| b.host == host.name)
    {
        s += &format!(
            "echo '@@base {i}'\n( cd {} 2>/dev/null || {{ echo '@@error no such directory'; exit 0; }}; git worktree list --porcelain 2>/dev/null || echo '@@error not a git repository' )\n",
            sh_path(&b.path)
        );
    }
    s += &format!(
        "echo '@@now'\ndate +%s\necho '@@tmux'\n{} list-sessions -F {} 2>/dev/null || true\n",
        remote.tmux(),
        quote(FIELDS)
    );
    s
}

fn parse(cfg: &Config, host: &Host, out: &str) -> HostView {
    let mut bases: Vec<BaseView> = vec![];
    let mut sessions: Vec<Session> = vec![];
    let mut now = 0u64;
    let mut section = "";
    for line in out.lines() {
        if let Some(i) = line.strip_prefix("@@base ") {
            if let Ok(index) = i.trim().parse::<usize>()
                && index < cfg.bases.len()
            {
                bases.push(BaseView {
                    index,
                    error: None,
                    worktrees: vec![],
                });
                section = "base";
                continue;
            }
            section = "";
            continue;
        }
        match line {
            "@@now" => section = "now",
            "@@tmux" => section = "tmux",
            _ => match section {
                "base" => parse_worktree_line(bases.last_mut().expect("base section"), line),
                "now" => now = line.trim().parse().unwrap_or(0),
                "tmux" => sessions.extend(parse_session(line, now)),
                _ => {}
            },
        }
    }

    let mut other = vec![];
    for s in sessions {
        match find_worktree(&bases, &s.path) {
            Some((bi, wi)) => bases[bi].worktrees[wi].sessions.push(s),
            None => other.push(s),
        }
    }
    for w in bases.iter_mut().flat_map(|b| b.worktrees.iter_mut()) {
        w.sessions.sort_by(|a, b| a.name.cmp(&b.name));
    }
    other.sort_by(|a, b| a.name.cmp(&b.name));
    HostView {
        name: host.name.clone(),
        error: None,
        bases,
        other,
    }
}

fn parse_worktree_line(base: &mut BaseView, line: &str) {
    if let Some(e) = line.strip_prefix("@@error ") {
        base.error = Some(e.to_string());
    } else if let Some(p) = line.strip_prefix("worktree ") {
        base.worktrees.push(Worktree {
            path: p.to_string(),
            branch: None,
            sessions: vec![],
        });
    } else if let Some(b) = line.strip_prefix("branch ")
        && let Some(w) = base.worktrees.last_mut()
    {
        w.branch = Some(b.trim_start_matches("refs/heads/").to_string());
    }
}

fn parse_session(line: &str, now: u64) -> Option<Session> {
    let f: Vec<&str> = line.split(SEP).collect();
    if f.len() != 6 {
        return None;
    }
    let activity: u64 = f[3].parse().unwrap_or(now);
    Some(Session {
        name: f[0].to_string(),
        path: if f[5].is_empty() { f[1] } else { f[5] }.to_string(),
        attached: f[2] != "0",
        idle_secs: now.saturating_sub(activity),
        state: (!f[4].is_empty()).then(|| f[4].to_string()),
    })
}

/// The worktree containing `path`, preferring the deepest match: linked
/// worktrees under `.claude/worktrees/` sit inside the main checkout.
fn find_worktree(bases: &[BaseView], path: &str) -> Option<(usize, usize)> {
    let mut best: Option<(usize, usize, usize)> = None;
    for (bi, b) in bases.iter().enumerate() {
        for (wi, w) in b.worktrees.iter().enumerate() {
            let inside = path == w.path || path.starts_with(&format!("{}/", w.path));
            if inside && best.is_none_or(|(_, _, len)| w.path.len() > len) {
                best = Some((bi, wi, w.path.len()));
            }
        }
    }
    best.map(|(bi, wi, _)| (bi, wi))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> Config {
        toml::from_str(
            r#"
            [[hosts]]
            name = "agent"
            ssh = "agent"

            [[bases]]
            host = "agent"
            path = "~/projects/api"
            "#,
        )
        .unwrap()
    }

    #[test]
    fn parses_worktrees_and_assigns_sessions() {
        let cfg = cfg();
        let out = "@@base 0\n\
            worktree /home/agent/projects/api\nHEAD abc\nbranch refs/heads/main\n\n\
            worktree /home/agent/projects/api/.claude/worktrees/auth\nHEAD def\nbranch refs/heads/worktree-auth\n\n\
            @@now\n1000\n@@tmux\n\
            api/auth|::|/home/agent/projects/api|::|1|::|990|::|needs_input|::|/home/agent/projects/api/.claude/worktrees/auth\n\
            api/tests|::|/home/agent/projects/api|::|0|::|400|::||::|\n\
            scratch|::|/tmp|::|0|::|1000|::||::|\n";
        let h = parse(&cfg, &cfg.hosts[0], out);
        let wts = &h.bases[0].worktrees;
        assert_eq!(wts.len(), 2);
        assert_eq!(wts[0].branch.as_deref(), Some("main"));
        assert_eq!(
            wts[0]
                .sessions
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>(),
            ["api/tests"]
        );
        assert_eq!(wts[1].sessions[0].name, "api/auth");
        assert_eq!(wts[1].sessions[0].state.as_deref(), Some("needs_input"));
        assert_eq!(wts[1].sessions[0].idle_secs, 10);
        assert!(wts[1].sessions[0].attached);
        assert_eq!(h.other[0].name, "scratch");
    }

    #[test]
    fn reports_base_errors() {
        let cfg = cfg();
        let h = parse(
            &cfg,
            &cfg.hosts[0],
            "@@base 0\n@@error no such directory\n@@now\n1\n@@tmux\n",
        );
        assert_eq!(h.bases[0].error.as_deref(), Some("no such directory"));
    }
}
