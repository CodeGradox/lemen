//! The window: a sidebar of sessions next to a live terminal.
//!
//! Every session you open keeps its own terminal (`tmux attach` over SSH),
//! so switching is instant and scrollback survives.

mod keys;
mod notification;
mod terminal;
mod terminal_view;

use crate::actions;
use crate::attach::Active;
use crate::config::{Config, ThemeChoice, leaf};
use crate::snapshot::{self, Activity, Session, Snapshot, Worktree};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu, PopupMenu, PopupMenuItem};
use gpui_kit::component::sidebar::{
    Sidebar, SidebarCollapsible, SidebarGroup, SidebarHeader, SidebarMenu, SidebarMenuItem,
};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{
    ActiveTheme, Icon, IconName, Sizable, StyledExt, Theme, ThemeMode, h_flex, v_flex,
};
use gpui_kit::*;
use notification::{Notification, NotificationEvent};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::Duration;
use terminal_view::TerminalView;

actions!(muxbox, [Quit, ToggleSidebar]);

const REFRESH: Duration = Duration::from_secs(3);

/// The sidebar icon for Claude sessions, drawn in Claude's orange.
const CLAUDE_ICON: &[u8] = include_bytes!("claude.svg");
const CLAUDE_ORANGE: u32 = 0xd97757;

type Key = (String, String);

/// What the name field in the header is asking for.
enum Ask {
    NewWorktree {
        base: usize,
    },
    /// Claude, or with `shell` just a shell, in an existing worktree.
    NewThread {
        base: usize,
        worktree: String,
        shell: bool,
    },
    Rename {
        host: String,
        session: String,
    },
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
    /// Where to remember the session you last opened; tests have none.
    state_file: Option<PathBuf>,
    snap: Snapshot,
    loaded: bool,
    terminals: HashMap<Key, Entity<TerminalView>>,
    active: Option<Key>,
    collapsed: bool,
    prompt: Option<Prompt>,
    confirm_kill: Option<Key>,
    /// A session being renamed keeps its terminal while its old name disappears.
    renaming: Option<Key>,
    /// Sessions that finished while you were looking at another one.
    done: HashSet<Key>,
    status: Option<(String, bool)>,
    /// The session from last time, opened once the first refresh is in.
    restore: Option<Key>,
    /// The main window, for background work that ends in UI changes.
    window: AnyWindowHandle,
    /// Notification popups that are showing, one per session.
    popups: HashMap<Key, (WindowHandle<Notification>, Subscription)>,
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
            let (main, _) = gpui_kit::open_window(options, cx, |window, cx| {
                cx.new(|cx| Muxbox::new(cfg, Active::path().ok(), window, cx))
            })
            .expect("failed to open window");
            // Closing the main window quits; notification popups come and go.
            let main = main.window_id();
            cx.on_window_closed(move |cx, closed| {
                if closed == main {
                    cx.quit();
                }
            })
            .detach();
            cx.activate(true);
        });
    Ok(())
}

impl Muxbox {
    fn new(
        cfg: Config,
        state_file: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let restore = state_file
            .as_deref()
            .and_then(|p| Active::load_from(p).ok())
            .map(|a| (a.host, a.session));
        let refresh = cx.spawn(async move |this, cx| {
            while let Ok(refreshed) = this.update(cx, |this, cx| this.refresh(cx)) {
                refreshed.await;
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
            state_file,
            snap: Snapshot::default(),
            loaded: false,
            terminals: HashMap::new(),
            active: None,
            collapsed: false,
            prompt: None,
            confirm_kill: None,
            renaming: None,
            done: HashSet::new(),
            status: None,
            restore,
            window: window.window_handle(),
            popups: HashMap::new(),
            _refresh: refresh,
            _appearance: appearance,
        }
    }

    /// Fetches a snapshot in the background and applies it.
    fn refresh(&self, cx: &mut Context<Self>) -> Task<()> {
        let cfg = self.cfg.clone();
        let window = self.window;
        cx.spawn(async move |this, cx| {
            let snap = cx
                .background_spawn(async move { snapshot::fetch(&cfg) })
                .await;
            window
                .update(cx, |_, window, cx| {
                    this.update(cx, |this, cx| this.set_snapshot(snap, window, cx))
                        .ok();
                })
                .ok();
        })
    }

    fn set_snapshot(&mut self, snap: Snapshot, window: &mut Window, cx: &mut Context<Self>) {
        let before: HashMap<Key, Activity> = sessions(&self.snap)
            .map(|(h, s)| (key(h, s), s.activity))
            .collect();
        let now: HashMap<Key, &Session> = sessions(&snap).map(|(h, s)| (key(h, s), s)).collect();
        // Forget terminals whose session is gone (killed, or ended elsewhere).
        let renaming = self.renaming.clone();
        let live = |k: &Key| now.contains_key(k) || renaming.as_ref() == Some(k);
        self.terminals.retain(|k, _| live(k));
        if self.active.as_ref().is_some_and(|a| !live(a)) {
            self.active = None;
        }
        // Working -> idle is "finished"; anything -> waiting is "needs input".
        let mut news = vec![];
        for (k, s) in &now {
            let was = before.get(k).copied().unwrap_or(Activity::Unknown);
            let finished = was == Activity::Working
                && matches!(s.activity, Activity::Idle | Activity::Unknown);
            if finished && self.active.as_ref() != Some(k) {
                self.done.insert(k.clone());
            }
            if finished || (s.activity == Activity::NeedsInput && was != Activity::NeedsInput) {
                news.push((k.clone(), s.activity, s.waiting_for.clone()));
            }
        }
        self.done
            .retain(|k| now.get(k).is_some_and(|s| s.activity != Activity::Working));
        let gone: Vec<Key> = self
            .popups
            .keys()
            .filter(|k| !now.contains_key(*k))
            .cloned()
            .collect();
        let restore = self
            .restore
            .take()
            .filter(|k| now.contains_key(k) && self.active.is_none());
        drop(now);
        for k in gone {
            self.close_popup(&k, cx);
        }
        self.snap = snap;
        self.loaded = true;
        self.notify_news(news, window, cx);
        if let Some((host, session)) = restore {
            self.open(host, session, window, cx);
        }
        cx.notify();
    }

    /// Pops up a notification for each session that finished or needs input,
    /// except the one you're looking at in the active window.
    fn notify_news(
        &mut self,
        news: Vec<(Key, Activity, Option<String>)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Decided once: opening a popup mustn't change the answer.
        let looking = window.is_window_active();
        for (key, activity, waiting_for) in news {
            if looking && self.active.as_ref() == Some(&key) {
                continue;
            }
            let Some(screen) = cx.primary_display() else {
                return;
            };
            let host = &key.0;
            let session = actions::label(&key.1);
            let (title, caption, icon) = match activity {
                Activity::NeedsInput => (
                    format!("{session} needs input"),
                    waiting_for.unwrap_or_else(|| host.clone()),
                    IconName::CircleAlert,
                ),
                _ => (
                    format!("{session} finished"),
                    host.clone(),
                    IconName::CircleCheck,
                ),
            };
            // A newer event for the same session replaces its popup.
            self.close_popup(&key, cx);
            let slot = self.popups.len();
            let Ok(popup) = cx.open_window(Notification::window_options(screen, slot), |_, cx| {
                cx.new(|_| Notification::new(title, caption, icon))
            }) else {
                continue;
            };
            let Ok(entity) = popup.entity(cx) else {
                continue;
            };
            let target = key.clone();
            let events = cx.subscribe_in(
                &entity,
                window,
                move |this, _, event: &NotificationEvent, window, cx| {
                    if let NotificationEvent::Show = event {
                        cx.activate(true);
                        window.activate_window();
                        this.open(target.0.clone(), target.1.clone(), window, cx);
                    }
                    this.close_popup(&target, cx);
                },
            );
            self.popups.insert(key, (popup, events));
            window.request_attention();
        }
    }

    fn close_popup(&mut self, key: &Key, cx: &mut App) {
        if let Some((popup, _events)) = self.popups.remove(key) {
            popup.update(cx, |_, window, _| window.remove_window()).ok();
        }
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
        self.close_popup(&key, cx);
        self.active = Some(key);
        self.confirm_kill = None;
        self.remember(host, session);
        view.read(cx).focus_handle(cx).focus(window, cx);
        cx.notify();
    }

    fn remember(&self, host: String, session: String) {
        if let Some(path) = &self.state_file {
            let _ = Active { host, session }.save_to(path);
        }
    }

    /// Opens the name field, selected so typing replaces it. It asks for a
    /// label, which several threads can share: it starts as `claude` or
    /// `shell`, a free random name for a worktree, or the current label.
    fn ask_name(&mut self, what: Ask, window: &mut Window, cx: &mut Context<Self>) {
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
        let events = cx.subscribe_in(&input, window, |this, _, event: &InputEvent, _, cx| {
            if let InputEvent::PressEnter { .. } = event {
                this.submit(cx);
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

    fn submit(&mut self, cx: &mut Context<Self>) {
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
        let window_handle = self.window;
        cx.spawn(async move |this, cx| {
            let done = cx
                .background_spawn(async move {
                    match what {
                        Ask::NewWorktree { base } => {
                            actions::new_worktree_thread(&cfg, base, &name, &thread)
                                .map(Done::Started)
                        }
                        Ask::NewThread {
                            base,
                            worktree,
                            shell,
                        } => actions::new_thread_in(&cfg, base, &worktree, &thread, shell)
                            .map(Done::Started),
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
                        this.refresh(cx).detach();
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
            self.remember(host, new);
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
                this.refresh(cx).detach();
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
                let index = b.index;
                let mut menu = SidebarMenu::new().child(
                    SidebarMenuItem::new("New worktree")
                        .icon(IconName::Plus)
                        .on_click(ask_on_click(&this, move || Ask::NewWorktree {
                            base: index,
                        })),
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
                    menu = menu.child(self.worktree_item(&h.name, index, wi, w, &prefix, cx));
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
                    this.update(cx, |this, cx| this.refresh(cx).detach()).ok();
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

    /// A worktree with its sessions; + offers a Claude thread or a shell.
    fn worktree_item(
        &self,
        host: &str,
        base: usize,
        position: usize,
        w: &Worktree,
        prefix: &str,
        cx: &mut Context<Self>,
    ) -> SidebarMenuItem {
        // The main checkout reads best as its branch; linked ones by directory.
        let label = match (&w.branch, position) {
            (Some(branch), 0) => branch.clone(),
            _ => leaf(&w.path).to_string(),
        };
        let this = cx.entity().downgrade();
        let worktree = w.path.clone();
        let add = move |_: &mut Window, _: &mut App| {
            let (this, worktree) = (this.clone(), worktree.clone());
            Button::new("add")
                .icon(IconName::Plus)
                .ghost()
                .xsmall()
                .tooltip("New session here")
                .dropdown_menu(move |menu, _, _| {
                    let item = |label: &'static str, shell: bool| {
                        let worktree = worktree.clone();
                        PopupMenuItem::new(label).on_click(ask_on_click(&this, move || {
                            Ask::NewThread {
                                base,
                                worktree: worktree.clone(),
                                shell,
                            }
                        }))
                    };
                    menu.item(item("Claude", false)).item(item("Shell", true))
                })
        };
        let threads = w
            .sessions
            .iter()
            .map(|s| self.session_item(host, s, prefix, cx));
        SidebarMenuItem::new(label)
            .icon(IconName::Folder)
            .default_open(true)
            .suffix(add)
            .children(threads)
    }

    fn session_item(
        &self,
        host: &str,
        s: &Session,
        prefix: &str,
        cx: &mut Context<Self>,
    ) -> SidebarMenuItem {
        let this = cx.entity().downgrade();
        let key = key(host, s);
        let active = self.active.as_ref() == Some(&key);
        let arming = self.confirm_kill.as_ref() == Some(&key);
        let label = actions::label(s.name.strip_prefix(prefix).unwrap_or(&s.name)).to_string();
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
                let key = key.clone();
                menu.item(
                    PopupMenuItem::new("Rename…").on_click(ask_on_click(&this, move || {
                        let (host, session) = key.clone();
                        Ask::Rename { host, session }
                    })),
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
        let icon = if s.runs_claude() {
            Icon::empty()
                .data(CLAUDE_ICON)
                .text_color(rgb(CLAUDE_ORANGE))
        } else {
            Icon::new(IconName::SquareTerminal)
        };
        SidebarMenuItem::new(label)
            .icon(icon)
            .active(active)
            .on_click(open)
            .context_menu(menu)
            .suffix(suffix)
    }

    fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        self.collapsed = !self.collapsed;
        cx.notify();
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
                this.update(cx, |this, cx| this.toggle_sidebar(cx)).ok();
            });
        let title = match &self.active {
            Some((host, session)) => format!("{}  ·  {host}", actions::label(session)),
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
                Ask::NewThread {
                    worktree, shell, ..
                } => {
                    let what = if *shell { "shell" } else { "Claude thread" };
                    format!("New {what} in {}", leaf(worktree))
                }
                Ask::Rename { session, .. } => format!("Rename {}", actions::label(session)),
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
            // Cached: sidebar redraws (the breathing dot) don't re-render the
            // terminal; it re-renders on its own output, size or focus change.
            Some(view) => div()
                .size_full()
                .child(view.clone().cached(StyleRefinement::default().size_full())),
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
            .on_action(cx.listener(|this, _: &ToggleSidebar, _, cx| this.toggle_sidebar(cx)))
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

fn key(host: &str, s: &Session) -> Key {
    (host.to_string(), s.name.clone())
}

/// A click handler that opens the name field for `ask()`.
fn ask_on_click(
    this: &WeakEntity<Muxbox>,
    ask: impl Fn() -> Ask + 'static,
) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
    let this = this.clone();
    move |_, window, cx| {
        this.update(cx, |this, cx| this.ask_name(ask(), window, cx))
            .ok();
    }
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
            // Every animation frame redraws the window; 15 fps is smooth enough.
            Animation::new(Duration::from_millis(1600))
                .repeat()
                .with_max_fps(15.0)
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
    use super::{Key, Muxbox, NotificationEvent};
    use crate::config::Config;
    use crate::snapshot::{Activity, HostView, Session, Snapshot};
    use crate::test_support::{TmuxServer, wait_until};
    use gpui::VisualTestContext;
    use gpui_kit::{AppContext, Entity, px, size};
    use std::time::Duration;

    fn key(s: &str) -> Key {
        ("local".into(), s.into())
    }

    fn snapshot(sessions: &[(&str, Activity)]) -> Snapshot {
        let other = sessions
            .iter()
            .map(|(name, activity)| Session {
                name: name.to_string(),
                path: "/tmp".into(),
                attached: false,
                idle_secs: 0,
                activity: *activity,
                waiting_for: None,
                started_claude: false,
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

    /// Applies a snapshot the way a refresh does.
    fn apply(app: &Entity<Muxbox>, sessions: &[(&str, Activity)], cx: &mut VisualTestContext) {
        let snap = snapshot(sessions);
        cx.update(|window, cx| app.update(cx, |app, cx| app.set_snapshot(snap, window, cx)));
    }

    fn open(app: &Entity<Muxbox>, session: &str, cx: &mut VisualTestContext) {
        cx.update(|window, cx| {
            app.update(cx, |app, cx| {
                app.open("local".into(), session.into(), window, cx)
            })
        });
    }

    /// The whole window: sidebar, header and the active terminal. Resizing
    /// the window must resize the terminal inside it.
    #[gpui::test]
    fn the_active_terminal_follows_the_window_size(cx: &mut gpui::TestAppContext) {
        let server = TmuxServer::new("app");
        server.tmux(&["new-session", "-d", "-s", "t", "sleep 600"]);
        // The PTY reader is a real thread, so this test can't be deterministic.
        cx.executor().allow_parking();
        cx.update(gpui_kit::init);
        // Wrapped in Root exactly as gpui_kit::open_window does.
        let mut app = None;
        let (_, cx) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| Muxbox::new(server.config(), None, window, cx));
            app = Some(view.clone());
            gpui_kit::base::Root::new(view, window, cx)
        });
        let app = app.unwrap();
        open(&app, "t", cx);
        let view = app.read_with(cx, |app, _| app.terminals[&key("t")].clone());

        cx.simulate_resize(size(px(900.0), px(600.0)));
        cx.run_until_parked();
        let small = view.read_with(cx, |v, _| v.pty_size()).expect("attached");
        cx.simulate_resize(size(px(1400.0), px(900.0)));
        cx.run_until_parked();
        let large = view.read_with(cx, |v, _| v.pty_size()).expect("attached");
        assert!(
            large.cols > small.cols && large.lines > small.lines,
            "{small:?} -> {large:?}"
        );
    }

    #[gpui::test]
    fn a_session_that_finishes_elsewhere_is_marked_done(cx: &mut gpui::TestAppContext) {
        // No hosts, so the background refresh has nothing to fetch.
        let cfg: Config = toml::from_str("").unwrap();
        cx.update(gpui_kit::init);
        let (app, cx) = cx.add_window_view(|window, cx| Muxbox::new(cfg, None, window, cx));
        app.update(cx, |app, _| app.active = Some(key("shown")));
        let working = [
            ("shown", Activity::Working),
            ("elsewhere", Activity::Working),
            ("asking", Activity::Working),
        ];
        apply(&app, &working, cx);
        app.read_with(cx, |app, _| assert!(app.done.is_empty()));
        apply(
            &app,
            &[
                ("shown", Activity::Idle),
                ("elsewhere", Activity::Idle),
                ("asking", Activity::NeedsInput),
            ],
            cx,
        );
        app.read_with(cx, |app, _| {
            assert!(app.done.contains(&key("elsewhere")), "{:?}", app.done);
            assert!(
                !app.done.contains(&key("shown")),
                "the shown session isn't news"
            );
            assert!(
                !app.done.contains(&key("asking")),
                "waiting on you isn't done"
            );
        });
        // Working again clears it.
        apply(&app, &[("elsewhere", Activity::Working)], cx);
        app.read_with(cx, |app, _| assert!(app.done.is_empty(), "{:?}", app.done));
    }

    #[gpui::test]
    fn finishing_out_of_sight_pops_up_a_notification(cx: &mut gpui::TestAppContext) {
        // No tmux runs here: Show still opens a short-lived terminal that
        // tries to attach, a real process.
        let server = TmuxServer::new("popup");
        cx.executor().allow_parking();
        cx.update(gpui_kit::init);
        let (app, cx) =
            cx.add_window_view(|window, cx| Muxbox::new(server.config(), None, window, cx));
        let windows = |cx: &mut VisualTestContext| cx.update(|_, cx| cx.windows().len());

        // In the active window: the session you're looking at doesn't pop up,
        // one you aren't does. The test platform activates windows
        // asynchronously.
        cx.update(|window, _| window.activate_window());
        cx.run_until_parked();
        app.update(cx, |app, _| app.active = Some(key("shown")));
        apply(
            &app,
            &[("shown", Activity::Working), ("a", Activity::Working)],
            cx,
        );
        apply(
            &app,
            &[("shown", Activity::Idle), ("a", Activity::Idle)],
            cx,
        );
        app.read_with(cx, |app, _| {
            assert!(
                !app.popups.contains_key(&key("shown")),
                "you're looking at it"
            );
            assert!(app.popups.contains_key(&key("a")));
        });
        assert_eq!(windows(cx), 2);

        // Away from the window, even the shown session pops up; needing input
        // does too.
        cx.deactivate_window();
        apply(
            &app,
            &[
                ("shown", Activity::Working),
                ("a", Activity::Idle),
                ("b", Activity::Working),
            ],
            cx,
        );
        apply(
            &app,
            &[
                ("shown", Activity::Idle),
                ("a", Activity::Idle),
                ("b", Activity::NeedsInput),
            ],
            cx,
        );
        assert_eq!(windows(cx), 4);
        let popup = app.read_with(cx, |app, cx| app.popups[&key("a")].0.entity(cx).unwrap());

        // Show opens that session and closes only its popup.
        popup.update(cx, |_, cx| cx.emit(NotificationEvent::Show));
        cx.run_until_parked();
        app.read_with(cx, |app, _| {
            assert_eq!(app.active, Some(key("a")));
            assert!(!app.popups.contains_key(&key("a")));
            assert!(app.popups.contains_key(&key("b")) && app.popups.contains_key(&key("shown")));
        });
        assert_eq!(windows(cx), 3);

        // Opening a session from the sidebar answers its popup too.
        open(&app, "b", cx);
        app.read_with(cx, |app, _| assert!(!app.popups.contains_key(&key("b"))));
        assert_eq!(windows(cx), 2);

        // Let the terminals' attach attempts end before the server cleans up.
        wait_until(Duration::from_secs(5), || {
            cx.run_until_parked();
            app.read_with(cx, |app, cx| {
                app.terminals.values().all(|t| t.read(cx).exited())
            })
        });
    }
}
