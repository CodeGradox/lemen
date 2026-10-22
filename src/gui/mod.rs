//! The window: a sidebar of sessions next to a live terminal.
//!
//! Every session you open keeps its own terminal (`tmux attach` over SSH),
//! so switching is instant and scrollback survives.

mod folder_picker;
mod keys;
mod notes;
mod notification;
mod rows;
mod settings;
mod spotlight;
mod terminal;
mod terminal_view;

use crate::actions;
use crate::attach::Active;
use crate::browse;
use crate::config::{Base, Config, Host, ThemeChoice, home, leaf};
use crate::snapshot::{self, Activity, Session, Snapshot, Worktree};
use folder_picker::{FolderPicker, HostChoice, PickerEvent};
use gpui_kit::component::WindowExt;
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu, PopupMenu, PopupMenuItem};
use gpui_kit::component::sidebar::{Sidebar, SidebarCollapsible, SidebarHeader, SidebarMenuItem};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme, Icon, IconName, Sizable, StyledExt, Theme, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use notes::NotesPanel;
use notification::{Notification, NotificationEvent};
use settings::Prefs;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;
use terminal_view::TerminalView;

actions!(
    lemen,
    [Quit, ToggleSidebar, OpenSettings, Hide, HideOthers, ShowAll]
);

const REFRESH: Duration = Duration::from_secs(3);

/// The sidebar icon for Claude sessions, drawn in Claude's orange.
const CLAUDE_ICON: &[u8] = include_bytes!("claude.svg");
const CLAUDE_ORANGE: u32 = 0xd97757;

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

enum Done {
    Started(Key),
    Renamed {
        host: String,
        old: String,
        new: String,
    },
}

/// A name being asked for, in the spotlight.
struct Prompt {
    what: Ask,
    input: Entity<InputState>,
    /// Why the name typed can't be used, and that name.
    error: Option<(String, String)>,
    _events: Subscription,
}

struct Lemen {
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
    /// Counts changes a snapshot fetched across one would undo (the thread
    /// order, a removed project); such a snapshot is dropped.
    changes: u64,
    /// The notes of the last worktree opened, beside the terminal.
    notes: Entity<NotesPanel>,
    notes_open: bool,
    /// Adding a folder, over the window.
    picker: Option<(Entity<FolderPicker>, Subscription)>,
    _refresh: Task<()>,
    _appearance: Subscription,
}

pub fn run(cfg: Config) -> anyhow::Result<()> {
    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(move |cx| {
            gpui_kit::init(cx);
            cx.set_global(Prefs::new(&cfg, Config::path().ok()));
            settings::apply_theme(None, cx);
            cx.bind_keys([
                KeyBinding::new("cmd-q", Quit, None),
                KeyBinding::new("cmd-,", OpenSettings, None),
                KeyBinding::new("cmd-h", Hide, None),
                KeyBinding::new("alt-cmd-h", HideOthers, None),
                KeyBinding::new("cmd-b", ToggleSidebar, None),
                KeyBinding::new("cmd-w", settings::CloseWindow, Some(settings::CONTEXT)),
                // gpui-component's Root moves focus on Tab; in a terminal it belongs to the program.
                KeyBinding::new("tab", NoAction, Some(terminal_view::CONTEXT)),
                KeyBinding::new("shift-tab", NoAction, Some(terminal_view::CONTEXT)),
            ]);
            folder_picker::bind_keys(cx);
            cx.on_action(|_: &Quit, cx| cx.quit());
            cx.on_action(|_: &OpenSettings, cx| settings::open(cx));
            cx.on_action(|_: &Hide, cx| cx.hide());
            cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
            cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
            // The app menu, as macOS apps have it; shortcuts come from the bindings above.
            cx.set_menus([Menu::new("lemen").items([
                MenuItem::action("Settings…", OpenSettings),
                MenuItem::separator(),
                MenuItem::os_submenu("Services", SystemMenuType::Services),
                MenuItem::separator(),
                MenuItem::action("Hide lemen", Hide),
                MenuItem::action("Hide Others", HideOthers),
                MenuItem::action("Show All", ShowAll),
                MenuItem::separator(),
                MenuItem::action("Quit lemen", Quit),
            ])]);
            let options = WindowOptions {
                titlebar: Some(TitlebarOptions {
                    title: Some("lemen".into()),
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
                cx.new(|cx| Lemen::new(cfg, Active::path().ok(), window, cx))
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

impl Lemen {
    fn new(
        cfg: Config,
        state_file: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        // Notes live beside the state file, in ~/.lemen/notes; tests have neither.
        let notes_dir = state_file
            .as_deref()
            .and_then(Path::parent)
            .map(|dir| dir.join("notes"));
        let notes = cx.new(|cx| NotesPanel::new(notes_dir, window, cx));
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
        let appearance = cx.observe_window_appearance(window, |_, window, cx| {
            if Prefs::get(cx).theme == ThemeChoice::System {
                Theme::sync_system_appearance(Some(window), cx);
            }
        });
        Lemen {
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
            changes: 0,
            notes,
            notes_open: false,
            picker: None,
            _refresh: refresh,
            _appearance: appearance,
        }
    }

    /// Fetches a snapshot in the background and applies it.
    fn refresh(&self, cx: &mut Context<Self>) -> Task<()> {
        let cfg = self.cfg.clone();
        let window = self.window;
        let changes = self.changes;
        cx.spawn(async move |this, cx| {
            let snap = cx
                .background_spawn(async move { snapshot::fetch(&cfg) })
                .await;
            window
                .update(cx, |_, window, cx| {
                    this.update(cx, |this, cx| {
                        if this.changes == changes {
                            this.set_snapshot(snap, window, cx)
                        }
                    })
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
        let h = self.snap.hosts.iter().find(|h| h.name == host)?;
        h.bases
            .iter()
            .flat_map(|b| b.worktrees.iter())
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

    fn open_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let hosts = host_choices(&self.cfg);
        let picker = cx.new(|cx| FolderPicker::new(hosts, window, cx));
        let events = cx.subscribe_in(&picker, window, |this, _, event, window, cx| {
            if let PickerEvent::Add { host, path } = event {
                this.add_folder(host.clone(), path.clone(), cx);
            }
            this.close_picker(window, cx);
        });
        self.picker = Some((picker, events));
        cx.notify();
    }

    fn close_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.picker = None;
        self.focus_terminal(window, cx);
    }

    fn close_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.prompt = None;
        self.focus_terminal(window, cx);
    }

    /// Gives the keyboard back to the session on show, after a spotlight.
    fn focus_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(view) = self.active.as_ref().and_then(|k| self.terminals.get(k)) {
            view.read(cx).focus_handle(cx).focus(window, cx);
        }
        cx.notify();
    }

    /// The name field, in the spotlight: what it's for, the name, and a
    /// hint or why the name can't be used.
    fn prompt_card(&self, prompt: &Prompt, cx: &App) -> Div {
        let (icon, what, verb) = match &prompt.what {
            Ask::NewWorktree { base } => (
                IconName::Folder,
                format!("New worktree in {}", self.cfg.bases[*base].name()),
                "start",
            ),
            Ask::NewThread {
                worktree, shell, ..
            } => (
                if *shell {
                    IconName::SquareTerminal
                } else {
                    IconName::Plus
                },
                format!(
                    "New {} in {}",
                    if *shell { "shell" } else { "Claude thread" },
                    self.worktree_name(worktree)
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

    /// A worktree's name as the sidebar shows it.
    fn worktree_name(&self, path: &str) -> String {
        self.snap
            .hosts
            .iter()
            .flat_map(|h| h.bases.iter().flat_map(|b| b.worktrees.iter()))
            .find(|w| w.path == path)
            .map(worktree_label)
            .unwrap_or_else(|| leaf(path).to_string())
    }

    /// Adds a repository to the sidebar, and to the config file so it stays.
    fn add_folder(&mut self, choice: HostChoice, path: String, cx: &mut Context<Self>) {
        let host = choice.host;
        if self
            .cfg
            .bases
            .iter()
            .any(|b| b.host == host.name && b.path == path)
        {
            self.status = Some((format!("{path} is in the sidebar already"), false));
            cx.notify();
            return;
        }
        let new_host = self.cfg.host(&host.name).is_none();
        let base = Base::new(&host.name, &path);
        if let Some(file) = Prefs::get(cx).config_file() {
            let saved = Config::save_folder(file, new_host.then_some(&host), &base);
            if let Err(e) = saved {
                self.status = Some((format!("{e:#}"), true));
                cx.notify();
                return;
            }
        }
        if new_host {
            self.cfg.hosts.push(host);
        }
        self.cfg.bases.push(base);
        self.refresh(cx).detach();
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

    /// Moves a thread within its list: the sidebar shows the new order at
    /// once, and tmux keeps it.
    fn reorder(&mut self, list: List, from: usize, to: usize, cx: &mut Context<Self>) {
        let Some(sessions) = self.list_mut(&list) else {
            return;
        };
        if from == to || from >= sessions.len() || to >= sessions.len() {
            return;
        }
        let moved = sessions.remove(from);
        sessions.insert(to, moved);
        let names: Vec<String> = sessions.iter().map(|s| s.name.clone()).collect();
        self.changes += 1;
        cx.notify();
        let cfg = self.cfg.clone();
        cx.spawn(async move |this, cx| {
            let host = list.0;
            let saved = cx
                .background_spawn(async move { actions::set_order(&cfg, &host, &names) })
                .await;
            this.update(cx, |this, cx| {
                // Refreshes that started before tmux had the order are stale too.
                this.changes += 1;
                if let Err(e) = saved {
                    this.status = Some((format!("{e:#}"), true));
                }
                this.refresh(cx).detach();
            })
            .ok();
        })
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

    /// Asks before removing a worktree: that deletes its folder on the host
    /// and closes the threads in it.
    fn confirm_remove_worktree(
        &mut self,
        base: usize,
        worktree: String,
        label: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let host = self.cfg.bases[base].host.clone();
        let sessions = self.sessions_in(&host, &worktree);
        let threads = match sessions.len() {
            0 => String::new(),
            1 => " Its thread will be closed.".into(),
            n => format!(" Its {n} threads will be closed."),
        };
        let description = format!("This deletes {worktree} on {host}. Its branch stays.{threads}");
        let this = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _, cx| {
            let (this, worktree, label, sessions) = (
                this.clone(),
                worktree.clone(),
                label.clone(),
                sessions.clone(),
            );
            alert
                .icon(Icon::new(IconName::TriangleAlert).text_color(cx.theme().warning))
                .title(format!("Remove worktree {label}?"))
                .description(description.clone())
                .show_cancel(true)
                .ok_text("Remove")
                .ok_variant(ButtonVariant::Danger)
                .on_ok(move |_, window, cx| {
                    let (worktree, label, sessions) =
                        (worktree.clone(), label.clone(), sessions.clone());
                    this.update(cx, |this, cx| {
                        this.remove_worktree(base, worktree, label, sessions, false, window, cx)
                    })
                    .ok();
                    true
                })
        });
    }

    /// The sessions running in a worktree, by name.
    fn sessions_in(&self, host: &str, worktree: &str) -> Vec<String> {
        self.snap
            .hosts
            .iter()
            .filter(|h| h.name == host)
            .flat_map(|h| h.bases.iter().flat_map(|b| b.worktrees.iter()))
            .filter(|w| w.path == worktree)
            .flat_map(|w| w.sessions.iter().map(|s| s.name.clone()))
            .collect()
    }

    #[allow(clippy::too_many_arguments)]
    fn remove_worktree(
        &mut self,
        base: usize,
        worktree: String,
        label: String,
        sessions: Vec<String>,
        force: bool,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let cfg = self.cfg.clone();
        self.status = Some((format!("removing {label}…"), false));
        cx.notify();
        let window_handle = self.window;
        cx.spawn(async move |this, cx| {
            let (w, s) = (worktree.clone(), sessions.clone());
            let removed = cx
                .background_spawn(
                    async move { actions::remove_worktree(&cfg, base, &w, &s, force) },
                )
                .await;
            window_handle
                .update(cx, |_, window, cx| {
                    this.update(cx, |this, cx| {
                        this.status = None;
                        match removed {
                            Ok(()) => {}
                            // Git keeps a locked or changed worktree unless forced.
                            Err(e) => match (force, actions::why_kept(&e)) {
                                (false, Some(kept)) => this.confirm_force_remove(
                                    base, worktree, label, sessions, kept, window, cx,
                                ),
                                _ => this.status = Some((format!("{e:#}"), true)),
                            },
                        }
                        this.refresh(cx).detach();
                        cx.notify();
                    })
                    .ok();
                })
                .ok();
        })
        .detach();
    }

    #[allow(clippy::too_many_arguments)]
    fn confirm_force_remove(
        &mut self,
        base: usize,
        worktree: String,
        label: String,
        sessions: Vec<String>,
        kept: actions::Kept,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (title, description) = match kept {
            actions::Kept::Changes => (
                format!("{label} has uncommitted changes"),
                format!("Removing it anyway deletes them for good, with {worktree}."),
            ),
            actions::Kept::Locked(reason) => {
                let why = if reason.is_empty() {
                    String::new()
                } else {
                    format!(" by {reason}")
                };
                (
                    format!("{label} is locked"),
                    format!(
                        "Git keeps it{why}. Removing it anyway unlocks and deletes {worktree}, with any uncommitted changes in it."
                    ),
                )
            }
        };
        let this = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _, cx| {
            let (this, worktree, label, sessions) = (
                this.clone(),
                worktree.clone(),
                label.clone(),
                sessions.clone(),
            );
            alert
                .icon(Icon::new(IconName::TriangleAlert).text_color(cx.theme().warning))
                .title(title.clone())
                .description(description.clone())
                .show_cancel(true)
                .ok_text("Remove anyway")
                .ok_variant(ButtonVariant::Danger)
                .on_ok(move |_, window, cx| {
                    let (worktree, label, sessions) =
                        (worktree.clone(), label.clone(), sessions.clone());
                    this.update(cx, |this, cx| {
                        this.remove_worktree(base, worktree, label, sessions, true, window, cx)
                    })
                    .ok();
                    true
                })
        });
    }

    /// Asks before taking a project out of lemen; nothing on the host changes.
    fn confirm_remove_project(
        &mut self,
        project: Base,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let this = cx.entity().downgrade();
        let description = format!(
            "Only lemen forgets it. {} on {}, its worktrees and running threads stay as they are.",
            project.path, project.host
        );
        window.open_alert_dialog(cx, move |alert, _, _| {
            let (this, project) = (this.clone(), project.clone());
            alert
                .title(format!("Remove {} from lemen?", project.name()))
                .description(description.clone())
                .show_cancel(true)
                .ok_text("Remove")
                .on_ok(move |_, _, cx| {
                    this.update(cx, |this, cx| this.remove_project(&project, cx))
                        .ok();
                    true
                })
        });
    }

    /// Takes a project out of the sidebar and the config file.
    fn remove_project(&mut self, project: &Base, cx: &mut Context<Self>) {
        let Some(index) = self
            .cfg
            .bases
            .iter()
            .position(|b| b.host == project.host && b.path == project.path)
        else {
            return;
        };
        if let Some(file) = Prefs::get(cx).config_file()
            && let Err(e) = Config::remove_folder(file, project)
        {
            self.status = Some((format!("{e:#}"), true));
            cx.notify();
            return;
        }
        self.cfg.bases.remove(index);
        // The snapshot points into the config's bases by index: drop the
        // project's view, shift the later ones, and drop snapshots on their
        // way that still use the old numbers.
        for h in &mut self.snap.hosts {
            h.bases.retain(|b| b.index != index);
            for b in &mut h.bases {
                if b.index > index {
                    b.index -= 1;
                }
            }
        }
        self.changes += 1;
        // A name field may be asking about it.
        self.prompt = None;
        self.refresh(cx).detach();
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
        let mut groups: Vec<rows::Group> = vec![];
        for h in &self.snap.hosts {
            if let Some(e) = &h.error {
                groups.push(rows::Group::new(
                    h.name.clone(),
                    rows::Menu::default().child(rows::Entry::Item(Box::new(
                        SidebarMenuItem::new(e.clone())
                            .icon(IconName::TriangleAlert)
                            .disable(true),
                    ))),
                ));
                continue;
            }
            for b in &h.bases {
                let base = &self.cfg.bases[b.index];
                let index = b.index;
                let mut menu = rows::Menu::default();
                // A folder without git has no worktrees to add.
                if !b.plain {
                    menu = menu.child(rows::Entry::Item(Box::new(
                        SidebarMenuItem::new("New worktree")
                            .icon(IconName::Plus)
                            .on_click(ask_on_click(&this, move || Ask::NewWorktree {
                                base: index,
                            })),
                    )));
                }
                if let Some(e) = &b.error {
                    menu = menu.child(rows::Entry::Item(Box::new(
                        SidebarMenuItem::new(e.clone())
                            .icon(IconName::TriangleAlert)
                            .disable(true),
                    )));
                }
                let prefix = format!("{}/", base.name());
                for (position, w) in b.worktrees.iter().enumerate() {
                    // The main checkout and a plain folder are the project itself.
                    let removable = !b.plain && position > 0;
                    menu =
                        menu.child(self.worktree_item(&h.name, index, w, removable, &prefix, cx));
                }
                let project = base.clone();
                let this = this.clone();
                let remove: rows::MenuBuilder = Rc::new(move |menu, _, _| {
                    let (this, project) = (this.clone(), project.clone());
                    menu.item(PopupMenuItem::new("Remove project…").on_click(
                        move |_, window, cx| {
                            this.update(cx, |this, cx| {
                                this.confirm_remove_project(project.clone(), window, cx)
                            })
                            .ok();
                        },
                    ))
                });
                groups.push(
                    rows::Group::new(format!("{} · {}", base.name(), h.name), menu).menu(remove),
                );
            }
            if !h.other.is_empty() {
                let threads = self.threads(&h.name, None, &h.other, "", cx);
                groups.push(rows::Group::new(
                    format!("other · {}", h.name),
                    rows::Menu::default().child(rows::Entry::Threads(threads)),
                ));
            }
        }
        let add = {
            let this = this.clone();
            Button::new("add-folder")
                .icon(IconName::Plus)
                .ghost()
                .xsmall()
                .tooltip("Add folder")
                .on_click(move |_, window, cx| {
                    this.update(cx, |this, cx| this.open_picker(window, cx))
                        .ok();
                })
        };
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
                        .child(div().font_semibold().child("lemen"))
                        .child(h_flex().gap_1().child(add).child(refresh)),
                ),
            )
            .children(groups)
    }

    /// A worktree with its sessions; + offers a Claude thread or a shell.
    fn worktree_item(
        &self,
        host: &str,
        base: usize,
        w: &Worktree,
        removable: bool,
        prefix: &str,
        cx: &mut Context<Self>,
    ) -> rows::Entry {
        let label = worktree_label(w);
        let menu: Option<rows::MenuBuilder> = removable.then(|| {
            let this = cx.entity().downgrade();
            let (worktree, label) = (w.path.clone(), label.clone());
            Rc::new(move |menu: PopupMenu, _: &mut Window, _: &mut App| {
                let (this, worktree, label) = (this.clone(), worktree.clone(), label.clone());
                menu.item(
                    PopupMenuItem::new("Remove worktree…").on_click(move |_, window, cx| {
                        this.update(cx, |this, cx| {
                            this.confirm_remove_worktree(
                                base,
                                worktree.clone(),
                                label.clone(),
                                window,
                                cx,
                            )
                        })
                        .ok();
                    }),
                )
            }) as rows::MenuBuilder
        });
        let this = cx.entity().downgrade();
        let worktree = w.path.clone();
        let (host_name, notes_label) = (host.to_string(), label.clone());
        let add = move |_: &mut Window, _: &mut App| {
            let notes = {
                let (this, host, worktree, label) = (
                    this.clone(),
                    host_name.clone(),
                    worktree.clone(),
                    notes_label.clone(),
                );
                Button::new("notes")
                    .icon(IconName::FileText)
                    .ghost()
                    .xsmall()
                    .tooltip("Notes")
                    .on_click(move |_, window, cx| {
                        this.update(cx, |this, cx| {
                            this.open_notes(&host, &worktree, &label, window, cx)
                        })
                        .ok();
                    })
            };
            let (this, worktree) = (this.clone(), worktree.clone());
            let add = Button::new("add")
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
                });
            h_flex().gap_1().child(notes).child(add)
        };
        rows::Entry::Worktree {
            label: label.into(),
            suffix: Rc::new(move |window, cx| add(window, cx).into_any_element()),
            menu,
            threads: self.threads(host, Some(&w.path), &w.sessions, prefix, cx),
        }
    }

    /// Threads that can be dragged into a new order.
    fn threads(
        &self,
        host: &str,
        worktree: Option<&str>,
        sessions: &[Session],
        prefix: &str,
        cx: &mut Context<Self>,
    ) -> rows::Threads {
        let this = cx.entity().downgrade();
        let list: List = (host.to_string(), worktree.map(str::to_string));
        rows::Threads {
            key: format!("{host}\n{}", worktree.unwrap_or_default()).into(),
            rows: sessions
                .iter()
                .map(|s| self.session_item(host, s, prefix, cx))
                .collect(),
            reorder: Rc::new(move |from, to, _, cx| {
                this.update(cx, |this, cx| this.reorder(list.clone(), from, to, cx))
                    .ok();
            }),
        }
    }

    fn session_item(
        &self,
        host: &str,
        s: &Session,
        prefix: &str,
        cx: &mut Context<Self>,
    ) -> rows::Thread {
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
        rows::Thread {
            icon,
            label: label.into(),
            active,
            on_click: Rc::new(open),
            menu: Rc::new(menu),
            suffix: Rc::new(move |window, cx| suffix(window, cx).into_any_element()),
        }
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
        let this = cx.entity().downgrade();
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
                .on_click(move |_, _, cx| {
                    this.update(cx, |this, cx| {
                        this.notes_open = !this.notes_open;
                        cx.notify();
                    })
                    .ok();
                }),
        )
    }
}

impl Render for Lemen {
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
            .on_key_down(cx.listener(|this, e: &KeyDownEvent, window, cx| {
                if e.keystroke.key == "escape" && this.prompt.is_some() {
                    this.close_prompt(window, cx);
                    cx.stop_propagation();
                }
            }))
            .child(self.sidebar(cx))
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
                                        .w(px(360.0))
                                        .h_full()
                                        .flex_shrink_0()
                                        .border_l_1()
                                        .border_color(cx.theme().border)
                                        .child(self.notes.clone()),
                                )
                            }),
                    ),
            )
            .children(picker)
            .children(prompt)
    }
}

/// The hosts a folder can be added from: lemen's own, then the SSH config's
/// other aliases, then this Mac.
fn host_choices(cfg: &Config) -> Vec<HostChoice> {
    let mut choices: Vec<HostChoice> = cfg
        .hosts
        .iter()
        .map(|host| HostChoice {
            host: host.clone(),
            configured: true,
        })
        .collect();
    let aliases = home()
        .map(|home| browse::ssh_aliases(&home.join(".ssh/config")))
        .unwrap_or_default();
    let known = |name: &str| cfg.hosts.iter().any(|h| h.name == name || h.ssh == name);
    for alias in aliases {
        if !known(&alias) {
            choices.push(HostChoice {
                host: Host::new(&alias, &alias),
                configured: false,
            });
        }
    }
    if !known("local") {
        choices.push(HostChoice {
            host: Host::new("local", "local"),
            configured: false,
        });
    }
    choices
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
/// the branches lemen and `claude -w` make, leaving the name you gave. A
/// detached worktree shows its commit.
fn worktree_label(w: &Worktree) -> String {
    match (&w.branch, &w.head) {
        (Some(branch), _) => branch
            .strip_prefix("worktree-")
            .unwrap_or(branch)
            .to_string(),
        (None, Some(head)) => format!("{} (detached)", &head[..head.len().min(7)]),
        (None, None) => leaf(&w.path).to_string(),
    }
}

fn key(host: &str, s: &Session) -> Key {
    (host.to_string(), s.name.clone())
}

/// A click handler that opens the name field for `ask()`.
fn ask_on_click(
    this: &WeakEntity<Lemen>,
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
    use super::{Ask, HostChoice, Key, Lemen, NotificationEvent};
    use crate::config::{Base, Config};
    use crate::snapshot::{self, Activity, BaseView, HostView, Session, Snapshot, Worktree};
    use crate::test_support::{TmuxServer, wait_until};
    use gpui::VisualTestContext;
    use gpui_kit::component::WindowExt;
    use gpui_kit::{AppContext, Entity, Modifiers, MouseButton, point, px, size};
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
    fn apply(app: &Entity<Lemen>, sessions: &[(&str, Activity)], cx: &mut VisualTestContext) {
        let snap = snapshot(sessions);
        cx.update(|window, cx| app.update(cx, |app, cx| app.set_snapshot(snap, window, cx)));
    }

    fn open(app: &Entity<Lemen>, session: &str, cx: &mut VisualTestContext) {
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
            let view = cx.new(|cx| Lemen::new(server.config(), None, window, cx));
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

    /// Dropping a thread on another moves it to that one's place: in the
    /// sidebar at once, and in tmux for good.
    #[gpui::test]
    fn dragging_a_thread_reorders_its_list(cx: &mut gpui::TestAppContext) {
        let server = TmuxServer::new("drag");
        for name in ["a", "b", "c"] {
            server.tmux(&["new-session", "-d", "-s", name]);
        }
        cx.executor().allow_parking();
        cx.update(gpui_kit::init);
        let mut app = None;
        let (_, cx) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| Lemen::new(server.config(), None, window, cx));
            app = Some(view.clone());
            gpui_kit::base::Root::new(view, window, cx)
        });
        let app = app.unwrap();
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

        let from = cx.debug_bounds("thread-0").expect("a's row").center();
        let to = cx.debug_bounds("thread-2").expect("c's row").center();
        let none = Modifiers::none();
        cx.simulate_mouse_down(from, MouseButton::Left, none);
        cx.simulate_mouse_move(from + point(px(0.0), px(6.0)), MouseButton::Left, none);
        cx.simulate_mouse_move(to, MouseButton::Left, none);
        cx.simulate_mouse_up(to, MouseButton::Left, none);
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
        let (app, cx) = cx.add_window_view(|window, cx| Lemen::new(cfg, None, window, cx));
        cx.run_until_parked();
        let worktree = |path: &str, branch: &str, sessions: &[&str]| Worktree {
            path: path.into(),
            branch: Some(branch.into()),
            head: None,
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
            sessions: vec![],
        };
        let label = |b, h| super::worktree_label(&worktree(b, h));
        assert_eq!(
            label(Some("feature/cleaning-app-react"), Some("abc")),
            "feature/cleaning-app-react"
        );
        assert_eq!(label(Some("worktree-auth"), None), "auth");
        assert_eq!(label(None, Some("0123456789abcdef")), "0123456 (detached)");
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
        let (app, cx) = cx.add_window_view(|window, cx| Lemen::new(cfg, None, window, cx));
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

    /// Wraps the app in Root, as `gpui_kit::open_window` does; dialogs need it.
    fn rooted(
        cfg: Config,
        cx: &mut gpui::TestAppContext,
    ) -> (Entity<Lemen>, &mut VisualTestContext) {
        cx.executor().allow_parking();
        cx.update(gpui_kit::init);
        let mut app = None;
        let (_, cx) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| Lemen::new(cfg, None, window, cx));
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
        let dir = std::env::temp_dir().join(format!("lemen-rm-dirty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("repo")).unwrap();
        let repo = dir.canonicalize().unwrap().join("repo");
        let git = |args: &[&str]| {
            let ok = std::process::Command::new("git")
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args(args)
                .current_dir(&repo)
                .status()
                .unwrap()
                .success();
            assert!(ok, "git {args:?}");
        };
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
            cx.update(|window, cx| {
                app.update(cx, |app, cx| {
                    app.remove_worktree(0, path.clone(), "draft".into(), vec![], force, window, cx)
                })
            });
            cx.run_until_parked();
        };
        remove(false, cx);
        assert!(wt.exists());
        assert!(cx.update(|window, cx| window.has_active_dialog(cx)));
        cx.update(|window, cx| window.close_dialog(cx));

        remove(true, cx);
        assert!(!wt.exists());
        let _ = std::fs::remove_dir_all(dir);
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

    #[gpui::test]
    fn a_session_that_finishes_elsewhere_is_marked_done(cx: &mut gpui::TestAppContext) {
        // No hosts, so the background refresh has nothing to fetch.
        let cfg: Config = toml::from_str("").unwrap();
        cx.update(gpui_kit::init);
        let (app, cx) = cx.add_window_view(|window, cx| Lemen::new(cfg, None, window, cx));
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
            cx.add_window_view(|window, cx| Lemen::new(server.config(), None, window, cx));
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
