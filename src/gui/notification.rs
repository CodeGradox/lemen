//! A popup at the top-right of the screen when a session finishes or needs
//! input while you can't see it, modeled on Zed's agent notifications.

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::{ActiveTheme, Icon, IconName, Sizable, StyledExt, h_flex, v_flex};
use gpui_kit::*;
use std::rc::Rc;

pub enum NotificationEvent {
    Show,
    Dismiss,
}

pub struct Notification {
    title: SharedString,
    caption: SharedString,
    icon: IconName,
}

impl EventEmitter<NotificationEvent> for Notification {}

impl Notification {
    pub fn new(
        title: impl Into<SharedString>,
        caption: impl Into<SharedString>,
        icon: IconName,
    ) -> Self {
        Notification {
            title: title.into(),
            caption: caption.into(),
            icon,
        }
    }

    /// A popup that doesn't take focus, at the top-right of `screen`;
    /// `slot` stacks it below popups that are already showing.
    pub fn window_options(screen: Rc<dyn PlatformDisplay>, slot: usize) -> WindowOptions {
        let size = size(px(420.0), px(72.0));
        let below_menu_bar = px(48.0) + (size.height + px(8.0)) * slot as f32;
        let origin = screen.bounds().top_right() - point(size.width + px(16.0), -below_menu_bar);
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds { origin, size })),
            titlebar: None,
            focus: false,
            show: true,
            kind: WindowKind::PopUp,
            is_movable: false,
            display_id: Some(screen.id()),
            window_background: WindowBackgroundAppearance::Transparent,
            window_decorations: Some(WindowDecorations::Client),
            ..Default::default()
        }
    }
}

impl Render for Notification {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        h_flex()
            .id("notification")
            .size_full()
            .p_3()
            .gap_4()
            .justify_between()
            .bg(theme.popover)
            .text_color(theme.popover_foreground)
            .border_1()
            .border_color(theme.border)
            .rounded_xl()
            .child(
                h_flex()
                    .items_start()
                    .gap_2()
                    .flex_1()
                    .min_w_0()
                    .child(
                        Icon::new(self.icon.clone())
                            .small()
                            .text_color(theme.muted_foreground),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .text_sm()
                                    .font_medium()
                                    .truncate()
                                    .child(self.title.clone()),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .truncate()
                                    .child(self.caption.clone()),
                            ),
                    ),
            )
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        Button::new("show")
                            .primary()
                            .xsmall()
                            .label("Show")
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(NotificationEvent::Show))),
                    )
                    .child(
                        Button::new("dismiss").xsmall().label("Dismiss").on_click(
                            cx.listener(|_, _, _, cx| cx.emit(NotificationEvent::Dismiss)),
                        ),
                    ),
            )
    }
}
