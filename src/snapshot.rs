//! One round trip per host: worktrees of every base, all tmux sessions, and
//! what Claude is doing in each.

use crate::config::{ClaudeThread, Config, Host};
use crate::remote::{Remote, quote, sh_path};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub hosts: Vec<HostView>,
}

impl Snapshot {
    /// The worktrees of every base on `host`.
    pub fn worktrees<'a>(&'a self, host: &'a str) -> impl Iterator<Item = &'a Worktree> {
        self.hosts
            .iter()
            .filter(move |h| h.name == host)
            .flat_map(|h| h.bases.iter().flat_map(|b| b.worktrees.iter()))
    }

    /// Drops the gone threads `keep` turns down.
    pub fn retain_gone(&mut self, keep: impl Fn(&ClaudeThread) -> bool) {
        for h in &mut self.hosts {
            h.gone.retain(&keep);
            for w in h.bases.iter_mut().flat_map(|b| b.worktrees.iter_mut()) {
                w.gone.retain(&keep);
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct HostView {
    pub name: String,
    pub error: Option<String>,
    pub bases: Vec<BaseView>,
    /// Sessions that don't belong to any base's worktrees.
    pub other: Vec<Session>,
    /// Threads kept to resume whose session is gone, outside every base's
    /// worktrees.
    pub gone: Vec<ClaudeThread>,
}

#[derive(Debug, Clone)]
pub struct BaseView {
    /// Index into `Config::bases`.
    pub index: usize,
    pub error: Option<String>,
    /// The main checkout comes first, as `git worktree list` prints it.
    pub worktrees: Vec<Worktree>,
    /// Not a git repository: its one "worktree" is the folder itself, and
    /// no others can be added.
    pub plain: bool,
}

#[derive(Debug, Clone)]
pub struct Worktree {
    pub path: String,
    pub branch: Option<String>,
    /// The commit checked out.
    pub head: Option<String>,
    /// The repository's own checkout (git lists it first), or a plain
    /// folder: the project itself rather than a linked worktree.
    pub main: bool,
    pub sessions: Vec<Session>,
    /// Threads kept to resume whose session is gone, as after the host
    /// restarted.
    pub gone: Vec<ClaudeThread>,
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
    /// lemur started it for Claude rather than as a plain shell.
    pub started_claude: bool,
    /// Its place in its worktree once threads were dragged into an order.
    pub order: Option<u32>,
    /// The Claude conversation running in it, as `claude agents` reports.
    pub claude: Option<ClaudeRun>,
}

/// A Claude conversation running in a session: its ID, and the folder it
/// runs in.
#[derive(Debug, Clone, PartialEq)]
pub struct ClaudeRun {
    pub id: String,
    pub path: String,
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
/// A session without a `@lemur_*` tag is read by its older names, from
/// before the renames: `@lemen_*`, then `@muxbox_*`. tmux takes an order of
/// `0` for false, so the old order is a field of its own.
const FIELDS: &str = "#{session_name}|::|#{session_path}|::|#{session_attached}|::|#{session_activity}|::|#{?@lemur_state,#{@lemur_state},#{?@lemen_state,#{@lemen_state},#{@muxbox_state}}}|::|#{?@lemur_worktree,#{@lemur_worktree},#{?@lemen_worktree,#{@lemen_worktree},#{@muxbox_worktree}}}|::|#{?@lemur_kind,#{@lemur_kind},#{?@lemen_kind,#{@lemen_kind},#{@muxbox_kind}}}|::|#{@lemur_order}|::|#{@lemen_order}";
/// How many fields `FIELDS` asks tmux for.
const FIELD_COUNT: usize = 9;
const PANES: &str = "#{session_name}|::|#{pane_pid}";

/// What Claude is doing. `claude agents` lists every running session with
/// its pid and status; the process tree (`@@ps`) ties each one to the tmux
/// pane it runs in. Starting `claude agents` takes a moment of CPU, so it
/// runs only when a Claude does: a `claude` process, or a live pid in
/// Claude's session registry. Without a registry (an older Claude Code,
/// or none) it runs anyway; no answer leaves the hooks to report.
const CLAUDE: &str = r#"echo '@@agents'
claude_runs() {
  dir=${CLAUDE_CONFIG_DIR:-$HOME/.claude}/sessions
  [ -d "$dir" ] || return 0
  pgrep -x claude >/dev/null 2>&1 && return 0
  for f in "$dir"/*.json; do
    pid=${f##*/}; pid=${pid%.json}
    case $pid in ''|*[!0-9]*) continue ;; esac
    kill -0 "$pid" 2>/dev/null && return 0
  done
  return 1
}
if claude_runs; then
  agents=$(claude agents --json 2>/dev/null) || agents=
  printf '%s\n' "$agents"
  case $agents in ''|'[]') ;; *) echo '@@ps'; ps -A -o pid= -o ppid= ;; esac
else
  echo '[]'
fi
"#;

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
            gone: vec![],
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
            // Only a folder git says isn't a repository is plain; any other
            // failure (no git, dubious ownership) is shown as git's error.
            "echo '@@base {i}'\n( cd {} 2>/dev/null || {{ echo '@@error no such directory'; exit 0; }}\nif out=$(LC_ALL=C git worktree list --porcelain 2>&1); then printf '%s\\n' \"$out\"\nelse case $out in *'not a git repository'*) echo \"@@plain $(pwd -P)\" ;; *) echo \"@@error $out\" | head -n 1 ;; esac; fi )\n",
            sh_path(&b.path)
        );
    }
    s += &format!(
        "echo '@@now'\ndate +%s\necho '@@tmux'\n{tmux} list-sessions -F {} 2>/dev/null || true\necho '@@panes'\n{tmux} list-panes -a -F {} 2>/dev/null || true\n",
        quote(FIELDS),
        quote(PANES),
        tmux = remote.tmux(),
    );
    s += CLAUDE;
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
                    plain: false,
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

    // Worktrees dragged into an order keep it; others follow in git's.
    for b in &mut bases {
        let order = &cfg.bases[b.index].worktree_order;
        b.worktrees.sort_by_key(|w| {
            order
                .iter()
                .position(|p| *p == w.path)
                .unwrap_or(usize::MAX)
        });
    }

    // When `claude agents` answers, it's the whole truth: a session it
    // doesn't list has no Claude, whatever a hook last recorded.
    if let Some(claude) = claude_activity(&agents, &panes, &parents) {
        for s in &mut sessions {
            let found = claude.get(&s.name);
            (s.activity, s.waiting_for) = found.map_or((Activity::Unknown, None), |f| {
                (f.activity, f.waiting_for.clone())
            });
            s.claude = found.and_then(|f| f.run.clone()).map(|(_, run)| run);
        }
    }

    let live: HashSet<String> = sessions.iter().map(|s| s.name.clone()).collect();
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
    // Threads kept to resume whose session is gone go where they ran.
    let mut gone = vec![];
    for t in cfg
        .threads
        .iter()
        .filter(|t| t.host == host.name && !live.contains(&t.session))
    {
        match find_worktree(&bases, &t.path) {
            Some((bi, wi)) => bases[bi].worktrees[wi].gone.push(t.clone()),
            None => gone.push(t.clone()),
        }
    }
    HostView {
        name: host.name.clone(),
        error: None,
        bases,
        other,
        gone,
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
    } else if let Some(p) = line.strip_prefix("@@plain ") {
        base.plain = true;
        base.worktrees.push(Worktree {
            path: p.to_string(),
            branch: None,
            head: None,
            main: true,
            sessions: vec![],
            gone: vec![],
        });
    } else if let Some(p) = line.strip_prefix("worktree ") {
        let main = base.worktrees.is_empty();
        base.worktrees.push(Worktree {
            path: p.to_string(),
            branch: None,
            head: None,
            main,
            sessions: vec![],
            gone: vec![],
        });
    } else if let Some(h) = line.strip_prefix("HEAD ")
        && let Some(w) = base.worktrees.last_mut()
    {
        w.head = Some(h.to_string());
    } else if let Some(b) = line.strip_prefix("branch ")
        && let Some(w) = base.worktrees.last_mut()
    {
        w.branch = Some(b.trim_start_matches("refs/heads/").to_string());
    }
}

fn parse_session(line: &str, now: u64) -> Option<Session> {
    let f: Vec<&str> = line.split(SEP).collect();
    if f.len() != FIELD_COUNT {
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
        order: f[7].parse().or_else(|_| f[8].parse()).ok(),
        claude: None,
    })
}

#[derive(Deserialize)]
struct Agent {
    kind: Option<String>,
    pid: Option<u32>,
    status: Option<String>,
    #[serde(rename = "waitingFor")]
    waiting_for: Option<serde_json::Value>,
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    cwd: Option<String>,
    #[serde(rename = "startedAt")]
    started_at: Option<u64>,
}

/// What `claude agents` says of the Claudes in one tmux session.
struct InSession {
    /// The most pressing one's activity, and what it waits on.
    activity: Activity,
    waiting_for: Option<String>,
    /// The newest one's conversation, and when it started.
    run: Option<(u64, ClaudeRun)>,
}

/// Maps `claude agents --json` output to tmux sessions: each interactive
/// Claude's pid is walked up the process tree to the pane it runs in.
fn claude_activity(
    agents: &str,
    panes: &HashMap<u32, String>,
    parents: &HashMap<u32, u32>,
) -> Option<HashMap<String, InSession>> {
    // No answer means an older Claude Code, or none: hooks report instead.
    let agents: Vec<Agent> = serde_json::from_str(agents.trim()).ok()?;
    let mut found: HashMap<String, InSession> = HashMap::new();
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
        let run = match (agent.session_id, agent.cwd) {
            (Some(id), Some(path)) => Some((agent.started_at.unwrap_or(0), ClaudeRun { id, path })),
            _ => None,
        };
        let mut p = pid;
        for _ in 0..32 {
            if let Some(session) = panes.get(&p) {
                let entry = found.entry(session.clone()).or_insert(InSession {
                    activity: Activity::Unknown,
                    waiting_for: None,
                    run: None,
                });
                if activity > entry.activity {
                    (entry.activity, entry.waiting_for) = (activity, waiting_for);
                }
                if let Some(run) = run
                    && entry.run.as_ref().is_none_or(|(at, _)| run.0 >= *at)
                {
                    entry.run = Some(run);
                }
                break;
            }
            match parents.get(&p) {
                Some(&parent) if parent > 1 => p = parent,
                _ => break,
            }
        }
    }
    Some(found)
}

/// The worktree containing `path`, preferring the deepest match: linked
/// worktrees under `.claude/worktrees/` sit inside the main checkout.
fn find_worktree(bases: &[BaseView], path: &str) -> Option<(usize, usize)> {
    let mut best: Option<(usize, usize, usize)> = None;
    for (bi, b) in bases.iter().enumerate() {
        for (wi, w) in b.worktrees.iter().enumerate() {
            let inside = path
                .strip_prefix(&w.path)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'));
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
    use crate::test_support::{TempDir, TmuxServer};

    /// `claude agents` is asked only when a Claude may run: without a
    /// session registry, or with a live pid in it. Otherwise the answer is
    /// known: none.
    #[test]
    fn claude_agents_runs_only_when_a_claude_does() {
        let dir = TempDir::new("claude-registry");
        let run = || {
            let out = std::process::Command::new("/bin/sh")
                .args(["-c", CLAUDE])
                .env("CLAUDE_CONFIG_DIR", &*dir)
                // Neither pgrep nor claude: only the registry finds a
                // Claude, and asking `claude agents` gets no answer.
                .env("PATH", "/nonexistent")
                .output()
                .unwrap();
            String::from_utf8(out.stdout).unwrap()
        };
        let (asked, none) = ("@@agents\n\n", "@@agents\n[]\n");
        assert_eq!(run(), asked, "no registry");
        let sessions = dir.join("sessions");
        std::fs::create_dir(&sessions).unwrap();
        assert_eq!(run(), none);
        // Ended long ago: no process has that pid.
        std::fs::write(sessions.join("999999999.json"), "{}").unwrap();
        assert_eq!(run(), none);
        let alive = format!("{}.json", std::process::id());
        std::fs::write(sessions.join(alive), "{}").unwrap();
        assert_eq!(run(), asked);
    }

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
            worktree /home/agent/projects/api/.worktrees/x/api\nHEAD 0123456789abcdef\ndetached\n\n\
            @@now\n1000\n@@tmux\n\
            api/auth|::|/home/agent/projects/api|::|1|::|990|::|needs_input|::|/home/agent/projects/api/.claude/worktrees/auth|::|claude|::||::|\n\
            api/tests|::|/home/agent/projects/api|::|0|::|400|::||::||::||::||::|\n\
            scratch|::|/tmp|::|0|::|1000|::||::||::|shell|::||::|\n\
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
        assert_eq!(wts.len(), 3);
        assert_eq!(wts[0].branch.as_deref(), Some("main"));
        assert_eq!(wts[2].branch, None);
        assert_eq!(wts[2].head.as_deref(), Some("0123456789abcdef"));
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
        // `claude agents` answered and doesn't list it: the hook's old state
        // is stale. pid 400 is in no pane.
        assert_eq!(wts[1].sessions[0].activity, Activity::Unknown);
        assert_eq!(h.other[0].activity, Activity::Unknown);
        // Claude's icon: started for Claude, or Claude found running.
        assert!(wts[1].sessions[0].runs_claude());
        assert!(tests.runs_claude());
        assert!(!h.other[0].runs_claude());
    }

    /// Each session learns its Claude's conversation, the newest one's if
    /// it has several; kept threads whose session is gone are filed where
    /// they ran.
    #[test]
    fn claude_conversations_and_gone_threads() {
        let mut cfg = cfg();
        let thread = |session: &str, path: &str| ClaudeThread {
            host: "agent".into(),
            session: session.into(),
            path: path.into(),
            claude_session: "0fdfabeb-ca80-4916-a103-275bbb5e8614".into(),
        };
        let auth = "/home/agent/projects/api/.claude/worktrees/auth";
        cfg.threads = vec![
            thread("api/tests", "/home/agent/projects/api"),
            thread("api/review--calm-heron", auth),
            thread("scratch--bold-wren", "/tmp/x"),
            ClaudeThread {
                host: "other".into(),
                ..thread("elsewhere", "/tmp")
            },
        ];
        let out = format!(
            "@@base 0\nworktree /home/agent/projects/api\nworktree {auth}\n\
            @@now\n1000\n@@tmux\n\
            api/tests|::|/home/agent/projects/api|::|0|::|400|::||::||::||::||::|\n\
            @@panes\napi/tests|::|100\n@@ps\n 100     1\n 200   100\n 300   100\n\
            @@agents\n[\n\
              {{\"kind\":\"interactive\",\"pid\":200,\"status\":\"busy\",\"sessionId\":\"old\",\"cwd\":\"/a\",\"startedAt\":5}},\n\
              {{\"kind\":\"interactive\",\"pid\":300,\"status\":\"idle\",\"sessionId\":\"new\",\"cwd\":\"/b\",\"startedAt\":9}}\n\
            ]\n"
        );
        let h = parse(&cfg, &cfg.hosts[0], &out);
        let tests = &h.bases[0].worktrees[0].sessions[0];
        assert_eq!(tests.activity, Activity::Working);
        assert_eq!(
            tests.claude,
            Some(ClaudeRun {
                id: "new".into(),
                path: "/b".into()
            })
        );
        let gone = |list: &[ClaudeThread]| -> Vec<String> {
            list.iter().map(|t| t.session.clone()).collect()
        };
        assert!(h.bases[0].worktrees[0].gone.is_empty(), "api/tests runs");
        assert_eq!(
            gone(&h.bases[0].worktrees[1].gone),
            ["api/review--calm-heron"]
        );
        assert_eq!(gone(&h.gone), ["scratch--bold-wren"]);
    }

    #[test]
    fn worktrees_keep_the_order_they_were_put_in() {
        let mut cfg = cfg();
        cfg.bases[0].worktree_order = vec!["/r/c".into(), "/r".into()];
        let out = "@@base 0\nworktree /r\nworktree /r/a\nworktree /r/b\nworktree /r/c\n@@now\n1\n@@tmux\n";
        let h = parse(&cfg, &cfg.hosts[0], out);
        let paths: Vec<&str> = h.bases[0]
            .worktrees
            .iter()
            .map(|w| w.path.as_str())
            .collect();
        assert_eq!(paths, ["/r/c", "/r", "/r/a", "/r/b"]);
        // Moved or not, the main checkout is known.
        let main: Vec<bool> = h.bases[0].worktrees.iter().map(|w| w.main).collect();
        assert_eq!(main, [false, true, false, false]);
    }

    /// Without `claude agents` (an older Claude Code), hooks report.
    #[test]
    fn hooks_report_when_claude_agents_cant() {
        let cfg = cfg();
        let out = "@@now\n1000\n@@tmux\n\
            api/auth|::|/srv|::|0|::|990|::|needs_input|::||::|claude|::||::|\n\
            @@panes\napi/auth|::|90\n@@ps\n  90     1\n@@agents\n";
        let h = parse(&cfg, &cfg.hosts[0], out);
        assert_eq!(h.other[0].activity, Activity::NeedsInput);
    }

    #[test]
    fn the_session_format_has_every_field() {
        assert_eq!(FIELDS.split(SEP).count(), FIELD_COUNT);
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

    /// Sessions tagged by muxbox or lemen still read right; the newest
    /// name wins, even with an order of 0.
    #[test]
    fn reads_tags_from_before_the_renames() {
        let server = TmuxServer::new("tags");
        let tag = |session: &str, option: &str, value: &str| {
            server.tmux(&["set-option", "-t", &format!("={session}:"), option, value]);
        };
        for session in ["muxbox", "lemen", "lemur"] {
            server.tmux(&["new-session", "-d", "-s", session]);
        }
        tag("muxbox", "@muxbox_kind", "claude");
        tag("muxbox", "@muxbox_worktree", "/srv/a,b");
        tag("lemen", "@lemen_kind", "claude");
        tag("lemen", "@lemen_worktree", "/srv/c,d");
        tag("lemen", "@lemen_order", "0");
        tag("lemen", "@muxbox_kind", "shell");
        tag("lemur", "@lemur_kind", "shell");
        tag("lemur", "@lemen_kind", "claude");
        tag("lemur", "@lemur_order", "0");
        tag("lemur", "@lemen_order", "3");
        let snap = fetch(&server.config());
        let other = &snap.hosts[0].other;
        let get = |name| other.iter().find(|s| s.name == name).expect(name);
        assert!(get("muxbox").started_claude);
        assert_eq!(get("muxbox").path, "/srv/a,b");
        assert_eq!(get("muxbox").order, None);
        assert!(get("lemen").started_claude);
        assert_eq!(get("lemen").path, "/srv/c,d");
        assert_eq!(get("lemen").order, Some(0));
        assert!(!get("lemur").started_claude);
        assert_eq!(get("lemur").order, Some(0));
    }

    /// A folder without git is one row, holding the threads started in it.
    #[test]
    fn a_folder_without_git_is_a_single_row() {
        let server = TmuxServer::new("plain");
        let dir = std::env::temp_dir().join(format!("lemur-plain-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let real = dir.canonicalize().unwrap().to_string_lossy().into_owned();
        server.tmux(&["new-session", "-d", "-s", "notes/claude--calm-heron"]);
        server.tmux(&[
            "set-option",
            "-t",
            "=notes/claude--calm-heron:",
            "@lemur_worktree",
            &real,
        ]);
        let mut cfg = server.config();
        cfg.bases
            .push(crate::config::Base::new("local", &dir.to_string_lossy()));

        let snap = fetch(&cfg);
        let base = &snap.hosts[0].bases[0];
        assert!(base.plain);
        assert_eq!(base.error, None);
        assert_eq!(base.worktrees.len(), 1);
        assert_eq!(base.worktrees[0].path, real);
        assert_eq!(
            base.worktrees[0].sessions[0].name,
            "notes/claude--calm-heron"
        );
        let _ = std::fs::remove_dir_all(dir);
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
