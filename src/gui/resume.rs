//! Threads Claude runs in, kept with their Claude session ID so they can be
//! resumed once their tmux session is gone, as after the host restarted.
//! Each starts with an ID of lemur's, and refreshes keep it current.

use super::settings::Prefs;
use super::{Lemur, sessions};
use crate::actions;
use crate::config::{ClaudeThread, Config, leaf};
use gpui_kit::*;

impl Lemur {
    /// Keeps `thread` to resume, in place of its session's entry.
    pub(super) fn keep_thread(&mut self, thread: ClaudeThread, cx: &mut Context<Self>) {
        if self.cfg.threads.contains(&thread) {
            return;
        }
        if let Some(file) = Prefs::get(cx).config_file()
            && let Err(e) = Config::save_thread(file, &thread)
        {
            self.fail(e, cx);
        }
        self.cfg
            .threads
            .retain(|t| !(t.host == thread.host && t.session == thread.session));
        self.cfg.threads.push(thread);
    }

    /// Forgets the kept threads of `sessions` on `host`: they won't be
    /// resumed, and the sidebar drops them.
    pub(super) fn forget_threads(
        &mut self,
        host: &str,
        sessions: &[String],
        cx: &mut Context<Self>,
    ) {
        let kept = self.cfg.threads.len();
        self.cfg
            .threads
            .retain(|t| !(t.host == host && sessions.contains(&t.session)));
        if self.cfg.threads.len() == kept {
            return;
        }
        if let Some(file) = Prefs::get(cx).config_file()
            && let Err(e) = Config::remove_threads(file, host, sessions)
        {
            self.fail(e, cx);
        }
        let threads = &self.cfg.threads;
        self.snap.retain_gone(|t| threads.contains(t));
        // A snapshot fetched before would bring them back.
        self.changes += 1;
        cx.notify();
    }

    /// Keeps each running Claude's session ID as `claude agents` reports
    /// it: a thread lemur didn't start gets one, and one whose Claude
    /// started over (`/clear`) gets the new one.
    pub(super) fn keep_claude_ids(&mut self, cx: &mut Context<Self>) {
        let running: Vec<ClaudeThread> = sessions(&self.snap)
            .filter_map(|(host, s)| {
                let run = s.claude.as_ref().filter(|r| actions::is_uuid(&r.id))?;
                Some(ClaudeThread {
                    host: host.to_string(),
                    session: s.name.clone(),
                    path: run.path.clone(),
                    claude_session: run.id.clone(),
                })
            })
            .collect();
        for thread in running {
            // Its conversation went on in this session: renamed, or
            // resumed by hand.
            let moved: Vec<String> = self
                .cfg
                .threads
                .iter()
                .filter(|t| {
                    t.host == thread.host
                        && t.claude_session == thread.claude_session
                        && t.session != thread.session
                })
                .map(|t| t.session.clone())
                .collect();
            self.forget_threads(&thread.host, &moved, cx);
            self.keep_thread(thread, cx);
        }
    }

    /// Starts gone threads' sessions again, each resuming its Claude
    /// conversation; one alone opens.
    pub(super) fn resume(&mut self, threads: Vec<ClaudeThread>, cx: &mut Context<Self>) {
        let what = match threads.as_slice() {
            [] => return,
            [one] => actions::label(leaf(&one.session)).to_string(),
            many => format!("{} threads", many.len()),
        };
        self.status = Some((format!("resuming {what}…"), false));
        cx.notify();
        let cfg = self.cfg.clone();
        self.in_background(
            cx,
            move || {
                threads
                    .into_iter()
                    .map(|t| {
                        let resumed = actions::resume_thread(&cfg, &t);
                        (t, resumed)
                    })
                    .collect::<Vec<_>>()
            },
            |this, results, window, cx| {
                this.status = None;
                let mut resumed = vec![];
                for (thread, result) in results {
                    match result {
                        Ok(()) => resumed.push(thread),
                        Err(e) => this.fail(
                            e.context(format!(
                                "resuming {}",
                                actions::label(leaf(&thread.session))
                            )),
                            cx,
                        ),
                    }
                }
                this.snap.retain_gone(|t| !resumed.contains(t));
                // Refreshes that started before it was back are stale.
                this.changes += 1;
                if let [one] = resumed.as_slice() {
                    this.open(one.host.clone(), one.session.clone(), window, cx);
                }
                this.refresh(cx).detach();
                cx.notify();
            },
        )
        .detach();
    }

    /// The gone threads of project `index`, to resume all at once.
    pub(super) fn gone_in(&self, index: usize) -> Vec<ClaudeThread> {
        self.snap
            .hosts
            .iter()
            .flat_map(|h| h.bases.iter())
            .filter(|b| b.index == index)
            .flat_map(|b| b.worktrees.iter())
            .flat_map(|w| w.gone.iter().cloned())
            .collect()
    }
}
