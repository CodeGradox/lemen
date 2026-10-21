//! The window: a sidebar of sessions next to a live terminal.
//!
//! Every session you open keeps its own terminal (`tmux attach` over SSH),
//! so switching is instant and scrollback survives.

mod keys;
mod terminal;
mod terminal_view;

use crate::actions;
use crate::attach::Active;
use crate::config::Config;
use crate::snapshot::{self, Session, Snapshot};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::sidebar::{
    Sidebar, SidebarCollapsible, SidebarGroup, SidebarHeader, SidebarMenu, SidebarMenuItem,
};
use gpui_kit::component::{
    ActiveTheme, IconName, Sizable, StyledExt, Theme, ThemeMode, h_flex, v_flex,
};
use gpui_kit::*;
use std::collections::HashMap;
use std::time::Duration;
use terminal_view::TerminalView;

actions!(muxbox, [Quit, ToggleSidebar]);

const REFRESH: Duration = Duration::from_secs(3);

type Key = (String, String);

enum NewThread {
    Worktree { base: usize },
    InWorktree { base: usize, worktree: String },
}

struct Prompt {
    what: NewThread,
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
    status: Option<(String, bool)>,
    _refresh: Task<()>,
}

pub fn run(cfg: Config) -> anyhow::Result<()> {
    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(move |cx| {
            gpui_kit::init(cx);
            Theme::change(ThemeMode::Dark, None, cx);
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
        Muxbox {
            cfg,
            snap: Snapshot::default(),
            loaded: false,
            terminals: HashMap::new(),
            active: None,
            collapsed: false,
            prompt: None,
            confirm_kill: None,
            status: None,
            _refresh: refresh,
        }
    }

    fn set_snapshot(&mut self, snap: Snapshot, cx: &mut Context<Self>) {
        // Forget terminals whose session is gone (killed, or ended elsewhere).
        let live: Vec<Key> = sessions(&snap)
            .map(|(h, s)| (h.to_string(), s.name.clone()))
            .collect();
        self.terminals.retain(|k, _| live.contains(k));
        if self.active.as_ref().is_some_and(|a| !live.contains(a)) {
            self.active = None;
        }
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
        self.active = Some(key);
        self.confirm_kill = None;
        let _ = Active { host, session }.save();
        view.read(cx).focus_handle(cx).focus(window, cx);
        cx.notify();
    }

    fn ask_name(&mut self, what: NewThread, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("name, e.g. auth-ui"));
        let events = cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| {
            if let InputEvent::PressEnter { .. } = event {
                this.start_thread(window, cx);
            }
        });
        input.update(cx, |i, cx| i.focus(window, cx));
        self.prompt = Some(Prompt {
            what,
            input,
            _events: events,
        });
        cx.notify();
    }

    fn start_thread(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(prompt) = &self.prompt else { return };
        let name = prompt.input.read(cx).value().trim().to_string();
        if let Err(e) = actions::validate_name(&name) {
            self.status = Some((format!("{e:#}"), true));
            cx.notify();
            return;
        }
        let Some(prompt) = self.prompt.take() else {
            return;
        };
        let cfg = self.cfg.clone();
        self.status = Some((format!("starting {name}…"), false));
        cx.notify();
        let window_handle = window.window_handle();
        cx.spawn(async move |this, cx| {
            let started = cx
                .background_spawn(async move {
                    match prompt.what {
                        NewThread::Worktree { base } => {
                            actions::new_worktree_thread(&cfg, base, &name)
                        }
                        NewThread::InWorktree { base, worktree } => {
                            actions::new_thread_in(&cfg, base, &worktree, &name)
                        }
                    }
                })
                .await;
            window_handle
                .update(cx, |_, window, cx| {
                    this.update(cx, |this, cx| {
                        match started {
                            Ok((host, session)) => {
                                this.status = None;
                                this.open(host, session, window, cx);
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
                                    this.ask_name(NewThread::Worktree { base: index }, window, cx)
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
                                    let what = NewThread::InWorktree {
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
        let dot = match s.state.as_deref() {
            Some("working") => cx.theme().success,
            Some("needs_input") => cx.theme().warning,
            _ => cx.theme().muted_foreground.opacity(0.5),
        };
        let detail = age(s.idle_secs);
        let muted = cx.theme().muted_foreground;
        let open = {
            let this = this.clone();
            let key = key.clone();
            move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                let (host, session) = key.clone();
                this.update(cx, |this, cx| this.open(host, session, window, cx))
                    .ok();
            }
        };
        let suffix = move |_: &mut Window, _: &mut App| {
            let this = this.clone();
            let key = key.clone();
            let kill = Button::new("kill")
                .ghost()
                .xsmall()
                .on_click(move |_, _, cx| {
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
                .child(div().size_2().rounded_full().bg(dot))
                .child(kill)
        };
        SidebarMenuItem::new(label)
            .icon(IconName::SquareTerminal)
            .active(active)
            .on_click(open)
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
                NewThread::Worktree { base } => {
                    format!("New worktree in {}", self.cfg.bases[*base].name())
                }
                NewThread::InWorktree { worktree, .. } => {
                    format!(
                        "New thread in {}",
                        worktree.rsplit('/').next().unwrap_or(worktree)
                    )
                }
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

fn age(secs: u64) -> String {
    match secs {
        0..60 => "now".into(),
        60..3600 => format!("{}m", secs / 60),
        3600..86400 => format!("{}h", secs / 3600),
        _ => format!("{}d", secs / 86400),
    }
}
