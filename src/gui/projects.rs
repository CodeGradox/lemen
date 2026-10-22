//! Projects and their worktrees: adding, folding, coloring, ordering and
//! removing them.

use super::folder_picker::{FolderPicker, HostChoice, PickerEvent};
use super::settings::Prefs;
use super::{Lemur, move_item};
use crate::actions;
use crate::browse;
use crate::config::{Base, Config, Host, home};
use gpui_kit::component::button::ButtonVariant;
use gpui_kit::component::{ActiveTheme, Icon, IconName, WindowExt};
use gpui_kit::*;

/// A worktree to remove, and the threads that close with it.
#[derive(Clone)]
pub(super) struct Removal {
    base: usize,
    worktree: String,
    label: String,
    sessions: Vec<String>,
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

    /// Moves a worktree within its project: the sidebar shows the new order
    /// at once, and the config keeps it.
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
        if !move_item(&mut view.worktrees, from, to) {
            return;
        }
        self.cfg.bases[index].worktree_order =
            view.worktrees.iter().map(|w| w.path.clone()).collect();
        // A refresh already under way sorts by the old order.
        self.changes += 1;
        self.save_look(index, cx);
    }

    /// Keeps a project's color, fold and worktree order in the config file.
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
        }
    }

    /// Asks before removing a worktree: that deletes its folder on the host
    /// and closes the threads in it. With `kept`, git refused, and this
    /// asks whether to force it.
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
        } = &removal;
        let force = kept.is_some();
        let (title, description, ok) = match kept {
            None => {
                let host = &self.cfg.bases[*base].host;
                let threads = match sessions.len() {
                    0 => String::new(),
                    1 => " Its thread will be closed.".into(),
                    n => format!(" Its {n} threads will be closed."),
                };
                (
                    format!("Remove worktree {label}?"),
                    format!("This deletes {worktree} on {host}. Its branch stays.{threads}"),
                    "Remove",
                )
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
                (
                    format!("{label} is locked"),
                    format!(
                        "Git keeps it{why}. Removing it anyway unlocks and deletes {worktree}, with any uncommitted changes in it."
                    ),
                    "Remove anyway",
                )
            }
        };
        let this = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _, cx| {
            let (this, removal) = (this.clone(), removal.clone());
            alert
                .icon(Icon::new(IconName::TriangleAlert).text_color(cx.theme().warning))
                .title(title.clone())
                .description(description.clone())
                .show_cancel(true)
                .ok_text(ok)
                .ok_variant(ButtonVariant::Danger)
                .on_ok(move |_, _, cx| {
                    let removal = removal.clone();
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
            ..
        } = removal.clone();
        self.in_background(
            cx,
            move || actions::remove_worktree(&cfg, base, &worktree, &sessions, force),
            move |this, removed, window, cx| {
                this.status = None;
                match removed {
                    Ok(()) => this.forget_worktree(removal.base, &removal.worktree, cx),
                    // Git keeps a locked or changed worktree unless forced.
                    Err(e) => match (force, actions::why_kept(&e)) {
                        (false, Some(kept)) => {
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

    /// Forgets a removed worktree's color and place in the order.
    fn forget_worktree(&mut self, index: usize, worktree: &str, cx: &mut Context<Self>) {
        let Some(base) = self.cfg.bases.get_mut(index) else {
            return;
        };
        let had_color = base.worktree_colors.remove(worktree).is_some();
        let placed = base.worktree_order.len();
        base.worktree_order.retain(|p| p != worktree);
        if had_color || base.worktree_order.len() != placed {
            self.save_look(index, cx);
        }
    }
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
