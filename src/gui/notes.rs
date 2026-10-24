//! The notes panel: one worktree's notes, always editable, saved as you
//! type. Markdown is highlighted, ⌘-click opens a URL in them, and a
//! task list's boxes tick.

use super::links::url_at;
use crate::notes;
use anyhow::Result;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{
    DefinitionProvider, Editor, EditorState, Enter, Indent, IndentInline, InputEvent, Paste,
    TextDecoration, TextDecorationCollection,
};
use gpui_kit::component::{
    ActiveTheme, Icon, IconName, Rope, RopeExt, Sizable, StyledExt, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use lsp_types::{LocationLink, Range};
use std::path::PathBuf;
use std::rc::Rc;

actions!(
    notes,
    [Link, Checklist, TickBox, MoveLinesUp, MoveLinesDown]
);

/// The key context of the notes' editor, for their own keys.
pub const CONTEXT: &str = "Notes";

/// The notes' keys besides the editor's: Apple Notes' for a checklist, its
/// boxes and moving items, and ⌘K for a link.
pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-k", Link, Some(CONTEXT)),
        KeyBinding::new("shift-cmd-l", Checklist, Some(CONTEXT)),
        KeyBinding::new("shift-cmd-u", TickBox, Some(CONTEXT)),
        KeyBinding::new("ctrl-cmd-up", MoveLinesUp, Some(CONTEXT)),
        KeyBinding::new("ctrl-cmd-down", MoveLinesDown, Some(CONTEXT)),
    ]);
}

pub struct NotesPanel {
    /// Where notes are saved; tests have none.
    root: Option<PathBuf>,
    shown: Option<Shown>,
    editor: Entity<EditorState>,
    /// The ticked items, faded and struck through.
    ticked: TextDecorationCollection,
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
            editor.lsp_mut().definition_provider = Some(Rc::new(Links));
            // ⌘-clicking a box comes back here, as its link being shown.
            let this = cx.weak_entity();
            editor.lsp_mut().show_document = Some(Rc::new(move |shown, window, cx| {
                let Some(at) = shown.selection.filter(|_| shown.uri.as_str() == TICK) else {
                    return false;
                };
                // The editor is busy following the link, so the tick waits.
                let this = this.clone();
                window.defer(cx, move |window, cx| {
                    let _ = this.update(cx, |editor, cx| {
                        let offset = editor.text().position_to_offset(&at.start);
                        tick(editor, offset, window, cx)
                    });
                });
                true
            }));
            editor
        });
        let ticked = editor.update(cx, |editor, cx| {
            editor.create_decorations_collection(Vec::new(), cx)
        });
        let events = cx.subscribe_in(&editor, window, |this, _, event: &InputEvent, _, cx| {
            if let InputEvent::Change = event {
                this.save(cx);
                this.mark_ticked(cx);
            }
        });
        NotesPanel {
            root,
            shown: None,
            editor,
            ticked,
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
        self.mark_ticked(cx);
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

    /// Fades the ticked items, and strikes through their words.
    fn mark_ticked(&self, cx: &mut App) {
        let fade = HighlightStyle {
            fade_out: Some(0.5),
            ..Default::default()
        };
        let strike = HighlightStyle {
            strikethrough: Some(StrikethroughStyle {
                thickness: px(1.),
                color: None,
            }),
            ..Default::default()
        };
        let text = self.editor.read(cx).value();
        let marks = ticked(&text)
            .into_iter()
            .flat_map(|(line, words)| {
                [
                    TextDecoration::new(line, fade),
                    TextDecoration::new(words, strike),
                ]
            })
            .collect();
        self.ticked.set(marks, cx);
    }

    /// Enter at the end of a list item starts the next one, and on an empty
    /// one ends the list; ⌘-Enter ticks the box on the cursor's line.
    /// The rest, and ⇧-Enter, are the editor's own.
    fn enter(&mut self, enter: &Enter, window: &mut Window, cx: &mut Context<Self>) {
        if enter.shift || !self.editor.focus_handle(cx).is_focused(window) {
            return;
        }
        let done = self.editor.update(cx, |editor, cx| {
            if enter.secondary {
                let cursor = editor.cursor();
                tick(editor, cursor, window, cx)
            } else {
                next_item(editor, window, cx)
            }
        });
        if done {
            cx.stop_propagation();
        }
    }

    /// Bullets the lines the selection touches, or gives them boxes with
    /// `boxed`; see [`bulleted`].
    fn bullet(&mut self, boxed: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.edit(
            |text, selected| Some(bulleted(text, selected, boxed)),
            window,
            cx,
        );
    }

    /// Makes the edit `edit` gives for the notes' text and selection, if
    /// any, as one undo step, and gives the keyboard back to the notes.
    /// False when there was none.
    fn edit(
        &mut self,
        edit: impl FnOnce(&str, std::ops::Range<usize>) -> Option<Edit>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let done = self.editor.update(cx, |editor, cx| {
            let text = editor.value();
            let Some((range, new, selected)) = edit(&text, editor.selected_range()) else {
                return false;
            };
            if text[range.clone()] != new {
                editor.set_selected_range(range, cx);
                editor.replace(new, window, cx);
            }
            editor.set_selected_range(selected, cx);
            true
        });
        self.editor.focus_handle(cx).focus(window, cx);
        done
    }

    /// ⌘K; see [`link`].
    fn link(&mut self, _: &Link, window: &mut Window, cx: &mut Context<Self>) {
        let url = clipboard_url(cx);
        self.edit(
            |text, selected| link(text, selected, url.as_deref()),
            window,
            cx,
        );
    }

    /// Pasting a URL over text on one line makes the text a link to it;
    /// any other paste is the editor's own.
    fn paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        if !self.editor.focus_handle(cx).is_focused(window) {
            return;
        }
        let Some(url) = clipboard_url(cx) else {
            return;
        };
        if self.edit(
            |text, selected| pasted_link(text, selected, &url),
            window,
            cx,
        ) {
            cx.stop_propagation();
        }
    }

    /// Tab on a list item indents the whole item, as ⌘] does, wherever the
    /// cursor is in it; anywhere else, Tab is the editor's own.
    fn indent(&mut self, _: &IndentInline, window: &mut Window, cx: &mut Context<Self>) {
        if !self.editor.focus_handle(cx).is_focused(window) {
            return;
        }
        let editor = self.editor.read(cx);
        let text = editor.text();
        let row = text.offset_to_point(editor.cursor()).row;
        if editor.selected_range().is_empty()
            && list_item(&text.slice_line(row).to_string()).is_some()
        {
            window.dispatch_action(Box::new(Indent), cx);
            cx.stop_propagation();
        }
    }

    /// ⇧⌘U, as ⌘-Enter.
    fn tick_box(&mut self, _: &TickBox, window: &mut Window, cx: &mut Context<Self>) {
        self.editor.update(cx, |editor, cx| {
            let cursor = editor.cursor();
            tick(editor, cursor, window, cx);
        });
    }

    /// ⌃⌘↑ and ⌃⌘↓; see [`moved`].
    fn move_lines(&mut self, up: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.edit(|text, selected| moved(text, selected, up), window, cx);
    }
}

/// The list buttons' icons: Lucide's, which gpui-kit's own bundle leaves
/// out.
const LIST_ICON: &[u8] = include_bytes!("list.svg");
const LIST_TODO_ICON: &[u8] = include_bytes!("list-todo.svg");

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
        let list_button = |id: &'static str, icon, tooltip, boxed| {
            div().debug_selector(move || id.into()).child(
                Button::new(id)
                    .icon(Icon::empty().data(icon))
                    .ghost()
                    .xsmall()
                    .tooltip(tooltip)
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.bullet(boxed, window, cx)),
                    ),
            )
        };
        let header = header.when(self.shown.is_some(), |this| {
            this.child(list_button("bullets", LIST_ICON, "Bullet list", false))
                .child(list_button("boxes", LIST_TODO_ICON, "Checklist", true))
        });
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
                .key_context(CONTEXT)
                .capture_action(cx.listener(Self::enter))
                .capture_action(cx.listener(Self::paste))
                .capture_action(cx.listener(Self::indent))
                .on_action(cx.listener(Self::link))
                .on_action(
                    cx.listener(|this, _: &Checklist, window, cx| this.bullet(true, window, cx)),
                )
                .on_action(cx.listener(Self::tick_box))
                .on_action(cx.listener(|this, _: &MoveLinesUp, window, cx| {
                    this.move_lines(true, window, cx)
                }))
                .on_action(cx.listener(|this, _: &MoveLinesDown, window, cx| {
                    this.move_lines(false, window, cx)
                }))
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

/// The URL or box under the pointer, for the editor's ⌘-hover underline
/// and ⌘-click, which opens web links only and leaves [`TICK`] to us.
struct Links;

impl DefinitionProvider for Links {
    fn definitions(
        &self,
        text: &Rope,
        offset: usize,
        _: &mut Window,
        _: &mut App,
    ) -> Task<Result<Vec<LocationLink>>> {
        let link = url_link(text, offset).or_else(|| box_link(text, offset));
        Task::ready(Ok(link.into_iter().collect()))
    }
}

/// Where a box links to, so that ⌘-clicking it ticks it.
const TICK: &str = "lemur:tick";

/// The box of a task-list item at byte `offset` in `text`, as a link from
/// itself to [`TICK`].
fn box_link(text: &Rope, offset: usize) -> Option<LocationLink> {
    let row = text.offset_to_point(offset).row;
    let at = text.line_start_offset(row) + checkbox(&text.slice_line(row).to_string())?;
    if !(at..at + 3).contains(&offset) {
        return None;
    }
    let range = Range::new(text.offset_to_position(at), text.offset_to_position(at + 3));
    Some(LocationLink {
        origin_selection_range: Some(range),
        target_uri: TICK.parse().ok()?,
        target_range: range,
        target_selection_range: range,
    })
}

/// Where a list item's words start in `line`: after its indent, its
/// marker (`-`, `*`, `+`, or a number and `.` or `)`) and a space.
fn list_item(line: &str) -> Option<usize> {
    let blank = |s: &str| s.len() - s.trim_start_matches([' ', '\t']).len();
    let indent = blank(line);
    let item = &line[indent..];
    let digits = item.len() - item.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    let marker = match digits {
        0 if item.starts_with(['-', '*', '+']) => 1,
        1..=9 if item[digits..].starts_with(['.', ')']) => digits + 1,
        _ => return None,
    };
    let gap = blank(&item[marker..]);
    (gap > 0).then_some(indent + marker + gap)
}

/// Where the box of a task-list item starts in `line`: `[ ]`, or `[x]`
/// once ticked, where the item's words would.
fn checkbox(line: &str) -> Option<usize> {
    let at = list_item(line)?;
    let rest = &line.as_bytes()[at..];
    let tickable = rest.len() >= 3
        && rest[0] == b'['
        && matches!(rest[1], b' ' | b'x' | b'X')
        && rest[2] == b']'
        && rest.get(3).is_none_or(u8::is_ascii_whitespace);
    tickable.then_some(at)
}

/// The start of the item after one that starts with `start`, up to its
/// box: the same indent and marker, with the next number on a numbered
/// list.
fn next_marker(start: &str) -> String {
    let indent = start.len() - start.trim_start_matches([' ', '\t']).len();
    let item = &start[indent..];
    let digits = item.len() - item.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    match item[..digits].parse::<u64>() {
        Ok(n) => format!("{}{}{}", &start[..indent], n + 1, &item[digits..]),
        Err(_) => start.to_string(),
    }
}

/// The ticked items in `text`: each one's line, to fade, and the words
/// after its box, to strike through.
fn ticked(text: &str) -> Vec<(std::ops::Range<usize>, std::ops::Range<usize>)> {
    let mut items = Vec::new();
    let mut start = 0;
    for line in text.split('\n') {
        if let Some(at) = checkbox(line)
            && line.as_bytes()[at + 1] != b' '
        {
            let end = start + line.trim_end().len();
            let words = end - line[at + 3..].trim().len();
            items.push((start..end, words..end));
        }
        start += line.len() + 1;
    }
    items
}

/// What a list button makes of the lines `selected` touches in `text`:
/// plain bullets, or items with a box with `boxed`, or plain lines again
/// when they all are that already. Blank lines among several stay as they
/// are. Gives the lines' range, what replaces them, and where the
/// selection goes.
fn bulleted(text: &str, selected: std::ops::Range<usize>, boxed: bool) -> Edit {
    // A selection that ends where a line starts stops at the line before.
    let last = match selected.end {
        end if end > selected.start && text[..end].ends_with('\n') => end - 1,
        end => end,
    };
    let start = text[..selected.start].rfind('\n').map_or(0, |i| i + 1);
    let end = text[last..].find('\n').map_or(text.len(), |i| last + i);
    let lines: Vec<&str> = text[start..end].split('\n').collect();
    let several = lines.len() > 1;
    let counts = |line: &str| !several || !line.trim().is_empty();
    let undo = lines
        .iter()
        .filter(|line| counts(line))
        .all(|line| list_item(line).is_some() && checkbox(line).is_some() == boxed);
    // Each line's edit: the bytes it replaces in `text`, and with what.
    let mut edits = Vec::new();
    let mut from = start;
    for line in &lines {
        let indent = line.len() - line.trim_start_matches([' ', '\t']).len();
        let past_box =
            |at: usize| line.len() - line[at + 3..].trim_start_matches([' ', '\t']).len();
        let boxed_now = checkbox(line).is_some();
        let edit = match list_item(line) {
            _ if !counts(line) => None,
            Some(at) if undo => Some((indent, if boxed_now { past_box(at) } else { at }, "")),
            Some(at) if boxed_now && !boxed => Some((at, past_box(at), "")),
            Some(at) if !boxed_now && boxed => Some((at, at, "[ ] ")),
            Some(_) => None,
            None => Some((indent, indent, if boxed { "- [ ] " } else { "- " })),
        };
        edits.extend(edit.map(|(a, b, with)| (from + a, from + b, with)));
        from += line.len() + 1;
    }
    let mut block = String::new();
    let mut copied = start;
    for (a, b, with) in &edits {
        block.push_str(&text[copied..*a]);
        block.push_str(with);
        copied = *b;
    }
    block.push_str(&text[copied..end]);
    // Where an offset goes: past what's put in at it, with `after`.
    let moved = |at: usize, after: bool| {
        let mut shift = 0isize;
        for (a, b, with) in &edits {
            if at < *a || at == *a && !after {
                break;
            }
            if at <= *b {
                return (*a as isize + shift) as usize + with.len();
            }
            shift += with.len() as isize - (b - a) as isize;
        }
        (at as isize + shift) as usize
    };
    let after = selected.is_empty();
    let selected = moved(selected.start, after)..moved(selected.end, true);
    (start..end, block, selected)
}

/// An edit to the notes: the bytes it replaces, what with, and where the
/// selection goes then.
type Edit = (std::ops::Range<usize>, String, std::ops::Range<usize>);

/// Moves the lines `selected` touches in `text` up past the line above,
/// or down past the one below. A list item takes the items nested under
/// it along, and passes a neighbor with its nested items in one go. None
/// at the top or the bottom.
fn moved(text: &str, selected: std::ops::Range<usize>, up: bool) -> Option<Edit> {
    let mut lines: Vec<&str> = text.split('\n').collect();
    // A last newline ends the last line rather than starting another.
    if text.ends_with('\n') {
        lines.pop();
    }
    let row = |at: usize| text[..at].matches('\n').count();
    // A selection that ends where a line starts stops at the line before.
    let end = match selected.end {
        end if end > selected.start && text[..end].ends_with('\n') => end - 1,
        end => end,
    };
    let (first, mut last) = (row(selected.start), row(end));
    if last >= lines.len() {
        return None;
    }
    let indent = |line: &str| line.len() - line.trim_start_matches([' ', '\t']).len();
    let nested = |i: usize, under: usize| !lines[i].trim().is_empty() && indent(lines[i]) > under;
    let base = indent(lines[first]);
    while last + 1 < lines.len() && nested(last + 1, base) {
        last += 1;
    }
    let (from, to, order) = if up {
        let mut above = first.checked_sub(1)?;
        while above > 0 && nested(above, base) {
            above -= 1;
        }
        (above, last, [first..last + 1, above..first])
    } else {
        let below = last + 1;
        if below == lines.len() {
            return None;
        }
        let mut end = below;
        let under = indent(lines[below]);
        while !lines[below].trim().is_empty() && end + 1 < lines.len() && nested(end + 1, under) {
            end += 1;
        }
        (first, end, [below..end + 1, first..last + 1])
    };
    let start_of = |i: usize| lines[..i].iter().map(|line| line.len() + 1).sum::<usize>();
    let range = start_of(from)..start_of(to) + lines[to].len();
    let block = start_of(first)..start_of(last) + lines[last].len();
    let lines: Vec<&str> = order.into_iter().flat_map(|r| lines[r].to_vec()).collect();
    let moved_to = if up {
        range.start
    } else {
        range.end - block.len()
    };
    let at = |offset: usize| offset - block.start + moved_to;
    Some((
        range,
        lines.join("\n"),
        at(selected.start)..at(selected.end),
    ))
}

/// Pasting `url` over `selected` in `text`, when that's text on one line
/// and not a URL itself: `[text](url)`, with the cursor after it.
fn pasted_link(text: &str, selected: std::ops::Range<usize>, url: &str) -> Option<Edit> {
    let words = &text[selected.clone()];
    if words.is_empty() || words.contains('\n') || is_url(words) {
        return None;
    }
    let link = format!("[{words}]({url})");
    let end = selected.start + link.len();
    Some((selected, link, end..end))
}

/// What ⌘K does at `selected` in `text`, given the clipboard's URL if it
/// holds one. Inside a `[text](url)` link it selects the URL, to edit it.
/// Over text on one line it links that, and with nothing selected it
/// starts a link with the cursor in its text. The clipboard's URL comes
/// selected, so typing replaces it; without one, the cursor waits where
/// the URL goes.
fn link(text: &str, selected: std::ops::Range<usize>, url: Option<&str>) -> Option<Edit> {
    let words = &text[selected.clone()];
    if words.contains('\n') {
        return None;
    }
    let start = text[..selected.start].rfind('\n').map_or(0, |i| i + 1);
    let end = text[start..].find('\n').map_or(text.len(), |i| start + i);
    if let Some(url) = link_url_at(&text[start..end], selected.start - start) {
        let url = start + url.start..start + url.end;
        return Some((selected, words.to_string(), url));
    }
    let url = url.unwrap_or("");
    let link = format!("[{words}]({url})");
    let after = match words.len() {
        0 => selected.start + 1..selected.start + 1,
        n if url.is_empty() => selected.start + n + 3..selected.start + n + 3,
        n => selected.start + n + 3..selected.start + n + 3 + url.len(),
    };
    Some((selected, link, after))
}

/// The URL of the `[text](url)` link in `line` around byte `at`.
fn link_url_at(line: &str, at: usize) -> Option<std::ops::Range<usize>> {
    let mut from = 0;
    while let Some(mid) = line[from..].find("](").map(|i| from + i) {
        let open = line[..mid].rfind('[');
        let close = line[mid + 2..].find(')').map(|i| mid + 2 + i);
        if let (Some(open), Some(close)) = (open, close)
            && (open + 1..=close).contains(&at)
        {
            return Some(mid + 2..close);
        }
        from = mid + 2;
    }
    None
}

/// Whether `text` is one web URL and nothing else.
fn is_url(text: &str) -> bool {
    let chars: Vec<char> = text.chars().collect();
    !chars.is_empty() && url_at(&chars, 0) == Some((0, chars.len()))
}

/// The URL on the clipboard, when that's all it holds.
fn clipboard_url(cx: &App) -> Option<String> {
    let text = cx.read_from_clipboard()?.text()?;
    let url = text.trim();
    is_url(url).then(|| url.to_string())
}

/// Ticks the box on the line at `offset`, or unticks it; false when the
/// line has none. The selection stays as it was.
fn tick(
    editor: &mut EditorState,
    offset: usize,
    window: &mut Window,
    cx: &mut Context<EditorState>,
) -> bool {
    let text = editor.text();
    let row = text.offset_to_point(offset).row;
    let line = text.slice_line(row).to_string();
    let Some(at) = checkbox(&line) else {
        return false;
    };
    let mark = text.line_start_offset(row) + at + 1;
    let tick = if line.as_bytes()[at + 1] == b' ' {
        "x"
    } else {
        " "
    };
    let selected = editor.selected_range();
    editor.set_selected_range(mark..mark + 1, cx);
    editor.replace(tick, window, cx);
    editor.set_selected_range(selected, cx);
    true
}

/// Starts a list item below the cursor's, with a box if that has one,
/// when the cursor ends one; an empty one is cleared instead, which ends
/// the list. False when the cursor isn't at the end of an item.
fn next_item(editor: &mut EditorState, window: &mut Window, cx: &mut Context<EditorState>) -> bool {
    let cursor = editor.cursor();
    let text = editor.text();
    let row = text.offset_to_point(cursor).row;
    let (start, end) = (text.line_start_offset(row), text.line_end_offset(row));
    let line = text.slice_line(row).to_string();
    let Some(at) = list_item(&line) else {
        return false;
    };
    if !editor.selected_range().is_empty() || cursor != end {
        return false;
    }
    let (words, next) = match checkbox(&line) {
        Some(_) => (at + 3, "[ ] "),
        None => (at, ""),
    };
    if line[words..].trim().is_empty() {
        editor.set_selected_range(start..end, cx);
        editor.replace("", window, cx);
    } else {
        let marker = next_marker(&line[..at]);
        editor.insert(format!("\n{marker}{next}"), window, cx);
    }
    true
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
    use super::{
        NotesPanel, bulleted, checkbox, link, list_item, moved, next_marker, pasted_link, ticked,
        url_link,
    };
    use crate::notes;
    use gpui::VisualTestContext;
    use gpui_kit::component::Rope;
    use gpui_kit::{AppContext, ClipboardItem, Entity, Modifiers, point, px, size};
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

    #[test]
    fn a_box_starts_a_list_item() {
        assert_eq!(checkbox("- [ ] milk"), Some(2));
        assert_eq!(checkbox("  * [x] eggs"), Some(4));
        assert_eq!(checkbox("12) [X]"), Some(4));
        assert_eq!(checkbox("+\t[ ]\tbread"), Some(2));
        for not in [
            "[ ] milk",
            "-[ ] milk",
            "- [ ]milk",
            "- [-] milk",
            "a. [ ] milk",
            "- milk [ ]",
            "1234567890. [ ] milk",
        ] {
            assert_eq!(checkbox(not), None, "{not:?}");
        }
        assert_eq!(next_marker("  9. "), "  10. ");
        assert_eq!(next_marker("- "), "- ");
    }

    #[test]
    fn a_list_item_is_a_marker_and_a_space() {
        assert_eq!(list_item("- milk"), Some(2));
        assert_eq!(list_item("  10) "), Some(6));
        assert_eq!(list_item("- [ ] milk"), Some(2));
        for not in ["-milk", "**bold**", "milk", ""] {
            assert_eq!(list_item(not), None, "{not:?}");
        }
    }

    #[test]
    fn list_buttons_bullet_the_lines_or_take_the_bullets_off() {
        // An empty line gets a bullet, with the cursor after it.
        assert_eq!(bulleted("", 0..0, false), (0..0, "- ".into(), 2..2));
        // The cursor's line keeps its indent, and the cursor its word.
        let text = "a\n  milk\nb";
        assert_eq!(
            bulleted(text, 5..5, true),
            (2..8, "  - [ ] milk".into(), 11..11)
        );
        // Of several lines, blank ones stay blank, a plain bullet gets a
        // box, and a ticked one keeps its tick.
        let text = "- milk\n\n- [x] eggs\nbread";
        let boxed = "- [ ] milk\n\n- [x] eggs\n- [ ] bread";
        assert_eq!(bulleted(text, 0..24, true), (0..24, boxed.into(), 0..34));
        // All boxed already, they lose their boxes and bullets.
        assert_eq!(
            bulleted(boxed, 0..34, true),
            (0..34, "milk\n\neggs\nbread".into(), 0..16)
        );
        // The bullet button takes a box off and keeps the number.
        assert_eq!(
            bulleted("1. [x] milk", 8..8, false),
            (0..11, "1. milk".into(), 4..4)
        );
        // A selection ending where a line starts leaves that line be.
        assert_eq!(
            bulleted("a\nb\nc", 0..4, false),
            (0..3, "- a\n- b".into(), 0..8)
        );
    }

    #[test]
    fn a_url_pasted_over_text_links_it() {
        let url = "https://x.io";
        assert_eq!(
            pasted_link("see docs", 4..8, url),
            Some((4..8, "[docs](https://x.io)".into(), 24..24))
        );
        // Not with nothing selected, over a URL, or across lines.
        assert_eq!(pasted_link("see docs", 8..8, url), None);
        assert_eq!(pasted_link("https://a.io", 0..12, url), None);
        assert_eq!(pasted_link("a\nb", 0..3, url), None);
    }

    #[test]
    fn cmd_k_links_the_selection_or_selects_a_links_url() {
        let url = Some("https://x.io");
        // The clipboard's URL comes selected, to type over.
        assert_eq!(
            link("see docs", 4..8, url),
            Some((4..8, "[docs](https://x.io)".into(), 11..23))
        );
        // Without one, the cursor waits where the URL goes.
        assert_eq!(
            link("see docs", 4..8, None),
            Some((4..8, "[docs]()".into(), 11..11))
        );
        // With nothing selected, the cursor goes where the text goes.
        assert_eq!(
            link("see ", 4..4, url),
            Some((4..4, "[](https://x.io)".into(), 5..5))
        );
        // In a link, its URL is selected.
        let text = "a [docs](https://x.io) b";
        assert_eq!(link(text, 4..4, None), Some((4..4, "".into(), 9..21)));
        assert_eq!(link("a\nb", 0..3, url), None);
    }

    #[test]
    fn moving_an_item_takes_its_nested_items_along() {
        // b and b1 go up past a and a1, and back down again.
        let text = "- a\n  - a1\n- b\n  - b1\nend";
        let up = "- b\n  - b1\n- a\n  - a1";
        assert_eq!(moved(text, 13..13, true), Some((0..21, up.into(), 2..2)));
        let text = format!("{up}\nend");
        let down = "- a\n  - a1\n- b\n  - b1";
        assert_eq!(
            moved(&text, 2..2, false),
            Some((0..21, down.into(), 13..13))
        );
        // Plain lines go one at a time, and not past the top or the end.
        assert_eq!(
            moved("a\nb\n", 2..2, true),
            Some((0..3, "b\na".into(), 0..0))
        );
        assert_eq!(moved("a\nb\n", 0..0, true), None);
        assert_eq!(moved("a\nb\n", 2..2, false), None);
        assert_eq!(moved("a\nb\n", 4..4, false), None);
    }

    #[test]
    fn ticked_items_fade_and_their_words_are_struck_through() {
        let text = "- [x] milk  \n- [ ] eggs\n  - [X] bread";
        assert_eq!(ticked(text), vec![(0..10, 6..10), (24..37, 32..37)]);
    }

    /// ⌘-clicking a box ticks it, and again unticks it; a plain click only
    /// puts the cursor there.
    #[gpui::test]
    fn cmd_click_ticks_a_box(cx: &mut gpui::TestAppContext) {
        let root = root("notes-ticks");
        let file = notes::file(&root, "agent", "/srv/a");
        notes::save(&file, "- [ ] milk").unwrap();
        let (panel, cx) = panel(&root, cx);
        show(&panel, "/srv/a", cx);
        let at = cx
            .update(|_, cx| panel.read(cx).editor.read(cx).range_to_bounds(&(3..4)))
            .expect("the box on show")
            .center();

        cx.simulate_click(at, Modifiers::none());
        cx.run_until_parked();
        assert_eq!(notes::load(&file).unwrap(), "- [ ] milk");
        for (saved, marked) in [("- [x] milk", vec![0..10, 6..10]), ("- [ ] milk", vec![])] {
            cx.simulate_mouse_move(at, None, Modifiers::command());
            cx.run_until_parked();
            cx.simulate_click(at, Modifiers::command());
            cx.run_until_parked();
            assert_eq!(notes::load(&file).unwrap(), saved);
            let ranges = cx.update(|_, cx| panel.read(cx).ticked.get_ranges(cx));
            assert_eq!(ranges, marked, "faded and struck through");
        }
        assert_eq!(cx.opened_url(), None);
        let _ = std::fs::remove_dir_all(root);
    }

    /// Enter at the end of an item starts the next, numbered on, and on an
    /// empty one ends the list; ⇧-Enter is a plain newline, and ⌘-Enter
    /// ticks the cursor's line.
    #[gpui::test]
    fn enter_goes_on_with_a_task_list(cx: &mut gpui::TestAppContext) {
        let root = root("notes-tasks");
        let (panel, cx) = panel(&root, cx);
        show(&panel, "/srv/a", cx);
        let editor = cx.debug_bounds("notes-editor").expect("the editor");
        cx.simulate_click(editor.center(), Modifiers::none());

        cx.simulate_input("1. [ ] milk");
        cx.simulate_keystrokes("enter");
        cx.simulate_input("eggs");
        cx.simulate_keystrokes("cmd-enter");
        assert_eq!(
            panel.read_with(cx, |p, cx| p.text(cx)),
            "1. [ ] milk\n2. [x] eggs"
        );
        cx.simulate_keystrokes("enter enter");
        cx.simulate_input("- [ ] tea");
        cx.simulate_keystrokes("shift-enter");
        cx.simulate_input("done");
        let text = "1. [ ] milk\n2. [x] eggs\n- [ ] tea\ndone";
        assert_eq!(panel.read_with(cx, |p, cx| p.text(cx)), text);
        let saved = notes::load(&notes::file(&root, "agent", "/srv/a")).unwrap();
        assert_eq!(saved, text);
        let _ = std::fs::remove_dir_all(root);
    }

    /// The list buttons bullet the cursor's line, or give it a box, and
    /// leave the keyboard in the notes; Enter goes on with plain bullets
    /// too.
    #[gpui::test]
    fn list_buttons_bullet_the_cursors_line(cx: &mut gpui::TestAppContext) {
        let root = root("notes-bullets");
        let (panel, cx) = panel(&root, cx);
        show(&panel, "/srv/a", cx);
        let editor = cx.debug_bounds("notes-editor").expect("the editor");
        cx.simulate_click(editor.center(), Modifiers::none());
        let click = |button: &'static str, cx: &mut VisualTestContext| {
            let at = cx.debug_bounds(button).expect(button).center();
            cx.simulate_click(at, Modifiers::none());
        };

        click("bullets", cx);
        cx.simulate_input("milk");
        cx.simulate_keystrokes("enter");
        cx.simulate_input("eggs");
        assert_eq!(panel.read_with(cx, |p, cx| p.text(cx)), "- milk\n- eggs");
        click("boxes", cx);
        cx.simulate_input(" now");
        cx.simulate_keystrokes("enter enter");
        let text = "- milk\n- [ ] eggs now\n";
        assert_eq!(panel.read_with(cx, |p, cx| p.text(cx)), text);
        let saved = notes::load(&notes::file(&root, "agent", "/srv/a")).unwrap();
        assert_eq!(saved, text);
        let _ = std::fs::remove_dir_all(root);
    }

    /// The notes' own keys, as typed: pasting a URL over text links it,
    /// ⇧⌘U ticks, ⇧⌘L makes a checklist, ⌃⌘↑ moves an item, Tab indents
    /// one from inside it, and ⌘K in a link selects its URL.
    #[gpui::test]
    fn the_notes_keys(cx: &mut gpui::TestAppContext) {
        let root = root("notes-keys");
        let (panel, cx) = panel(&root, cx);
        cx.update(|_, cx| super::bind_keys(cx));
        show(&panel, "/srv/a", cx);
        let editor = cx.debug_bounds("notes-editor").expect("the editor");
        cx.simulate_click(editor.center(), Modifiers::none());
        let text = |cx: &mut VisualTestContext| panel.read_with(cx, |p, cx| p.text(cx));

        cx.simulate_input("see docs");
        cx.simulate_keystrokes("shift-left shift-left shift-left shift-left");
        cx.write_to_clipboard(ClipboardItem::new_string("https://x.io\n".into()));
        cx.simulate_keystrokes("cmd-v");
        assert_eq!(text(cx), "see [docs](https://x.io)");

        cx.simulate_keystrokes("enter");
        cx.simulate_input("- [ ] tea");
        cx.simulate_keystrokes("shift-cmd-u enter enter");
        cx.simulate_input("milk");
        cx.simulate_keystrokes("shift-cmd-l ctrl-cmd-up");
        assert_eq!(text(cx), "see [docs](https://x.io)\n- [ ] milk\n- [x] tea");

        cx.simulate_keystrokes("left tab");
        assert_eq!(
            text(cx),
            "see [docs](https://x.io)\n  - [ ] milk\n- [x] tea"
        );

        cx.simulate_keystrokes("cmd-up right right right right right cmd-k");
        cx.simulate_input("https://y.io");
        let edited = "see [docs](https://y.io)\n  - [ ] milk\n- [x] tea";
        assert_eq!(text(cx), edited);
        let saved = notes::load(&notes::file(&root, "agent", "/srv/a")).unwrap();
        assert_eq!(saved, edited);
        let _ = std::fs::remove_dir_all(root);
    }
}
