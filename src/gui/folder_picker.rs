//! Adding a folder, as in Zed's remote project picker: pick a host
//! (lemur's own, an SSH alias, or this Mac), then type or pick a path while
//! the folders on that host are suggested.

use super::spotlight;
use crate::browse::{self, Folder};
use crate::config::Host;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme, Icon, IconName, Sizable, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use std::collections::HashMap;
use std::time::Duration;

actions!(folder_picker, [Up, Down, Complete, Dismiss]);

/// Key context of the picker.
pub const CONTEXT: &str = "FolderPicker";

/// The picker's keys. Its path field goes first and passes on the keys it
/// has no use for.
pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("up", Up, Some(CONTEXT)),
        KeyBinding::new("down", Down, Some(CONTEXT)),
        KeyBinding::new("tab", Complete, Some(CONTEXT)),
        KeyBinding::new("escape", Dismiss, Some(CONTEXT)),
    ]);
}

/// Where a path starts: the home folder.
const HOME: &str = "~/";

/// A host to pick, and whether the config has it yet.
#[derive(Clone, Debug, PartialEq)]
pub struct HostChoice {
    pub host: Host,
    pub configured: bool,
}

#[derive(Debug, PartialEq)]
pub enum PickerEvent {
    /// Add the folder at `path` on `host`.
    Add {
        host: HostChoice,
        path: String,
    },
    Dismiss,
}

impl EventEmitter<PickerEvent> for FolderPicker {}

enum Stage {
    Host,
    Path(HostChoice),
}

/// What a row does when picked.
enum Row {
    Host(HostChoice),
    /// Adds the path as typed.
    Add(String),
    Folder(Folder),
}

pub struct FolderPicker {
    hosts: Vec<HostChoice>,
    stage: Stage,
    input: Entity<InputState>,
    selected: usize,
    scroll: ScrollHandle,
    /// Each listed folder of the picked host, by the path typed for it.
    listings: HashMap<String, Result<Vec<Folder>, String>>,
    listing: Option<Task<()>>,
    /// Finding the folder to add: a git repository's top folder.
    checking: Option<Task<()>>,
    error: Option<String>,
    _events: Subscription,
}

impl FolderPicker {
    pub fn new(hosts: Vec<HostChoice>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Search hosts"));
        let events =
            cx.subscribe_in(
                &input,
                window,
                |this, _, event: &InputEvent, window, cx| match event {
                    InputEvent::Change => this.changed(cx),
                    InputEvent::PressEnter { .. } => this.pick(this.selected, window, cx),
                    _ => {}
                },
            );
        input.update(cx, |i, cx| i.focus(window, cx));
        FolderPicker {
            hosts,
            stage: Stage::Host,
            input,
            selected: 0,
            scroll: ScrollHandle::new(),
            listings: HashMap::new(),
            listing: None,
            checking: None,
            error: None,
            _events: events,
        }
    }

    fn text(&self, cx: &App) -> String {
        self.input.read(cx).value().to_string()
    }

    fn changed(&mut self, cx: &mut Context<Self>) {
        self.selected = 0;
        self.error = None;
        self.list(cx);
        cx.notify();
    }

    /// Lists the folder the typed path is in, unless it's listed already.
    fn list(&mut self, cx: &mut Context<Self>) {
        let Stage::Path(choice) = &self.stage else {
            return;
        };
        let (dir, _) = split(&self.text(cx));
        if self.listings.contains_key(&dir) {
            return;
        }
        let host = choice.host.clone();
        self.listing = Some(cx.spawn(async move |this, cx| {
            // Typing a name doesn't list at every key.
            cx.background_executor()
                .timer(Duration::from_millis(80))
                .await;
            let key = dir.clone();
            let listed = cx
                .background_spawn(async move { browse::list_folders(&host, &dir) })
                .await;
            this.update(cx, |this, cx| {
                this.listings
                    .insert(key, listed.map_err(|e| format!("{e:#}")));
                this.listing = None;
                cx.notify();
            })
            .ok();
        }));
    }

    fn rows(&self, cx: &App) -> Vec<Row> {
        let text = self.text(cx);
        match &self.stage {
            Stage::Host => {
                let query = text.trim().to_lowercase();
                let mut rows: Vec<Row> = self
                    .hosts
                    .iter()
                    .filter(|c| {
                        c.host.name.to_lowercase().contains(&query)
                            || c.host.ssh.to_lowercase().contains(&query)
                    })
                    .cloned()
                    .map(Row::Host)
                    .collect();
                // A host no one knows, like `me@build.example.com`, can be
                // typed in full.
                if rows.is_empty() && !query.is_empty() && !query.contains(char::is_whitespace) {
                    let host = Host::new(text.trim(), text.trim());
                    rows.push(Row::Host(HostChoice {
                        host,
                        configured: false,
                    }));
                }
                rows
            }
            Stage::Path(_) => {
                let (dir, prefix) = split(&text);
                let prefix = prefix.to_lowercase();
                let mut rows = vec![Row::Add(folder_path(&text))];
                if let Some(Ok(folders)) = self.listings.get(&dir) {
                    rows.extend(
                        folders
                            .iter()
                            // Hidden folders only when you start typing one.
                            .filter(|f| !f.name.starts_with('.') || prefix.starts_with('.'))
                            .filter(|f| f.name.to_lowercase().starts_with(&prefix))
                            .cloned()
                            .map(Row::Folder),
                    );
                }
                rows
            }
        }
    }

    fn pick(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = self.rows(cx).into_iter().nth(index) else {
            return;
        };
        match row {
            Row::Host(choice) => {
                self.stage = Stage::Path(choice);
                self.set_text(HOME, window, cx);
            }
            Row::Folder(folder) => self.enter(&folder, window, cx),
            Row::Add(path) => self.add(path, cx),
        }
    }

    /// Goes into a folder: its path, then a slash, so its folders show.
    fn enter(&mut self, folder: &Folder, window: &mut Window, cx: &mut Context<Self>) {
        let (dir, _) = split(&self.text(cx));
        self.set_text(&format!("{dir}{}/", folder.name), window, cx);
    }

    fn set_text(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |i, cx| {
            i.set_placeholder("Path", window, cx);
            i.set_value(text.to_string(), window, cx);
        });
        self.changed(cx);
    }

    /// Adds the folder; one inside a git repository adds the repository's
    /// top folder.
    fn add(&mut self, path: String, cx: &mut Context<Self>) {
        let Stage::Path(choice) = &self.stage else {
            return;
        };
        let choice = choice.clone();
        let host = choice.host.clone();
        self.error = None;
        self.checking = Some(cx.spawn(async move |this, cx| {
            let root = cx
                .background_spawn(async move { browse::folder_to_add(&host, &path) })
                .await;
            this.update(cx, |this, cx| {
                this.checking = None;
                match root {
                    Ok(path) => cx.emit(PickerEvent::Add { host: choice, path }),
                    Err(e) => this.error = Some(format!("{e:#}")),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn select(&mut self, index: usize, cx: &mut Context<Self>) {
        self.selected = index;
        self.scroll.scroll_to_item(index);
        cx.notify();
    }

    fn up(&mut self, _: &Up, _: &mut Window, cx: &mut Context<Self>) {
        let len = self.rows(cx).len().max(1);
        self.select((self.selected + len - 1) % len, cx);
    }

    fn down(&mut self, _: &Down, _: &mut Window, cx: &mut Context<Self>) {
        let len = self.rows(cx).len().max(1);
        self.select((self.selected + 1) % len, cx);
    }

    /// Tab: goes into the selected folder, or picks the selected host.
    fn complete(&mut self, _: &Complete, window: &mut Window, cx: &mut Context<Self>) {
        match self.rows(cx).into_iter().nth(self.selected) {
            Some(Row::Folder(folder)) => self.enter(&folder, window, cx),
            Some(Row::Host(_)) => self.pick(self.selected, window, cx),
            _ => {}
        }
    }

    fn dismiss(&mut self, _: &Dismiss, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(PickerEvent::Dismiss);
    }

    #[cfg(test)]
    pub fn type_text(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.set_text(text, window, cx);
    }

    #[cfg(test)]
    pub fn row_labels(&self, cx: &App) -> Vec<String> {
        self.rows(cx)
            .iter()
            .map(|row| match row {
                Row::Host(c) => c.host.name.clone(),
                Row::Add(path) => format!("add {path}"),
                Row::Folder(f) if f.repo => format!("{} (git)", f.name),
                Row::Folder(f) => f.name.clone(),
            })
            .collect()
    }

    #[cfg(test)]
    pub fn selected(&self) -> usize {
        self.selected
    }

    #[cfg(test)]
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }
}

/// Splits a typed path into the folder it's in and the start of a name:
/// `~/projects/la` is `~/projects/` and `la`. A bare name is in home.
fn split(text: &str) -> (String, String) {
    match text.rsplit_once('/') {
        Some((dir, name)) => (format!("{dir}/"), name.to_string()),
        None if text == "~" => (HOME.into(), String::new()),
        None => (String::new(), text.to_string()),
    }
}

/// The folder a typed path names, without a trailing slash.
fn folder_path(text: &str) -> String {
    let path = text.trim().trim_end_matches('/');
    match path {
        "" if text.starts_with('/') => "/".into(),
        "" => "~".into(),
        path => path.into(),
    }
}

impl Render for FolderPicker {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (muted, accent, accent_text) = (
            theme.muted_foreground,
            theme.accent,
            theme.accent_foreground,
        );
        let rows = self.rows(cx);
        let selected = self.selected.min(rows.len().saturating_sub(1));
        let host_line = match &self.stage {
            Stage::Host => None,
            Stage::Path(choice) => Some((host_icon(&choice.host), host_label(&choice.host))),
        };
        let note = if self.checking.is_some() {
            Some("Checking…".to_string())
        } else if let Stage::Path(_) = &self.stage {
            let (dir, _) = split(&self.text(cx));
            match self.listings.get(&dir) {
                Some(Err(e)) => Some(e.clone()),
                None if self.listing.is_some() => Some("Listing folders…".into()),
                _ => None,
            }
        } else {
            None
        };
        let items = rows.into_iter().enumerate().map(|(ix, row)| {
            let (icon, label, detail) = match row {
                Row::Host(c) => {
                    let detail = if !c.configured && !self.hosts.contains(&c) {
                        Some("connect".to_string())
                    } else {
                        (c.host.ssh != c.host.name && c.host.ssh != "local")
                            .then(|| c.host.ssh.clone())
                    };
                    (host_icon(&c.host), host_label(&c.host), detail)
                }
                Row::Add(path) => (IconName::Plus, format!("Add {path}"), None),
                Row::Folder(f) => (IconName::Folder, f.name, f.repo.then(|| "git".into())),
            };
            h_flex()
                .id(("row", ix))
                .px_2()
                .py_1()
                .gap_2()
                .rounded_md()
                .text_sm()
                .when(ix == selected, |this| {
                    this.bg(accent).text_color(accent_text)
                })
                .on_click(cx.listener(move |this, _, window, cx| this.pick(ix, window, cx)))
                .child(Icon::new(icon).small().text_color(muted))
                .child(div().flex_1().child(label))
                .when_some(detail, |this, detail| {
                    this.child(div().text_xs().text_color(muted).child(detail))
                })
        });
        spotlight::card(cx)
            .key_context(CONTEXT)
            .on_action(cx.listener(Self::up))
            .on_action(cx.listener(Self::down))
            .on_action(cx.listener(Self::complete))
            .on_action(cx.listener(Self::dismiss))
            .when_some(host_line, |this, (icon, host)| {
                this.child(spotlight::title(icon, host, cx))
            })
            .child(
                div()
                    .p_1()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(Input::new(&self.input).appearance(false)),
            )
            .child(
                v_flex()
                    .id("rows")
                    .p_1()
                    .gap_0p5()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .children(items),
            )
            .when_some(self.error.clone().or(note), |this, line| {
                this.child(spotlight::footer(line, self.error.is_some(), cx))
            })
    }
}

fn host_icon(host: &Host) -> IconName {
    if host.ssh == "local" {
        IconName::HardDrive
    } else {
        IconName::Network
    }
}

fn host_label(host: &Host) -> String {
    if host.ssh == "local" {
        "This Mac".into()
    } else {
        host.name.clone()
    }
}

#[cfg(test)]
mod tests {
    // Not `super::*`: GPUI's `test` macro would shadow the built-in one.
    use super::{FolderPicker, HostChoice, PickerEvent, folder_path, split};
    use crate::config::Host;
    use crate::test_support::{TempDir, git_in};
    use gpui::VisualTestContext;
    use gpui_kit::{AppContext, Entity};
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::time::Duration;

    #[test]
    fn typed_paths_split_into_folder_and_name() {
        assert_eq!(split("~/projects/la"), ("~/projects/".into(), "la".into()));
        assert_eq!(split("~/"), ("~/".into(), "".into()));
        assert_eq!(split("~"), ("~/".into(), "".into()));
        assert_eq!(split("/"), ("/".into(), "".into()));
        assert_eq!(split("pro"), ("".into(), "pro".into()));
        assert_eq!(folder_path("~/projects/api/"), "~/projects/api");
        assert_eq!(folder_path("~/"), "~");
        assert_eq!(folder_path("/"), "/");
    }

    fn temp_repos() -> TempDir {
        let dir = TempDir::new("picker");
        for d in ["api/src", "web", "Docs", ".hidden"] {
            std::fs::create_dir_all(dir.join(d)).unwrap();
        }
        git_in(&dir.join("api"), &["init", "-q"]);
        dir
    }

    fn picker(
        cx: &mut gpui::TestAppContext,
    ) -> (
        Entity<FolderPicker>,
        Rc<RefCell<Vec<PickerEvent>>>,
        &mut VisualTestContext,
    ) {
        cx.executor().allow_parking();
        cx.update(gpui_kit::init);
        cx.update(super::bind_keys);
        let hosts = vec![
            HostChoice {
                host: Host::new("agent", "agent"),
                configured: true,
            },
            HostChoice {
                host: Host::new("local", "local"),
                configured: false,
            },
        ];
        let mut picker = None;
        let (_, cx) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| FolderPicker::new(hosts, window, cx));
            picker = Some(view.clone());
            gpui_kit::base::Root::new(view, window, cx)
        });
        let picker = picker.unwrap();
        let events = Rc::new(RefCell::new(vec![]));
        let seen = events.clone();
        cx.update(|_, cx| {
            cx.subscribe(&picker, move |_, event: &PickerEvent, _| {
                seen.borrow_mut().push(match event {
                    PickerEvent::Add { host, path } => PickerEvent::Add {
                        host: host.clone(),
                        path: path.clone(),
                    },
                    PickerEvent::Dismiss => PickerEvent::Dismiss,
                })
            })
            .detach()
        });
        cx.run_until_parked();
        (picker, events, cx)
    }

    fn labels(picker: &Entity<FolderPicker>, cx: &mut VisualTestContext) -> Vec<String> {
        picker.read_with(cx, |p, cx| p.row_labels(cx))
    }

    fn type_text(picker: &Entity<FolderPicker>, text: &str, cx: &mut VisualTestContext) {
        cx.update(|window, cx| picker.update(cx, |p, cx| p.type_text(text, window, cx)));
        settle(cx);
    }

    /// Lets the pause before listing pass, and the listing finish.
    fn settle(cx: &mut VisualTestContext) {
        cx.executor().advance_clock(Duration::from_millis(100));
        cx.run_until_parked();
    }

    #[gpui::test]
    fn picking_a_host_then_a_folder_adds_its_repo(cx: &mut gpui::TestAppContext) {
        let dir = temp_repos();
        let base = dir.to_string_lossy().into_owned();
        let (picker, events, cx) = picker(cx);
        assert_eq!(labels(&picker, cx), ["agent", "local"]);
        // Filter to this Mac, and pick it with Enter.
        cx.simulate_input("loc");
        cx.run_until_parked();
        assert_eq!(labels(&picker, cx), ["local"]);
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();

        // Its folders show; hidden ones only when asked for.
        type_text(&picker, &format!("{base}/"), cx);
        assert_eq!(
            labels(&picker, cx),
            [
                format!("add {base}"),
                "api (git)".into(),
                "Docs".into(),
                "web".into()
            ]
        );
        cx.simulate_input("w");
        cx.run_until_parked();
        assert_eq!(labels(&picker, cx), [format!("add {base}/w"), "web".into()]);

        // Down selects web; Tab goes into it.
        cx.simulate_keystrokes("down");
        assert_eq!(picker.read_with(cx, |p, _| p.selected()), 1);
        cx.simulate_keystrokes("tab");
        settle(cx);
        assert_eq!(labels(&picker, cx), [format!("add {base}/web")]);

        // web has no git, and is added as it is.
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        // A folder inside a repository adds the repository.
        type_text(&picker, &format!("{base}/api/src"), cx);
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        let local = HostChoice {
            host: Host::new("local", "local"),
            configured: false,
        };
        assert_eq!(
            *events.borrow(),
            [
                PickerEvent::Add {
                    host: local.clone(),
                    path: format!("{base}/web"),
                },
                PickerEvent::Add {
                    host: local,
                    path: format!("{base}/api"),
                }
            ]
        );
        // A folder that isn't there says so.
        type_text(&picker, &format!("{base}/nope"), cx);
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert!(picker.read_with(cx, |p, _| p.error().is_some()));

        cx.simulate_keystrokes("escape");
        assert_eq!(events.borrow().last(), Some(&PickerEvent::Dismiss));
    }

    #[gpui::test]
    fn a_typed_destination_offers_to_connect(cx: &mut gpui::TestAppContext) {
        let (picker, _, cx) = picker(cx);
        cx.simulate_input("me@build.example.com");
        cx.run_until_parked();
        assert_eq!(labels(&picker, cx), ["me@build.example.com"]);
    }
}
