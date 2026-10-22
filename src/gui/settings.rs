//! Settings (lemen → Settings…, ⌘,): the theme, and the terminal's font
//! and colors. Changes apply at once in every window and are saved to the
//! config file.

use super::terminal::{Palette, SLOTS, Slot, set_color};
use crate::config::{Config, FONT_SIZES, Terminal, ThemeChoice};
use alacritty_terminal::vte::ansi::Rgb;
use gpui_kit::component::color_picker::{ColorPicker, ColorPickerEvent, ColorPickerState};
use gpui_kit::component::setting::{
    NumberFieldOptions, SettingField, SettingGroup, SettingItem, SettingPage, Settings,
};
use gpui_kit::component::{ActiveTheme, IconName, Sizable, Theme, ThemeMode, v_flex};
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
    /// A picker for each terminal color, dark then light.
    pickers: Vec<Picker>,
    focus: FocusHandle,
    _prefs: Subscription,
    _pickers: Vec<Subscription>,
}

struct Picker {
    dark: bool,
    slot: Slot,
    label: &'static str,
    state: Entity<ColorPickerState>,
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
        let terminal = Prefs::get(cx).terminal.clone();
        let (mut pickers, mut subscriptions) = (vec![], vec![]);
        for dark in [true, false] {
            let palette = Palette::themed(dark, &terminal);
            for (slot, label) in SLOTS {
                let color = to_hsla(palette.get(slot));
                let state = cx.new(|cx| ColorPickerState::new(window, cx).default_value(color));
                subscriptions.push(cx.subscribe_in(
                    &state,
                    window,
                    move |_, _, event: &ColorPickerEvent, _, cx| {
                        let ColorPickerEvent::Change(color) = event;
                        let color = color.map(to_rgb);
                        Prefs::update(cx, |p| set_color(&mut p.terminal, dark, slot, color));
                    },
                ));
                pickers.push(Picker {
                    dark,
                    slot,
                    label,
                    state,
                });
            }
        }
        SettingsView {
            fonts,
            pickers,
            focus,
            _prefs: cx.observe_global_in::<Prefs>(window, |this, window, cx| {
                this.sync_pickers(window, cx);
                cx.notify();
            }),
            _pickers: subscriptions,
        }
    }

    /// Shows each terminal color as Settings have it, after a reset or a
    /// change elsewhere.
    fn sync_pickers(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let terminal = Prefs::get(cx).terminal.clone();
        for p in &self.pickers {
            let color = to_hsla(Palette::themed(p.dark, &terminal).get(p.slot));
            if p.state.read(cx).value() != Some(color) {
                p.state
                    .update(cx, |state, cx| state.set_value(color, window, cx));
            }
        }
    }

    /// The colors for a dark or a light app, each with a reset to
    /// lemen's own.
    fn color_group(&self, dark: bool) -> SettingGroup {
        let items = self.pickers.iter().filter(|p| p.dark == dark).map(|p| {
            let (state, slot) = (p.state.clone(), p.slot);
            let field = SettingField::render(move |_, _, _| ColorPicker::new(&state).small())
                .on_reset(
                    move |cx| {
                        Palette::themed(dark, &Prefs::get(cx).terminal).get(slot)
                            != Palette::default_for(dark).get(slot)
                    },
                    move |_, cx| {
                        Prefs::update(cx, |p| set_color(&mut p.terminal, dark, slot, None))
                    },
                );
            SettingItem::new(p.label, field)
        });
        let (title, description) = if dark {
            ("Dark", "While lemen is dark. Starts as One Half Dark.")
        } else {
            ("Light", "While lemen is light. Starts as One Half Light.")
        };
        SettingGroup::new()
            .title(title)
            .description(description)
            .items(items)
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
            SettingPage::new("Appearance").icon(IconName::Sun).group(
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
            SettingPage::new("Terminal colors")
                .icon(IconName::Palette)
                .groups([self.color_group(true), self.color_group(false)]),
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

fn to_hsla(c: Rgb) -> Hsla {
    rgb(u32::from(c.r) << 16 | u32::from(c.g) << 8 | u32::from(c.b)).into()
}

fn to_rgb(c: Hsla) -> Rgb {
    let c: Rgba = c.into();
    let channel = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    Rgb {
        r: channel(c.r),
        g: channel(c.g),
        b: channel(c.b),
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
    use super::{Prefs, SettingsView, open, to_hsla};
    use crate::config::ThemeChoice;
    use crate::gui::terminal::{DARK, Palette, Slot, set_color};
    use gpui_kit::component::ActiveTheme;
    use gpui_kit::hsla;

    /// A color picked in Settings becomes that slot's color and is kept;
    /// a reset puts lemen's own back, in the picker too.
    #[gpui::test]
    fn picked_terminal_colors_are_kept(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_kit::init);
        let (view, cx) = cx.add_window_view(SettingsView::new);
        let background = view.read_with(cx, |v, _| {
            let p = &v.pickers[0];
            assert!(p.dark && p.slot == Slot::Background);
            p.state.clone()
        });
        let red = hsla(0.0, 1.0, 0.5, 1.0);
        cx.update(|window, cx| background.update(cx, |s, cx| s.select_color(red, window, cx)));
        cx.run_until_parked();
        let terminal = cx.update(|_, cx| Prefs::get(cx).terminal.clone());
        assert_eq!(terminal.dark.background.as_deref(), Some("#ff0000"));
        assert_eq!(
            Palette::themed(false, &terminal).background,
            Palette::default_for(false).background
        );

        cx.update(|_, cx| {
            Prefs::update(cx, |p| {
                set_color(&mut p.terminal, true, Slot::Background, None)
            })
        });
        cx.run_until_parked();
        assert_eq!(
            background.read_with(cx, |s, _| s.value()),
            Some(to_hsla(DARK.background))
        );
        assert!(cx.update(|_, cx| Prefs::get(cx).terminal.dark.is_empty()));
    }

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
