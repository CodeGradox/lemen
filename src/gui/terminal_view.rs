//! Draws a Pty's grid with GPUI and feeds it keys, mouse, pastes and drops.
//! Dropped or pasted images go to the host first; the session gets the path.

use super::keys::{self, Key};
use super::terminal::{self, Pty, Size};
use crate::actions;
use crate::attach::attach_argv;
use crate::config::Host;
use alacritty_terminal::event::Event as TermEvent;
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::term::TermMode;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::vte::ansi::{CursorShape, NamedColor, Rgb};
use futures::StreamExt;
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;

/// Key context for terminal-only key bindings.
pub const CONTEXT: &str = "Terminal";
const FONT_FAMILY: &str = "Menlo";
const FONT_SIZE: f32 = 13.0;

pub enum TerminalEvent {
    Exited,
}

pub struct TerminalView {
    pub host: Host,
    pub session: String,
    pty: Option<Pty>,
    focus: FocusHandle,
    /// Where the grid was last painted, for mapping mouse positions to cells.
    bounds: Rc<Cell<Bounds<Pixels>>>,
    cell: Size,
    mouse_cell: Option<(usize, usize)>,
    scroll_lines: f32,
    status: Option<String>,
    _reader: Option<Task<()>>,
}

impl EventEmitter<TerminalEvent> for TerminalView {}

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
            bounds: Rc::new(Cell::new(Bounds::default())),
            cell: Size {
                cols: 80,
                lines: 24,
                cell_width: 8,
                cell_height: 16,
            },
            mouse_cell: None,
            scroll_lines: 0.0,
            status: None,
            _reader: None,
        };
        view.connect(cx);
        view
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
                let color = terminal::indexed(index, pty.term.lock().colors());
                pty.write(format(color).into_bytes());
            }
            TermEvent::TextAreaSizeRequest(format) => {
                pty.write(format(pty.size().window()).into_bytes())
            }
            TermEvent::ChildExit(_) | TermEvent::Exit => {
                self.pty = None;
                self._reader = None;
                self.status = Some("disconnected · click to reconnect".into());
                cx.emit(TerminalEvent::Exited);
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
            cmd: m.platform,
        };
        if let Some(bytes) = keys::encode(&key, self.mode().contains(TermMode::APP_CURSOR)) {
            if let Some(pty) = &self.pty {
                pty.term.lock().scroll_display(Scroll::Bottom);
            }
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
        for entry in item.entries() {
            match entry {
                ClipboardEntry::ExternalPaths(p) => paths.extend(p.paths().iter().cloned()),
                ClipboardEntry::Image(i) => image = Some(i.clone()),
                ClipboardEntry::String(s) => text = Some(s.text.clone()),
            }
        }
        // Finder puts a file's icon next to its path; the path wins.
        if !paths.is_empty() {
            self.upload(paths, vec![], cx);
        } else if let Some(image) = image {
            match save_image(&image) {
                Ok(path) => self.upload(vec![path.clone()], vec![path], cx),
                Err(e) => self.status = Some(format!("paste failed: {e:#}")),
            }
        } else if let Some(text) = text {
            self.paste_text(&text);
        }
    }

    /// Copies files to the host, then pastes their remote paths.
    fn upload(&mut self, files: Vec<PathBuf>, temporary: Vec<PathBuf>, cx: &mut Context<Self>) {
        let host = self.host.clone();
        self.status = Some(format!("uploading {} file(s)…", files.len()));
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { actions::upload(&host, &files) })
                .await;
            for t in temporary {
                let _ = std::fs::remove_file(t);
            }
            this.update(cx, |this, cx| {
                match result {
                    Ok(paths) => {
                        this.status = None;
                        this.paste_text(&format!("{} ", paths.join(" ")));
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
        let b = self.bounds.get();
        let col = ((position.x - b.origin.x) / px(self.cell.cell_width as f32)).max(0.0) as usize;
        let line = ((position.y - b.origin.y) / px(self.cell.cell_height as f32)).max(0.0) as usize;
        (
            col.min(self.cell.cols.saturating_sub(1) as usize),
            line.min(self.cell.lines.saturating_sub(1) as usize),
        )
    }

    /// tmux in mouse mode wants SGR reports; otherwise clicks only focus.
    fn reports_mouse(&self) -> bool {
        let mode = self.mode();
        mode.intersects(TermMode::MOUSE_MODE) && mode.contains(TermMode::SGR_MOUSE)
    }

    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        if self.pty.is_none() {
            self.connect(cx);
            return;
        }
        if self.reports_mouse() {
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
        if self.mouse_cell.take().is_some() && self.reports_mouse() {
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
            || !self.reports_mouse()
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
        if self.reports_mouse() {
            let (col, line) = self.cell_at(event.position);
            let button = if lines > 0 {
                keys::MouseButton::WheelUp
            } else {
                keys::MouseButton::WheelDown
            };
            for _ in 0..lines.abs() {
                self.write(keys::sgr_mouse(button, false, col, line, true));
            }
        } else if mode.contains(TermMode::ALT_SCREEN) {
            let arrow: &[u8] = if lines > 0 { b"\x1b[A" } else { b"\x1b[B" };
            self.write(arrow.repeat(lines.unsigned_abs() as usize));
        } else if let Some(pty) = &self.pty {
            pty.term.lock().scroll_display(Scroll::Delta(lines));
            cx.notify();
        }
    }

    /// Cell size for the terminal font.
    fn measure(window: &mut Window) -> (Pixels, Pixels) {
        let text = window.text_system();
        let font_id = text.resolve_font(&font(FONT_FAMILY));
        let width = text
            .advance(font_id, px(FONT_SIZE), 'm')
            .map(|s| s.width)
            .unwrap_or(px(8.0));
        (width, px((FONT_SIZE * 1.3).round()))
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
        "muxbox-paste-{}-{}.{ext}",
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

/// One screen row: its text, styled runs, and background spans.
struct Row {
    text: String,
    runs: Vec<TextRun>,
    backgrounds: Vec<(usize, usize, Hsla)>,
}

struct Frame {
    rows: Vec<Row>,
    background: Hsla,
    cursor: Option<(usize, usize, CursorShape, Hsla)>,
}

/// Snapshots the grid under the lock so painting doesn't block the reader.
fn snapshot(pty: &Pty) -> Frame {
    let term = pty.term.lock();
    let content = term.renderable_content();
    let colors = content.colors;
    let default_bg = terminal::indexed(NamedColor::Background as usize, colors);
    let offset = content.display_offset as i32;
    let mut rows: Vec<Row> = (0..term.screen_lines())
        .map(|_| Row {
            text: String::new(),
            runs: vec![],
            backgrounds: vec![],
        })
        .collect();
    let base = font(FONT_FAMILY);
    for cell in content.display_iter {
        let Some(row) = usize::try_from(cell.point.line.0 + offset)
            .ok()
            .and_then(|i| rows.get_mut(i))
        else {
            continue;
        };
        let flags = cell.flags;
        let mut fg = terminal::resolve(cell.fg, colors);
        let mut bg = terminal::resolve(cell.bg, colors);
        if flags.contains(Flags::DIM) {
            fg = Rgb {
                r: fg.r / 3 * 2,
                g: fg.g / 3 * 2,
                b: fg.b / 3 * 2,
            };
        }
        if flags.contains(Flags::INVERSE) {
            std::mem::swap(&mut fg, &mut bg);
        }
        let col = cell.point.column.0;
        if bg != default_bg {
            match row.backgrounds.last_mut() {
                Some((_, end, color)) if *end == col && *color == hsla(bg) => *end = col + 1,
                _ => row.backgrounds.push((col, col + 1, hsla(bg))),
            }
        }
        // A wide character's spacer stays a space so later cells keep their column.
        let c = if flags.intersects(Flags::HIDDEN | Flags::WIDE_CHAR_SPACER) || cell.c == '\0' {
            ' '
        } else {
            cell.c
        };
        let mut run_font = base.clone();
        if flags.contains(Flags::BOLD) {
            run_font.weight = FontWeight::BOLD;
        }
        if flags.contains(Flags::ITALIC) {
            run_font.style = FontStyle::Italic;
        }
        let color = hsla(fg);
        let underline = flags
            .intersects(Flags::ALL_UNDERLINES)
            .then(|| UnderlineStyle {
                thickness: px(1.0),
                color: Some(color),
                wavy: flags.contains(Flags::UNDERCURL),
            });
        let strikethrough = flags
            .contains(Flags::STRIKEOUT)
            .then(|| StrikethroughStyle {
                thickness: px(1.0),
                color: Some(color),
            });
        let len = c.len_utf8();
        row.text.push(c);
        match row.runs.last_mut() {
            Some(r)
                if r.font == run_font
                    && r.color == color
                    && r.underline == underline
                    && r.strikethrough == strikethrough =>
            {
                r.len += len
            }
            _ => row.runs.push(TextRun {
                len,
                font: run_font,
                color,
                background_color: None,
                underline,
                strikethrough,
            }),
        }
    }
    let cursor = (content.mode.contains(TermMode::SHOW_CURSOR)
        && content.cursor.shape != CursorShape::Hidden)
        .then(|| {
            let line = usize::try_from(content.cursor.point.line.0 + offset).ok()?;
            let color = hsla(terminal::indexed(NamedColor::Cursor as usize, colors));
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
    cell: (Pixels, Pixels),
    focused: bool,
    window: &mut Window,
    cx: &mut App,
) {
    let (w, h) = cell;
    window.paint_quad(fill(bounds, frame.background));
    for (i, row) in frame.rows.iter().enumerate() {
        let y = bounds.origin.y + h * i as f32;
        for &(start, end, color) in &row.backgrounds {
            let origin = point(bounds.origin.x + w * start as f32, y);
            window.paint_quad(fill(
                Bounds::new(origin, size(w * (end - start) as f32, h)),
                color,
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
        let line =
            window
                .text_system()
                .shape_line(row.text.into(), px(FONT_SIZE), &row.runs, Some(w));
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
        let (w, h) = Self::measure(window);
        let focused = self.focus.is_focused(window);
        let view = cx.entity().downgrade();
        let bounds_cell = self.bounds.clone();
        let frame = self.pty.as_ref().map(snapshot);
        let grid = canvas(
            move |bounds, _, cx| {
                bounds_cell.set(bounds);
                let size = Size {
                    cols: (bounds.size.width / w).floor().max(1.0) as u16,
                    lines: (bounds.size.height / h).floor().max(1.0) as u16,
                    cell_width: f32::from(w).round() as u16,
                    cell_height: f32::from(h) as u16,
                };
                view.update(cx, |view, _| {
                    view.cell = size;
                    if let Some(pty) = &mut view.pty {
                        pty.resize(size);
                    }
                })
                .ok();
            },
            move |bounds, (), window, cx| {
                if let Some(frame) = frame {
                    paint(frame, bounds, (w, h), focused, window, cx);
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
            .bg(hsla(terminal::BACKGROUND))
            .on_key_down(cx.listener(Self::key_down))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_mouse_move(cx.listener(Self::mouse_move))
            .on_scroll_wheel(cx.listener(Self::scroll))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                this.upload(paths.paths().to_vec(), vec![], cx)
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
                        .bg(hsla(terminal::FOREGROUND).opacity(0.15))
                        .text_color(hsla(terminal::FOREGROUND))
                        .text_sm()
                        .child(status),
                )
            })
    }
}
