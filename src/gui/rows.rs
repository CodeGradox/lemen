//! Sidebar rows lemur draws itself, so threads, worktrees and projects can
//! be dragged into a new order and projects folded:
//! gpui-component's SidebarMenuItem and SidebarGroup can't. They copy its
//! look.

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::menu::{ContextMenuExt, DropdownMenu, PopupMenu};
use gpui_kit::component::sidebar::{SidebarItem, SidebarMenuItem};
use gpui_kit::component::{
    ActiveTheme, Collapsible, Icon, IconName, Sizable, StyledExt, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use std::rc::Rc;

pub type OnClick = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;
pub type MenuBuilder = Rc<dyn Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu>;
pub type Suffix = Rc<dyn Fn(&mut Window, &mut App) -> AnyElement>;
/// Moves the row at the first index to the second.
pub type Reorder = Rc<dyn Fn(usize, usize, &mut Window, &mut App)>;
pub type Toggle = Rc<dyn Fn(&mut Window, &mut App)>;

#[derive(Clone)]
pub struct Thread {
    pub icon: Icon,
    pub label: SharedString,
    pub active: bool,
    pub on_click: OnClick,
    pub menu: MenuBuilder,
    pub suffix: Suffix,
}

/// A worktree's threads, in order. Dropping one on another moves it to
/// that one's place.
#[derive(Clone)]
pub struct Threads {
    /// Tells lists apart: a thread only drops into its own.
    pub key: SharedString,
    pub rows: Vec<Thread>,
    /// Threads whose session is gone, dimmed under the rest.
    pub gone: Vec<Thread>,
    pub reorder: Reorder,
}

#[derive(Clone)]
pub enum Entry {
    /// One of gpui-component's own rows, such as "New worktree".
    Item(Box<SidebarMenuItem>),
    /// A worktree row with its threads under it.
    Worktree {
        label: SharedString,
        suffix: Suffix,
        /// Its right-click menu.
        menu: MenuBuilder,
        /// Its folder icon's color.
        color: Option<Hsla>,
        threads: Threads,
        /// Its threads are folded away; clicking it calls `toggle`.
        folded: bool,
        toggle: Toggle,
    },
    /// Threads with no worktree row above them.
    Threads(Threads),
    /// A project's hidden worktrees, under a Hidden row of their own, shown
    /// while it's `open`; clicking it calls `toggle`.
    Hidden {
        entries: Vec<Entry>,
        open: bool,
        toggle: Toggle,
    },
}

/// A sidebar menu of entries, standing in for gpui-component's SidebarMenu.
#[derive(Clone, Default)]
pub struct Menu {
    entries: Vec<Entry>,
    collapsed: bool,
    /// Lets the worktrees be dragged into a new order: the list's key, and
    /// what moves one.
    worktree_order: Option<(SharedString, Reorder)>,
}

impl Menu {
    pub fn child(mut self, entry: Entry) -> Self {
        self.entries.push(entry);
        self
    }

    pub fn reorder_worktrees(mut self, key: impl Into<SharedString>, reorder: Reorder) -> Self {
        self.worktree_order = Some((key.into(), reorder));
        self
    }
}

impl Collapsible for Menu {
    fn is_collapsed(&self) -> bool {
        self.collapsed
    }

    fn collapsed(mut self, collapsed: bool) -> Self {
        self.collapsed = collapsed;
        self
    }
}

impl SidebarItem for Menu {
    fn render(
        self,
        id: impl Into<ElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> impl IntoElement {
        let id: SharedString = id.into().to_string().into();
        let order = self.worktree_order;
        let mut worktrees = 0;
        v_flex()
            .gap_2()
            .children(self.entries.into_iter().enumerate().map(|(ix, entry)| {
                let id = SharedString::from(format!("{id}-{ix}"));
                match entry {
                    Entry::Item(item) => (*item).render(id, window, cx).into_any_element(),
                    Entry::Worktree {
                        label,
                        suffix,
                        menu,
                        color,
                        threads,
                        folded,
                        toggle,
                    } => {
                        let look = WorktreeLook {
                            color,
                            folded,
                            toggle,
                            drag: order
                                .clone()
                                .map(|(key, reorder)| (key, worktrees, reorder)),
                        };
                        worktrees += 1;
                        worktree(id, label, suffix, menu, threads, look, window, cx)
                    }
                    Entry::Threads(threads) => thread_list(threads, window, cx),
                    Entry::Hidden {
                        entries,
                        open,
                        toggle,
                    } => hidden(id, entries, open, toggle, window, cx),
                }
            }))
    }
}

/// A project in the sidebar: its name on a tinted band over its rows, with
/// a menu for it on hover (⋯) and on right-click; clicking the band folds
/// the rows away. Stands in for gpui-component's SidebarGroup, which does
/// neither.
#[derive(Clone)]
pub struct Group {
    label: SharedString,
    /// The host it's on, after the name and quieter.
    host: Option<SharedString>,
    menu: Option<MenuBuilder>,
    folded: bool,
    toggle: Option<Toggle>,
    /// The list it can be dragged within, its place there, and the move.
    drag: Option<(SharedString, usize, Reorder)>,
    rows: Menu,
}

impl Group {
    pub fn new(label: impl Into<SharedString>, rows: Menu) -> Self {
        Group {
            label: label.into(),
            host: None,
            menu: None,
            folded: false,
            toggle: None,
            drag: None,
            rows,
        }
    }

    pub fn host(mut self, host: impl Into<SharedString>) -> Self {
        self.host = Some(host.into());
        self
    }

    pub fn menu(mut self, menu: MenuBuilder) -> Self {
        self.menu = Some(menu);
        self
    }

    /// Lets its name be dragged onto another group's in the list `key`,
    /// where it's at `index`, to take that one's place.
    pub fn draggable(
        mut self,
        key: impl Into<SharedString>,
        index: usize,
        reorder: Reorder,
    ) -> Self {
        self.drag = Some((key.into(), index, reorder));
        self
    }

    /// Folds the rows away (`folded`); clicking the name calls `toggle`.
    pub fn foldable(mut self, folded: bool, toggle: Toggle) -> Self {
        self.folded = folded;
        self.toggle = Some(toggle);
        self
    }
}

impl Collapsible for Group {
    fn is_collapsed(&self) -> bool {
        self.rows.is_collapsed()
    }

    fn collapsed(mut self, collapsed: bool) -> Self {
        self.rows = self.rows.collapsed(collapsed);
        self
    }
}

impl SidebarItem for Group {
    fn render(
        self,
        id: impl Into<ElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> impl IntoElement {
        let id: SharedString = id.into().to_string().into();
        let theme = cx.theme();
        let hover_group = SharedString::from(format!("{id}-header"));
        let text = theme.sidebar_foreground;
        let label: SharedString = match &self.host {
            Some(host) => format!("{} · {host}", self.label).into(),
            None => self.label.clone(),
        };
        let dragged_label = label.clone();
        let name = h_flex()
            .flex_1()
            .gap_1p5()
            .items_center()
            .child(div().text_sm().font_semibold().child(self.label))
            .when_some(self.host, |this, host| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(text.opacity(0.8))
                        .child(format!("· {host}")),
                )
            });
        // On the right, as a worktree's caret is; a click on it reaches the
        // header, which folds.
        let (foldable, folded) = (self.toggle.is_some(), self.folded);
        let chevron = move || {
            foldable.then(|| {
                Button::new("fold").xsmall().ghost().icon(
                    Icon::new(IconName::ChevronRight)
                        .size_4()
                        .when(!folded, |this| this.rotate(percentage(90. / 360.))),
                )
            })
        };
        let header = h_flex()
            .id("header")
            .debug_selector(move || format!("project {label}"))
            .group(hover_group.clone())
            .flex_shrink_0()
            .px_2()
            .h_8()
            .gap_1()
            .rounded(theme.radius)
            .border_1()
            .border_color(theme.sidebar_border)
            // Lighter than a selected thread's row, so the two don't look alike.
            .bg(theme.sidebar_accent.opacity(0.5))
            .text_color(text)
            .child(name)
            .when_some(self.toggle, |this, toggle| {
                this.cursor_pointer()
                    .on_click(move |_, window, cx| toggle(window, cx))
            });
        let header = match self.drag {
            Some((key, index, reorder)) => {
                draggable(header, key, index, dragged_label, reorder, cx)
            }
            None => header,
        };
        let header = match self.menu {
            Some(menu) => {
                let dropdown = menu.clone();
                header
                    .child(
                        div()
                            .id("menu")
                            .invisible()
                            .group_hover(hover_group, |style| style.visible())
                            // Opening the menu doesn't fold the project.
                            .on_click(|_, _, cx| cx.stop_propagation())
                            .child(
                                Button::new("menu")
                                    .icon(IconName::Ellipsis)
                                    .ghost()
                                    .xsmall()
                                    .dropdown_menu(move |m, window, cx| dropdown(m, window, cx)),
                            ),
                    )
                    .children(chevron())
                    .context_menu(move |m, window, cx| menu(m, window, cx))
                    .into_any_element()
            }
            None => header.children(chevron()).into_any_element(),
        };
        v_flex()
            .id(id.clone())
            .gap_1p5()
            .child(header)
            .when(!self.folded, |this| {
                this.child(self.rows.render(format!("{id}-rows"), window, cx))
            })
    }
}

/// What a dragged thread or worktree carries; it also draws the label
/// under the pointer.
#[derive(Clone)]
struct Dragged {
    list: SharedString,
    index: usize,
    label: SharedString,
}

impl Render for Dragged {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_1()
            .rounded(cx.theme().radius)
            .text_sm()
            .bg(cx.theme().sidebar_accent)
            .text_color(cx.theme().sidebar_accent_foreground)
            .shadow_md()
            .child(self.label.clone())
    }
}

/// A row as SidebarMenuItem draws one: icon, label, suffix.
fn row(
    id: impl Into<ElementId>,
    icon: Icon,
    label: SharedString,
    active: bool,
    suffix: AnyElement,
    cx: &App,
) -> Stateful<Div> {
    let theme = cx.theme();
    let (hover, active_bg, accent_text) = (
        theme.sidebar_accent.opacity(0.8),
        theme.tokens.sidebar_accent,
        theme.sidebar_accent_foreground,
    );
    h_flex()
        .id(id)
        .w_full()
        .h_7()
        .flex_shrink_0()
        .overflow_x_hidden()
        .p_2()
        .gap_x_2()
        .rounded(theme.radius)
        .text_sm()
        .when(!active, |this| {
            this.hover(|this| this.bg(hover).text_color(accent_text))
        })
        .when(active, |this| {
            this.font_medium().bg(active_bg).text_color(accent_text)
        })
        .child(icon)
        .child(
            h_flex()
                .flex_1()
                .gap_x_2()
                .justify_between()
                .overflow_x_hidden()
                .child(h_flex().flex_1().overflow_x_hidden().child(label))
                .child(suffix),
        )
}

/// How a worktree row shows: its folder's color, whether its threads are
/// folded and what folds them, and, when it can be dragged, the list's
/// key, its place and what moves it.
struct WorktreeLook {
    color: Option<Hsla>,
    folded: bool,
    toggle: Toggle,
    drag: Option<(SharedString, usize, Reorder)>,
}

/// A worktree row; clicking it or its caret folds the threads away.
#[allow(clippy::too_many_arguments)]
fn worktree(
    id: SharedString,
    label: SharedString,
    suffix: Suffix,
    menu: MenuBuilder,
    threads: Threads,
    look: WorktreeLook,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let has_threads = !threads.rows.is_empty() || !threads.gone.is_empty();
    let is_open = has_threads && !look.folded;
    let toggle = look.toggle;
    let fold = move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
        cx.stop_propagation();
        toggle(window, cx);
    };
    let icon = Icon::new(IconName::Folder).when_some(look.color, |i, c| i.text_color(c));
    // Its buttons do what they say without folding it.
    let buttons = format!("buttons {label}");
    let suffix = h_flex()
        .id("suffix")
        .debug_selector(move || buttons)
        .on_click(|_, _, cx| cx.stop_propagation())
        .child(suffix(window, cx));
    let header = row(
        "worktree",
        icon,
        label.clone(),
        false,
        suffix.into_any_element(),
        cx,
    )
    .when(has_threads, |this| {
        this.cursor_pointer().on_click(fold.clone()).child(
            Button::new("caret")
                .xsmall()
                .ghost()
                .icon(
                    Icon::new(IconName::ChevronRight)
                        .size_4()
                        .when(is_open, |this| this.rotate(percentage(90. / 360.))),
                )
                .on_click(fold),
        )
    });
    let header = match look.drag {
        Some((key, index, reorder)) => draggable(header, key, index, label, reorder, cx)
            .debug_selector(move || format!("worktree-{index}")),
        None => header,
    };
    let header = header
        .context_menu(move |m, window, cx| menu(m, window, cx))
        .into_any_element();
    let border = cx.theme().sidebar_border;
    div()
        .id(id)
        .w_full()
        .child(header)
        .when(is_open, |this| {
            this.child(
                div()
                    .border_l_1()
                    .border_color(border)
                    .ml_3p5()
                    .pl_2p5()
                    .py_0p5()
                    .child(thread_list(threads, window, cx)),
            )
        })
        .into_any_element()
}

/// The Hidden row at the bottom of a project, with how many it holds, and
/// while `is_open` the hidden worktrees under it; clicking it calls
/// `toggle`.
fn hidden(
    id: SharedString,
    entries: Vec<Entry>,
    is_open: bool,
    toggle: Toggle,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let fold = move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
        cx.stop_propagation();
        toggle(window, cx);
    };
    let count = div()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(entries.len().to_string());
    let header = row(
        "hidden",
        Icon::new(IconName::EyeOff),
        "Hidden".into(),
        false,
        count.into_any_element(),
        cx,
    )
    .debug_selector(|| "hidden".into())
    .cursor_pointer()
    .on_click(fold.clone())
    .child(
        Button::new("caret")
            .xsmall()
            .ghost()
            .icon(
                Icon::new(IconName::ChevronRight)
                    .size_4()
                    .when(is_open, |this| this.rotate(percentage(90. / 360.))),
            )
            .on_click(fold),
    );
    let rows = Menu {
        entries,
        ..Default::default()
    };
    let border = cx.theme().sidebar_border;
    div()
        .id(id.clone())
        .w_full()
        .child(header)
        .when(is_open, |this| {
            this.child(
                div()
                    .border_l_1()
                    .border_color(border)
                    .ml_3p5()
                    .pl_2p5()
                    .py_0p5()
                    .child(rows.render(format!("{id}-rows"), window, cx)),
            )
        })
        .into_any_element()
}

/// Makes a row draggable within the list `key`: dropped on another row of
/// it, it takes that row's place.
fn draggable(
    row: Stateful<Div>,
    key: SharedString,
    index: usize,
    label: SharedString,
    reorder: Reorder,
    cx: &App,
) -> Stateful<Div> {
    let line = cx.theme().drag_border;
    let dragged = Dragged {
        list: key.clone(),
        index,
        label,
    };
    row.on_drag(dragged, |dragged, _, _, cx| cx.new(|_| dragged.clone()))
        .can_drop(move |value, _, _| {
            value
                .downcast_ref::<Dragged>()
                .is_some_and(|d| d.list == key)
        })
        // A line on the side the row will land.
        .drag_over::<Dragged>(move |style, dragged, _, _| {
            if dragged.index > index {
                style.border_t_2().border_color(line)
            } else if dragged.index < index {
                style.border_b_2().border_color(line)
            } else {
                style
            }
        })
        .on_drop(move |dragged: &Dragged, window, cx| {
            if dragged.index != index {
                reorder(dragged.index, index, window, cx);
            }
        })
}

fn thread_list(threads: Threads, window: &mut Window, cx: &mut App) -> AnyElement {
    let Threads {
        key,
        rows,
        gone,
        reorder,
    } = threads;
    let gone: Vec<_> = gone
        .into_iter()
        .enumerate()
        .map(|(ix, t)| {
            let (on_click, menu) = (t.on_click, t.menu);
            let id = format!("gone-{ix}");
            let suffix = (t.suffix)(window, cx);
            row(
                SharedString::from(id.clone()),
                t.icon,
                t.label,
                false,
                suffix,
                cx,
            )
            .debug_selector(move || id)
            .opacity(0.6)
            .on_click(move |event, window, cx| on_click(event, window, cx))
            .context_menu(move |m, window, cx| menu(m, window, cx))
        })
        .collect();
    v_flex()
        .gap_1()
        .children(rows.into_iter().enumerate().map(|(ix, t)| {
            let (on_click, menu) = (t.on_click, t.menu);
            let row = row(
                SharedString::from(format!("thread-{ix}")),
                t.icon,
                t.label.clone(),
                t.active,
                (t.suffix)(window, cx),
                cx,
            )
            .debug_selector(move || format!("thread-{ix}"))
            .on_click(move |event, window, cx| on_click(event, window, cx));
            draggable(row, key.clone(), ix, t.label, reorder.clone(), cx)
                .context_menu(move |m, window, cx| menu(m, window, cx))
        }))
        .children(gone)
        .into_any_element()
}
