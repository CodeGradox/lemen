//! Names asked for in the spotlight: for a new worktree, a new thread or
//! a rename, and what happens once one is given.

use super::{Ask, Key, Lemen, Prompt, sessions, spotlight, worktree_label};
use crate::actions;
use crate::config::leaf;
use gpui_kit::component::IconName;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::*;

enum Done {
    Started(Key),
    Renamed {
        host: String,
        old: String,
        new: String,
    },
}

impl Lemen {
    /// Opens the name field, selected so typing replaces it. It asks for a
    /// label, which several threads can share: it starts as `claude` or
    /// `shell`, a free random name for a worktree, or the current label.
    pub(super) fn ask_name(&mut self, what: Ask, window: &mut Window, cx: &mut Context<Self>) {
        let name = match &what {
            Ask::NewWorktree { base } => actions::random_name(|n| self.worktree_taken(*base, n)),
            Ask::NewThread { shell: true, .. } => "shell".to_string(),
            Ask::NewThread { shell: false, .. } => "claude".to_string(),
            Ask::Rename { session, .. } => actions::label(leaf(session)).to_string(),
        };
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("name")
                .default_value(name)
        });
        let events =
            cx.subscribe_in(
                &input,
                window,
                |this, _, event: &InputEvent, _, cx| match event {
                    InputEvent::PressEnter { .. } => this.submit(cx),
                    // Enter also reports a change; only new text answers the error.
                    InputEvent::Change => {
                        if let Some(prompt) = &mut this.prompt {
                            let text = prompt.input.read(cx).value();
                            if prompt.error.as_ref().is_some_and(|(_, name)| *name != text) {
                                prompt.error = None;
                            }
                        }
                        cx.notify();
                    }
                    _ => {}
                },
            );
        input.update(cx, |i, cx| {
            i.focus(window, cx);
            i.select_all(window, cx);
        });
        self.prompt = Some(Prompt {
            what,
            input,
            error: None,
            _events: events,
        });
        cx.notify();
    }

    /// The name field, in the spotlight: what it's for, the name, and a
    /// hint or why the name can't be used.
    pub(super) fn prompt_card(&self, prompt: &Prompt, cx: &App) -> Div {
        let (icon, what, verb) = match &prompt.what {
            Ask::NewWorktree { base } => (
                IconName::Folder,
                format!("New worktree in {}", self.cfg.bases[*base].name()),
                "start",
            ),
            Ask::NewThread {
                base,
                worktree,
                shell,
            } => (
                if *shell {
                    IconName::SquareTerminal
                } else {
                    IconName::Plus
                },
                format!(
                    "New {} in {}",
                    if *shell { "shell" } else { "Claude thread" },
                    self.worktree_name(&self.cfg.bases[*base].host, worktree)
                ),
                "start",
            ),
            Ask::Rename { session, .. } => (
                IconName::CaseSensitive,
                format!("Rename {}", actions::label(leaf(session))),
                "rename",
            ),
        };
        let footer = match &prompt.error {
            Some((error, _)) => spotlight::footer(error.clone(), true, cx),
            None => spotlight::footer(format!("⏎ {verb} · esc cancel"), false, cx),
        };
        spotlight::card(cx)
            .child(spotlight::title(icon, what, cx))
            .child(
                div()
                    .p_1()
                    .child(Input::new(&prompt.input).appearance(false)),
            )
            .child(footer)
    }

    pub(super) fn close_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.prompt = None;
        self.focus_terminal(window, cx);
    }

    fn submit(&mut self, cx: &mut Context<Self>) {
        let Some(prompt) = &mut self.prompt else {
            return;
        };
        let text = prompt.input.read(cx).value().to_string();
        let name = text.trim().to_string();
        if let Err(e) = actions::validate_name(&name) {
            prompt.error = Some((format!("{e:#}"), text));
            cx.notify();
            return;
        }
        let Some(Prompt { what, .. }) = self.prompt.take() else {
            return;
        };
        let thread = match &what {
            Ask::NewWorktree { base } | Ask::NewThread { base, .. } => {
                self.unique_thread(*base, &name)
            }
            Ask::Rename { .. } => String::new(),
        };
        let cfg = self.cfg.clone();
        let doing = match &what {
            Ask::Rename { host, session } => {
                self.renaming = Some((host.clone(), session.clone()));
                "renaming"
            }
            _ => "starting",
        };
        self.status = Some((format!("{doing} {name}…"), false));
        cx.notify();
        self.in_background(
            cx,
            move || match what {
                Ask::NewWorktree { base } => {
                    actions::new_worktree_thread(&cfg, base, &name, &thread).map(Done::Started)
                }
                Ask::NewThread {
                    base,
                    worktree,
                    shell,
                } => {
                    actions::new_thread_in(&cfg, base, &worktree, &thread, shell).map(Done::Started)
                }
                Ask::Rename { host, session } => {
                    actions::rename_session(&cfg, &host, &session, &name).map(|new| Done::Renamed {
                        host,
                        old: session,
                        new,
                    })
                }
            },
            |this, done, window, cx| {
                this.renaming = None;
                match done {
                    Ok(Done::Started((host, session))) => {
                        this.status = None;
                        this.open(host, session, window, cx);
                    }
                    Ok(Done::Renamed { host, old, new }) => {
                        this.status = None;
                        this.follow_rename(host, old, new, cx);
                    }
                    Err(e) => this.fail(e, cx),
                }
                this.refresh(cx).detach();
            },
        )
        .detach();
    }

    /// Moves the open terminal and the selection to a renamed session.
    fn follow_rename(&mut self, host: String, old: String, new: String, cx: &mut Context<Self>) {
        let old_key = (host.clone(), old);
        let new_key = (host.clone(), new.clone());
        if let Some(view) = self.terminals.remove(&old_key) {
            // Reconnects attach by name, so the view must learn the new one.
            view.update(cx, |v, _| v.session = new.clone());
            self.terminals.insert(new_key.clone(), view);
        }
        if self.active.as_ref() == Some(&old_key) {
            self.active = Some(new_key);
            self.remember(host, new);
        }
        cx.notify();
    }

    /// A worktree's name as the sidebar shows it.
    fn worktree_name(&self, host: &str, path: &str) -> String {
        self.snap
            .worktrees(host)
            .find(|w| w.path == path)
            .map(worktree_label)
            .unwrap_or_else(|| leaf(path).to_string())
    }

    /// Whether `base` already has a worktree called `name`.
    fn worktree_taken(&self, base: usize, name: &str) -> bool {
        self.snap
            .hosts
            .iter()
            .flat_map(|h| h.bases.iter())
            .filter(|v| v.index == base)
            .flat_map(|v| v.worktrees.iter())
            .any(|w| leaf(&w.path) == name)
    }

    /// A thread name for `label` in `base` that no session uses yet.
    fn unique_thread(&self, base: usize, label: &str) -> String {
        let b = &self.cfg.bases[base];
        actions::unique_thread(label, |thread| {
            let session = b.session_name(thread);
            sessions(&self.snap).any(|(h, s)| h == b.host && s.name == session)
        })
    }
}
