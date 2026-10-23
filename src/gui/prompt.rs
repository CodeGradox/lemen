//! Names asked for in the spotlight: for a new worktree, a new thread or
//! a rename, and what happens once one is given.

use super::sidebar::claude_icon;
use super::{Ask, Key, Lemur, Prompt, sessions, spotlight, worktree_label};
use crate::actions;
use crate::config::leaf;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::select::{SearchableVec, Select, SelectState};
use gpui_kit::component::{ActiveTheme, Icon, IconName, IndexPath, Sizable, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;

actions!(new_thread, [OtherKind]);

/// Key context of the spotlight for a new thread.
const NEW_THREAD: &str = "NewThread";

/// ↑↓ pick Claude or Shell for a new thread. The name field goes first
/// and passes on the keys it has no use for.
pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("up", OtherKind, Some(NEW_THREAD)),
        KeyBinding::new("down", OtherKind, Some(NEW_THREAD)),
    ]);
}

/// What a new thread is called until you name it.
fn default_label(shell: bool) -> &'static str {
    if shell { "shell" } else { "claude" }
}

/// A new worktree's branch, empty for none, and the branch it starts
/// from, picked from the host's branches once they've loaded.
pub(super) struct WorktreeFields {
    pub(super) branch: Entity<InputState>,
    pub(super) from: Entity<SelectState<SearchableVec<String>>>,
    _events: Subscription,
    _load: Task<()>,
}

enum Done {
    /// With a note about how it went, if there's one.
    Started(Key, Option<String>),
    Renamed {
        host: String,
        old: String,
        new: String,
    },
}

impl Lemur {
    /// Opens the name field, selected so typing replaces it. It asks for a
    /// label, which several threads can share: it starts as `claude` or
    /// `shell`, a free random name for a worktree, or the current label.
    pub(super) fn ask_name(&mut self, what: Ask, window: &mut Window, cx: &mut Context<Self>) {
        let name = match &what {
            Ask::NewWorktree { base } => actions::random_name(|n| self.worktree_taken(*base, n)),
            Ask::NewThread { shell, .. } => default_label(*shell).to_string(),
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
        let worktree = match &what {
            Ask::NewWorktree { base } => Some(self.worktree_fields(*base, window, cx)),
            _ => None,
        };
        self.prompt = Some(Prompt {
            what,
            input,
            worktree,
            error: None,
            _events: events,
        });
        cx.notify();
    }

    /// The branch field, empty, and the branches of `base` to start from,
    /// the default one picked once they've loaded. Without them, the host
    /// picks its default branch.
    fn worktree_fields(
        &self,
        base: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> WorktreeFields {
        let branch = cx.new(|cx| InputState::new(window, cx).placeholder("none, to detach"));
        let from = cx.new(|cx| {
            SelectState::new(SearchableVec::new(Vec::<String>::new()), None, window, cx)
                .searchable(true)
        });
        let events =
            cx.subscribe_in(
                &branch,
                window,
                |this, _, event: &InputEvent, _, cx| match event {
                    InputEvent::PressEnter { .. } => this.submit(cx),
                    InputEvent::Change => {
                        if let Some(prompt) = &mut this.prompt {
                            prompt.error = None;
                        }
                        cx.notify();
                    }
                    _ => {}
                },
            );
        let cfg = self.cfg.clone();
        let load = self.in_background(
            cx,
            move || actions::branches(&cfg, base),
            |this, branches, window, cx| {
                let fields = this.prompt.as_ref().and_then(|p| p.worktree.as_ref());
                if let (Ok(branches), Some(fields)) = (branches, fields) {
                    fields.from.update(cx, |from, cx| {
                        from.set_items(SearchableVec::new(branches), window, cx);
                        from.set_selected_index(Some(IndexPath::default()), window, cx);
                    });
                }
            },
        );
        WorktreeFields {
            branch,
            from,
            _events: events,
            _load: load,
        }
    }

    /// The name field, in the spotlight: what it's for, the name, and a
    /// hint or why the name can't be used. A new thread is Claude or a
    /// shell, picked above the name.
    pub(super) fn prompt_card(&self, prompt: &Prompt, cx: &Context<Self>) -> Div {
        let mut kind = None;
        let (icon, what, hint) = match &prompt.what {
            Ask::NewWorktree { base } => (
                IconName::Folder,
                format!("New worktree in {}", self.cfg.bases[*base].name()),
                "⏎ start · esc cancel",
            ),
            Ask::NewThread {
                base,
                worktree,
                shell,
            } => {
                kind = Some(*shell);
                (
                    IconName::Plus,
                    format!(
                        "New in {}",
                        self.worktree_name(&self.cfg.bases[*base].host, worktree)
                    ),
                    "↑↓ Claude or shell · ⏎ start · esc cancel",
                )
            }
            Ask::Rename { session, .. } => (
                IconName::CaseSensitive,
                format!("Rename {}", actions::label(leaf(session))),
                "⏎ rename · esc cancel",
            ),
        };
        let footer = match &prompt.error {
            Some((error, _)) => spotlight::footer(error.clone(), true, cx),
            None => spotlight::footer(hint, false, cx),
        };
        spotlight::card(cx)
            .child(spotlight::title(icon, what, cx))
            .when_some(kind, |card, shell| {
                card.key_context(NEW_THREAD)
                    .on_action(cx.listener(move |this, _: &OtherKind, window, cx| {
                        this.pick_kind(!shell, window, cx)
                    }))
                    .child(kinds(shell, cx))
            })
            .child(
                div()
                    .p_1()
                    .child(Input::new(&prompt.input).appearance(false)),
            )
            .when_some(prompt.worktree.as_ref(), |card, fields| {
                card.child(
                    v_flex()
                        .px_3()
                        .pb_2()
                        .gap_2()
                        .child(field("Branch", Input::new(&fields.branch).small(), cx))
                        .child(field(
                            "From",
                            Select::new(&fields.from)
                                .small()
                                .placeholder("the default branch")
                                .search_placeholder("Search branches"),
                            cx,
                        )),
                )
            })
            .child(footer)
    }

    /// Picks Claude or, with `shell`, a shell for the new thread asked for.
    /// The name follows, unless you typed one.
    fn pick_kind(&mut self, shell: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Prompt {
            what: Ask::NewThread { shell: picked, .. },
            input,
            ..
        }) = &mut self.prompt
        else {
            return;
        };
        if *picked != shell {
            let typed = input.read(cx).value() != default_label(*picked);
            *picked = shell;
            if !typed {
                input.update(cx, |i, cx| {
                    i.set_value(default_label(shell), window, cx);
                    i.select_all(window, cx);
                });
            }
        }
        // A click on a kind took the keyboard.
        input.update(cx, |i, cx| i.focus(window, cx));
        cx.notify();
    }

    /// ⌘J: asks for a new Claude thread or shell in the worktree of the one
    /// on show.
    pub(super) fn new_thread_here(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.prompt.is_some() || self.picker.is_some() {
            return;
        }
        let Some((host, session)) = self.active.clone() else {
            return;
        };
        let Some((worktree, _)) = self.worktree_of(&host, &session) else {
            return;
        };
        let base = self
            .snap
            .hosts
            .iter()
            .filter(|h| h.name == host)
            .flat_map(|h| h.bases.iter())
            .find(|b| b.worktrees.iter().any(|w| w.path == worktree))
            .map(|b| b.index);
        if let Some(base) = base {
            let ask = Ask::NewThread {
                base,
                worktree,
                shell: false,
            };
            self.ask_name(ask, window, cx);
        }
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
        let (branch, from) = match &prompt.worktree {
            Some(fields) => {
                let branch = fields.branch.read(cx).value().trim().to_string();
                let from = fields.from.read(cx).selected_value().cloned();
                ((!branch.is_empty()).then_some(branch), from)
            }
            None => (None, None),
        };
        let valid = actions::validate_name(&name)
            .and_then(|()| branch.as_deref().map_or(Ok(()), actions::validate_branch));
        if let Err(e) = valid {
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
                Ask::NewWorktree { base } => actions::new_worktree_thread(
                    &cfg,
                    base,
                    &name,
                    &thread,
                    branch.as_deref(),
                    from.as_deref(),
                )
                .map(|(started, note)| Done::Started(started, note)),
                Ask::NewThread {
                    base,
                    worktree,
                    shell,
                } => actions::new_thread_in(&cfg, base, &worktree, &thread, shell)
                    .map(|started| Done::Started(started, None)),
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
                    Ok(Done::Started((host, session), note)) => {
                        this.status = note.map(|note| (note, false));
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
        for key in &mut self.recent {
            if *key == old_key {
                *key = new_key.clone();
            }
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

/// Claude and Shell, to pick from for a new thread; with `shell` the
/// shell is picked.
fn kinds(shell: bool, cx: &Context<Lemur>) -> Div {
    let theme = cx.theme();
    let row = |label: &'static str, icon: Icon, is_shell: bool| {
        h_flex()
            .id(label)
            .debug_selector(move || format!("kind {label}"))
            .px_2()
            .py_1()
            .gap_2()
            .rounded(theme.radius)
            .text_sm()
            .cursor_pointer()
            .when(shell == is_shell, |row| {
                row.bg(theme.accent).text_color(theme.accent_foreground)
            })
            .child(icon.small())
            .child(label)
            .on_click(cx.listener(move |this, _, window, cx| this.pick_kind(is_shell, window, cx)))
    };
    v_flex()
        .p_1()
        .gap_0p5()
        .border_b_1()
        .border_color(theme.border)
        .child(row("Claude", claude_icon(), false))
        .child(row("Shell", Icon::new(IconName::SquareTerminal), true))
}

/// A line of the new worktree's fields: what it is, then the field.
fn field(label: &str, input: impl IntoElement, cx: &App) -> Div {
    let selector = format!("field {label}");
    h_flex()
        .debug_selector(move || selector)
        .gap_2()
        .child(
            div()
                .w(px(56.0))
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(label.to_string()),
        )
        .child(div().flex_1().child(input))
}
