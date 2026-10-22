//! The window: a sidebar of sessions next to a live terminal.
//!
//! Every session you open keeps its own terminal (`tmux attach` over SSH),
//! so switching is instant and scrollback survives.

mod folder_picker;
mod keys;
mod notes;
mod notification;
mod projects;
mod prompt;
mod rows;
mod settings;
mod sidebar;
mod spotlight;
mod terminal;
mod terminal_view;

use crate::actions;
use crate::attach::Active;
use crate::config::{Config, ThemeChoice, leaf};
use crate::snapshot::{self, Activity, Session, Snapshot, Worktree};
use folder_picker::FolderPicker;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::InputState;
use gpui_kit::component::resizable::{HandleEdge, resize_handle};
use gpui_kit::component::{ActiveTheme, IconName, Sizable, Theme, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use notes::NotesPanel;
use notification::{Notification, NotificationEvent};
use settings::Prefs;
use sidebar::{SIDEBAR_WIDTH, SidebarView};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;
use terminal_view::TerminalView;

actions!(
    lemur,
    [
        Quit,
        ToggleSidebar,
        ToggleNotes,
        OpenSettings,
        AddProject,
        Refresh,
        Hide,
        HideOthers,
        ShowAll
    ]
);

/// lemur's keys; menus show them next to their items.
fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-,", OpenSettings, None),
        KeyBinding::new("cmd-h", Hide, None),
        KeyBinding::new("alt-cmd-h", HideOthers, None),
        KeyBinding::new("cmd-o", AddProject, None),
        KeyBinding::new("cmd-r", Refresh, None),
        KeyBinding::new("cmd-b", ToggleSidebar, None),
        KeyBinding::new("cmd-w", settings::CloseWindow, Some(settings::CONTEXT)),
        // gpui-component's Root moves focus on Tab; in a terminal it belongs to the program.
        KeyBinding::new("tab", NoAction, Some(terminal_view::CONTEXT)),
        KeyBinding::new("shift-tab", NoAction, Some(terminal_view::CONTEXT)),
    ]);
    folder_picker::bind_keys(cx);
}

const REFRESH: Duration = Duration::from_secs(3);
/// Out of sight, the sidebar refreshes every this many REFRESH ticks.
const AWAY: u32 = 5;

/// The notes panel's width until its edge is dragged.
const NOTES_WIDTH: Pixels = px(360.0);
/// The narrowest a dragged side panel gets, and the room it leaves the
/// terminal.
const MIN_SIDEBAR: Pixels = px(180.0);
const MIN_NOTES: Pixels = px(240.0);
const MIN_TERMINAL: Pixels = px(320.0);

/// A side panel, resized by dragging its inner edge.
#[derive(Clone, Copy)]
enum Panel {
    Sidebar,
    Notes,
}

/// The drag of a side panel's edge; the window follows it and sets the
/// width. It draws nothing at the pointer.
#[derive(Clone)]
struct ResizePanel(Panel);

impl Render for ResizePanel {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

type Key = (String, String);
/// A list of threads: a host's worktree, by path, or with `None` the host's
/// sessions outside every base.
type List = (String, Option<String>);

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

/// A name being asked for, in the spotlight.
struct Prompt {
    what: Ask,
    input: Entity<InputState>,
    /// For a new worktree, its branch and what it starts from.
    worktree: Option<prompt::WorktreeFields>,
    /// Why the name typed can't be used, and that name.
    error: Option<(String, String)>,
    _events: Subscription,
}

struct Lemur {
    cfg: Config,
    /// Where to remember the session you last opened; tests have none.
    state_file: Option<PathBuf>,
    snap: Snapshot,
    loaded: bool,
    terminals: HashMap<Key, Entity<TerminalView>>,
    active: Option<Key>,
    collapsed: bool,
    /// The side panels' widths; the config keeps them once a drag ends.
    sidebar_width: Pixels,
    notes_width: Pixels,
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
    /// Counts changes a snapshot fetched across one would undo (the thread
    /// order, a removed project); such a snapshot is dropped.
    changes: u64,
    /// The notes of the last worktree opened, beside the terminal.
    notes: Entity<NotesPanel>,
    notes_open: bool,
    /// Adding a folder, over the window.
    picker: Option<(Entity<FolderPicker>, Subscription)>,
    /// The sidebar, drawn by `sidebar()` but as a view of its own, so a
    /// terminal's output doesn't redraw it.
    sidebar: Entity<SidebarView>,
    _refresh: Task<()>,
    /// Focused when no terminal is, so keys and menu actions reach the
    /// window.
    focus: FocusHandle,
    _appearance: Subscription,
    _sidebar_follows: Subscription,
}

pub fn run(cfg: Config) -> anyhow::Result<()> {
    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(move |cx| {
            gpui_kit::init(cx);
            cx.set_global(Prefs::new(&cfg, Config::path().ok()));
            settings::apply_theme(None, cx);
            bind_keys(cx);
            cx.on_action(|_: &Quit, cx| cx.quit());
            cx.on_action(|_: &OpenSettings, cx| settings::open(cx));
            cx.on_action(|_: &Hide, cx| cx.hide());
            cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
            cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
            // Menus as macOS apps have them; shortcuts come from bind_keys. The
            // File and View items act on the main window, and are greyed out
            // while another window, like Settings, is in front.
            cx.set_menus([
                Menu::new("lemur").items([
                    MenuItem::action("Settings…", OpenSettings),
                    MenuItem::separator(),
                    MenuItem::os_submenu("Services", SystemMenuType::Services),
                    MenuItem::separator(),
                    MenuItem::action("Hide lemur", Hide),
                    MenuItem::action("Hide Others", HideOthers),
                    MenuItem::action("Show All", ShowAll),
                    MenuItem::separator(),
                    MenuItem::action("Quit lemur", Quit),
                ]),
                Menu::new("File").items([
                    MenuItem::action("Add Project…", AddProject),
                    MenuItem::separator(),
                    MenuItem::action("Refresh", Refresh),
                ]),
                Menu::new("View").items([
                    MenuItem::action("Toggle Sidebar", ToggleSidebar),
                    MenuItem::action("Toggle Notes", ToggleNotes),
                ]),
            ]);
            let options = WindowOptions {
                titlebar: Some(TitlebarOptions {
                    title: Some("lemur".into()),
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
                cx.new(|cx| Lemur::new(cfg, Active::path().ok(), window, cx))
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

impl Lemur {
    fn new(
        cfg: Config,
        state_file: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        // Notes live beside the state file, in ~/.lemur/notes; tests have neither.
        let notes_dir = state_file
            .as_deref()
            .and_then(Path::parent)
            .map(|dir| dir.join("notes"));
        let notes = cx.new(|cx| NotesPanel::new(notes_dir, window, cx));
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        let lemur = cx.weak_entity();
        let sidebar = cx.new(|_| SidebarView::new(lemur));
        let sidebar_follows =
            cx.observe_self(|this, cx| this.sidebar.update(cx, |_, cx| cx.notify()));
        let restore = state_file
            .as_deref()
            .and_then(|p| Active::load_from(p).ok())
            .map(|a| (a.host, a.session));
        // Every tick while you look at lemur; out of sight, where a refresh
        // still costs each host a `claude agents`, every AWAY ticks.
        let handle = window.window_handle();
        let refresh = cx.spawn(async move |this, cx| {
            let mut since = AWAY;
            loop {
                let looking = handle
                    .update(cx, |_, window, _| window.is_window_active())
                    .unwrap_or(false);
                if looking || since >= AWAY {
                    let Ok(refreshed) = this.update(cx, |this, cx| this.refresh(cx)) else {
                        break;
                    };
                    refreshed.await;
                    since = 0;
                }
                cx.background_executor().timer(REFRESH).await;
                since += 1;
            }
        });
        // Follow macOS when it switches between light and dark.
        let appearance = cx.observe_window_appearance(window, |_, window, cx| {
            if Prefs::get(cx).theme == ThemeChoice::System {
                Theme::sync_system_appearance(Some(window), cx);
            }
        });
        let sidebar_width = cfg.sidebar_width.map_or(SIDEBAR_WIDTH, px).max(MIN_SIDEBAR);
        let notes_width = cfg.notes_width.map_or(NOTES_WIDTH, px).max(MIN_NOTES);
        Lemur {
            cfg,
            state_file,
            snap: Snapshot::default(),
            loaded: false,
            terminals: HashMap::new(),
            active: None,
            collapsed: false,
            sidebar_width,
            notes_width,
            prompt: None,
            confirm_kill: None,
            renaming: None,
            done: HashSet::new(),
            status: None,
            restore,
            window: window.window_handle(),
            popups: HashMap::new(),
            changes: 0,
            notes,
            notes_open: false,
            picker: None,
            sidebar,
            _refresh: refresh,
            focus,
            _appearance: appearance,
            _sidebar_follows: sidebar_follows,
        }
    }

    /// Runs `work` off the main thread, then `then` with its result on the
    /// main window; that's skipped if the window has closed.
    fn in_background<R: Send + 'static>(
        &self,
        cx: &mut Context<Self>,
        work: impl FnOnce() -> R + Send + 'static,
        then: impl FnOnce(&mut Self, R, &mut Window, &mut Context<Self>) + 'static,
    ) -> Task<()> {
        let window = self.window;
        cx.spawn(async move |this, cx| {
            let result = cx.background_spawn(async move { work() }).await;
            window
                .update(cx, |_, window, cx| {
                    this.update(cx, |this, cx| then(this, result, window, cx))
                        .ok();
                })
                .ok();
        })
    }

    /// Shows what went wrong in the header.
    fn fail(&mut self, e: anyhow::Error, cx: &mut Context<Self>) {
        self.status = Some((format!("{e:#}"), true));
        cx.notify();
    }

    /// Fetches a snapshot in the background and applies it.
    fn refresh(&self, cx: &mut Context<Self>) -> Task<()> {
        let cfg = self.cfg.clone();
        let changes = self.changes;
        self.in_background(
            cx,
            move || snapshot::fetch(&cfg),
            move |this, snap, window, cx| {
                if this.changes == changes {
                    this.set_snapshot(snap, window, cx)
                }
            },
        )
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
        if let Some((worktree, label)) = self.worktree_of(&host, &session) {
            self.notes
                .update(cx, |n, cx| n.show(&host, &worktree, &label, window, cx));
        }
        self.remember(host, session);
        view.read(cx).focus_handle(cx).focus(window, cx);
        cx.notify();
    }

    /// The worktree a session runs in, and its label.
    fn worktree_of(&self, host: &str, session: &str) -> Option<(String, String)> {
        self.snap
            .worktrees(host)
            .find(|w| w.sessions.iter().any(|s| s.name == session))
            .map(|w| (w.path.clone(), worktree_label(w)))
    }

    /// Opens the notes panel on a worktree's notes.
    fn open_notes(
        &mut self,
        host: &str,
        worktree: &str,
        label: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.notes
            .update(cx, |n, cx| n.show(host, worktree, label, window, cx));
        self.notes_open = true;
        cx.notify();
    }

    /// Gives the keyboard back to the session on show, after a spotlight.
    fn focus_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.active.as_ref().and_then(|k| self.terminals.get(k)) {
            Some(view) => view.read(cx).focus_handle(cx).focus(window, cx),
            None => self.focus.focus(window, cx),
        }
        cx.notify();
    }

    fn remember(&self, host: String, session: String) {
        if let Some(path) = &self.state_file {
            let _ = Active { host, session }.save_to(path);
        }
    }

    /// Moves a thread within its list: the sidebar shows the new order at
    /// once, and tmux keeps it.
    fn reorder(&mut self, list: List, from: usize, to: usize, cx: &mut Context<Self>) {
        let Some(sessions) = self.list_mut(&list) else {
            return;
        };
        if !move_item(sessions, from, to) {
            return;
        }
        let names: Vec<String> = sessions.iter().map(|s| s.name.clone()).collect();
        self.changes += 1;
        cx.notify();
        let cfg = self.cfg.clone();
        let host = list.0;
        self.in_background(
            cx,
            move || actions::set_order(&cfg, &host, &names),
            |this, saved, _, cx| {
                // Refreshes that started before tmux had the order are stale too.
                this.changes += 1;
                if let Err(e) = saved {
                    this.fail(e, cx);
                }
                this.refresh(cx).detach();
            },
        )
        .detach();
    }

    fn list_mut(&mut self, (host, worktree): &List) -> Option<&mut Vec<Session>> {
        let h = self.snap.hosts.iter_mut().find(|h| &h.name == host)?;
        match worktree {
            None => Some(&mut h.other),
            Some(path) => h
                .bases
                .iter_mut()
                .flat_map(|b| b.worktrees.iter_mut())
                .find(|w| &w.path == path)
                .map(|w| &mut w.sessions),
        }
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
        let (host, session) = key;
        self.in_background(
            cx,
            move || actions::kill_session(&cfg, &host, &session),
            |this, killed, _, cx| {
                if let Err(e) = killed {
                    this.fail(e, cx);
                }
                this.refresh(cx).detach();
            },
        )
        .detach();
    }

    fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        self.collapsed = !self.collapsed;
        cx.notify();
    }

    fn toggle_notes(&mut self, cx: &mut Context<Self>) {
        self.notes_open = !self.notes_open;
        cx.notify();
    }

    /// Follows the pointer (`x`) dragging a side panel's edge across the
    /// window (`bounds`).
    fn resize_panel(
        &mut self,
        panel: Panel,
        x: Pixels,
        bounds: Bounds<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let shown = |open: bool, width: Pixels| if open { width } else { px(0.0) };
        match panel {
            Panel::Sidebar => {
                let room =
                    bounds.size.width - shown(self.notes_open, self.notes_width) - MIN_TERMINAL;
                self.sidebar_width = panel_width(x - bounds.left(), MIN_SIDEBAR, room);
            }
            Panel::Notes => {
                let room =
                    bounds.size.width - shown(!self.collapsed, self.sidebar_width) - MIN_TERMINAL;
                self.notes_width = panel_width(bounds.right() - x, MIN_NOTES, room);
            }
        }
        cx.notify();
    }

    /// Keeps the side panels' widths in the config file once a drag has
    /// changed them; lemur's own widths are left out of it.
    fn save_panel_widths(&mut self, cx: &mut Context<Self>) {
        let kept = |width: Pixels, default: Pixels| (width != default).then(|| f32::from(width));
        let widths = (
            kept(self.sidebar_width, SIDEBAR_WIDTH),
            kept(self.notes_width, NOTES_WIDTH),
        );
        if widths == (self.cfg.sidebar_width, self.cfg.notes_width) {
            return;
        }
        (self.cfg.sidebar_width, self.cfg.notes_width) = widths;
        if let Some(file) = Prefs::get(cx).config_file()
            && let Err(e) = Config::save_panel_widths(file, widths.0, widths.1)
        {
            self.fail(e, cx);
        }
    }

    fn header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let toggle = Button::new("toggle-sidebar")
            .icon(if self.collapsed {
                IconName::PanelLeftOpen
            } else {
                IconName::PanelLeftClose
            })
            .ghost()
            .small()
            .tooltip("Toggle sidebar (⌘B)")
            .on_click(cx.listener(|this, _, _, cx| this.toggle_sidebar(cx)));
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
        if let Some((message, error)) = &self.status {
            let color = if *error {
                cx.theme().danger
            } else {
                cx.theme().muted_foreground
            };
            bar = bar
                .child(div().flex_1())
                .child(div().text_sm().text_color(color).child(message.clone()));
        } else {
            bar = bar.child(div().flex_1());
        }
        bar.child(
            Button::new("toggle-notes")
                .icon(if self.notes_open {
                    IconName::PanelRightClose
                } else {
                    IconName::PanelRightOpen
                })
                .ghost()
                .small()
                .tooltip("Notes")
                .on_click(cx.listener(|this, _, _, cx| this.toggle_notes(cx))),
        )
    }
}

impl Render for Lemur {
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
        let this = cx.entity().downgrade();
        let picker = self.picker.as_ref().map(|(picker, _)| {
            let this = this.clone();
            spotlight::overlay(picker.clone(), move |window, cx| {
                this.update(cx, |this, cx| this.close_picker(window, cx))
                    .ok();
            })
        });
        let prompt = self.prompt.as_ref().map(|prompt| {
            let this = this.clone();
            spotlight::overlay(self.prompt_card(prompt, cx), move |window, cx| {
                this.update(cx, |this, cx| this.close_prompt(window, cx))
                    .ok();
            })
        });
        h_flex()
            .relative()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .on_action(cx.listener(|this, _: &ToggleSidebar, _, cx| this.toggle_sidebar(cx)))
            .on_action(cx.listener(|this, _: &ToggleNotes, _, cx| this.toggle_notes(cx)))
            .on_action(cx.listener(|this, _: &AddProject, window, cx| this.open_picker(window, cx)))
            .on_action(cx.listener(|this, _: &Refresh, _, cx| this.refresh(cx).detach()))
            .on_key_down(cx.listener(|this, e: &KeyDownEvent, window, cx| {
                if e.keystroke.key == "escape" && this.prompt.is_some() {
                    this.close_prompt(window, cx);
                    cx.stop_propagation();
                }
            }))
            .on_drag_move(cx.listener(|this, e: &DragMoveEvent<ResizePanel>, _, cx| {
                let panel = e.drag(cx).0;
                this.resize_panel(panel, e.event.position.x, e.bounds, cx);
            }))
            // A drag ends wherever the button comes up: on the handle, which
            // hides the window behind it from the pointer, too.
            .capture_any_mouse_up(cx.listener(|this, _, _, cx| this.save_panel_widths(cx)))
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.save_panel_widths(cx)),
            )
            .when(!self.collapsed, |this| {
                this.child(
                    div()
                        .relative()
                        .h_full()
                        .w(self.sidebar_width)
                        .flex_shrink_0()
                        .child(
                            self.sidebar
                                .clone()
                                .cached(StyleRefinement::default().size_full()),
                        )
                        .child(panel_edge(
                            "sidebar-edge",
                            Panel::Sidebar,
                            HandleEdge::Trailing,
                        )),
                )
            })
            .child(
                v_flex()
                    .flex_1()
                    .h_full()
                    .min_w_0()
                    .child(self.header(cx))
                    .child(
                        h_flex()
                            .flex_1()
                            .min_h_0()
                            .child(div().flex_1().min_w_0().h_full().child(main))
                            .when(self.notes_open, |this| {
                                this.child(
                                    div()
                                        .relative()
                                        .w(self.notes_width)
                                        .h_full()
                                        .flex_shrink_0()
                                        // Cached like the terminal; it redraws on its own changes.
                                        .child(
                                            self.notes
                                                .clone()
                                                .cached(StyleRefinement::default().size_full()),
                                        )
                                        // Its line is the panel's left border.
                                        .child(panel_edge(
                                            "notes-edge",
                                            Panel::Notes,
                                            HandleEdge::Leading,
                                        )),
                                )
                            }),
                    ),
            )
            .children(picker)
            .children(prompt)
            // Holds the window's focus with no terminal open. It takes no room
            // and can't be clicked, so clicks elsewhere keep their focus.
            .child(div().absolute().track_focus(&self.focus))
    }
}

/// A dragged side panel's width: `wanted` in whole pixels, at most `room`
/// and at least `min`; in a narrow window `min` wins.
fn panel_width(wanted: Pixels, min: Pixels, room: Pixels) -> Pixels {
    wanted.round().min(room).max(min)
}

/// The band on a side panel's inner `edge` that resizes it.
fn panel_edge(id: &'static str, panel: Panel, edge: HandleEdge) -> impl IntoElement {
    resize_handle(id, Axis::Horizontal)
        .inside(edge)
        .on_drag(ResizePanel(panel), |drag, _, _, cx| {
            cx.new(|_| drag.clone())
        })
}

/// Moves `items[from]` to `to`. False if that's no move, or either is out
/// of range.
fn move_item<T>(items: &mut Vec<T>, from: usize, to: usize) -> bool {
    if from == to || from >= items.len() || to >= items.len() {
        return false;
    }
    let moved = items.remove(from);
    items.insert(to, moved);
    true
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

/// A worktree's name in the sidebar: its branch, which tells worktrees
/// apart even when their folders share a name. `worktree-` is dropped from
/// the branches lemur and `claude -w` made, leaving the name you gave. A
/// detached worktree shows its folder's name, the one you gave lemur.
fn worktree_label(w: &Worktree) -> String {
    match (&w.branch, &w.head) {
        (Some(branch), _) => branch
            .strip_prefix("worktree-")
            .unwrap_or(branch)
            .to_string(),
        (None, Some(_)) => format!("{} (detached)", leaf(&w.path)),
        (None, None) => leaf(&w.path).to_string(),
    }
}

fn key(host: &str, s: &Session) -> Key {
    (host.to_string(), s.name.clone())
}

#[cfg(test)]
mod tests {
    // Not `super::*`: GPUI's `test` macro would shadow the built-in one.
    use super::folder_picker::HostChoice;
    use super::{
        Ask, Key, Lemur, MIN_NOTES, MIN_SIDEBAR, MIN_TERMINAL, NOTES_WIDTH, NotificationEvent,
        SIDEBAR_WIDTH,
    };
    use crate::config::{Base, Config};
    use crate::snapshot::{self, Activity, BaseView, HostView, Session, Snapshot, Worktree};
    use crate::test_support::{TempDir, TmuxServer, git_in, wait_until};
    use gpui::VisualTestContext;
    use gpui_kit::component::WindowExt;
    use gpui_kit::{AppContext, Entity, Modifiers, MouseButton, Pixels, point, px, size};
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
                order: None,
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
    fn apply(app: &Entity<Lemur>, sessions: &[(&str, Activity)], cx: &mut VisualTestContext) {
        let snap = snapshot(sessions);
        cx.update(|window, cx| app.update(cx, |app, cx| app.set_snapshot(snap, window, cx)));
    }

    fn open(app: &Entity<Lemur>, session: &str, cx: &mut VisualTestContext) {
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
        let (app, cx) = rooted(server.config(), cx);
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

    /// Dropping a thread on another moves it to that one's place: in the
    /// sidebar at once, and in tmux for good.
    #[gpui::test]
    fn dragging_a_thread_reorders_its_list(cx: &mut gpui::TestAppContext) {
        let server = TmuxServer::new("drag");
        for name in ["a", "b", "c"] {
            server.tmux(&["new-session", "-d", "-s", name]);
        }
        let (app, cx) = rooted(server.config(), cx);
        cx.simulate_resize(size(px(1000.0), px(600.0)));
        cx.run_until_parked();
        let shown = |cx: &mut VisualTestContext| {
            app.read_with(cx, |app, _| {
                app.snap.hosts[0]
                    .other
                    .iter()
                    .map(|s| s.name.clone())
                    .collect::<Vec<_>>()
            })
        };
        assert_eq!(shown(cx), ["a", "b", "c"]);

        drag("thread-0", "thread-2", cx);
        assert_eq!(shown(cx), ["b", "c", "a"]);

        cx.run_until_parked();
        let kept: Vec<String> = snapshot::fetch(&server.config()).hosts[0]
            .other
            .iter()
            .map(|s| s.name.clone())
            .collect();
        assert_eq!(kept, ["b", "c", "a"]);
        assert_eq!(
            shown(cx),
            ["b", "c", "a"],
            "the refresh after the move agrees"
        );
    }

    /// Dragging a side panel's inner edge resizes it: down to its minimum,
    /// and up to what leaves the terminal its own. The config keeps the
    /// widths, but not lemur's own.
    #[gpui::test]
    fn dragging_a_side_panels_edge_resizes_it(cx: &mut gpui::TestAppContext) {
        let server = TmuxServer::new("resize-panels");
        let (app, cx) = rooted(server.config(), cx);
        cx.simulate_resize(size(px(1000.0), px(600.0)));
        cx.run_until_parked();
        let widths = |cx: &mut VisualTestContext| {
            app.read_with(cx, |app, _| (app.sidebar_width, app.notes_width))
        };
        let kept = |cx: &mut VisualTestContext| {
            app.read_with(cx, |app, _| (app.cfg.sidebar_width, app.cfg.notes_width))
        };
        // The bands lie just inside the panels' inner edges.
        let across = |from: Pixels, to: Pixels, cx: &mut VisualTestContext| {
            let (from, to) = (point(from, px(300.0)), point(to, px(300.0)));
            let none = Modifiers::none();
            cx.simulate_mouse_down(from, MouseButton::Left, none);
            cx.simulate_mouse_move(from + point(px(0.0), px(6.0)), MouseButton::Left, none);
            cx.simulate_mouse_move(to, MouseButton::Left, none);
            cx.simulate_mouse_up(to, MouseButton::Left, none);
        };

        across(SIDEBAR_WIDTH - px(2.0), px(400.4), cx);
        assert_eq!(widths(cx).0, px(400.0));
        assert_eq!(kept(cx), (Some(400.0), None));
        across(px(398.0), px(20.0), cx);
        assert_eq!(widths(cx).0, MIN_SIDEBAR);
        across(MIN_SIDEBAR - px(2.0), px(900.0), cx);
        assert_eq!(widths(cx).0, px(1000.0) - MIN_TERMINAL);
        across(px(1000.0) - MIN_TERMINAL - px(2.0), SIDEBAR_WIDTH, cx);
        assert_eq!(kept(cx), (None, None), "back at lemur's own width");

        app.update(cx, |app, cx| app.toggle_notes(cx));
        cx.run_until_parked();
        let edge = |width: Pixels| px(1000.0) - width + px(2.0);
        across(edge(NOTES_WIDTH), px(200.0), cx);
        assert_eq!(widths(cx).1, px(1000.0) - SIDEBAR_WIDTH - MIN_TERMINAL);
        across(edge(widths(cx).1), px(990.0), cx);
        assert_eq!(widths(cx).1, MIN_NOTES);
        assert_eq!(kept(cx), (None, Some(f32::from(MIN_NOTES))));
    }

    /// Opening a thread points the notes at its worktree without opening
    /// the panel; a worktree's notes icon opens it.
    #[gpui::test]
    fn notes_follow_the_opened_thread(cx: &mut gpui::TestAppContext) {
        let server = TmuxServer::new("notes");
        let mut cfg = server.config();
        cfg.bases = toml::from_str::<Config>("[[bases]]\nhost = \"local\"\npath = \"/srv/api\"")
            .unwrap()
            .bases;
        cx.executor().allow_parking();
        cx.update(gpui_kit::init);
        let (app, cx) = cx.add_window_view(|window, cx| Lemur::new(cfg, None, window, cx));
        cx.run_until_parked();
        let worktree = |path: &str, branch: &str, sessions: &[&str]| Worktree {
            path: path.into(),
            branch: Some(branch.into()),
            head: None,
            main: false,
            sessions: snapshot(
                &sessions
                    .iter()
                    .map(|s| (*s, Activity::Idle))
                    .collect::<Vec<_>>(),
            )
            .hosts
            .remove(0)
            .other,
        };
        let snap = Snapshot {
            hosts: vec![HostView {
                name: "local".into(),
                error: None,
                bases: vec![BaseView {
                    index: 0,
                    error: None,
                    plain: false,
                    worktrees: vec![
                        worktree("/srv/api", "main", &[]),
                        worktree(
                            "/srv/api/.claude/worktrees/auth",
                            "worktree-auth",
                            &["api/t"],
                        ),
                    ],
                }],
                other: vec![],
            }],
        };
        cx.update(|window, cx| app.update(cx, |app, cx| app.set_snapshot(snap, window, cx)));
        let notes = |cx: &mut VisualTestContext| {
            app.read_with(cx, |app, cx| {
                let shown = app.notes.read(cx).worktree().map(str::to_string);
                (shown, app.notes_open)
            })
        };
        assert_eq!(notes(cx), (None, false));

        open(&app, "api/t", cx);
        assert_eq!(
            notes(cx),
            (Some("/srv/api/.claude/worktrees/auth".into()), false)
        );
        cx.update(|window, cx| {
            app.update(cx, |app, cx| {
                app.open_notes("local", "/srv/api", "main", window, cx)
            })
        });
        assert_eq!(notes(cx), (Some("/srv/api".into()), true));

        // Let the terminal's attach attempt end before the server cleans up.
        wait_until(Duration::from_secs(5), || {
            cx.run_until_parked();
            app.read_with(cx, |app, cx| {
                app.terminals.values().all(|t| t.read(cx).exited())
            })
        });
    }

    #[test]
    fn worktrees_are_named_after_their_branch() {
        let worktree = |branch: Option<&str>, head: Option<&str>| super::Worktree {
            path: "/srv/web/.worktrees/equal-burrow/web".into(),
            branch: branch.map(str::to_string),
            head: head.map(str::to_string),
            main: false,
            sessions: vec![],
        };
        let label = |b, h| super::worktree_label(&worktree(b, h));
        assert_eq!(
            label(Some("feature/cleaning-app-react"), Some("abc")),
            "feature/cleaning-app-react"
        );
        assert_eq!(label(Some("worktree-auth"), None), "auth");
        assert_eq!(label(None, Some("0123456789abcdef")), "web (detached)");
        // A bare repository has neither.
        assert_eq!(label(None, None), "web");
    }

    /// The picker closes with Esc; a folder it adds joins the sidebar once.
    #[gpui::test]
    fn an_added_folder_joins_the_sidebar_once(cx: &mut gpui::TestAppContext) {
        let server = TmuxServer::new("add");
        let cfg = server.config();
        let local = HostChoice {
            host: cfg.hosts[0].clone(),
            configured: true,
        };
        cx.executor().allow_parking();
        cx.update(|cx| {
            gpui_kit::init(cx);
            super::folder_picker::bind_keys(cx);
        });
        let (app, cx) = cx.add_window_view(|window, cx| Lemur::new(cfg, None, window, cx));
        cx.update(|window, cx| app.update(cx, |app, cx| app.open_picker(window, cx)));
        cx.run_until_parked();
        cx.simulate_keystrokes("escape");
        app.read_with(cx, |app, _| assert!(app.picker.is_none()));

        for _ in 0..2 {
            app.update(cx, |app, cx| {
                app.add_folder(local.clone(), "/srv/api".into(), cx)
            });
        }
        cx.run_until_parked();
        app.read_with(cx, |app, _| {
            let bases: Vec<&str> = app.cfg.bases.iter().map(|b| b.path.as_str()).collect();
            assert_eq!(bases, ["/srv/api"]);
            assert_eq!(app.cfg.hosts.len(), 1);
            assert_eq!(app.snap.hosts[0].bases.len(), 1, "the refresh lists it");
            let status = app.status.as_ref().map(|(s, _)| s.as_str());
            assert_eq!(status, Some("/srv/api is in the sidebar already"));
        });
    }

    /// Drags one element onto another, by their debug selectors, as a mouse
    /// does: down, a little move to start the drag, then over and up.
    fn drag(from: &'static str, to: &'static str, cx: &mut VisualTestContext) {
        let from = cx.debug_bounds(from).expect(from).center();
        let to = cx.debug_bounds(to).expect(to).center();
        let none = Modifiers::none();
        cx.simulate_mouse_down(from, MouseButton::Left, none);
        cx.simulate_mouse_move(from + point(px(0.0), px(6.0)), MouseButton::Left, none);
        cx.simulate_mouse_move(to, MouseButton::Left, none);
        cx.simulate_mouse_up(to, MouseButton::Left, none);
    }

    /// Wraps the app in Root, as `gpui_kit::open_window` does; dialogs need it.
    fn rooted(
        cfg: Config,
        cx: &mut gpui::TestAppContext,
    ) -> (Entity<Lemur>, &mut VisualTestContext) {
        cx.executor().allow_parking();
        cx.update(gpui_kit::init);
        let mut app = None;
        let (_, cx) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| Lemur::new(cfg, None, window, cx));
            app = Some(view.clone());
            gpui_kit::base::Root::new(view, window, cx)
        });
        cx.run_until_parked();
        (app.unwrap(), cx)
    }

    /// Removing a project asks first, then forgets it; the other projects'
    /// views keep pointing at their own config entries, and a snapshot
    /// fetched before the removal is dropped.
    #[gpui::test]
    fn removing_a_project_keeps_the_sidebar_consistent(cx: &mut gpui::TestAppContext) {
        let server = TmuxServer::new("rm-project");
        let mut cfg = server.config();
        cfg.bases.push(Base::new("local", "/srv/one"));
        cfg.bases.push(Base::new("local", "/srv/two"));
        let (app, cx) = rooted(cfg, cx);

        let one = Base::new("local", "/srv/one");
        cx.update(|window, cx| {
            app.update(cx, |app, cx| {
                app.confirm_remove_project(one.clone(), window, cx)
            })
        });
        assert!(cx.update(|window, cx| window.has_active_dialog(cx)));
        cx.update(|window, cx| window.close_dialog(cx));

        // A refresh is on its way when the project goes.
        app.update(cx, |app, cx| app.refresh(cx).detach());
        app.update(cx, |app, cx| app.remove_project(&one, cx));
        cx.run_until_parked();
        app.read_with(cx, |app, _| {
            let paths: Vec<&str> = app.cfg.bases.iter().map(|b| b.path.as_str()).collect();
            assert_eq!(paths, ["/srv/two"]);
            let views = &app.snap.hosts[0].bases;
            assert_eq!(views.len(), 1);
            assert_eq!(views[0].index, 0);
        });
    }

    /// A worktree with changes isn't removed without asking again.
    #[gpui::test]
    fn removing_a_changed_worktree_asks_again(cx: &mut gpui::TestAppContext) {
        let server = TmuxServer::new("rm-dirty");
        let dir = TempDir::new("rm-dirty");
        let repo = dir.join("repo");
        std::fs::create_dir(&repo).unwrap();
        let git = |args: &[&str]| git_in(&repo, args);
        git(&["init", "-q"]);
        git(&["commit", "-q", "--allow-empty", "-m", "init"]);
        git(&["worktree", "add", "-q", "-b", "draft", "wt"]);
        let wt = repo.join("wt");
        std::fs::write(wt.join("notes.txt"), "unsaved").unwrap();
        let mut cfg = server.config();
        cfg.bases.push(Base::new("local", &repo.to_string_lossy()));
        let (app, cx) = rooted(cfg, cx);
        let path = wt.to_string_lossy().into_owned();

        let remove = |force: bool, cx: &mut VisualTestContext| {
            app.update(cx, |app, cx| {
                let mut removal = app.removal(0, path.clone(), "draft".into());
                // Only deleting its folder would lose the changes.
                removal.delete = true;
                app.remove_worktree(removal, force, cx)
            });
            cx.run_until_parked();
        };
        remove(false, cx);
        assert!(wt.exists());
        assert!(cx.update(|window, cx| window.has_active_dialog(cx)));
        cx.update(|window, cx| window.close_dialog(cx));

        // Its folder color goes with it.
        app.update(cx, |app, cx| {
            app.set_worktree_color(0, &path, Some("red"), cx)
        });
        remove(true, cx);
        assert!(!wt.exists());
        app.read_with(cx, |app, _| {
            assert!(app.cfg.bases[0].worktree_colors.is_empty())
        });
    }

    /// The remove dialog takes a worktree out of git and keeps its folder,
    /// unless its box is ticked; then the folder goes too.
    #[gpui::test]
    fn removing_a_worktree_deletes_its_folder_when_ticked(cx: &mut gpui::TestAppContext) {
        let server = TmuxServer::new("rm-tick");
        let dir = TempDir::new("rm-tick");
        let repo = dir.join("repo");
        std::fs::create_dir(&repo).unwrap();
        let git = |args: &[&str]| git_in(&repo, args);
        git(&["init", "-q"]);
        git(&["commit", "-q", "--allow-empty", "-m", "init"]);
        git(&["worktree", "add", "-q", "-b", "kept", "wt/kept"]);
        git(&["worktree", "add", "-q", "-b", "gone", "wt/gone"]);
        let (kept, gone) = (repo.join("wt/kept"), repo.join("wt/gone"));
        let mut cfg = server.config();
        cfg.bases.push(Base::new("local", &repo.to_string_lossy()));
        let (app, cx) = rooted(cfg, cx);
        cx.simulate_resize(size(px(1000.0), px(600.0)));
        cx.run_until_parked();
        let confirm = |worktree: &std::path::Path, cx: &mut VisualTestContext| {
            let path = worktree.to_string_lossy().into_owned();
            cx.update(|window, cx| {
                app.update(cx, |app, cx| {
                    let removal = app.removal(0, path, "x".into());
                    app.confirm_remove_worktree(removal, None, window, cx)
                })
            });
            cx.run_until_parked();
            assert!(cx.update(|window, cx| window.has_active_dialog(cx)));
        };
        let listed = || {
            let out = std::process::Command::new("git")
                .args(["worktree", "list"])
                .current_dir(&repo)
                .output()
                .unwrap();
            String::from_utf8_lossy(&out.stdout).into_owned()
        };

        confirm(&kept, cx);
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert!(wait_until(Duration::from_secs(5), || !listed().contains("wt/kept")));
        assert!(kept.exists() && !kept.join(".git").exists());

        confirm(&gone, cx);
        let tick = cx.debug_bounds("delete-folder").expect("the box").center();
        cx.simulate_click(tick, Modifiers::none());
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert!(wait_until(Duration::from_secs(5), || !gone.exists()));
        assert!(!listed().contains("wt/gone"));
    }

    /// A new worktree's prompt offers the host's branches to start from,
    /// the default one first; the worktree gets the branch typed, or with
    /// none is detached. Picking another branch keeps the prompt open.
    #[gpui::test]
    fn a_new_worktree_gets_the_branch_and_start_asked_for(cx: &mut gpui::TestAppContext) {
        let server = TmuxServer::new("new-wt-ui");
        let dir = TempDir::new("new-wt-ui");
        let origin = dir.join("origin");
        std::fs::create_dir(&origin).unwrap();
        git_in(&origin, &["init", "-q", "-b", "main"]);
        git_in(&origin, &["commit", "-q", "--allow-empty", "-m", "one"]);
        git_in(&dir, &["clone", "-q", "origin", "repo"]);
        let repo = dir.join("repo");
        git_in(&repo, &["branch", "side"]);
        let mut cfg = server.config();
        // Typed into the new sessions; nothing to start.
        cfg.hosts[0].claude = "true".into();
        cfg.bases.push(Base::new("local", &repo.to_string_lossy()));
        let (app, cx) = rooted(cfg, cx);
        cx.simulate_resize(size(px(1000.0), px(600.0)));
        cx.run_until_parked();
        let ask = |cx: &mut VisualTestContext| {
            cx.update(|window, cx| {
                app.update(cx, |app, cx| {
                    app.ask_name(Ask::NewWorktree { base: 0 }, window, cx)
                })
            });
            cx.run_until_parked();
        };
        let from = |cx: &mut VisualTestContext| {
            app.read_with(cx, |app, cx| {
                let fields = app.prompt.as_ref()?.worktree.as_ref()?;
                fields.from.read(cx).selected_value().cloned()
            })
        };
        let branch_field = |cx: &mut VisualTestContext| {
            let branch = app.read_with(cx, |app, _| {
                app.prompt
                    .as_ref()
                    .unwrap()
                    .worktree
                    .as_ref()
                    .unwrap()
                    .branch
                    .clone()
            });
            cx.update(|window, cx| branch.update(cx, |input, cx| input.focus(window, cx)));
        };
        let current = |name: &str| {
            let out = std::process::Command::new("git")
                .args(["branch", "--show-current"])
                .current_dir(repo.join(".claude/worktrees").join(name))
                .output()
                .unwrap();
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };

        ask(cx);
        assert_eq!(from(cx).as_deref(), Some("origin/main"));
        cx.simulate_input("feat");
        branch_field(cx);
        cx.simulate_input("feature/x");
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        let made = repo.join(".claude/worktrees/feat");
        assert!(wait_until(Duration::from_secs(5), || made.exists()));
        assert_eq!(current("feat"), "feature/x");

        // Another start, picked from the list, and no branch.
        ask(cx);
        cx.simulate_input("loose");
        let picker = cx.debug_bounds("field From").expect("the start").center();
        // Esc closes the list first, not the prompt.
        cx.simulate_click(picker, Modifiers::none());
        cx.simulate_keystrokes("escape");
        app.read_with(cx, |app, _| {
            assert!(app.prompt.is_some(), "Esc closed the prompt")
        });
        cx.simulate_click(picker, Modifiers::none());
        cx.simulate_keystrokes("down enter");
        cx.run_until_parked();
        app.read_with(cx, |app, _| assert!(app.prompt.is_some(), "still asking"));
        let picked = from(cx);
        assert!(
            picked.as_deref().is_some_and(|b| b != "origin/main"),
            "{picked:?}"
        );
        cx.update(|window, cx| {
            let input = app.read(cx).prompt.as_ref().unwrap().input.clone();
            input.update(cx, |input, cx| input.focus(window, cx))
        });
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        let made = repo.join(".claude/worktrees/loose");
        assert!(wait_until(Duration::from_secs(5), || made.exists()));
        assert_eq!(current("loose"), "", "detached");
    }

    /// Names are asked for in the spotlight: a bad one is explained there
    /// rather than in the header, and Esc closes it.
    #[gpui::test]
    fn names_are_asked_for_in_the_spotlight(cx: &mut gpui::TestAppContext) {
        let (app, cx) = rooted(toml::from_str("").unwrap(), cx);
        let rename = Ask::Rename {
            host: "local".into(),
            session: "api/claude--calm-heron".into(),
        };
        cx.update(|window, cx| app.update(cx, |app, cx| app.ask_name(rename, window, cx)));
        cx.run_until_parked();
        app.read_with(cx, |app, cx| {
            let prompt = app.prompt.as_ref().expect("asking");
            assert_eq!(prompt.input.read(cx).value(), "claude");
        });

        // The label starts selected, so typing replaces it.
        cx.simulate_input("not ok!");
        cx.simulate_keystrokes("enter");
        app.read_with(cx, |app, cx| {
            let prompt = app.prompt.as_ref().expect("still asking");
            assert_eq!(prompt.input.read(cx).value(), "not ok!");
            let error = prompt.error.as_ref().map(|(e, _)| e.as_str());
            assert!(error.is_some_and(|e| e.contains("letters")), "{error:?}");
            assert!(app.status.is_none(), "the header stays quiet");
        });
        cx.simulate_input("x");
        app.read_with(cx, |app, _| {
            assert!(
                app.prompt.as_ref().unwrap().error.is_none(),
                "typing clears it"
            )
        });

        cx.simulate_keystrokes("escape");
        app.read_with(cx, |app, _| assert!(app.prompt.is_none()));
    }

    /// Clicking a project's name folds it away and back; a worktree's
    /// folder takes a color and gives it up.
    #[gpui::test]
    fn projects_fold_and_worktrees_take_a_color(cx: &mut gpui::TestAppContext) {
        let server = TmuxServer::new("fold");
        let mut cfg = server.config();
        cfg.bases.push(Base::new("local", "/srv/api"));
        let (app, cx) = rooted(cfg, cx);
        cx.simulate_resize(size(px(1000.0), px(600.0)));
        cx.run_until_parked();
        let folded =
            |cx: &mut VisualTestContext| app.read_with(cx, |app, _| app.cfg.bases[0].collapsed);

        let header = cx
            .debug_bounds("project api · local")
            .expect("header")
            .center();
        cx.simulate_click(header, Modifiers::none());
        assert!(folded(cx));
        cx.run_until_parked();
        cx.simulate_click(header, Modifiers::none());
        assert!(!folded(cx));

        app.update(cx, |app, cx| {
            app.set_worktree_color(0, "/srv/api", Some("teal"), cx)
        });
        app.read_with(cx, |app, _| {
            assert!(super::sidebar::worktree_color(&app.cfg.bases[0], "/srv/api").is_some());
            assert!(super::sidebar::worktree_color(&app.cfg.bases[0], "/srv/api/wt").is_none());
        });
        app.update(cx, |app, cx| {
            app.set_worktree_color(0, "/srv/api", None, cx)
        });
        app.read_with(cx, |app, _| {
            assert!(app.cfg.bases[0].worktree_colors.is_empty())
        });
    }

    /// Dropping a worktree on another moves it there, and the next refresh
    /// keeps that order; the main checkout stays known wherever it is.
    #[gpui::test]
    fn dragging_a_worktree_reorders_its_project(cx: &mut gpui::TestAppContext) {
        let server = TmuxServer::new("drag-wt");
        let dir = TempDir::new("drag-wt");
        let repo = dir.join("api");
        std::fs::create_dir(&repo).unwrap();
        let git = |args: &[&str]| git_in(&repo, args);
        git(&["init", "-q", "-b", "main"]);
        git(&["commit", "-q", "--allow-empty", "-m", "init"]);
        git(&["worktree", "add", "-q", "-b", "a", "wt/a"]);
        git(&["worktree", "add", "-q", "-b", "b", "wt/b"]);
        let mut cfg = server.config();
        cfg.bases.push(Base::new("local", &repo.to_string_lossy()));
        let (app, cx) = rooted(cfg, cx);
        cx.simulate_resize(size(px(1000.0), px(600.0)));
        cx.run_until_parked();
        let shown = |cx: &mut VisualTestContext| {
            app.read_with(cx, |app, _| {
                app.snap.hosts[0].bases[0]
                    .worktrees
                    .iter()
                    .map(|w| (super::worktree_label(w), w.main))
                    .collect::<Vec<_>>()
            })
        };
        let names = |cx: &mut VisualTestContext| -> Vec<String> {
            shown(cx).into_iter().map(|(name, _)| name).collect()
        };
        assert_eq!(names(cx), ["main", "a", "b"]);

        drag("worktree-0", "worktree-2", cx);
        assert_eq!(names(cx), ["a", "b", "main"]);

        app.update(cx, |app, cx| app.refresh(cx).detach());
        cx.run_until_parked();
        assert_eq!(
            shown(cx),
            [
                ("a".to_string(), false),
                ("b".to_string(), false),
                ("main".to_string(), true)
            ]
        );
    }

    /// The menus' actions and keys reach the main window: ⌘O opens the
    /// folder picker, Toggle Notes the notes, ⌘R refreshes.
    #[gpui::test]
    fn menu_actions_reach_the_main_window(cx: &mut gpui::TestAppContext) {
        let (app, cx) = rooted(toml::from_str("").unwrap(), cx);
        cx.update(|_, cx| super::bind_keys(cx));

        cx.simulate_keystrokes("cmd-o");
        app.read_with(cx, |app, _| assert!(app.picker.is_some()));
        cx.simulate_keystrokes("escape");
        app.read_with(cx, |app, _| assert!(app.picker.is_none()));

        cx.dispatch_action(super::ToggleNotes);
        app.read_with(cx, |app, _| assert!(app.notes_open));

        app.update(cx, |app, _| app.loaded = false);
        cx.simulate_keystrokes("cmd-r");
        cx.run_until_parked();
        app.read_with(cx, |app, _| assert!(app.loaded, "a refresh came in"));
    }

    /// Out of sight, refreshes come every AWAY ticks rather than every
    /// tick; back in sight, every tick again.
    #[gpui::test]
    fn refreshes_slow_down_out_of_sight(cx: &mut gpui::TestAppContext) {
        // Test windows start inactive.
        let (app, cx) = rooted(toml::from_str("").unwrap(), cx);
        // Whether a refresh came in since the last ask.
        let refreshed = |cx: &mut VisualTestContext| {
            app.update(cx, |app, _| std::mem::replace(&mut app.loaded, false))
        };
        let tick = |cx: &mut VisualTestContext| {
            cx.executor().advance_clock(super::REFRESH);
            cx.run_until_parked();
        };
        assert!(refreshed(cx), "the first one");
        for _ in 1..super::AWAY {
            tick(cx);
            assert!(!refreshed(cx));
        }
        tick(cx);
        assert!(refreshed(cx));

        cx.update(|window, _| window.activate_window());
        for _ in 0..2 {
            tick(cx);
            assert!(refreshed(cx));
        }
    }

    /// Terminal output redraws the terminal alone; the cached sidebar waits
    /// until the app itself changes.
    #[gpui::test]
    fn terminal_output_leaves_the_sidebar_alone(cx: &mut gpui::TestAppContext) {
        let server = TmuxServer::new("cache");
        server.tmux(&["new-session", "-d", "-s", "t", "sleep 600"]);
        let (app, cx) = rooted(server.config(), cx);
        cx.simulate_resize(size(px(1000.0), px(600.0)));
        open(&app, "t", cx);
        cx.run_until_parked();
        let renders =
            |cx: &mut VisualTestContext| app.read_with(cx, |app, cx| app.sidebar.read(cx).renders);
        let before = renders(cx);
        assert!(before > 0, "drawn at least once");

        let terminal = app.read_with(cx, |app, _| app.terminals[&key("t")].clone());
        terminal.update(cx, |_, cx| cx.notify());
        cx.run_until_parked();
        assert_eq!(renders(cx), before);

        app.update(cx, |_, cx| cx.notify());
        cx.run_until_parked();
        assert!(renders(cx) > before);
    }

    #[gpui::test]
    fn a_session_that_finishes_elsewhere_is_marked_done(cx: &mut gpui::TestAppContext) {
        // No hosts, so the background refresh has nothing to fetch.
        let cfg: Config = toml::from_str("").unwrap();
        cx.update(gpui_kit::init);
        let (app, cx) = cx.add_window_view(|window, cx| Lemur::new(cfg, None, window, cx));
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
            cx.add_window_view(|window, cx| Lemur::new(server.config(), None, window, cx));
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
