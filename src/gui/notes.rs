//! The notes panel: one worktree's notes, shown as Markdown with clickable
//! links. The Edit button edits them; every change is saved at once.

use crate::notes;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{InputEvent, Textarea, TextareaState};
use gpui_kit::component::text::TextView;
use gpui_kit::component::{ActiveTheme, Icon, IconName, Sizable, StyledExt, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use std::path::PathBuf;

/// Shared by the rendered notes and the editor, so lines don't jump when
/// switching between them.
const LINE_HEIGHT: f32 = 20.0;

pub struct NotesPanel {
    /// Where notes are saved; tests have none.
    root: Option<PathBuf>,
    shown: Option<Shown>,
    editor: Entity<TextareaState>,
    editing: bool,
    error: Option<String>,
    focus: FocusHandle,
    _events: Subscription,
}

struct Shown {
    host: String,
    worktree: String,
    label: String,
    /// None when the notes couldn't be read: saving would overwrite them.
    file: Option<PathBuf>,
}

impl NotesPanel {
    pub fn new(root: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let editor = cx.new(|cx| {
            TextareaState::new(window, cx).placeholder("Markdown; pasted links become clickable")
        });
        // Editing ends with Done or Esc only: focus also leaves for a click
        // elsewhere or the emoji picker, and the editor should stay.
        let events = cx.subscribe_in(&editor, window, |this, _, event: &InputEvent, _, cx| {
            if let InputEvent::Change = event {
                this.save(cx)
            }
        });
        NotesPanel {
            root,
            shown: None,
            editor,
            editing: false,
            error: None,
            focus: cx.focus_handle(),
            _events: events,
        }
    }

    /// Shows a worktree's notes; `label` names it in the header.
    pub fn show(
        &mut self,
        host: &str,
        worktree: &str,
        label: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(shown) = &mut self.shown
            && shown.host == host
            && shown.worktree == worktree
        {
            shown.label = label.to_string();
            cx.notify();
            return;
        }
        let file = self.root.as_ref().map(|r| notes::file(r, host, worktree));
        let loaded = file.as_deref().map(notes::load).transpose();
        let (text, file) = match loaded {
            Ok(text) => {
                self.error = None;
                (text.unwrap_or_default(), file)
            }
            Err(e) => {
                self.error = Some(format!("{e:#}"));
                (String::new(), None)
            }
        };
        self.editor
            .update(cx, |editor, cx| editor.set_value(text, window, cx));
        self.editing = false;
        self.shown = Some(Shown {
            host: host.to_string(),
            worktree: worktree.to_string(),
            label: label.to_string(),
            file,
        });
        cx.notify();
    }

    #[cfg(test)]
    pub fn text(&self, cx: &App) -> String {
        self.editor.read(cx).value().to_string()
    }

    #[cfg(test)]
    pub fn worktree(&self) -> Option<&str> {
        self.shown.as_ref().map(|s| s.worktree.as_str())
    }

    #[cfg(test)]
    pub fn editing(&self) -> bool {
        self.editing
    }

    fn edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.editing = true;
        self.editor
            .update(cx, |editor, cx| editor.focus(window, cx));
        cx.notify();
    }

    fn done(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.editing = false;
        self.focus.focus(window, cx);
        cx.notify();
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        let Some(file) = self.shown.as_ref().and_then(|s| s.file.as_deref()) else {
            return;
        };
        let text = self.editor.read(cx).value();
        self.error = notes::save(file, &text).err().map(|e| format!("{e:#}"));
        cx.notify();
    }
}

impl Render for NotesPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (muted, danger) = (theme.muted_foreground, theme.danger);
        let title = match &self.shown {
            Some(shown) => format!("Notes · {}", shown.label),
            None => "Notes".to_string(),
        };
        let toggle = if self.editing {
            Button::new("done")
                .label("Done")
                .ghost()
                .xsmall()
                .on_click(cx.listener(|this, _, window, cx| this.done(window, cx)))
        } else {
            Button::new("edit")
                .label("Edit")
                .ghost()
                .xsmall()
                .debug_selector(|| "notes-edit".into())
                .on_click(cx.listener(|this, _, window, cx| this.edit(window, cx)))
        };
        let header = h_flex()
            .h_10()
            .px_3()
            .gap_2()
            .items_center()
            .child(Icon::new(IconName::FileText).text_color(muted))
            .child(div().flex_1().text_sm().font_semibold().child(title))
            .when(self.shown.is_some(), |this| this.child(toggle));
        let text = self.editor.read(cx).value();
        let body = if self.shown.is_none() {
            div()
                .p_3()
                .text_sm()
                .text_color(muted)
                .child("Open a thread, or a worktree's notes icon, to see its notes.")
                .into_any_element()
        } else if self.editing {
            div()
                .flex_1()
                .min_h_0()
                .px_1()
                .line_height(px(LINE_HEIGHT))
                .child(Textarea::new(&self.editor).h_full().bordered(false))
                .into_any_element()
        } else {
            let content = if text.trim().is_empty() {
                div()
                    .text_sm()
                    .text_color(muted)
                    .child("No notes yet. Click Edit to write; Markdown works, and pasted links become clickable.")
                    .into_any_element()
            } else {
                // Opens links itself.
                TextView::markdown("notes", text)
                    .selectable(true)
                    .into_any_element()
            };
            div()
                .id("notes-view")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                // Text as the editor shows it: its size, line height, and
                // where it starts (the 4 px margin plus the editor's 10 by
                // 8 px padding).
                .text_sm()
                .line_height(px(LINE_HEIGHT))
                .px(px(14.0))
                .py_2()
                .cursor_text()
                .debug_selector(|| "notes-view".into())
                .child(content)
                .into_any_element()
        };
        v_flex()
            .track_focus(&self.focus)
            .size_full()
            .on_key_down(cx.listener(|this, e: &KeyDownEvent, window, cx| {
                if this.editing && e.keystroke.key == "escape" {
                    this.done(window, cx);
                    cx.stop_propagation();
                }
            }))
            .child(header)
            .child(body)
            .when_some(self.error.clone(), |this, error| {
                this.child(
                    div()
                        .px_3()
                        .py_2()
                        .text_xs()
                        .text_color(danger)
                        .child(error),
                )
            })
    }
}

#[cfg(test)]
mod tests {
    // Not `super::*`: GPUI's `test` macro would shadow the built-in one.
    use super::NotesPanel;
    use crate::notes;
    use gpui::VisualTestContext;
    use gpui_kit::{AppContext, Entity, Modifiers, point, px, size};
    use std::path::{Path, PathBuf};

    fn root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("lemur-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        root
    }

    fn panel<'a>(
        root: &Path,
        cx: &'a mut gpui::TestAppContext,
    ) -> (Entity<NotesPanel>, &'a mut VisualTestContext) {
        cx.update(gpui_kit::init);
        let mut panel = None;
        let (_, cx) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| NotesPanel::new(Some(root.to_path_buf()), window, cx));
            panel = Some(view.clone());
            gpui_kit::base::Root::new(view, window, cx)
        });
        cx.simulate_resize(size(px(420.0), px(500.0)));
        (panel.unwrap(), cx)
    }

    fn show(panel: &Entity<NotesPanel>, worktree: &str, cx: &mut VisualTestContext) {
        cx.update(|window, cx| {
            panel.update(cx, |p, cx| p.show("agent", worktree, "wt", window, cx))
        });
        cx.run_until_parked();
    }

    #[gpui::test]
    fn edits_are_saved_and_each_worktree_keeps_its_own(cx: &mut gpui::TestAppContext) {
        let root = root("notes-edit");
        notes::save(&notes::file(&root, "agent", "/srv/b"), "b's notes").unwrap();
        let (panel, cx) = panel(&root, cx);
        show(&panel, "/srv/a", cx);
        assert_eq!(panel.read_with(cx, |p, cx| p.text(cx)), "");

        cx.update(|window, cx| panel.update(cx, |p, cx| p.edit(window, cx)));
        cx.run_until_parked();
        cx.simulate_input("See https://example.com");
        let saved = notes::load(&notes::file(&root, "agent", "/srv/a")).unwrap();
        assert_eq!(saved, "See https://example.com");

        // Focus leaving (a click elsewhere, the emoji picker) keeps editing;
        // Esc ends it.
        // Focus events need an active window; the test platform starts it inactive.
        cx.update(|window, _| window.activate_window());
        cx.run_until_parked();
        cx.update(|window, cx| panel.update(cx, |p, cx| p.focus.focus(window, cx)));
        cx.run_until_parked();
        assert!(panel.read_with(cx, |p, _| p.editing()));
        cx.update(|window, cx| panel.update(cx, |p, cx| p.edit(window, cx)));
        cx.simulate_keystrokes("escape");
        assert!(!panel.read_with(cx, |p, _| p.editing()));

        show(&panel, "/srv/b", cx);
        assert_eq!(panel.read_with(cx, |p, cx| p.text(cx)), "b's notes");
        show(&panel, "/srv/a", cx);
        assert_eq!(
            panel.read_with(cx, |p, cx| p.text(cx)),
            "See https://example.com"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// A click on a link opens it; other clicks on the text leave it to be
    /// selected. Only the Edit button edits.
    #[gpui::test]
    fn links_open_and_only_the_edit_button_edits(cx: &mut gpui::TestAppContext) {
        let root = root("notes-links");
        let text = "[docs](https://example.com/docs)\n\nplain words that are not a link";
        notes::save(&notes::file(&root, "agent", "/srv/a"), text).unwrap();
        let (panel, cx) = panel(&root, cx);
        show(&panel, "/srv/a", cx);
        let view = cx.debug_bounds("notes-view").expect("rendered notes");

        // "docs" starts the first line, inside the 12 px padding.
        cx.simulate_click(view.origin + point(px(20.0), px(20.0)), Modifiers::none());
        cx.run_until_parked();
        assert_eq!(cx.opened_url().as_deref(), Some("https://example.com/docs"));
        assert!(
            !panel.read_with(cx, |p, _| p.editing()),
            "a link isn't a click to edit"
        );

        // On the plain words, and below the text.
        for at in [point(px(40.0), px(60.0)), point(px(200.0), px(300.0))] {
            cx.simulate_click(view.origin + at, Modifiers::none());
            cx.run_until_parked();
            assert!(
                !panel.read_with(cx, |p, _| p.editing()),
                "clicked at {at:?}"
            );
        }

        let edit = cx.debug_bounds("notes-edit").expect("Edit button").center();
        cx.simulate_click(edit, Modifiers::none());
        cx.run_until_parked();
        assert!(panel.read_with(cx, |p, _| p.editing()));
        let _ = std::fs::remove_dir_all(root);
    }
}
