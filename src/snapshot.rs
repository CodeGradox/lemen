//! One round trip per host: worktrees of every base, all tmux sessions, and
//! what Claude is doing in each.

use crate::config::{Config, Host};
use crate::remote::{Remote, quote, sh_path};
use serde::Deserialize;
use std::collections::HashMap;

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
    /// What Claude in this session is doing.
    pub activity: Activity,
    /// What Claude is waiting on, when it needs input.
    pub waiting_for: Option<String>,
    /// lemen started it for Claude rather than as a plain shell.
    pub started_claude: bool,
    /// Its place in its worktree once threads were dragged into an order.
    pub order: Option<u32>,
}

impl Session {
    /// Started for Claude, or Claude found running in it.
    pub fn runs_claude(&self) -> bool {
        self.started_claude || self.activity != Activity::Unknown
    }
}

/// Ordered by urgency, so a session with several Claudes shows the most
/// pressing one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Activity {
    /// No Claude found in the session.
    Unknown,
    Idle,
    Working,
    NeedsInput,
}

/// tmux prints control characters such as tabs as `_`, and session names
/// can't contain `:`, so fields are split on a printable marker instead.
const SEP: &str = "|::|";
/// The `@muxbox_*` fallbacks read sessions tagged before the rename to lemen.
const FIELDS: &str = "#{session_name}|::|#{session_path}|::|#{session_attached}|::|#{session_activity}|::|#{?@lemen_state,#{@lemen_state},#{@muxbox_state}}|::|#{?@lemen_worktree,#{@lemen_worktree},#{@muxbox_worktree}}|::|#{?@lemen_kind,#{@lemen_kind},#{@muxbox_kind}}|::|#{@lemen_order}";
const PANES: &str = "#{session_name}|::|#{pane_pid}";

pub fn fetch(cfg: &Config) -> Snapshot {
    // In parallel: an unreachable host takes the whole SSH connect timeout.
    let hosts = std::thread::scope(|scope| {
        let fetches: Vec<_> = cfg
            .hosts
            .iter()
            .map(|h| scope.spawn(|| fetch_host(cfg, h)))
            .collect();
        fetches
            .into_iter()
            .map(|f| f.join().expect("fetch thread"))
            .collect()
    });
    Snapshot { hosts }
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
        "echo '@@now'\ndate +%s\necho '@@tmux'\n{tmux} list-sessions -F {} 2>/dev/null || true\necho '@@panes'\n{tmux} list-panes -a -F {} 2>/dev/null || true\n",
        quote(FIELDS),
        quote(PANES),
        tmux = remote.tmux(),
    );
    // Claude Code lists every running session with its pid and status; the
    // process tree ties each one to the tmux pane it runs in.
    s += "echo '@@ps'\nps -A -o pid= -o ppid=\necho '@@agents'\nclaude agents --json 2>/dev/null || true\n";
    s
}

fn parse(cfg: &Config, host: &Host, out: &str) -> HostView {
    let mut bases: Vec<BaseView> = vec![];
    let mut sessions: Vec<Session> = vec![];
    let mut now = 0u64;
    let mut panes: HashMap<u32, String> = HashMap::new();
    let mut parents: HashMap<u32, u32> = HashMap::new();
    let mut agents = String::new();
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
            "@@panes" => section = "panes",
            "@@ps" => section = "ps",
            "@@agents" => section = "agents",
            _ => match section {
                "base" => parse_worktree_line(bases.last_mut().expect("base section"), line),
                "now" => now = line.trim().parse().unwrap_or(0),
                "tmux" => sessions.extend(parse_session(line, now)),
                "panes" => {
                    if let Some((name, pid)) = line.split_once(SEP)
                        && let Ok(pid) = pid.trim().parse()
                    {
                        panes.insert(pid, name.to_string());
                    }
                }
                "ps" => {
                    let mut f = line.split_whitespace().map(str::parse::<u32>);
                    if let (Some(Ok(pid)), Some(Ok(ppid))) = (f.next(), f.next()) {
                        parents.insert(pid, ppid);
                    }
                }
                "agents" => {
                    agents.push_str(line);
                    agents.push('\n');
                }
                _ => {}
            },
        }
    }

    let claude = claude_activity(&agents, &panes, &parents);
    for s in &mut sessions {
        if let Some((activity, waiting_for)) = claude.get(&s.name) {
            s.activity = *activity;
            s.waiting_for = waiting_for.clone();
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
        sort(&mut w.sessions);
    }
    sort(&mut other);
    HostView {
        name: host.name.clone(),
        error: None,
        bases,
        other,
    }
}

/// Threads in the order they were dragged into; ones never placed follow,
/// by name.
fn sort(sessions: &mut [Session]) {
    sessions.sort_by(|a, b| {
        (a.order.is_none(), a.order, &a.name).cmp(&(b.order.is_none(), b.order, &b.name))
    });
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
    if f.len() != 8 {
        return None;
    }
    let activity: u64 = f[3].parse().unwrap_or(now);
    Some(Session {
        name: f[0].to_string(),
        path: if f[5].is_empty() { f[1] } else { f[5] }.to_string(),
        attached: f[2] != "0",
        idle_secs: now.saturating_sub(activity),
        // What hooks reported; `claude agents`, when available, overrides it.
        activity: match f[4] {
            "needs_input" => Activity::NeedsInput,
            "working" => Activity::Working,
            "idle" => Activity::Idle,
            _ => Activity::Unknown,
        },
        waiting_for: None,
        started_claude: f[6] == "claude",
        order: f[7].parse().ok(),
    })
}

#[derive(Deserialize)]
struct Agent {
    kind: Option<String>,
    pid: Option<u32>,
    status: Option<String>,
    #[serde(rename = "waitingFor")]
    waiting_for: Option<serde_json::Value>,
}

/// Maps `claude agents --json` output to tmux sessions: each interactive
/// Claude's pid is walked up the process tree to the pane it runs in.
fn claude_activity(
    agents: &str,
    panes: &HashMap<u32, String>,
    parents: &HashMap<u32, u32>,
) -> HashMap<String, (Activity, Option<String>)> {
    let agents: Vec<Agent> = serde_json::from_str(agents.trim()).unwrap_or_default();
    let mut found: HashMap<String, (Activity, Option<String>)> = HashMap::new();
    for agent in agents {
        let (Some("interactive"), Some(pid)) = (agent.kind.as_deref(), agent.pid) else {
            continue;
        };
        let activity = match agent.status.as_deref() {
            Some("busy") => Activity::Working,
            Some("waiting") => Activity::NeedsInput,
            Some("idle") => Activity::Idle,
            _ => continue,
        };
        let waiting_for = agent.waiting_for.map(|v| match v {
            serde_json::Value::String(s) => s,
            other => other.to_string(),
        });
        let mut p = pid;
        for _ in 0..32 {
            if let Some(session) = panes.get(&p) {
                let entry = found
                    .entry(session.clone())
                    .or_insert((Activity::Unknown, None));
                if activity > entry.0 {
                    *entry = (activity, waiting_for);
                }
                break;
            }
            match parents.get(&p) {
                Some(&parent) if parent > 1 => p = parent,
                _ => break,
            }
        }
    }
    found
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
    use crate::test_support::TmuxServer;

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
            api/auth|::|/home/agent/projects/api|::|1|::|990|::|needs_input|::|/home/agent/projects/api/.claude/worktrees/auth|::|claude|::|\n\
            api/tests|::|/home/agent/projects/api|::|0|::|400|::||::||::||::|\n\
            scratch|::|/tmp|::|0|::|1000|::||::||::|shell|::|\n\
            @@panes\napi/auth|::|90\napi/tests|::|100\nscratch|::|110\n\
            @@ps\n  90     1\n 100     1\n 110     1\n 200   100\n 250   100\n 300   250\n 400     1\n\
            @@agents\n[\n\
              {\"kind\":\"interactive\",\"pid\":200,\"status\":\"busy\"},\n\
              {\"kind\":\"interactive\",\"pid\":300,\"status\":\"waiting\",\"waitingFor\":\"Allow Bash(rm -rf build)?\"},\n\
              {\"kind\":\"interactive\",\"pid\":400,\"status\":\"busy\"},\n\
              {\"kind\":\"background\",\"state\":\"working\"}\n\
            ]\n";
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
        assert_eq!(wts[1].sessions[0].idle_secs, 10);
        assert!(wts[1].sessions[0].attached);
        assert_eq!(h.other[0].name, "scratch");
        // Two Claudes in one session: the one waiting (two levels below the
        // pane's shell) outranks the busy one.
        let tests = &wts[0].sessions[0];
        assert_eq!(tests.activity, Activity::NeedsInput);
        assert_eq!(
            tests.waiting_for.as_deref(),
            Some("Allow Bash(rm -rf build)?")
        );
        // No Claude found: hooks state, else unknown. pid 400 is in no pane.
        assert_eq!(wts[1].sessions[0].activity, Activity::NeedsInput);
        assert_eq!(h.other[0].activity, Activity::Unknown);
        // Claude's icon: started for Claude, or Claude found running.
        assert!(wts[1].sessions[0].runs_claude());
        assert!(tests.runs_claude());
        assert!(!h.other[0].runs_claude());
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

    #[test]
    fn reads_tags_from_before_the_rename() {
        let server = TmuxServer::new("tags");
        server.tmux(&["new-session", "-d", "-s", "old"]);
        server.tmux(&["new-session", "-d", "-s", "new"]);
        server.tmux(&["set-option", "-t", "=old:", "@muxbox_kind", "claude"]);
        server.tmux(&["set-option", "-t", "=old:", "@muxbox_worktree", "/srv/a,b"]);
        server.tmux(&["set-option", "-t", "=new:", "@lemen_kind", "shell"]);
        server.tmux(&["set-option", "-t", "=new:", "@muxbox_kind", "claude"]);
        let snap = fetch(&server.config());
        let other = &snap.hosts[0].other;
        let get = |name| other.iter().find(|s| s.name == name).expect(name);
        assert!(get("old").started_claude);
        assert_eq!(get("old").path, "/srv/a,b");
        assert!(!get("new").started_claude);
    }

    #[test]
    fn threads_keep_the_order_they_were_put_in() {
        let server = TmuxServer::new("order");
        for name in ["a", "b", "c", "d"] {
            server.tmux(&["new-session", "-d", "-s", name]);
        }
        let names = |cfg: &Config| -> Vec<String> {
            fetch(cfg).hosts[0]
                .other
                .iter()
                .map(|s| s.name.clone())
                .collect()
        };
        let cfg = server.config();
        assert_eq!(names(&cfg), ["a", "b", "c", "d"]);
        // d and b were placed; a and c never were, so they follow by name.
        crate::actions::set_order(&cfg, "local", &["d".into(), "b".into()]).unwrap();
        assert_eq!(names(&cfg), ["d", "b", "a", "c"]);
    }
}
