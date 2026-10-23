//! The sidebar: each project's worktrees and their threads, as rows to
//! open, fold, drag into order and right-click.

use super::{Ask, Key, Lemur, List, key, rows, worktree_label};
use crate::actions;
use crate::config::{Base, COLORS};
use crate::snapshot::{Activity, Session, Worktree};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::component::sidebar::{Sidebar, SidebarCollapsible, SidebarHeader, SidebarMenuItem};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme, Icon, IconName, Sizable, StyledExt, h_flex};
use gpui_kit::*;
use std::rc::Rc;
use std::time::Duration;

/// The sidebar icon for Claude sessions, drawn in Claude's orange.
const CLAUDE_ICON: &[u8] = include_bytes!("claude.svg");
const CLAUDE_ORANGE: u32 = 0xd97757;

/// The sidebar's width until its edge is dragged.
pub(super) const SIDEBAR_WIDTH: Pixels = px(255.0);

/// Renders `Lemur::sidebar`. As a cached view it's redrawn only when
/// Lemur changes, or for its own animations and hovers; not for every
/// frame of terminal output.
pub(super) struct SidebarView {
    lemur: WeakEntity<Lemur>,
    #[cfg(test)]
    pub(super) renders: usize,
}

impl SidebarView {
    pub(super) fn new(lemur: WeakEntity<Lemur>) -> Self {
        SidebarView {
            lemur,
            #[cfg(test)]
            renders: 0,
        }
    }
}

impl Render for SidebarView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(test)]
        {
            self.renders += 1;
        }
        match self.lemur.upgrade() {
            Some(lemur) => lemur.update(cx, |lemur, cx| lemur.sidebar(cx).into_any_element()),
            None => div().into_any_element(),
        }
    }
}

impl Lemur {
    fn sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let this = cx.entity().downgrade();
        let mut groups: Vec<rows::Group> = vec![];
        // Projects first, in the config's order whatever their host; then
        // each host's threads outside them, or why it couldn't be reached.
        let mut projects: Vec<_> = self
            .snap
            .hosts
            .iter()
            .flat_map(|h| h.bases.iter().map(move |b| (h, b)))
            .collect();
        projects.sort_by_key(|(_, b)| b.index);
        let move_project: rows::Reorder = {
            let this = this.clone();
            Rc::new(move |from, to, _, cx| {
                this.update(cx, |this, cx| this.reorder_projects(from, to, cx))
                    .ok();
            })
        };
        for (h, b) in projects {
            let base = &self.cfg.bases[b.index];
            let index = b.index;
            let reorder: rows::Reorder = {
                let this = this.clone();
                Rc::new(move |from, to, _, cx| {
                    this.update(cx, |this, cx| this.reorder_worktrees(index, from, to, cx))
                        .ok();
                })
            };
            let mut menu = rows::Menu::default()
                .reorder_worktrees(format!("project {}\n{}", base.host, base.path), reorder);
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
            for w in &b.worktrees {
                // The main checkout and a plain folder are the project itself.
                let removable = !w.main;
                menu = menu.child(self.worktree_item(&h.name, index, w, removable, &prefix, cx));
            }
            let toggle: rows::Toggle = {
                let this = this.clone();
                Rc::new(move |_, cx| {
                    this.update(cx, |this, cx| this.toggle_project(index, cx))
                        .ok();
                })
            };
            // Projects move by their place in the config.
            groups.push(
                rows::Group::new(format!("{} · {}", base.name(), h.name), menu)
                    .foldable(base.collapsed, toggle)
                    .draggable("projects", index, move_project.clone())
                    .menu(self.project_menu(index, cx)),
            );
        }
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
            } else if !h.other.is_empty() {
                let threads = self.threads(&h.name, None, &h.other, "", cx);
                groups.push(rows::Group::new(
                    format!("other · {}", h.name),
                    rows::Menu::default().child(rows::Entry::Threads(threads)),
                ));
            }
        }
        // Named as in the File menu.
        let add = Button::new("add-folder")
            .icon(IconName::Plus)
            .ghost()
            .xsmall()
            .tooltip("Add project (⌘O)")
            .on_click(cx.listener(|this, _, window, cx| this.open_picker(window, cx)));
        let refresh = Button::new("refresh")
            .icon(IconName::RefreshCw)
            .ghost()
            .xsmall()
            .tooltip("Refresh")
            .on_click(cx.listener(|this, _, _, cx| this.refresh(cx).detach()));
        // Collapsing hides the cached view whole (see render).
        Sidebar::new("sessions")
            .collapsible(SidebarCollapsible::None)
            .w(self.sidebar_width)
            .header(
                SidebarHeader::new().child(
                    h_flex()
                        .w_full()
                        .justify_between()
                        .child(div().font_semibold().child("lemur"))
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
        // Color for every worktree; removing only for linked ones.
        let menu: rows::MenuBuilder = {
            let this = cx.entity().downgrade();
            let colors = Rc::new(self.color_menu(base, &w.path, cx));
            let (worktree, label) = (w.path.clone(), label.clone());
            Rc::new(
                move |menu: PopupMenu, window: &mut Window, cx: &mut Context<PopupMenu>| {
                    let colors = colors.clone();
                    let menu = menu.submenu("Color", window, cx, move |m, w, cx| colors(m, w, cx));
                    if !removable {
                        return menu;
                    }
                    let (this, worktree, label) = (this.clone(), worktree.clone(), label.clone());
                    menu.separator()
                        .item(PopupMenuItem::new("Remove worktree…").on_click(
                            move |_, window, cx| {
                                this.update(cx, |this, cx| {
                                    let removal =
                                        this.removal(base, worktree.clone(), label.clone());
                                    this.confirm_remove_worktree(removal, None, window, cx)
                                })
                                .ok();
                            },
                        ))
                },
            )
        };
        let color = worktree_color(&self.cfg.bases[base], &w.path);
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
            // The spotlight asks Claude or Shell, as ⌘J does.
            let add = Button::new("add")
                .icon(IconName::Plus)
                .ghost()
                .xsmall()
                .tooltip("New Claude thread or shell here")
                .on_click(ask_on_click(&this, move || Ask::NewThread {
                    base,
                    worktree: worktree.clone(),
                    shell: false,
                }));
            h_flex().gap_1().child(notes).child(add)
        };
        rows::Entry::Worktree {
            label: label.into(),
            suffix: Rc::new(move |window, cx| add(window, cx).into_any_element()),
            menu,
            color,
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
            move |menu: PopupMenu, _: &mut Window, _: &mut Context<PopupMenu>| {
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
            claude_icon()
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

    /// A project's menu: removing it.
    fn project_menu(&self, index: usize, cx: &mut Context<Self>) -> rows::MenuBuilder {
        let this = cx.entity().downgrade();
        let project = self.cfg.bases[index].clone();
        Rc::new(move |menu, _, _| {
            let (this, project) = (this.clone(), project.clone());
            menu.item(
                PopupMenuItem::new("Remove project…").on_click(move |_, window, cx| {
                    this.update(cx, |this, cx| {
                        this.confirm_remove_project(project.clone(), window, cx)
                    })
                    .ok();
                }),
            )
        })
    }

    /// The colors a worktree's folder can take, as a submenu: each with its
    /// swatch, the current one checked, and none.
    fn color_menu(
        &self,
        index: usize,
        worktree: &str,
        cx: &mut Context<Self>,
    ) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
        let this = cx.entity().downgrade();
        let current = self.cfg.bases[index].worktree_colors.get(worktree).cloned();
        let worktree = worktree.to_string();
        move |menu, _, _| {
            let set = |color: Option<&'static str>| {
                let (this, worktree) = (this.clone(), worktree.clone());
                move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
                    this.update(cx, |this, cx| {
                        this.set_worktree_color(index, &worktree, color, cx)
                    })
                    .ok();
                }
            };
            let menu = COLORS.iter().fold(menu, |menu, &(name, hex)| {
                menu.item(
                    PopupMenuItem::element(move |_, _| {
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(div().size_3().rounded_full().bg(rgb(hex)))
                            .child(title_case(name))
                    })
                    .checked(current.as_deref() == Some(name))
                    .on_click(set(Some(name))),
                )
            });
            menu.separator().item(
                PopupMenuItem::new("No color")
                    .checked(current.is_none())
                    .on_click(set(None)),
            )
        }
    }
}

/// A worktree's folder color, from its name in the config.
pub(super) fn worktree_color(base: &Base, worktree: &str) -> Option<Hsla> {
    let name = base.worktree_colors.get(worktree)?;
    let (_, hex) = COLORS.iter().find(|(n, _)| n == name)?;
    Some(rgb(*hex).into())
}

/// `blue` as `Blue`, for menus.
fn title_case(name: &str) -> String {
    let mut chars = name.chars();
    chars
        .next()
        .map(|c| c.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

/// Claude's spark, in Claude's orange.
pub(super) fn claude_icon() -> Icon {
    Icon::empty()
        .data(CLAUDE_ICON)
        .text_color(rgb(CLAUDE_ORANGE))
}

/// A click handler that opens the name field for `ask()`.
fn ask_on_click(
    this: &WeakEntity<Lemur>,
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
