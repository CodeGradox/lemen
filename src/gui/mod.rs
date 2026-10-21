//! The window: a sidebar of sessions next to a live terminal.
//!
//! Every session you open keeps its own terminal (`tmux attach` over SSH),
//! so switching is instant and scrollback survives.

mod keys;
mod terminal;
mod terminal_view;

use crate::actions;
use crate::attach::Active;
use crate::config::{Config, ThemeChoice};
use crate::snapshot::{self, Activity, Session, Snapshot};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::component::sidebar::{
    Sidebar, SidebarCollapsible, SidebarGroup, SidebarHeader, SidebarMenu, SidebarMenuItem,
};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{
    ActiveTheme, IconName, Sizable, StyledExt, Theme, ThemeMode, h_flex, v_flex,
};
use gpui_kit::*;
use std::collections::{HashMap, HashSet};
use std::time::Duration;
use terminal_view::TerminalView;

actions!(muxbox, [Quit, ToggleSidebar]);

const REFRESH: Duration = Duration::from_secs(3);

type Key = (String, String);

/// What the name field in the header is asking for.
enum Ask {
    NewWorktree { base: usize },
    NewThread { base: usize, worktree: String },
    Rename { host: String, session: String },
}

enum Done {
    Started(Key),
    Renamed {
        host: String,
        old: String,
        new: String,
    },
}

struct Prompt {
    what: Ask,
    input: Entity<InputState>,
    _events: Subscription,
}

struct Muxbox {
    cfg: Config,
    snap: Snapshot,
    loaded: bool,
    terminals: HashMap<Key, Entity<TerminalView>>,
    active: Option<Key>,
    collapsed: bool,
    prompt: Option<Prompt>,
    confirm_kill: Option<Key>,
    /// A session being renamed keeps its terminal while its old name disappears.
    renaming: Option<Key>,
    /// Sessions whose Claude was working at the last refresh.
    working: HashSet<Key>,
    /// Sessions that finished while you were looking at another one.
    done: HashSet<Key>,
    status: Option<(String, bool)>,
    _refresh: Task<()>,
    _appearance: Option<Subscription>,
}

pub fn run(cfg: Config) -> anyhow::Result<()> {
    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(move |cx| {
            gpui_kit::init(cx);
            match cfg.theme {
                ThemeChoice::System => Theme::sync_system_appearance(None, cx),
                ThemeChoice::Light => Theme::change(ThemeMode::Light, None, cx),
                ThemeChoice::Dark => Theme::change(ThemeMode::Dark, None, cx),
            }
            cx.bind_keys([
                KeyBinding::new("cmd-q", Quit, None),
                KeyBinding::new("cmd-b", ToggleSidebar, None),
                // gpui-component's Root moves focus on Tab; in a terminal it belongs to the program.
                KeyBinding::new("tab", NoAction, Some(terminal_view::CONTEXT)),
                KeyBinding::new("shift-tab", NoAction, Some(terminal_view::CONTEXT)),
            ]);
            cx.on_action(|_: &Quit, cx| cx.quit());
            cx.on_window_closed(|cx, _| cx.quit()).detach();
            let options = WindowOptions {
                titlebar: Some(TitlebarOptions {
                    title: Some("muxbox".into()),
                    ..Default::default()
                }),
                window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                    None,
                    size(px(1280.0), px(820.0)),
                    cx,
                ))),
                ..Default::default()
            };
            gpui_kit::open_window(options, cx, |window, cx| {
                cx.new(|cx| Muxbox::new(cfg, window, cx))
            })
            .expect("failed to open window");
            cx.activate(true);
        });
    Ok(())
}

impl Muxbox {
    fn new(cfg: Config, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let refresh_cfg = cfg.clone();
        // Reopen the session from last time once we know it still exists.
        let mut restore = Active::load().ok().map(|a| (a.host, a.session));
        let window_handle = window.window_handle();
        let refresh = cx.spawn(async move |this, cx| {
            loop {
                let cfg = refresh_cfg.clone();
                let snap = cx
                    .background_spawn(async move { snapshot::fetch(&cfg) })
                    .await;
                if this
                    .update(cx, |this, cx| this.set_snapshot(snap, cx))
                    .is_err()
                {
                    break;
                }
                if let Some((host, session)) = restore.take() {
                    window_handle
                        .update(cx, |_, window, cx| {
                            this.update(cx, |this, cx| {
                                let exists = sessions(&this.snap)
                                    .any(|(h, s)| h == host && s.name == session);
                                if exists && this.active.is_none() {
                                    this.open(host, session, window, cx);
                                }
                            })
                        })
                        .ok();
                }
                cx.background_executor().timer(REFRESH).await;
            }
        });
        // Follow macOS when it switches between light and dark.
        let appearance = (cfg.theme == ThemeChoice::System).then(|| {
            cx.observe_window_appearance(window, |_, window, cx| {
                Theme::sync_system_appearance(Some(window), cx);
            })
        });
        Muxbox {
            cfg,
            snap: Snapshot::default(),
            loaded: false,
            terminals: HashMap::new(),
            active: None,
            collapsed: false,
            prompt: None,
            confirm_kill: None,
            renaming: None,
            working: HashSet::new(),
            done: HashSet::new(),
            status: None,
            _refresh: refresh,
            _appearance: appearance,
        }
    }

    fn set_snapshot(&mut self, snap: Snapshot, cx: &mut Context<Self>) {
        // Forget terminals whose session is gone (killed, or ended elsewhere).
        let live: Vec<Key> = sessions(&snap)
            .map(|(h, s)| (h.to_string(), s.name.clone()))
            .collect();
        self.terminals
            .retain(|k, _| live.contains(k) || self.renaming.as_ref() == Some(k));
        if self
            .active
            .as_ref()
            .is_some_and(|a| !live.contains(a) && self.renaming.as_ref() != Some(a))
        {
            self.active = None;
        }
        // Working -> idle while you're elsewhere means "done, unseen".
        let activity: HashMap<Key, Activity> = sessions(&snap)
            .map(|(h, s)| ((h.to_string(), s.name.clone()), s.activity))
            .collect();
        let working: HashSet<Key> = activity
            .iter()
            .filter(|(_, a)| **a == Activity::Working)
            .map(|(k, _)| k.clone())
            .collect();
        for k in self.working.difference(&working) {
            let finished = matches!(activity.get(k), Some(Activity::Idle | Activity::Unknown));
            if finished && self.active.as_ref() != Some(k) {
                self.done.insert(k.clone());
            }
        }
        self.done
            .retain(|k| live.contains(k) && !working.contains(k));
        self.working = working;
        self.snap = snap;
        self.loaded = true;
        cx.notify();
    }

    fn refresh_now(&mut self, cx: &mut Context<Self>) {
        let cfg = self.cfg.clone();
        cx.spawn(async move |this, cx| {
            let snap = cx
                .background_spawn(async move { snapshot::fetch(&cfg) })
                .await;
            this.update(cx, |this, cx| this.set_snapshot(snap, cx)).ok();
        })
        .detach();
    }

    fn open(&mut self, host: String, session: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(h) = self.cfg.host(&host).cloned() else {
            return;
        };
        let key = (host.clone(), session.clone());
        let view = self
            .terminals
            .entry(key.clone())
            .or_insert_with(|| cx.new(|cx| TerminalView::new(h, session.clone(), cx)))
            .clone();
        if view.read(cx).exited() {
            view.update(cx, |v, cx| v.connect(cx));
        }
        self.done.remove(&key);
        self.active = Some(key);
        self.confirm_kill = None;
        let _ = Active { host, session }.save();
        view.read(cx).focus_handle(cx).focus(window, cx);
        cx.notify();
    }

    /// Opens the name field, selected so typing replaces it. New threads get
    /// a free random name; a rename starts from the current one.
    fn ask_name(&mut self, what: Ask, window: &mut Window, cx: &mut Context<Self>) {
        let name = match &what {
            Ask::NewWorktree { base } | Ask::NewThread { base, .. } => {
                actions::random_name(|n| self.name_taken(*base, n))
            }
            Ask::Rename { session, .. } => {
                session.rsplit('/').next().unwrap_or(session).to_string()
            }
        };
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("name")
                .default_value(name)
        });
        let events = cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| {
            if let InputEvent::PressEnter { .. } = event {
                this.submit(window, cx);
            }
        });
        input.update(cx, |i, cx| {
            i.focus(window, cx);
            i.select_all(window, cx);
        });
        self.prompt = Some(Prompt {
            what,
            input,
            _events: events,
        });
        cx.notify();
    }

    /// Whether a thread or worktree called `name` already exists in `base`.
    fn name_taken(&self, base: usize, name: &str) -> bool {
        let b = &self.cfg.bases[base];
        let session = format!("{}/{name}", b.name());
        let in_tmux = sessions(&self.snap).any(|(h, s)| h == b.host && s.name == session);
        let as_worktree = self
            .snap
            .hosts
            .iter()
            .flat_map(|h| h.bases.iter())
            .filter(|v| v.index == base)
            .flat_map(|v| v.worktrees.iter())
            .any(|w| w.path.rsplit('/').next() == Some(name));
        in_tmux || as_worktree
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(prompt) = &self.prompt else { return };
        let name = prompt.input.read(cx).value().trim().to_string();
        if let Err(e) = actions::validate_name(&name) {
            self.status = Some((format!("{e:#}"), true));
            cx.notify();
            return;
        }
        let Some(Prompt { what, .. }) = self.prompt.take() else {
            return;
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
        let window_handle = window.window_handle();
        cx.spawn(async move |this, cx| {
            let done = cx
                .background_spawn(async move {
                    match what {
                        Ask::NewWorktree { base } => {
                            actions::new_worktree_thread(&cfg, base, &name).map(Done::Started)
                        }
                        Ask::NewThread { base, worktree } => {
                            actions::new_thread_in(&cfg, base, &worktree, &name).map(Done::Started)
                        }
                        Ask::Rename { host, session } => {
                            actions::rename_session(&cfg, &host, &session, &name).map(|new| {
                                Done::Renamed {
                                    host,
                                    old: session,
                                    new,
                                }
                            })
                        }
                    }
                })
                .await;
            window_handle
                .update(cx, |_, window, cx| {
                    this.update(cx, |this, cx| {
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
                            Err(e) => this.status = Some((format!("{e:#}"), true)),
                        }
                        this.refresh_now(cx);
                    })
                })
                .ok();
        })
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
            let _ = Active { host, session: new }.save();
        }
        cx.notify();
    }

    /// First click arms, second click kills; the worktree stays.
    fn kill(&mut self, key: Key, cx: &mut Context<Self>) {
        if self.confirm_kill.as_ref() != Some(&key) {
            self.confirm_kill = Some(key);
            cx.notify();
            return;
        }
        self.confirm_kill = None;
        let cfg = self.cfg.clone();
        cx.spawn(async move |this, cx| {
            let (host, session) = key;
            let killed = cx
                .background_spawn(async move { actions::kill_session(&cfg, &host, &session) })
                .await;
            this.update(cx, |this, cx| {
                if let Err(e) = killed {
                    this.status = Some((format!("{e:#}"), true));
                }
                this.refresh_now(cx);
            })
            .ok();
        })
        .detach();
    }

    fn sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let this = cx.entity().downgrade();
        let mut groups: Vec<SidebarGroup<SidebarMenu>> = vec![];
        for h in &self.snap.hosts {
            if let Some(e) = &h.error {
                groups.push(
                    SidebarGroup::new(h.name.clone()).child(
                        SidebarMenu::new().child(
                            SidebarMenuItem::new(e.clone())
                                .icon(IconName::TriangleAlert)
                                .disable(true),
                        ),
                    ),
                );
                continue;
            }
            for b in &h.bases {
                let base = &self.cfg.bases[b.index];
                let mut menu = SidebarMenu::new();
                let index = b.index;
                menu = menu.child(
                    SidebarMenuItem::new("New worktree")
                        .icon(IconName::Plus)
                        .on_click({
                            let this = this.clone();
                            move |_, window, cx| {
                                this.update(cx, |this, cx| {
                                    this.ask_name(Ask::NewWorktree { base: index }, window, cx)
                                })
                                .ok();
                            }
                        }),
                );
                if let Some(e) = &b.error {
                    menu = menu.child(
                        SidebarMenuItem::new(e.clone())
                            .icon(IconName::TriangleAlert)
                            .disable(true),
                    );
                }
                let prefix = format!("{}/", base.name());
                for (wi, w) in b.worktrees.iter().enumerate() {
                    let label = match (&w.branch, wi) {
                        (Some(branch), 0) => branch.clone(),
                        _ => w.path.rsplit('/').next().unwrap_or(&w.path).to_string(),
                    };
                    let worktree = w.path.clone();
                    let add = {
                        let this = this.clone();
                        move |_: &mut Window, _: &mut App| {
                            let this = this.clone();
                            let worktree = worktree.clone();
                            Button::new("add")
                                .icon(IconName::Plus)
                                .ghost()
                                .xsmall()
                                .tooltip("New thread here")
                                .on_click(move |_, window, cx| {
                                    let what = Ask::NewThread {
                                        base: index,
                                        worktree: worktree.clone(),
                                    };
                                    this.update(cx, |this, cx| this.ask_name(what, window, cx))
                                        .ok();
                                })
                        }
                    };
                    let threads = w
                        .sessions
                        .iter()
                        .map(|s| self.session_item(&h.name, s, &prefix, cx));
                    menu = menu.child(
                        SidebarMenuItem::new(label)
                            .icon(IconName::Folder)
                            .default_open(true)
                            .suffix(add)
                            .children(threads),
                    );
                }
                groups.push(SidebarGroup::new(format!("{} · {}", base.name(), h.name)).child(menu));
            }
            if !h.other.is_empty() {
                let items = h
                    .other
                    .iter()
                    .map(|s| self.session_item(&h.name, s, "", cx));
                groups.push(
                    SidebarGroup::new(format!("other · {}", h.name))
                        .child(SidebarMenu::new().children(items)),
                );
            }
        }
        let refresh = {
            let this = this.clone();
            Button::new("refresh")
                .icon(IconName::RefreshCw)
                .ghost()
                .xsmall()
                .tooltip("Refresh")
                .on_click(move |_, _, cx| {
                    this.update(cx, |this, cx| this.refresh_now(cx)).ok();
                })
        };
        Sidebar::new("sessions")
            .collapsible(SidebarCollapsible::Offcanvas)
            .collapsed(self.collapsed)
            .header(
                SidebarHeader::new().child(
                    h_flex()
                        .w_full()
                        .justify_between()
                        .child(div().font_semibold().child("muxbox"))
                        .child(refresh),
                ),
            )
            .children(groups)
    }

    fn session_item(
        &self,
        host: &str,
        s: &Session,
        prefix: &str,
        cx: &mut Context<Self>,
    ) -> SidebarMenuItem {
        let this = cx.entity().downgrade();
        let key: Key = (host.to_string(), s.name.clone());
        let active = self.active.as_ref() == Some(&key);
        let arming = self.confirm_kill.as_ref() == Some(&key);
        let label = s.name.strip_prefix(prefix).unwrap_or(&s.name).to_string();
        let working = s.activity == Activity::Working;
        let (dot, tip): (Hsla, SharedString) = match s.activity {
            Activity::NeedsInput => (
                cx.theme().warning,
                match &s.waiting_for {
                    Some(what) => format!("Needs input: {what}").into(),
                    None => "Needs input".into(),
                },
            ),
            Activity::Working => (cx.theme().success, "Working".into()),
            _ if self.done.contains(&key) => (cx.theme().info, "Done".into()),
            Activity::Idle => (cx.theme().muted_foreground.opacity(0.5), "Idle".into()),
            Activity::Unknown => (
                cx.theme().muted_foreground.opacity(0.5),
                "No Claude found".into(),
            ),
        };
        let detail = age(s.idle_secs);
        let muted = cx.theme().muted_foreground;
        // Click opens; a double click (its second click) starts a rename.
        let open = {
            let this = this.clone();
            let key = key.clone();
            move |event: &ClickEvent, window: &mut Window, cx: &mut App| {
                let (host, session) = key.clone();
                this.update(cx, |this, cx| {
                    if event.click_count() >= 2 {
                        this.ask_name(Ask::Rename { host, session }, window, cx);
                    } else {
                        this.open(host, session, window, cx);
                    }
                })
                .ok();
            }
        };
        let menu = {
            let this = this.clone();
            let key = key.clone();
            move |menu: PopupMenu, _: &mut Window, _: &mut App| {
                let this = this.clone();
                let key = key.clone();
                menu.item(
                    PopupMenuItem::new("Rename…").on_click(move |_, window, cx| {
                        let (host, session) = key.clone();
                        this.update(cx, |this, cx| {
                            this.ask_name(Ask::Rename { host, session }, window, cx)
                        })
                        .ok();
                    }),
                )
            }
        };
        let suffix = move |_: &mut Window, _: &mut App| {
            let this = this.clone();
            let key = key.clone();
            let status = status_dot(&key, dot, working, tip.clone());
            let kill = Button::new("kill")
                .ghost()
                .xsmall()
                .on_click(move |_, _, cx| {
                    // Otherwise the row opens the session too, which disarms the kill.
                    cx.stop_propagation();
                    this.update(cx, |this, cx| this.kill(key.clone(), cx)).ok();
                });
            let kill = if arming {
                kill.danger().label("Kill?")
            } else {
                kill.icon(IconName::Close).tooltip("Kill session")
            };
            h_flex()
                .gap_1()
                .items_center()
                .child(div().text_xs().text_color(muted).child(detail.clone()))
                .child(status)
                .child(kill)
        };
        SidebarMenuItem::new(label)
            .icon(IconName::SquareTerminal)
            .active(active)
            .on_click(open)
            .context_menu(menu)
            .suffix(suffix)
    }

    fn header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let this = cx.entity().downgrade();
        let toggle = Button::new("toggle-sidebar")
            .icon(if self.collapsed {
                IconName::PanelLeftOpen
            } else {
                IconName::PanelLeftClose
            })
            .ghost()
            .small()
            .tooltip("Toggle sidebar (⌘B)")
            .on_click(move |_, _, cx| {
                this.update(cx, |this, cx| {
                    this.collapsed = !this.collapsed;
                    cx.notify();
                })
                .ok();
            });
        let title = match &self.active {
            Some((host, session)) => format!("{session}  ·  {host}"),
            None => String::new(),
        };
        let mut bar = h_flex()
            .h_10()
            .px_2()
            .gap_2()
            .items_center()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(toggle)
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(title),
            );
        if let Some(prompt) = &self.prompt {
            let what = match &prompt.what {
                Ask::NewWorktree { base } => {
                    format!("New worktree in {}", self.cfg.bases[*base].name())
                }
                Ask::NewThread { worktree, .. } => {
                    format!(
                        "New thread in {}",
                        worktree.rsplit('/').next().unwrap_or(worktree)
                    )
                }
                Ask::Rename { session, .. } => format!("Rename {session}"),
            };
            bar = bar
                .child(div().flex_1())
                .child(div().text_sm().child(what))
                .child(div().w_64().child(Input::new(&prompt.input).small()))
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("⏎ start · esc cancel"),
                );
        } else if let Some((message, error)) = &self.status {
            let color = if *error {
                cx.theme().danger
            } else {
                cx.theme().muted_foreground
            };
            bar = bar
                .child(div().flex_1())
                .child(div().text_sm().text_color(color).child(message.clone()));
        }
        bar
    }
}

impl Render for Muxbox {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let main = match self.active.as_ref().and_then(|k| self.terminals.get(k)) {
            Some(view) => div().size_full().child(view.clone()),
            None => div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_color(cx.theme().muted_foreground)
                .child(if self.loaded {
                    "Pick a session, or start one with +"
                } else {
                    "Loading sessions…"
                }),
        };
        h_flex()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .on_action(cx.listener(|this, _: &ToggleSidebar, _, cx| {
                this.collapsed = !this.collapsed;
                cx.notify();
            }))
            .on_key_down(cx.listener(|this, e: &KeyDownEvent, _, cx| {
                if e.keystroke.key == "escape" && this.prompt.take().is_some() {
                    cx.stop_propagation();
                    cx.notify();
                }
            }))
            .child(self.sidebar(cx))
            .child(
                v_flex()
                    .flex_1()
                    .h_full()
                    .min_w_0()
                    .child(self.header(cx))
                    .child(div().flex_1().min_h_0().child(main)),
            )
    }
}

fn sessions(snap: &Snapshot) -> impl Iterator<Item = (&str, &Session)> {
    snap.hosts.iter().flat_map(|h| {
        let in_bases = h
            .bases
            .iter()
            .flat_map(|b| b.worktrees.iter())
            .flat_map(|w| w.sessions.iter());
        in_bases
            .chain(h.other.iter())
            .map(move |s| (h.name.as_str(), s))
    })
}

/// A session's status dot; it breathes while Claude works.
fn status_dot(key: &Key, color: Hsla, working: bool, tip: SharedString) -> AnyElement {
    let id = SharedString::from(format!("status-{}-{}", key.0, key.1));
    let dot = div()
        .id(id.clone())
        .size_2()
        .rounded_full()
        .bg(color)
        .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx));
    if working {
        dot.with_animation(
            id,
            Animation::new(Duration::from_millis(1600))
                .repeat()
                .with_easing(pulsating_between(0.35, 1.0)),
            |dot, delta| dot.opacity(delta),
        )
        .into_any_element()
    } else {
        dot.into_any_element()
    }
}

fn age(secs: u64) -> String {
    match secs {
        0..60 => "now".into(),
        60..3600 => format!("{}m", secs / 60),
        3600..86400 => format!("{}h", secs / 3600),
        _ => format!("{}d", secs / 86400),
    }
}

#[cfg(test)]
mod tests {
    // Not `super::*`: GPUI's `test` macro would shadow the built-in one.
    use super::{Muxbox, TerminalView};
    use crate::config::Config;
    use gpui_kit::{AppContext, px, size};
    use std::process::Command;

    fn tmux(socket: &str, args: &[&str]) -> String {
        let out = Command::new("tmux")
            .arg("-L")
            .arg(socket)
            .args(args)
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// The whole window: sidebar, header and the active terminal. Resizing
    /// the window must resize the terminal inside it.
    #[gpui::test]
    fn the_active_terminal_follows_the_window_size(cx: &mut gpui::TestAppContext) {
        let socket = format!("muxbox-unit-app-{}", std::process::id());
        tmux(&socket, &["new-session", "-d", "-s", "t", "sleep 600"]);
        let cfg: Config = toml::from_str(&format!(
            "[[hosts]]\nname = \"local\"\nssh = \"local\"\ntmux_socket = \"{socket}\"\n"
        ))
        .unwrap();
        let host = cfg.hosts[0].clone();
        // The PTY reader is a real thread, so this test can't be deterministic.
        cx.executor().allow_parking();
        cx.update(gpui_kit::init);
        // Wrapped in Root exactly as gpui_kit::open_window does.
        let mut app = None;
        let (_, cx) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| Muxbox::new(cfg, window, cx));
            app = Some(view.clone());
            gpui_kit::base::Root::new(view, window, cx)
        });
        let app = app.unwrap();
        // Show the session without `open`, which would remember it in ~/.muxbox.
        let view = app.update(cx, |app, cx| {
            let view = cx.new(|cx| TerminalView::new(host, "t".into(), cx));
            let key = ("local".to_string(), "t".to_string());
            app.terminals.insert(key.clone(), view.clone());
            app.active = Some(key);
            cx.notify();
            view
        });

        cx.simulate_resize(size(px(900.0), px(600.0)));
        cx.run_until_parked();
        let small = view.read_with(cx, |v, _| v.pty_size()).expect("attached");
        cx.simulate_resize(size(px(1400.0), px(900.0)));
        cx.run_until_parked();
        let large = view.read_with(cx, |v, _| v.pty_size()).expect("attached");
        // kill-server leaves the socket file behind.
        let path = tmux(&socket, &["display-message", "-p", "#{socket_path}"]);
        tmux(&socket, &["kill-server"]);
        let _ = std::fs::remove_file(path);
        assert!(
            large.cols > small.cols && large.lines > small.lines,
            "{small:?} -> {large:?}"
        );
    }
}

#[cfg(test)]
mod status_tests {
    // Not `super::*`: GPUI's `test` macro would shadow the built-in one.
    use super::{Key, Muxbox};
    use crate::config::Config;
    use crate::snapshot::{Activity, HostView, Session, Snapshot};

    fn snapshot(sessions: &[(&str, Activity)]) -> Snapshot {
        let other = sessions
            .iter()
            .map(|(name, activity)| Session {
                name: name.to_string(),
                path: "/tmp".into(),
                attached: false,
                idle_secs: 0,
                state: None,
                activity: *activity,
                waiting_for: None,
            })
            .collect();
        Snapshot {
            hosts: vec![HostView {
                name: "local".into(),
                error: None,
                bases: vec![],
                other,
            }],
        }
    }

    #[gpui::test]
    fn a_session_that_finishes_elsewhere_is_marked_done(cx: &mut gpui::TestAppContext) {
        // No hosts, so the background refresh has nothing to fetch.
        let cfg: Config = toml::from_str("").unwrap();
        cx.update(gpui_kit::init);
        let (app, cx) = cx.add_window_view(|window, cx| Muxbox::new(cfg, window, cx));
        let key = |s: &str| -> Key { ("local".into(), s.into()) };
        app.update(cx, |app, cx| {
            app.active = Some(key("shown"));
            app.set_snapshot(
                snapshot(&[
                    ("shown", Activity::Working),
                    ("elsewhere", Activity::Working),
                    ("asking", Activity::Working),
                ]),
                cx,
            );
            assert!(app.done.is_empty());
            app.set_snapshot(
                snapshot(&[
                    ("shown", Activity::Idle),
                    ("elsewhere", Activity::Idle),
                    ("asking", Activity::NeedsInput),
                ]),
                cx,
            );
            assert!(app.working.is_empty(), "{:?}", app.working);
            assert!(app.done.contains(&key("elsewhere")), "{:?}", app.done);
            assert!(
                !app.done.contains(&key("shown")),
                "the shown session isn't news"
            );
            assert!(
                !app.done.contains(&key("asking")),
                "waiting on you isn't done"
            );
            // Working again clears it.
            app.set_snapshot(snapshot(&[("elsewhere", Activity::Working)]), cx);
            assert!(app.done.is_empty(), "{:?}", app.done);
        });
    }
}
