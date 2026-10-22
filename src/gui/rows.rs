//! Sidebar rows lemen draws itself, so threads and worktrees can be
//! dragged into a new order and projects folded:
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
    },
    /// Threads with no worktree row above them.
    Threads(Threads),
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
                    } => {
                        let look = WorktreeLook {
                            color,
                            drag: order
                                .clone()
                                .map(|(key, reorder)| (key, worktrees, reorder)),
                        };
                        worktrees += 1;
                        worktree(id, label, suffix, menu, threads, look, window, cx)
                    }
                    Entry::Threads(threads) => thread_list(threads, window, cx),
                }
            }))
    }
}

/// A project in the sidebar: its name over its rows, with a menu for it on
/// hover (⋯) and on right-click; clicking the name folds the rows away.
/// Stands in for gpui-component's SidebarGroup, which does neither.
#[derive(Clone)]
pub struct Group {
    label: SharedString,
    menu: Option<MenuBuilder>,
    folded: bool,
    toggle: Option<Toggle>,
    rows: Menu,
}

impl Group {
    pub fn new(label: impl Into<SharedString>, rows: Menu) -> Self {
        Group {
            label: label.into(),
            menu: None,
            folded: false,
            toggle: None,
            rows,
        }
    }

    pub fn menu(mut self, menu: MenuBuilder) -> Self {
        self.menu = Some(menu);
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
        let muted = theme.sidebar_foreground.opacity(0.7);
        let label = self.label.clone();
        let name = h_flex()
            .gap_1p5()
            .items_center()
            .when(self.toggle.is_some(), |this| {
                this.child(
                    Icon::new(IconName::ChevronRight)
                        .size_3()
                        .text_color(muted)
                        .when(!self.folded, |this| this.rotate(percentage(90. / 360.))),
                )
            })
            .child(self.label);
        let header = h_flex()
            .id("header")
            .debug_selector(move || format!("project {label}"))
            .group(hover_group.clone())
            .flex_shrink_0()
            .px_2()
            .h_8()
            .justify_between()
            .rounded(theme.radius)
            .text_xs()
            .text_color(muted)
            .child(name)
            .when_some(self.toggle, |this, toggle| {
                this.cursor_pointer()
                    .on_click(move |_, window, cx| toggle(window, cx))
            });
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
                    .context_menu(move |m, window, cx| menu(m, window, cx))
                    .into_any_element()
            }
            None => header.into_any_element(),
        };
        v_flex()
            .id(id.clone())
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

/// How a worktree row shows: its folder's color, and, when it can be
/// dragged, the list's key, its place and what moves it.
struct WorktreeLook {
    color: Option<Hsla>,
    drag: Option<(SharedString, usize, Reorder)>,
}

/// A worktree row, open by default; its caret folds the threads away.
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
    let has_threads = !threads.rows.is_empty();
    let open = window.use_keyed_state(id.clone(), cx, |_, _| true);
    let is_open = has_threads && *open.read(cx);
    let icon = Icon::new(IconName::Folder).when_some(look.color, |i, c| i.text_color(c));
    let header = row(
        "worktree",
        icon,
        label.clone(),
        false,
        suffix(window, cx),
        cx,
    )
    .when(has_threads, |this| {
        this.child(
            Button::new("caret")
                .xsmall()
                .ghost()
                .icon(
                    Icon::new(IconName::ChevronRight)
                        .size_4()
                        .when(is_open, |this| this.rotate(percentage(90. / 360.))),
                )
                .on_click(move |_, _, cx| {
                    cx.stop_propagation();
                    open.update(cx, |open, cx| {
                        *open = !*open;
                        cx.notify();
                    })
                }),
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
    let Threads { key, rows, reorder } = threads;
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
        .into_any_element()
}
