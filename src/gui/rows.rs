//! Sidebar rows lemen draws itself, so threads can be dragged into a new
//! order: gpui-component's SidebarMenuItem can't be dragged. They copy its
//! look.

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::menu::{ContextMenuExt, PopupMenu};
use gpui_kit::component::sidebar::{SidebarItem, SidebarMenuItem};
use gpui_kit::component::{
    ActiveTheme, Collapsible, Icon, IconName, Sizable, StyledExt, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use std::rc::Rc;

pub type OnClick = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;
pub type MenuBuilder = Rc<dyn Fn(PopupMenu, &mut Window, &mut App) -> PopupMenu>;
pub type Suffix = Rc<dyn Fn(&mut Window, &mut App) -> AnyElement>;
/// Moves the thread at the first index to the second.
pub type Reorder = Rc<dyn Fn(usize, usize, &mut Window, &mut App)>;

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
}

impl Menu {
    pub fn child(mut self, entry: Entry) -> Self {
        self.entries.push(entry);
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
        v_flex()
            .gap_2()
            .children(self.entries.into_iter().enumerate().map(|(ix, entry)| {
                let id = SharedString::from(format!("{id}-{ix}"));
                match entry {
                    Entry::Item(item) => (*item).render(id, window, cx).into_any_element(),
                    Entry::Worktree {
                        label,
                        suffix,
                        threads,
                    } => worktree(id, label, suffix, threads, window, cx),
                    Entry::Threads(threads) => thread_list(threads, window, cx),
                }
            }))
    }
}

/// What a dragged thread carries; it also draws the label under the pointer.
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

/// A worktree row, open by default; its caret folds the threads away.
fn worktree(
    id: SharedString,
    label: SharedString,
    suffix: Suffix,
    threads: Threads,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let has_threads = !threads.rows.is_empty();
    let open = window.use_keyed_state(id.clone(), cx, |_, _| true);
    let is_open = has_threads && *open.read(cx);
    let header = row(
        "worktree",
        Icon::new(IconName::Folder),
        label,
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

fn thread_list(threads: Threads, window: &mut Window, cx: &mut App) -> AnyElement {
    let Threads { key, rows, reorder } = threads;
    let line = cx.theme().drag_border;
    v_flex()
        .gap_1()
        .children(rows.into_iter().enumerate().map(|(ix, t)| {
            let dragged = Dragged {
                list: key.clone(),
                index: ix,
                label: t.label.clone(),
            };
            let list = key.clone();
            let reorder = reorder.clone();
            let (on_click, menu) = (t.on_click, t.menu);
            row(
                SharedString::from(format!("thread-{ix}")),
                t.icon,
                t.label,
                t.active,
                (t.suffix)(window, cx),
                cx,
            )
            .debug_selector(move || format!("thread-{ix}"))
            .on_click(move |event, window, cx| on_click(event, window, cx))
            .on_drag(dragged, |dragged, _, _, cx| cx.new(|_| dragged.clone()))
            .can_drop(move |value, _, _| {
                value
                    .downcast_ref::<Dragged>()
                    .is_some_and(|d| d.list == list)
            })
            // A line on the side the thread will land.
            .drag_over::<Dragged>(move |style, dragged, _, _| {
                if dragged.index > ix {
                    style.border_t_2().border_color(line)
                } else if dragged.index < ix {
                    style.border_b_2().border_color(line)
                } else {
                    style
                }
            })
            .on_drop(move |dragged: &Dragged, window, cx| {
                if dragged.index != ix {
                    reorder(dragged.index, ix, window, cx);
                }
            })
            .context_menu(move |m, window, cx| menu(m, window, cx))
        }))
        .into_any_element()
}
