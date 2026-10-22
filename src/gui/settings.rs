//! Settings (lemen → Settings…, ⌘,): the theme and the terminal font.
//! Changes apply at once in every window and are saved to the config file.

use crate::config::{Config, FONT_SIZES, Terminal, ThemeChoice};
use gpui_kit::component::setting::{
    NumberFieldOptions, SettingField, SettingGroup, SettingItem, SettingPage, Settings,
};
use gpui_kit::component::{ActiveTheme, IconName, Theme, ThemeMode, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use std::path::PathBuf;
use std::sync::LazyLock;

actions!(settings, [CloseWindow]);

/// Key context of the Settings window.
pub const CONTEXT: &str = "Settings";

/// The settings every window follows. Change them with [`Prefs::update`].
#[derive(Clone, Default)]
pub struct Prefs {
    pub theme: ThemeChoice,
    pub terminal: Terminal,
    /// The config file that keeps changes; tests have none.
    file: Option<PathBuf>,
    /// Why the last change couldn't be saved.
    error: Option<String>,
}

impl Global for Prefs {}

impl Prefs {
    pub fn new(cfg: &Config, file: Option<PathBuf>) -> Self {
        Prefs {
            theme: cfg.theme,
            terminal: cfg.terminal.clone(),
            file,
            error: None,
        }
    }

    /// The config file changes are saved to; tests have none.
    pub fn config_file(&self) -> Option<&std::path::Path> {
        self.file.as_deref()
    }

    /// The current settings; the defaults until the app sets them.
    pub fn get(cx: &App) -> &Prefs {
        static DEFAULT: LazyLock<Prefs> = LazyLock::new(Prefs::default);
        cx.try_global().unwrap_or(&DEFAULT)
    }

    /// Changes the settings: every window follows, and the config file keeps them.
    pub fn update(cx: &mut App, change: impl FnOnce(&mut Prefs)) {
        let mut prefs = Prefs::get(cx).clone();
        let theme = prefs.theme;
        change(&mut prefs);
        prefs.error = prefs.file.as_deref().and_then(|path| {
            Config::save_settings(path, prefs.theme, &prefs.terminal)
                .err()
                .map(|e| format!("{e:#}"))
        });
        cx.set_global(prefs);
        if Prefs::get(cx).theme != theme {
            apply_theme(None, cx);
        }
    }
}

/// Goes light or dark as the theme setting says; `window` when one is at hand.
pub fn apply_theme(window: Option<&mut Window>, cx: &mut App) {
    match Prefs::get(cx).theme {
        ThemeChoice::System => Theme::sync_system_appearance(window, cx),
        ThemeChoice::Light => Theme::change(ThemeMode::Light, window, cx),
        ThemeChoice::Dark => Theme::change(ThemeMode::Dark, window, cx),
    }
}

/// The Settings window while it's open, so ⌘, brings it forward instead of
/// opening another.
struct Open(AnyWindowHandle);

impl Global for Open {}

pub fn open(cx: &mut App) {
    if let Some(&Open(window)) = cx.try_global::<Open>()
        && window
            .update(cx, |_, window, _| window.activate_window())
            .is_ok()
    {
        return;
    }
    let options = WindowOptions {
        titlebar: Some(TitlebarOptions {
            title: Some("Settings".into()),
            ..Default::default()
        }),
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(760.0), px(480.0)),
            cx,
        ))),
        ..Default::default()
    };
    if let Ok((window, _)) = gpui_kit::open_window(options, cx, |window, cx| {
        cx.new(|cx| SettingsView::new(window, cx))
    }) {
        cx.set_global(Open(window));
    }
}

struct SettingsView {
    /// Installed monospaced fonts, as dropdown options.
    fonts: Vec<(SharedString, SharedString)>,
    focus: FocusHandle,
    _prefs: Subscription,
}

impl SettingsView {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        // Focused, so ⌘W closes it before anything is clicked.
        focus.focus(window, cx);
        let fonts = monospace_fonts(cx)
            .into_iter()
            .map(|f| (f.clone(), f))
            .collect();
        SettingsView {
            fonts,
            focus,
            _prefs: cx.observe_global::<Prefs>(|_, cx| cx.notify()),
        }
    }
}

impl Render for SettingsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let themes = ThemeChoice::ALL
            .map(|t| (t.as_str().into(), theme_label(t).into()))
            .to_vec();
        let theme = SettingField::dropdown(
            themes,
            |cx| Prefs::get(cx).theme.as_str().into(),
            |value: SharedString, cx| {
                let theme = ThemeChoice::ALL
                    .into_iter()
                    .find(|t| t.as_str() == value.as_ref())
                    .unwrap_or_default();
                Prefs::update(cx, |p| p.theme = theme);
            },
        )
        .default_value(ThemeChoice::default().as_str());
        let defaults = Terminal::default();
        let font = SettingField::scrollable_dropdown(
            self.fonts.clone(),
            |cx| Prefs::get(cx).terminal.font_family.clone().into(),
            |family: SharedString, cx| {
                Prefs::update(cx, |p| p.terminal.font_family = family.to_string())
            },
        )
        .default_value(defaults.font_family);
        let font_size = SettingField::number_input(
            NumberFieldOptions {
                min: *FONT_SIZES.start(),
                max: *FONT_SIZES.end(),
                step: 1.0,
            },
            |cx| Prefs::get(cx).terminal.font_size,
            |size, cx| Prefs::update(cx, |p| p.terminal.font_size = size),
        )
        .default_value(defaults.font_size);
        let settings = Settings::new("settings").sidebar_width(px(200.0)).pages([
            SettingPage::new("Appearance")
                .icon(IconName::Palette)
                .group(
                    SettingGroup::new().item(
                        SettingItem::new("Theme", theme)
                            .description("System follows macOS's light or dark mode."),
                    ),
                ),
            SettingPage::new("Terminal")
                .icon(IconName::SquareTerminal)
                .group(
                    SettingGroup::new()
                        .item(
                            SettingItem::new("Font", font)
                                .description("Monospaced fonts installed on this Mac."),
                        )
                        .item(SettingItem::new("Font size", font_size).description("In points.")),
                ),
        ]);
        let error = Prefs::get(cx).error.clone();
        let danger = cx.theme().danger;
        v_flex()
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            .on_action(|_: &CloseWindow, window, _| window.remove_window())
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(div().flex_1().min_h_0().child(settings))
            .when_some(error, |this, error| {
                this.child(
                    div()
                        .px_4()
                        .py_2()
                        .text_sm()
                        .text_color(danger)
                        .child(format!("Couldn't save: {error}")),
                )
            })
    }
}

fn theme_label(theme: ThemeChoice) -> &'static str {
    match theme {
        ThemeChoice::System => "System",
        ThemeChoice::Light => "Light",
        ThemeChoice::Dark => "Dark",
    }
}

/// Installed font families whose letters all have the same width.
fn monospace_fonts(cx: &App) -> Vec<SharedString> {
    let text = cx.text_system();
    text.all_font_names()
        .into_iter()
        // A leading dot marks macOS's private system fonts.
        .filter(|name| !name.starts_with('.'))
        .filter(|name| {
            let id = text.resolve_font(&font(name.clone()));
            let width = |c| text.advance(id, px(16.0), c).map(|s| s.width).ok();
            let i = width('i');
            i.is_some() && i == width('m') && i == width('W')
        })
        .map(SharedString::from)
        .collect()
}

#[cfg(test)]
mod tests {
    // Not `super::*`: GPUI's `test` macro would shadow the built-in one.
    use super::{Prefs, open};
    use crate::config::ThemeChoice;
    use gpui_kit::component::ActiveTheme;

    #[gpui::test]
    fn the_theme_setting_switches_light_and_dark(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_kit::init);
        cx.update(|cx| {
            Prefs::update(cx, |p| p.theme = ThemeChoice::Dark);
            assert!(cx.theme().is_dark());
            Prefs::update(cx, |p| p.theme = ThemeChoice::Light);
            assert!(!cx.theme().is_dark());
        });
    }

    /// ⌘, brings an open Settings window forward rather than opening another;
    /// once it's closed, ⌘, opens a new one.
    #[gpui::test]
    fn settings_open_in_one_window(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_kit::init);
        let windows = |cx: &mut gpui::TestAppContext| cx.update(|cx| cx.windows());
        cx.update(open);
        cx.update(open);
        let open_windows = windows(cx);
        assert_eq!(open_windows.len(), 1);

        open_windows[0]
            .update(cx, |_, window, _| window.remove_window())
            .unwrap();
        cx.run_until_parked();
        assert!(windows(cx).is_empty());
        cx.update(open);
        assert_eq!(windows(cx).len(), 1);
    }
}
