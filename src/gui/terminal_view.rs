//! Draws a Pty's grid with GPUI and feeds it keys, mouse, pastes and drops.
//! Dropped or pasted images go to the host first; the session gets the path.

use super::keys::{self, Key};
use super::settings::Prefs;
use super::terminal::{Palette, Pty, Size};
use crate::actions;
use crate::attach::attach_argv;
use crate::config::Host;
use alacritty_terminal::event::Event as TermEvent;
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::term::TermMode;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::vte::ansi::{CursorShape, NamedColor, Rgb};
use futures::StreamExt;
use gpui_kit::component::ActiveTheme;
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use std::path::PathBuf;

/// Key context for terminal-only key bindings.
pub const CONTEXT: &str = "Terminal";

pub struct TerminalView {
    pub host: Host,
    pub session: String,
    pty: Option<Pty>,
    focus: FocusHandle,
    /// Where the grid was last painted, for mapping mouse positions to cells.
    bounds: Bounds<Pixels>,
    cell: Size,
    /// Measured on the next render after Settings change the font.
    metrics: Option<Metrics>,
    mouse_cell: Option<(usize, usize)>,
    scroll_lines: f32,
    status: Option<String>,
    _reader: Option<Task<()>>,
    _prefs: Subscription,
}

/// The terminal font and the size of one cell in it.
#[derive(Clone)]
struct Metrics {
    font: Font,
    size: Pixels,
    cell_width: Pixels,
    line_height: Pixels,
}

impl Focusable for TerminalView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl TerminalView {
    pub fn new(host: Host, session: String, cx: &mut Context<Self>) -> Self {
        let mut view = TerminalView {
            host,
            session,
            pty: None,
            focus: cx.focus_handle(),
            bounds: Bounds::default(),
            cell: Size {
                cols: 80,
                lines: 24,
                cell_width: 8,
                cell_height: 16,
            },
            metrics: None,
            mouse_cell: None,
            scroll_lines: 0.0,
            status: None,
            _reader: None,
            _prefs: cx.observe_global::<Prefs>(|this, cx| {
                this.metrics = None;
                cx.notify();
            }),
        };
        view.connect(cx);
        view
    }

    #[cfg(test)]
    pub fn pty_size(&self) -> Option<Size> {
        self.pty.as_ref().map(|p| p.size())
    }

    pub fn exited(&self) -> bool {
        self.pty.is_none()
    }

    /// (Re)starts `tmux attach` for this session, e.g. after the laptop slept.
    pub fn connect(&mut self, cx: &mut Context<Self>) {
        let argv = attach_argv(&self.host, &self.session);
        match Pty::spawn(&argv, self.cell) {
            Ok((pty, mut events)) => {
                self.pty = Some(pty);
                self.status = None;
                self._reader = Some(cx.spawn(async move |this, cx| {
                    while let Some(event) = events.next().await {
                        if this
                            .update(cx, |this, cx| this.on_term_event(event, cx))
                            .is_err()
                        {
                            break;
                        }
                    }
                }));
            }
            Err(e) => {
                self.pty = None;
                self.status = Some(format!("{e:#}"));
            }
        }
        cx.notify();
    }

    fn on_term_event(&mut self, event: TermEvent, cx: &mut Context<Self>) {
        let Some(pty) = &self.pty else { return };
        match event {
            TermEvent::Wakeup => cx.notify(),
            TermEvent::PtyWrite(text) => pty.write(text.into_bytes()),
            // tmux copies selections with OSC 52.
            TermEvent::ClipboardStore(_, text) => {
                cx.write_to_clipboard(ClipboardItem::new_string(text))
            }
            TermEvent::ColorRequest(index, format) => {
                let palette = Palette::themed(cx.theme().is_dark(), &Prefs::get(cx).terminal);
                let color = palette.indexed(index, pty.term.lock().colors());
                pty.write(format(color).into_bytes());
            }
            TermEvent::TextAreaSizeRequest(format) => {
                pty.write(format(pty.size().window()).into_bytes())
            }
            TermEvent::ChildExit(_) | TermEvent::Exit => {
                self.pty = None;
                self._reader = None;
                self.status = Some("disconnected · click to reconnect".into());
                cx.notify();
            }
            _ => {}
        }
    }

    fn mode(&self) -> TermMode {
        self.pty
            .as_ref()
            .map(|p| *p.term.lock().mode())
            .unwrap_or(TermMode::NONE)
    }

    fn write(&self, bytes: Vec<u8>) {
        if let Some(pty) = &self.pty {
            pty.write(bytes);
        }
    }

    fn paste_text(&self, text: &str) {
        self.write(keys::paste(
            text,
            self.mode().contains(TermMode::BRACKETED_PASTE),
        ));
    }

    fn key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let ks = &event.keystroke;
        let m = &ks.modifiers;
        if m.platform {
            if ks.key == "v" && !m.shift && !m.alt && !m.control {
                self.paste_clipboard(cx);
                cx.stop_propagation();
            }
            return;
        }
        let key = Key {
            key: &ks.key,
            key_char: ks.key_char.as_deref(),
            ctrl: m.control,
            alt: m.alt,
            shift: m.shift,
        };
        let mut term = self.pty.as_ref().map(|p| p.term.lock());
        let app_cursor = term
            .as_ref()
            .is_some_and(|t| t.mode().contains(TermMode::APP_CURSOR));
        if let Some(bytes) = keys::encode(&key, app_cursor) {
            if let Some(term) = term.as_mut() {
                term.scroll_display(Scroll::Bottom);
            }
            drop(term);
            self.write(bytes);
            cx.stop_propagation();
        }
    }

    /// Cmd+V: files and images are uploaded and pasted as remote paths;
    /// text is pasted as is.
    fn paste_clipboard(&mut self, cx: &mut Context<Self>) {
        let Some(item) = cx.read_from_clipboard() else {
            return;
        };
        let mut paths = vec![];
        let mut image = None;
        let mut text = None;
        for entry in item.into_entries() {
            match entry {
                ClipboardEntry::ExternalPaths(p) => paths.extend(p.0),
                ClipboardEntry::Image(i) => image = Some(i),
                ClipboardEntry::String(s) => text = Some(s.text),
            }
        }
        // Finder puts a file's icon next to its path; the path wins.
        if !paths.is_empty() {
            self.upload(paths, None, cx);
        } else if image.is_some() {
            self.upload(vec![], image, cx);
        } else if let Some(text) = text {
            self.paste_text(&text);
        }
    }

    /// Copies files (and a pasted image) to the host, then pastes their
    /// remote paths.
    fn upload(&mut self, mut files: Vec<PathBuf>, image: Option<Image>, cx: &mut Context<Self>) {
        let host = self.host.clone();
        let count = files.len() + usize::from(image.is_some());
        self.status = Some(format!("uploading {count} file(s)…"));
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    // An image goes up as a temporary file.
                    let temporary = image.map(|i| save_image(&i)).transpose()?;
                    files.extend(temporary.clone());
                    let uploaded = actions::upload(&host, &files);
                    if let Some(t) = temporary {
                        let _ = std::fs::remove_file(t);
                    }
                    uploaded
                })
                .await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(paths) => {
                        this.status = None;
                        this.paste_text(&actions::paste_line(&paths));
                    }
                    Err(e) => this.status = Some(format!("upload failed: {e:#}")),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn cell_at(&self, position: Point<Pixels>) -> (usize, usize) {
        let b = self.bounds;
        let col = ((position.x - b.origin.x) / px(self.cell.cell_width as f32)).max(0.0) as usize;
        let line = ((position.y - b.origin.y) / px(self.cell.cell_height as f32)).max(0.0) as usize;
        (
            col.min(self.cell.cols.saturating_sub(1) as usize),
            line.min(self.cell.lines.saturating_sub(1) as usize),
        )
    }

    /// tmux in mouse mode wants SGR reports; otherwise clicks only focus.
    fn reports_mouse(mode: TermMode) -> bool {
        mode.intersects(TermMode::MOUSE_MODE) && mode.contains(TermMode::SGR_MOUSE)
    }

    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        if self.pty.is_none() {
            self.connect(cx);
            return;
        }
        if Self::reports_mouse(self.mode()) {
            let (col, line) = self.cell_at(event.position);
            self.mouse_cell = Some((col, line));
            self.write(keys::sgr_mouse(
                keys::MouseButton::Left,
                false,
                col,
                line,
                true,
            ));
        }
    }

    fn mouse_up(&mut self, event: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        if self.mouse_cell.take().is_some() && Self::reports_mouse(self.mode()) {
            let (col, line) = self.cell_at(event.position);
            self.write(keys::sgr_mouse(
                keys::MouseButton::Left,
                false,
                col,
                line,
                false,
            ));
        }
    }

    fn mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, _: &mut Context<Self>) {
        if event.pressed_button != Some(MouseButton::Left)
            || self.mouse_cell.is_none()
            || !Self::reports_mouse(self.mode())
        {
            return;
        }
        let cell = self.cell_at(event.position);
        if Some(cell) != self.mouse_cell {
            self.mouse_cell = Some(cell);
            self.write(keys::sgr_mouse(
                keys::MouseButton::Left,
                true,
                cell.0,
                cell.1,
                true,
            ));
        }
    }

    fn scroll(&mut self, event: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.scroll_lines += match event.delta {
            ScrollDelta::Lines(p) => p.y,
            ScrollDelta::Pixels(p) => p.y / px(self.cell.cell_height as f32),
        };
        let lines = self.scroll_lines.trunc() as i32;
        if lines == 0 {
            return;
        }
        self.scroll_lines -= lines as f32;
        let mode = self.mode();
        let notches = lines.unsigned_abs() as usize;
        if Self::reports_mouse(mode) {
            let (col, line) = self.cell_at(event.position);
            let button = if lines > 0 {
                keys::MouseButton::WheelUp
            } else {
                keys::MouseButton::WheelDown
            };
            self.write(keys::sgr_mouse(button, false, col, line, true).repeat(notches));
        } else if mode.contains(TermMode::ALT_SCREEN) {
            let arrow: &[u8] = if lines > 0 { b"\x1b[A" } else { b"\x1b[B" };
            self.write(arrow.repeat(notches));
        } else if let Some(pty) = &self.pty {
            pty.term.lock().scroll_display(Scroll::Delta(lines));
            cx.notify();
        }
    }

    /// The terminal font from Settings, and its cell size.
    fn measure(window: &mut Window, cx: &App) -> Metrics {
        let terminal = &Prefs::get(cx).terminal;
        let font = font(terminal.font_family.clone());
        let size = terminal.font_size as f32;
        let text = window.text_system();
        let font_id = text.resolve_font(&font);
        let cell_width = text
            .advance(font_id, px(size), 'm')
            .map(|s| s.width)
            .unwrap_or(px(8.0));
        Metrics {
            font,
            size: px(size),
            cell_width,
            line_height: px((size * 1.3).round()),
        }
    }
}

fn save_image(image: &Image) -> anyhow::Result<PathBuf> {
    let ext = match image.format {
        ImageFormat::Png => "png",
        ImageFormat::Jpeg => "jpg",
        ImageFormat::Webp => "webp",
        ImageFormat::Gif => "gif",
        ImageFormat::Svg => "svg",
        ImageFormat::Bmp => "bmp",
        _ => "tiff",
    };
    let path = std::env::temp_dir().join(format!(
        "lemen-paste-{}-{}.{ext}",
        std::process::id(),
        image.id
    ));
    std::fs::write(&path, &image.bytes)?;
    Ok(path)
}

fn hsla(c: Rgb) -> Hsla {
    Rgba {
        r: c.r as f32 / 255.0,
        g: c.g as f32 / 255.0,
        b: c.b as f32 / 255.0,
        a: 1.0,
    }
    .into()
}

/// What a run of text looks like; runs merge while it stays the same.
#[derive(Clone, Copy, PartialEq)]
struct Style {
    fg: Rgb,
    /// Only the flags that change how text is drawn.
    flags: Flags,
}

impl Style {
    const DRAWN: Flags = Flags::BOLD
        .union(Flags::ITALIC)
        .union(Flags::ALL_UNDERLINES)
        .union(Flags::STRIKEOUT);

    fn text_run(self, len: usize, font: &Font) -> TextRun {
        let mut font = font.clone();
        if self.flags.contains(Flags::BOLD) {
            font.weight = FontWeight::BOLD;
        }
        if self.flags.contains(Flags::ITALIC) {
            font.style = FontStyle::Italic;
        }
        let color = hsla(self.fg);
        TextRun {
            len,
            font,
            color,
            background_color: None,
            underline: self
                .flags
                .intersects(Flags::ALL_UNDERLINES)
                .then(|| UnderlineStyle {
                    thickness: px(1.0),
                    color: Some(color),
                    wavy: self.flags.contains(Flags::UNDERCURL),
                }),
            strikethrough: self
                .flags
                .contains(Flags::STRIKEOUT)
                .then(|| StrikethroughStyle {
                    thickness: px(1.0),
                    color: Some(color),
                }),
        }
    }
}

/// One screen row: its text, styled runs (style, byte length), and
/// background spans (first column, end column, color).
struct Row {
    text: String,
    runs: Vec<(Style, usize)>,
    backgrounds: Vec<(usize, usize, Rgb)>,
}

struct Frame {
    rows: Vec<Row>,
    background: Hsla,
    cursor: Option<(usize, usize, CursorShape, Hsla)>,
}

/// Snapshots the grid under the lock so painting doesn't block the reader.
fn snapshot(pty: &Pty, palette: &Palette) -> Frame {
    let term = pty.term.lock();
    let content = term.renderable_content();
    let colors = content.colors;
    let default_bg = palette.indexed(NamedColor::Background as usize, colors);
    let offset = content.display_offset as i32;
    let cols = term.columns();
    let mut rows: Vec<Row> = (0..term.screen_lines())
        .map(|_| Row {
            text: String::with_capacity(cols),
            runs: vec![],
            backgrounds: vec![],
        })
        .collect();
    for cell in content.display_iter {
        let Some(row) = usize::try_from(cell.point.line.0 + offset)
            .ok()
            .and_then(|i| rows.get_mut(i))
        else {
            continue;
        };
        let flags = cell.flags;
        let mut fg = palette.resolve(cell.fg, colors);
        let mut bg = palette.resolve(cell.bg, colors);
        if flags.contains(Flags::DIM) {
            fg = palette.dim(fg);
        }
        if flags.contains(Flags::INVERSE) {
            std::mem::swap(&mut fg, &mut bg);
        }
        let col = cell.point.column.0;
        if bg != default_bg {
            match row.backgrounds.last_mut() {
                Some((_, end, color)) if *end == col && *color == bg => *end = col + 1,
                _ => row.backgrounds.push((col, col + 1, bg)),
            }
        }
        // A wide character's spacer stays a space so later cells keep their column.
        let c = if flags.intersects(Flags::HIDDEN | Flags::WIDE_CHAR_SPACER) || cell.c == '\0' {
            ' '
        } else {
            cell.c
        };
        let style = Style {
            fg,
            flags: flags.intersection(Style::DRAWN),
        };
        let len = c.len_utf8();
        row.text.push(c);
        match row.runs.last_mut() {
            Some((last, run_len)) if *last == style => *run_len += len,
            _ => row.runs.push((style, len)),
        }
    }
    let cursor = (content.mode.contains(TermMode::SHOW_CURSOR)
        && content.cursor.shape != CursorShape::Hidden)
        .then(|| {
            let line = usize::try_from(content.cursor.point.line.0 + offset).ok()?;
            let color = hsla(palette.indexed(NamedColor::Cursor as usize, colors));
            Some((
                content.cursor.point.column.0,
                line,
                content.cursor.shape,
                color,
            ))
        })
        .flatten();
    Frame {
        rows,
        background: hsla(default_bg),
        cursor,
    }
}

fn paint(
    frame: Frame,
    bounds: Bounds<Pixels>,
    metrics: &Metrics,
    focused: bool,
    window: &mut Window,
    cx: &mut App,
) {
    let (w, h) = (metrics.cell_width, metrics.line_height);
    window.paint_quad(fill(bounds, frame.background));
    for (i, row) in frame.rows.iter().enumerate() {
        let y = bounds.origin.y + h * i as f32;
        for &(start, end, color) in &row.backgrounds {
            let origin = point(bounds.origin.x + w * start as f32, y);
            window.paint_quad(fill(
                Bounds::new(origin, size(w * (end - start) as f32, h)),
                hsla(color),
            ));
        }
    }
    if let Some((col, line, shape, color)) = frame.cursor {
        let origin = point(
            bounds.origin.x + w * col as f32,
            bounds.origin.y + h * line as f32,
        );
        let cursor = match shape {
            CursorShape::Beam => Bounds::new(origin, size(px(2.0), h)),
            CursorShape::Underline => {
                Bounds::new(point(origin.x, origin.y + h - px(2.0)), size(w, px(2.0)))
            }
            _ => Bounds::new(origin, size(w, h)),
        };
        if focused || !matches!(shape, CursorShape::Block | CursorShape::HollowBlock) {
            window.paint_quad(fill(cursor, color.opacity(0.6)));
        } else {
            window.paint_quad(outline(cursor, color, BorderStyle::Solid));
        }
    }
    for (i, row) in frame.rows.into_iter().enumerate() {
        if row.text.trim_end().is_empty() {
            continue;
        }
        let y = bounds.origin.y + h * i as f32;
        let runs: Vec<TextRun> = row
            .runs
            .iter()
            .map(|&(style, len)| style.text_run(len, &metrics.font))
            .collect();
        let line = window
            .text_system()
            .shape_line(row.text.into(), metrics.size, &runs, Some(w));
        let _ = line.paint(
            point(bounds.origin.x, y),
            h,
            TextAlign::Left,
            None,
            window,
            cx,
        );
    }
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let metrics = self
            .metrics
            .get_or_insert_with(|| Self::measure(window, cx))
            .clone();
        let (w, h) = (metrics.cell_width, metrics.line_height);
        let focused = self.focus.is_focused(window);
        let view = cx.entity().downgrade();
        let palette = Palette::themed(cx.theme().is_dark(), &Prefs::get(cx).terminal);
        let frame = self.pty.as_ref().map(|pty| snapshot(pty, &palette));
        let grid = canvas(
            move |bounds, _, cx| {
                let size = Size {
                    cols: (bounds.size.width / w).floor().max(1.0) as u16,
                    lines: (bounds.size.height / h).floor().max(1.0) as u16,
                    cell_width: f32::from(w).round() as u16,
                    cell_height: f32::from(h) as u16,
                };
                view.update(cx, |view, _| {
                    view.bounds = bounds;
                    view.cell = size;
                    if let Some(pty) = &mut view.pty {
                        pty.resize(size);
                    }
                })
                .ok();
            },
            move |bounds, (), window, cx| {
                if let Some(frame) = frame {
                    paint(frame, bounds, &metrics, focused, window, cx);
                }
            },
        )
        .size_full();

        div()
            .id("terminal")
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            .relative()
            .size_full()
            .bg(hsla(palette.background))
            .on_key_down(cx.listener(Self::key_down))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_mouse_move(cx.listener(Self::mouse_move))
            .on_scroll_wheel(cx.listener(Self::scroll))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                this.upload(paths.paths().to_vec(), None, cx)
            }))
            .child(grid)
            .when_some(self.status.clone(), |this, status| {
                this.child(
                    div()
                        .absolute()
                        .bottom_2()
                        .right_2()
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .bg(hsla(palette.foreground).opacity(0.15))
                        .text_color(hsla(palette.foreground))
                        .text_sm()
                        .child(status),
                )
            })
    }
}

#[cfg(test)]
mod tests {
    // Not `super::*`: GPUI's `test` macro would shadow the built-in one.
    use super::TerminalView;
    use crate::gui::settings::Prefs;
    use crate::test_support::{TmuxServer, wait_until};
    use gpui_kit::{px, size};
    use std::time::Duration;

    /// A real `tmux attach` in a headless window: resizing the window must
    /// resize the PTY, and tmux must see the new size.
    #[gpui::test]
    fn the_terminal_follows_the_window_size(cx: &mut gpui::TestAppContext) {
        let server = TmuxServer::new("resize");
        server.tmux(&["new-session", "-d", "-s", "t", "sleep 600"]);
        let host = server.config().hosts.remove(0);
        // The PTY reader is a real thread, so this test can't be deterministic.
        cx.executor().allow_parking();
        cx.update(gpui_kit::init);
        let (view, cx) = cx.add_window_view(|_, cx| TerminalView::new(host, "t".into(), cx));

        cx.simulate_resize(size(px(600.0), px(400.0)));
        cx.run_until_parked();
        let small = view.read_with(cx, |v, _| v.pty_size()).expect("attached");
        cx.simulate_resize(size(px(1000.0), px(700.0)));
        cx.run_until_parked();
        let large = view.read_with(cx, |v, _| v.pty_size()).expect("attached");
        assert!(
            large.cols > small.cols && large.lines > small.lines,
            "{small:?} -> {large:?}"
        );

        // tmux runs in real time; give it a moment to see the new size.
        let want = format!("{}x{}", large.cols, large.lines);
        let client = || server.tmux(&["list-clients", "-F", "#{client_width}x#{client_height}"]);
        assert!(
            wait_until(Duration::from_secs(5), || client() == want),
            "tmux sees {}",
            client()
        );
    }

    /// A font size from Settings reaches open terminals: a bigger font fits
    /// fewer columns and lines in the same window.
    #[gpui::test]
    fn the_terminal_follows_the_font_size(cx: &mut gpui::TestAppContext) {
        let server = TmuxServer::new("font");
        server.tmux(&["new-session", "-d", "-s", "t", "sleep 600"]);
        let host = server.config().hosts.remove(0);
        // The PTY reader is a real thread, so this test can't be deterministic.
        cx.executor().allow_parking();
        cx.update(gpui_kit::init);
        let (view, cx) = cx.add_window_view(|_, cx| TerminalView::new(host, "t".into(), cx));
        cx.simulate_resize(size(px(800.0), px(600.0)));
        cx.run_until_parked();
        let small = view.read_with(cx, |v, _| v.pty_size()).expect("attached");

        cx.update(|_, cx| Prefs::update(cx, |p| p.terminal.font_size = 26.0));
        cx.run_until_parked();
        let large = view.read_with(cx, |v, _| v.pty_size()).expect("attached");
        assert!(
            large.cols < small.cols && large.lines < small.lines,
            "{small:?} -> {large:?}"
        );
    }
}
