//! The spotlight: a card over the window for picking or naming something,
//! like Zed's pickers. A click outside it, or Esc, closes it.

use gpui_kit::component::{ActiveTheme, Icon, IconName, Sizable, h_flex, v_flex};
use gpui_kit::*;

/// Dims the window behind `card`; a click outside the card calls `dismiss`.
pub fn overlay(card: impl IntoElement, dismiss: impl Fn(&mut Window, &mut App) + 'static) -> Div {
    div()
        .absolute()
        .inset_0()
        .occlude()
        .flex()
        .justify_center()
        .pt(px(72.0))
        .bg(gpui_kit::black().opacity(0.2))
        .on_mouse_down(MouseButton::Left, move |_, window, cx| dismiss(window, cx))
        .child(
            div()
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(card),
        )
}

/// The card's frame, for its lines to go in.
pub fn card(cx: &App) -> Div {
    let theme = cx.theme();
    v_flex()
        .w(px(560.0))
        .max_h(px(460.0))
        .rounded_lg()
        .border_1()
        .border_color(theme.border)
        .bg(theme.popover)
        .text_color(theme.popover_foreground)
        .shadow_lg()
        .overflow_hidden()
}

/// The line at the top: what the card is for.
pub fn title(icon: IconName, text: impl Into<SharedString>, cx: &App) -> Div {
    let theme = cx.theme();
    h_flex()
        .px_3()
        .py_2()
        .gap_2()
        .text_sm()
        .border_b_1()
        .border_color(theme.border)
        .child(Icon::new(icon).small().text_color(theme.muted_foreground))
        .child(text.into())
}

/// The line at the bottom: a hint, or what went wrong.
pub fn footer(text: impl Into<SharedString>, error: bool, cx: &App) -> Div {
    let theme = cx.theme();
    div()
        .px_3()
        .py_2()
        .text_xs()
        .text_color(if error {
            theme.danger
        } else {
            theme.muted_foreground
        })
        .border_t_1()
        .border_color(theme.border)
        .child(text.into())
}
