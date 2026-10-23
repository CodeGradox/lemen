//! The notes panel: one worktree's notes, always editable, saved as you
//! type. Markdown is highlighted, and ⌘-click opens a URL in them.

use super::links::url_at;
use crate::notes;
use anyhow::Result;
use gpui_kit::component::input::{DefinitionProvider, Editor, EditorState, InputEvent};
use gpui_kit::component::{ActiveTheme, Icon, IconName, Rope, RopeExt, StyledExt, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use lsp_types::{LocationLink, Range};
use std::path::PathBuf;
use std::rc::Rc;

pub struct NotesPanel {
    /// Where notes are saved; tests have none.
    root: Option<PathBuf>,
    shown: Option<Shown>,
    editor: Entity<EditorState>,
    error: Option<String>,
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
            let mut editor = EditorState::new(window, cx)
                .language("markdown")
                .line_number(false)
                .indent_guides(false)
                .folding(false)
                .soft_wrap(true)
                // The editor breaks its placeholder at newlines only, so it
                // comes in lines that fit the narrowest notes panel.
                .placeholder("Write here.\nMarkdown works, and\n⌘-click opens a link.");
            editor.lsp_mut().definition_provider = Some(Rc::new(Urls));
            editor
        });
        let events = cx.subscribe_in(&editor, window, |this, _, event: &InputEvent, _, cx| {
            if let InputEvent::Change = event {
                this.save(cx)
            }
        });
        NotesPanel {
            root,
            shown: None,
            editor,
            error: None,
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
        let header = h_flex()
            .h_10()
            .px_3()
            .gap_2()
            .items_center()
            .child(Icon::new(IconName::FileText).text_color(muted))
            .child(div().flex_1().text_sm().font_semibold().child(title));
        let body = if self.shown.is_none() {
            div()
                .p_3()
                .text_sm()
                .text_color(muted)
                .child("Open a thread, or a worktree's notes icon, to see its notes.")
                .into_any_element()
        } else {
            div()
                .flex_1()
                .min_h_0()
                .px_1()
                .debug_selector(|| "notes-editor".into())
                .child(Editor::new(&self.editor).h_full().bordered(false))
                .into_any_element()
        };
        v_flex().size_full().child(header).child(body).when_some(
            self.error.clone(),
            |this, error| {
                this.child(
                    div()
                        .px_3()
                        .py_2()
                        .text_xs()
                        .text_color(danger)
                        .child(error),
                )
            },
        )
    }
}

/// The URL under the pointer, for the editor's ⌘-hover underline and
/// ⌘-click, which opens web links only.
struct Urls;

impl DefinitionProvider for Urls {
    fn definitions(
        &self,
        text: &Rope,
        offset: usize,
        _: &mut Window,
        _: &mut App,
    ) -> Task<Result<Vec<LocationLink>>> {
        Task::ready(Ok(url_link(text, offset).into_iter().collect()))
    }
}

/// The URL written out in `text` at byte `offset`, as a link from its own
/// range to itself.
fn url_link(text: &Rope, offset: usize) -> Option<LocationLink> {
    let row = text.offset_to_point(offset).row;
    let start = text.line_start_offset(row);
    let line = text.slice_line(row).to_string();
    let at = line.get(..offset.checked_sub(start)?)?.chars().count();
    let chars: Vec<char> = line.chars().collect();
    let (first, end) = url_at(&chars, at)?;
    let byte = |i: usize| start + chars[..i].iter().map(|c| c.len_utf8()).sum::<usize>();
    let range = Range::new(
        text.offset_to_position(byte(first)),
        text.offset_to_position(byte(end)),
    );
    let url: String = chars[first..end].iter().collect();
    Some(LocationLink {
        origin_selection_range: Some(range),
        target_uri: url.parse().ok()?,
        target_range: range,
        target_selection_range: range,
    })
}

#[cfg(test)]
mod tests {
    // Not `super::*`: GPUI's `test` macro would shadow the built-in one.
    use super::{NotesPanel, url_link};
    use crate::notes;
    use gpui::VisualTestContext;
    use gpui_kit::component::Rope;
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

    /// Typing goes straight into the notes and is saved; each worktree
    /// keeps its own.
    #[gpui::test]
    fn typing_is_saved_and_each_worktree_keeps_its_own(cx: &mut gpui::TestAppContext) {
        let root = root("notes-edit");
        notes::save(&notes::file(&root, "agent", "/srv/b"), "b's notes").unwrap();
        let (panel, cx) = panel(&root, cx);
        show(&panel, "/srv/a", cx);
        assert_eq!(panel.read_with(cx, |p, cx| p.text(cx)), "");

        let editor = cx.debug_bounds("notes-editor").expect("the editor");
        cx.simulate_click(editor.center(), Modifiers::none());
        cx.simulate_input("See https://example.com");
        let saved = notes::load(&notes::file(&root, "agent", "/srv/a")).unwrap();
        assert_eq!(saved, "See https://example.com");

        show(&panel, "/srv/b", cx);
        assert_eq!(panel.read_with(cx, |p, cx| p.text(cx)), "b's notes");
        // Showing them didn't write them anywhere else.
        let a = notes::load(&notes::file(&root, "agent", "/srv/a")).unwrap();
        assert_eq!(a, "See https://example.com");
        show(&panel, "/srv/a", cx);
        assert_eq!(
            panel.read_with(cx, |p, cx| p.text(cx)),
            "See https://example.com"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_url_in_the_notes_links_to_itself() {
        let text = Rope::from("first line\nsee https://example.com/a?b=c, then");
        let at = text.to_string().find("example").unwrap();
        let link = url_link(&text, at).expect("on the URL");
        assert_eq!(link.target_uri.as_str(), "https://example.com/a?b=c");
        let range = link.origin_selection_range.unwrap();
        assert_eq!((range.start.line, range.start.character), (1, 4));
        assert_eq!((range.end.line, range.end.character), (1, 29));
        assert!(url_link(&text, 2).is_none(), "on plain words");
    }

    /// Holding ⌘ over a URL and clicking opens it; a plain click puts the
    /// cursor there.
    #[gpui::test]
    fn cmd_click_opens_a_url(cx: &mut gpui::TestAppContext) {
        let root = root("notes-links");
        let text = "https://example.com/docs and more words";
        notes::save(&notes::file(&root, "agent", "/srv/a"), text).unwrap();
        let (panel, cx) = panel(&root, cx);
        show(&panel, "/srv/a", cx);
        let editor = cx.debug_bounds("notes-editor").expect("the editor");
        // In the URL, which starts the first line.
        let at = editor.origin + point(px(40.0), px(16.0));

        cx.simulate_click(at, Modifiers::none());
        cx.run_until_parked();
        assert_eq!(cx.opened_url(), None);
        cx.simulate_mouse_move(at, None, Modifiers::command());
        cx.run_until_parked();
        cx.simulate_click(at, Modifiers::command());
        cx.run_until_parked();
        assert_eq!(cx.opened_url().as_deref(), Some("https://example.com/docs"));
        assert_eq!(panel.read_with(cx, |p, cx| p.text(cx)), text);
        let _ = std::fs::remove_dir_all(root);
    }
}
