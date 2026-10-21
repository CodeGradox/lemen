//! The sidebar: host → base → worktree → session.

use crate::actions;
use crate::config::Config;
use crate::snapshot::{self, Session, Snapshot};
use crate::viewer::{self, Viewer};
use anyhow::Result;
use ratatui::crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers,
};
use ratatui::crossterm::execute;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState, Paragraph};
use ratatui::{DefaultTerminal, Frame};
use std::io::stdout;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

const REFRESH: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, PartialEq)]
enum Target {
    Host(String),
    Base {
        host: String,
        base: usize,
    },
    Worktree {
        host: String,
        base: usize,
        path: String,
    },
    Session {
        host: String,
        name: String,
        base: Option<usize>,
        worktree: Option<String>,
    },
}

impl Target {
    fn base(&self) -> Option<usize> {
        match self {
            Target::Base { base, .. } | Target::Worktree { base, .. } => Some(*base),
            Target::Session { base, .. } => *base,
            Target::Host(_) => None,
        }
    }

    fn worktree(&self) -> Option<(usize, String)> {
        match self {
            Target::Worktree { base, path, .. } => Some((*base, path.clone())),
            Target::Session {
                base: Some(b),
                worktree: Some(w),
                ..
            } => Some((*b, w.clone())),
            _ => None,
        }
    }
}

struct Row {
    target: Target,
    line: Line<'static>,
}

enum Job {
    Refresh,
    NewWorktree {
        base: usize,
        name: String,
    },
    NewThread {
        base: usize,
        worktree: String,
        name: String,
    },
    Kill {
        host: String,
        session: String,
    },
    Upload {
        host: String,
        session: String,
        files: Vec<PathBuf>,
    },
}

enum Done {
    Snapshot(Snapshot),
    Opened { host: String, session: String },
    Info(String),
    Error(String),
}

enum Mode {
    Normal,
    Input {
        prompt: String,
        buf: String,
        then: InputFor,
    },
    Confirm {
        prompt: String,
        job: Job,
    },
}

enum InputFor {
    NewWorktree { base: usize },
    NewThread { base: usize, worktree: String },
}

pub fn run(cfg: Config) -> Result<()> {
    let (jobs_tx, jobs_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    let worker_cfg = cfg.clone();
    std::thread::spawn(move || worker(worker_cfg, jobs_rx, done_tx));

    let mut app = App {
        cfg,
        snap: Snapshot::default(),
        rows: vec![],
        list: ListState::default(),
        mode: Mode::Normal,
        status: String::new(),
        error: false,
        viewer: Viewer::new(),
        jobs: jobs_tx,
        busy: false,
        last: Instant::now(),
        follow: None,
        quit: false,
    };
    app.send(Job::Refresh);
    let mut term = ratatui::init();
    let _ = execute!(stdout(), EnableBracketedPaste);
    let res = app.run(&mut term, &done_rx);
    let _ = execute!(stdout(), DisableBracketedPaste);
    ratatui::restore();
    app.viewer.close(&app.cfg);
    res
}

/// Runs remote work off the UI thread; every job ends with a fresh snapshot.
fn worker(cfg: Config, jobs: Receiver<Job>, done: Sender<Done>) {
    for job in jobs {
        let result = match job {
            Job::Refresh => Ok(None),
            Job::NewWorktree { base, name } => actions::new_worktree_thread(&cfg, base, &name)
                .map(|(host, session)| Some(Done::Opened { host, session })),
            Job::NewThread {
                base,
                worktree,
                name,
            } => actions::new_thread_in(&cfg, base, &worktree, &name)
                .map(|(host, session)| Some(Done::Opened { host, session })),
            Job::Kill { host, session } => actions::kill_session(&cfg, &host, &session)
                .map(|()| Some(Done::Info(format!("killed {session}")))),
            Job::Upload {
                host,
                session,
                files,
            } => actions::upload_and_type(&cfg, &host, &session, &files)
                .map(|paths| Some(Done::Info(format!("sent {} to {session}", paths.join(" "))))),
        };
        let message = result.unwrap_or_else(|e| Some(Done::Error(format!("{e:#}"))));
        if let Some(m) = message
            && done.send(m).is_err()
        {
            return;
        }
        if done.send(Done::Snapshot(snapshot::fetch(&cfg))).is_err() {
            return;
        }
    }
}

struct App {
    cfg: Config,
    snap: Snapshot,
    rows: Vec<Row>,
    list: ListState,
    mode: Mode,
    status: String,
    error: bool,
    viewer: Viewer,
    jobs: Sender<Job>,
    busy: bool,
    last: Instant,
    /// (host, session) to select once it appears.
    follow: Option<(String, String)>,
    quit: bool,
}

impl App {
    fn run(&mut self, term: &mut DefaultTerminal, done: &Receiver<Done>) -> Result<()> {
        while !self.quit {
            term.draw(|f| self.draw(f))?;
            if event::poll(Duration::from_millis(200))? {
                match event::read()? {
                    Event::Key(k) if k.kind == KeyEventKind::Press => self.key(k, term),
                    Event::Paste(text) => self.paste(&text),
                    _ => {}
                }
            }
            while let Ok(d) = done.try_recv() {
                self.done(d, term);
            }
            if !self.busy && self.last.elapsed() >= REFRESH {
                self.send(Job::Refresh);
            }
        }
        Ok(())
    }

    fn send(&mut self, job: Job) {
        self.busy = true;
        let _ = self.jobs.send(job);
    }

    fn info(&mut self, s: impl Into<String>) {
        self.status = s.into();
        self.error = false;
    }

    fn fail(&mut self, s: impl Into<String>) {
        self.status = s.into();
        self.error = true;
    }

    fn done(&mut self, d: Done, term: &mut DefaultTerminal) {
        match d {
            Done::Snapshot(s) => {
                self.snap = s;
                self.busy = false;
                self.last = Instant::now();
                self.rebuild();
            }
            Done::Opened { host, session } => {
                // Select the new session once it shows up in a snapshot.
                self.follow = Some((host.clone(), session.clone()));
                self.open(&host, &session, term);
            }
            Done::Info(m) => self.info(m),
            Done::Error(m) => self.fail(m),
        }
    }

    fn open(&mut self, host: &str, session: &str, term: &mut DefaultTerminal) {
        let Some(h) = self.cfg.host(host).cloned() else {
            return self.fail(format!("unknown host {host}"));
        };
        if viewer::in_wezterm().is_some() {
            match self.viewer.show(&h, session) {
                Ok(()) => self.info(format!("showing {session}")),
                Err(e) => self.fail(format!("{e:#}")),
            }
        } else {
            // Outside WezTerm: attach in this terminal until you detach.
            let _ = execute!(stdout(), DisableBracketedPaste);
            ratatui::restore();
            let res = viewer::attach_here(&h, session);
            *term = ratatui::init();
            let _ = execute!(stdout(), EnableBracketedPaste);
            self.viewer.host = Some(host.to_string());
            self.viewer.session = Some(session.to_string());
            match res {
                Ok(()) => self.info(format!("detached from {session}")),
                Err(e) => self.fail(format!("{e:#}")),
            }
        }
        self.rebuild();
    }

    fn key(&mut self, k: KeyEvent, term: &mut DefaultTerminal) {
        if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c') {
            self.quit = true;
            return;
        }
        match std::mem::replace(&mut self.mode, Mode::Normal) {
            Mode::Normal => self.normal_key(k, term),
            Mode::Input {
                prompt,
                mut buf,
                then,
            } => match k.code {
                KeyCode::Esc => self.info("cancelled"),
                KeyCode::Enter => match actions::validate_name(&buf) {
                    Err(e) => {
                        self.fail(format!("{e:#}"));
                        self.mode = Mode::Input { prompt, buf, then };
                    }
                    Ok(()) => {
                        self.info(format!("starting {buf}…"));
                        self.send(match then {
                            InputFor::NewWorktree { base } => Job::NewWorktree { base, name: buf },
                            InputFor::NewThread { base, worktree } => Job::NewThread {
                                base,
                                worktree,
                                name: buf,
                            },
                        });
                    }
                },
                KeyCode::Backspace => {
                    buf.pop();
                    self.mode = Mode::Input { prompt, buf, then };
                }
                KeyCode::Char(c) => {
                    if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                        buf.push(c);
                    }
                    self.mode = Mode::Input { prompt, buf, then };
                }
                _ => self.mode = Mode::Input { prompt, buf, then },
            },
            Mode::Confirm { job, .. } => {
                if k.code == KeyCode::Char('y') {
                    self.send(job);
                } else {
                    self.info("cancelled");
                }
            }
        }
    }

    fn normal_key(&mut self, k: KeyEvent, term: &mut DefaultTerminal) {
        let sel = self.selected();
        match k.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Char('j') | KeyCode::Down => self.step(1),
            KeyCode::Char('k') | KeyCode::Up => self.step(-1),
            KeyCode::Char('r') => self.send(Job::Refresh),
            KeyCode::Tab => {
                if let Err(e) = self.viewer.focus() {
                    self.fail(format!("{e:#}"));
                }
            }
            KeyCode::Enter => match sel {
                Some(Target::Session { host, name, .. }) => self.open(&host, &name, term),
                Some(Target::Worktree { host, path, .. }) => match self.first_session_in(&path) {
                    Some(name) => self.open(&host, &name, term),
                    None => self.info("no session in this worktree; press t to start one"),
                },
                _ => {}
            },
            KeyCode::Char('n') => match sel.as_ref().and_then(Target::base) {
                Some(base) => {
                    self.mode = Mode::Input {
                        prompt: format!("New worktree in {}", self.cfg.bases[base].name()),
                        buf: String::new(),
                        then: InputFor::NewWorktree { base },
                    }
                }
                None => self.info("select a base, worktree or session first"),
            },
            KeyCode::Char('t') => match sel.as_ref().and_then(Target::worktree) {
                Some((base, worktree)) => {
                    self.mode = Mode::Input {
                        prompt: format!("New thread in {}", last(&worktree)),
                        buf: String::new(),
                        then: InputFor::NewThread { base, worktree },
                    }
                }
                None => self.info("select a worktree or session first"),
            },
            KeyCode::Char('x') => match sel {
                Some(Target::Session { host, name, .. }) => {
                    self.mode = Mode::Confirm {
                        prompt: format!("Kill {name}? Claude stops; the worktree stays."),
                        job: Job::Kill {
                            host,
                            session: name,
                        },
                    }
                }
                _ => self.info("select a session to kill"),
            },
            _ => {}
        }
    }

    /// A drop on the sidebar arrives as pasted text: upload the files and
    /// paste their remote paths into the session in the Claude pane.
    fn paste(&mut self, text: &str) {
        if let Mode::Input { buf, .. } = &mut self.mode {
            buf.extend(
                text.chars()
                    .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_'),
            );
            return;
        }
        let files: Vec<PathBuf> = split_dropped(text).into_iter().map(PathBuf::from).collect();
        if files.is_empty() || !files.iter().all(|f| f.is_file()) {
            return self.info("paste ignored: drop files here to upload them");
        }
        let target = match (&self.viewer.host, &self.viewer.session) {
            (Some(h), Some(s)) => Some((h.clone(), s.clone())),
            _ => match self.selected() {
                Some(Target::Session { host, name, .. }) => Some((host, name)),
                _ => None,
            },
        };
        match target {
            Some((host, session)) => {
                self.info(format!("uploading {} file(s) to {session}…", files.len()));
                self.send(Job::Upload {
                    host,
                    session,
                    files,
                });
            }
            None => self.info("open a session first, then drop files here"),
        }
    }

    fn selected(&self) -> Option<Target> {
        self.list
            .selected()
            .and_then(|i| self.rows.get(i))
            .map(|r| r.target.clone())
    }

    fn step(&mut self, delta: i32) {
        if self.rows.is_empty() {
            return;
        }
        let i = self.list.selected().unwrap_or(0) as i32 + delta;
        self.list
            .select(Some(i.clamp(0, self.rows.len() as i32 - 1) as usize));
    }

    fn first_session_in(&self, path: &str) -> Option<String> {
        self.rows.iter().find_map(|r| match &r.target {
            Target::Session {
                name,
                worktree: Some(w),
                ..
            } if w == path => Some(name.clone()),
            _ => None,
        })
    }

    fn rebuild(&mut self) {
        let keep = self.selected();
        let bold = Style::new().add_modifier(Modifier::BOLD);
        let dim = Style::new().fg(Color::DarkGray);
        let red = Style::new().fg(Color::Red);
        let mut rows = vec![];
        for h in &self.snap.hosts {
            let active = |s: &Session| {
                self.viewer.host.as_deref() == Some(h.name.as_str())
                    && self.viewer.session.as_deref() == Some(&s.name)
            };
            let mut spans = vec![Span::styled(h.name.clone(), bold)];
            if let Some(e) = &h.error {
                spans.push(Span::styled(format!("  {e}"), red));
            }
            rows.push(Row {
                target: Target::Host(h.name.clone()),
                line: Line::from(spans),
            });
            for b in &h.bases {
                let base = &self.cfg.bases[b.index];
                let mut spans = vec![
                    Span::raw("  "),
                    Span::styled(base.name(), bold),
                    Span::styled(format!("  {}", base.path), dim),
                ];
                if let Some(e) = &b.error {
                    spans.push(Span::styled(format!("  {e}"), red));
                }
                rows.push(Row {
                    target: Target::Base {
                        host: h.name.clone(),
                        base: b.index,
                    },
                    line: Line::from(spans),
                });
                let prefix = format!("{}/", base.name());
                for (wi, w) in b.worktrees.iter().enumerate() {
                    // The main checkout reads best as its branch; linked ones by directory.
                    let label = match (&w.branch, wi) {
                        (Some(branch), 0) => branch.clone(),
                        _ => last(&w.path).to_string(),
                    };
                    rows.push(Row {
                        target: Target::Worktree {
                            host: h.name.clone(),
                            base: b.index,
                            path: w.path.clone(),
                        },
                        line: Line::from(format!("    {label}")),
                    });
                    for s in &w.sessions {
                        let label = s.name.strip_prefix(&prefix).unwrap_or(&s.name);
                        rows.push(Row {
                            target: Target::Session {
                                host: h.name.clone(),
                                name: s.name.clone(),
                                base: Some(b.index),
                                worktree: Some(w.path.clone()),
                            },
                            line: session_line("      ", s, label, active(s)),
                        });
                    }
                }
            }
            if !h.other.is_empty() {
                rows.push(Row {
                    target: Target::Host(h.name.clone()),
                    line: Line::styled("  other sessions", dim),
                });
                for s in &h.other {
                    rows.push(Row {
                        target: Target::Session {
                            host: h.name.clone(),
                            name: s.name.clone(),
                            base: None,
                            worktree: None,
                        },
                        line: session_line("    ", s, &s.name, active(s)),
                    });
                }
            }
        }
        self.rows = rows;
        let followed = self.follow.as_ref().and_then(|(fh, fs)| {
            self.rows.iter().position(|r| matches!(&r.target, Target::Session { host, name, .. } if host == fh && name == fs))
        });
        if followed.is_some() {
            self.follow = None;
        }
        let first_session = || {
            self.rows
                .iter()
                .position(|r| matches!(r.target, Target::Session { .. }))
        };
        let index = match (followed, keep) {
            (Some(i), _) => Some(i),
            (None, Some(t)) => self
                .rows
                .iter()
                .position(|r| r.target == t)
                .or(self.list.selected()),
            (None, None) => first_session(),
        };
        self.list.select(match self.rows.len() {
            0 => None,
            n => Some(index.unwrap_or(0).min(n - 1)),
        });
    }

    fn draw(&mut self, f: &mut Frame) {
        let [list_area, footer] =
            Layout::vertical([Constraint::Min(1), Constraint::Length(3)]).areas(f.area());
        let items: Vec<ListItem> = self
            .rows
            .iter()
            .map(|r| ListItem::new(r.line.clone()))
            .collect();
        let list = List::new(items).highlight_style(Style::new().add_modifier(Modifier::REVERSED));
        f.render_stateful_widget(list, list_area, &mut self.list);

        let dim = Style::new().fg(Color::DarkGray);
        let (first, second) = match &self.mode {
            Mode::Normal => (
                Line::styled("n new worktree · t thread · x kill", dim),
                Line::styled("⏎ open · Tab claude · r refresh · q quit", dim),
            ),
            Mode::Input { prompt, buf, .. } => (
                Line::from(format!("{prompt}: {buf}▏")),
                Line::styled("⏎ start · Esc cancel", dim),
            ),
            Mode::Confirm { prompt, .. } => (
                Line::from(prompt.clone()),
                Line::styled("y confirm · any other key cancels", dim),
            ),
        };
        let status = match (self.rows.is_empty() && self.busy, self.error) {
            (true, _) => Line::styled("loading…", dim),
            (false, true) => Line::styled(self.status.clone(), Style::new().fg(Color::Red)),
            (false, false) => Line::styled(self.status.clone(), dim),
        };
        f.render_widget(Paragraph::new(vec![first, second, status]), footer);
    }
}

fn session_line(indent: &str, s: &Session, label: &str, active: bool) -> Line<'static> {
    let (mark, color) = match s.state.as_deref() {
        Some("working") => ("●", Color::Green),
        Some("needs_input") => ("●", Color::Yellow),
        _ => ("○", Color::DarkGray),
    };
    let name = if active {
        Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD)
    } else {
        Style::new()
    };
    let mut info = vec![];
    if let Some(state) = &s.state {
        info.push(state.replace('_', " "));
    }
    info.push(age(s.idle_secs));
    Line::from(vec![
        Span::raw(indent.to_string()),
        Span::styled(format!("{mark} "), Style::new().fg(color)),
        Span::styled(label.to_string(), name),
        Span::styled(
            format!("  {}", info.join(" · ")),
            Style::new().fg(Color::DarkGray),
        ),
    ])
}

fn age(secs: u64) -> String {
    match secs {
        0..60 => "now".into(),
        60..3600 => format!("{}m", secs / 60),
        3600..86400 => format!("{}h", secs / 3600),
        _ => format!("{}d", secs / 86400),
    }
}

fn last(path: &str) -> &str {
    path.trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(path)
}

/// Splits pasted text the way WezTerm quotes dropped files: backslash-escaped
/// spaces by default (`quote_dropped_files = "SpacesOnly"`), or POSIX quotes.
fn split_dropped(text: &str) -> Vec<String> {
    let mut words = vec![];
    let mut cur = String::new();
    let mut in_word = false;
    let mut quote: Option<char> = None;
    let mut chars = text.trim().chars();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some('"'), '\\') => cur.extend(chars.next()),
            (Some(_), c) => cur.push(c),
            (None, '\'' | '"') => {
                quote = Some(c);
                in_word = true;
            }
            (None, '\\') => {
                cur.extend(chars.next());
                in_word = true;
            }
            (None, c) if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut cur));
                    in_word = false;
                }
            }
            (None, c) => {
                cur.push(c);
                in_word = true;
            }
        }
    }
    if in_word {
        words.push(cur);
    }
    words
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_wezterm_dropped_paths() {
        assert_eq!(
            split_dropped("/a/my\\ shot.png /b/c.png"),
            ["/a/my shot.png", "/b/c.png"]
        );
        assert_eq!(
            split_dropped("\"/a/hello (\\$x).png\""),
            ["/a/hello ($x).png"]
        );
        assert_eq!(split_dropped("'/a/b c.png'\n"), ["/a/b c.png"]);
        assert!(split_dropped("  ").is_empty());
    }

    #[test]
    fn ages_read_short() {
        assert_eq!(age(5), "now");
        assert_eq!(age(125), "2m");
        assert_eq!(age(7200), "2h");
        assert_eq!(age(200_000), "2d");
    }
}
