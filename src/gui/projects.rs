//! Projects and their worktrees: adding, folding, coloring, ordering,
//! hiding and removing them.

use super::folder_picker::{FolderPicker, HostChoice, PickerEvent};
use super::settings::Prefs;
use super::{Lemur, move_item, moved_to};
use crate::actions;
use crate::browse;
use crate::config::{Base, Config, Host, home, leaf};
use gpui_kit::component::button::ButtonVariant;
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::{ActiveTheme, Icon, IconName, WindowExt, v_flex};
use gpui_kit::*;
use std::cell::Cell;
use std::rc::Rc;

/// A worktree to remove, and the threads to close first.
#[derive(Clone)]
pub(super) struct Removal {
    base: usize,
    worktree: String,
    label: String,
    pub(super) sessions: Vec<String>,
    /// Its folder goes too; otherwise its files stay, out of git.
    pub(super) delete: bool,
}

impl Lemur {
    pub(super) fn open_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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

    pub(super) fn close_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.picker = None;
        self.focus_terminal(window, cx);
    }

    /// Adds a repository to the sidebar, and to the config file so it stays.
    pub(super) fn add_folder(&mut self, choice: HostChoice, path: String, cx: &mut Context<Self>) {
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
                self.fail(e, cx);
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

    /// Folds a project away in the sidebar, or opens it again.
    pub(super) fn toggle_project(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(base) = self.cfg.bases.get_mut(index) else {
            return;
        };
        base.collapsed = !base.collapsed;
        self.save_look(index, cx);
    }

    /// Folds a worktree's threads away in the sidebar, or opens them again.
    pub(super) fn toggle_worktree(&mut self, index: usize, worktree: &str, cx: &mut Context<Self>) {
        let Some(base) = self.cfg.bases.get_mut(index) else {
            return;
        };
        let folded = base.collapsed_worktrees.len();
        base.collapsed_worktrees.retain(|p| p != worktree);
        if base.collapsed_worktrees.len() == folded {
            base.collapsed_worktrees.push(worktree.to_string());
        }
        self.save_look(index, cx);
    }

    /// Opens a project's Hidden row, or folds it again.
    pub(super) fn toggle_hidden(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(base) = self.cfg.bases.get_mut(index) else {
            return;
        };
        base.hidden_open = !base.hidden_open;
        self.save_look(index, cx);
    }

    /// Colors a worktree's folder icon, or with `None` takes its color off.
    pub(super) fn set_worktree_color(
        &mut self,
        index: usize,
        worktree: &str,
        color: Option<&str>,
        cx: &mut Context<Self>,
    ) {
        let Some(base) = self.cfg.bases.get_mut(index) else {
            return;
        };
        match color {
            Some(color) => base
                .worktree_colors
                .insert(worktree.to_string(), color.to_string()),
            None => base.worktree_colors.remove(worktree),
        };
        self.save_look(index, cx);
    }

    /// Moves a worktree to its project's Hidden section, or with `hidden`
    /// false back out of it, to its place in the order.
    pub(super) fn set_worktree_hidden(
        &mut self,
        index: usize,
        worktree: &str,
        hidden: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(base) = self.cfg.bases.get_mut(index) else {
            return;
        };
        base.hidden_worktrees.retain(|p| p != worktree);
        if hidden {
            base.hidden_worktrees.push(worktree.to_string());
        }
        self.save_look(index, cx);
    }

    /// Moves a worktree within its project, `from` and `to` counting the
    /// worktrees that aren't hidden: the sidebar shows the new order at
    /// once, and the config keeps it.
    pub(super) fn reorder_worktrees(
        &mut self,
        index: usize,
        from: usize,
        to: usize,
        cx: &mut Context<Self>,
    ) {
        let Some(view) = self
            .snap
            .hosts
            .iter_mut()
            .flat_map(|h| h.bases.iter_mut())
            .find(|b| b.index == index)
        else {
            return;
        };
        // Hidden ones keep their places among the rest.
        let hidden = &self.cfg.bases[index].hidden_worktrees;
        let shown: Vec<usize> = (0..view.worktrees.len())
            .filter(|&i| !hidden.contains(&view.worktrees[i].path))
            .collect();
        let (Some(&from), Some(&to)) = (shown.get(from), shown.get(to)) else {
            return;
        };
        if !move_item(&mut view.worktrees, from, to) {
            return;
        }
        self.cfg.bases[index].worktree_order =
            view.worktrees.iter().map(|w| w.path.clone()).collect();
        // A refresh already under way sorts by the old order.
        self.changes += 1;
        self.save_look(index, cx);
    }

    /// Moves the project at `from` in the config's bases to `to`: the
    /// sidebar shows them in that order, and the config file's `[[bases]]`
    /// keep it.
    pub(super) fn reorder_projects(&mut self, from: usize, to: usize, cx: &mut Context<Self>) {
        if !move_item(&mut self.cfg.bases, from, to) {
            return;
        }
        // The snapshot points into the config's bases by index: renumber
        // its views, and drop snapshots on their way that use the old numbers.
        for b in self.snap.hosts.iter_mut().flat_map(|h| h.bases.iter_mut()) {
            b.index = moved_to(b.index, from, to);
        }
        self.changes += 1;
        if let Some(file) = Prefs::get(cx).config_file()
            && let Err(e) = Config::save_project_order(file, &self.cfg.bases)
        {
            self.fail(e, cx);
        }
        cx.notify();
    }

    /// Keeps how a project shows in the sidebar in the config file: its
    /// fold, and its worktrees' order, colors, folds and which are hidden.
    fn save_look(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(file) = Prefs::get(cx).config_file()
            && let Err(e) = Config::save_project_look(file, &self.cfg.bases[index])
        {
            self.fail(e, cx);
        }
        cx.notify();
    }

    /// Asks before taking a project out of lemur; nothing on the host changes.
    pub(super) fn confirm_remove_project(
        &mut self,
        project: Base,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let this = cx.entity().downgrade();
        let description = format!(
            "Only lemur forgets it. {} on {}, its worktrees and running threads stay as they are.",
            project.path, project.host
        );
        window.open_alert_dialog(cx, move |alert, _, _| {
            let (this, project) = (this.clone(), project.clone());
            alert
                .title(format!("Remove {} from lemur?", project.name()))
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
    pub(super) fn remove_project(&mut self, project: &Base, cx: &mut Context<Self>) {
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
            self.fail(e, cx);
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

    /// What removing a worktree takes with it: the threads running there.
    pub(super) fn removal(&self, base: usize, worktree: String, label: String) -> Removal {
        let host = &self.cfg.bases[base].host;
        let sessions = self
            .snap
            .worktrees(host)
            .filter(|w| w.path == worktree)
            .flat_map(|w| w.sessions.iter().map(|s| s.name.clone()))
            .collect();
        Removal {
            base,
            worktree,
            label,
            sessions,
            delete: false,
        }
    }

    /// Asks before removing a worktree: git forgets it, and deleting its
    /// folder on the host is up to you. Threads running in it are named,
    /// and close first. With `kept`, the host refused: it found threads in
    /// it, and this asks to close them, or git kept it, and this asks
    /// whether to force it.
    pub(super) fn confirm_remove_worktree(
        &mut self,
        removal: Removal,
        kept: Option<actions::Kept>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Removal {
            base,
            worktree,
            label,
            sessions,
            delete,
        } = &removal;
        let force = matches!(
            kept,
            Some(actions::Kept::Changes | actions::Kept::Locked(_))
        );
        // The first time asks whether to delete the folder too.
        let mut ask_delete = None;
        let mut close = sessions.clone();
        let (title, description, ok) = match kept {
            None => {
                let host = &self.cfg.bases[*base].host;
                ask_delete = Some(format!("Also delete {worktree} on {host}"));
                removing(label, sessions)
            }
            Some(actions::Kept::Threads(found)) => {
                let asked = removing(label, &found);
                close = found;
                asked
            }
            Some(actions::Kept::Changes) => (
                format!("{label} has uncommitted changes"),
                format!("Removing it anyway deletes them for good, with {worktree}."),
                "Remove anyway",
            ),
            Some(actions::Kept::Locked(reason)) => {
                let why = if reason.is_empty() {
                    String::new()
                } else {
                    format!(" by {reason}")
                };
                let then = if *delete {
                    format!("unlocks and deletes {worktree}, with any uncommitted changes in it")
                } else {
                    format!("unlocks it; its files stay in {worktree}")
                };
                (
                    format!("{label} is locked"),
                    format!("Git keeps it{why}. Removing it anyway {then}."),
                    "Remove anyway",
                )
            }
        };
        let delete = Rc::new(Cell::new(*delete));
        let this = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _, cx| {
            let (this, removal, delete, close) =
                (this.clone(), removal.clone(), delete.clone(), close.clone());
            let mut text = v_flex().gap_3().child(description.clone());
            if let Some(label) = &ask_delete {
                let delete = delete.clone();
                text = text.child(
                    div().debug_selector(|| "delete-folder".into()).child(
                        Checkbox::new("delete-folder")
                            .label(label.clone())
                            .checked(delete.get())
                            .on_click(move |checked, window, _| {
                                delete.set(*checked);
                                window.refresh();
                            }),
                    ),
                );
            }
            alert
                .icon(Icon::new(IconName::TriangleAlert).text_color(cx.theme().warning))
                .title(title.clone())
                .description(text)
                .show_cancel(true)
                .ok_text(ok)
                .ok_variant(ButtonVariant::Danger)
                .on_ok(move |_, _, cx| {
                    let removal = Removal {
                        sessions: close.clone(),
                        delete: delete.get(),
                        ..removal.clone()
                    };
                    this.update(cx, |this, cx| this.remove_worktree(removal, force, cx))
                        .ok();
                    true
                })
        });
    }

    pub(super) fn remove_worktree(
        &mut self,
        removal: Removal,
        force: bool,
        cx: &mut Context<Self>,
    ) {
        self.status = Some((format!("removing {}…", removal.label), false));
        cx.notify();
        let cfg = self.cfg.clone();
        let Removal {
            base,
            worktree,
            sessions,
            delete,
            ..
        } = removal.clone();
        self.in_background(
            cx,
            move || actions::remove_worktree(&cfg, base, &worktree, &sessions, force, delete),
            move |this, removed, window, cx| {
                this.status = None;
                match removed {
                    Ok(()) => this.forget_worktree(removal.base, &removal.worktree, cx),
                    // Threads it wasn't to close keep it until you say; git
                    // keeps a locked or changed worktree unless forced.
                    Err(e) => match (force, actions::why_kept(&e)) {
                        (_, Some(kept @ actions::Kept::Threads(_))) | (false, Some(kept)) => {
                            this.confirm_remove_worktree(removal, Some(kept), window, cx)
                        }
                        _ => this.fail(e, cx),
                    },
                }
                this.refresh(cx).detach();
                cx.notify();
            },
        )
        .detach();
    }

    /// Forgets a removed worktree's color, place in the order, that it was
    /// hidden, and its fold, so a new one in its folder starts open.
    fn forget_worktree(&mut self, index: usize, worktree: &str, cx: &mut Context<Self>) {
        let Some(base) = self.cfg.bases.get_mut(index) else {
            return;
        };
        let mut changed = base.worktree_colors.remove(worktree).is_some();
        let lists = [
            &mut base.worktree_order,
            &mut base.hidden_worktrees,
            &mut base.collapsed_worktrees,
        ];
        for list in lists {
            let had = list.len();
            list.retain(|p| p != worktree);
            changed |= list.len() != had;
        }
        if changed {
            self.save_look(index, cx);
        }
    }
}

/// What removing worktree `label` asks: its title, its description and its
/// button. Threads running in it, `close`, are named; they close first.
fn removing(label: &str, close: &[String]) -> (String, String, &'static str) {
    let names: Vec<&str> = close.iter().map(|s| actions::label(leaf(s))).collect();
    let title = match names.len() {
        0 => {
            return (
                format!("Remove worktree {label}?"),
                "Git forgets it, and its branch stays.".into(),
                "Remove",
            );
        }
        1 => format!("A thread is running in {label}"),
        n => format!("{n} threads are running in {label}"),
    };
    (
        title,
        format!(
            "Lemur closes {} first; then git forgets the worktree, and its branch stays.",
            names.join(", ")
        ),
        "Close and remove",
    )
}

/// The hosts a folder can be added from: lemur's own, then the SSH config's
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
