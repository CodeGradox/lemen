//! A PTY running a command (`tmux attach`, usually over SSH) whose output is
//! parsed by alacritty_terminal. Knows nothing about drawing.

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, EventLoopSender, Msg};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::tty;
use alacritty_terminal::vte::ansi::{Color, Rgb};
use anyhow::{Context, Result};
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Clone)]
pub struct Listener(UnboundedSender<Event>);

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        let _ = self.0.unbounded_send(event);
    }
}

/// Grid size in cells, plus the cell size in pixels (for programs that ask).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Size {
    pub cols: u16,
    pub lines: u16,
    pub cell_width: u16,
    pub cell_height: u16,
}

impl Size {
    pub fn window(self) -> WindowSize {
        WindowSize {
            num_lines: self.lines,
            num_cols: self.cols,
            cell_width: self.cell_width,
            cell_height: self.cell_height,
        }
    }
}

impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.lines as usize
    }

    fn screen_lines(&self) -> usize {
        self.lines as usize
    }

    fn columns(&self) -> usize {
        self.cols as usize
    }
}

pub struct Pty {
    pub term: Arc<FairMutex<Term<Listener>>>,
    sender: EventLoopSender,
    size: Size,
}

impl Pty {
    /// Starts `argv` in a new PTY. Terminal events arrive on the receiver.
    pub fn spawn(argv: &[String], size: Size) -> Result<(Pty, UnboundedReceiver<Event>)> {
        let (tx, rx) = unbounded();
        let listener = Listener(tx);
        let mut env = HashMap::from([
            ("TERM".into(), "xterm-256color".into()),
            ("COLORTERM".into(), "truecolor".into()),
        ]);
        // Apps started from Finder get no locale; ssh forwards LANG to the host.
        let utf8 = ["LC_ALL", "LC_CTYPE", "LANG"]
            .iter()
            .any(|v| std::env::var(v).is_ok_and(|s| s.to_uppercase().contains("UTF-8")));
        if !utf8 {
            env.insert("LANG".into(), "en_US.UTF-8".into());
        }
        let options = tty::Options {
            shell: Some(tty::Shell::new(argv[0].clone(), argv[1..].to_vec())),
            working_directory: None,
            drain_on_exit: true,
            env,
        };
        let pty = tty::new(&options, size.window(), 0).context("opening a pty")?;
        let term = Term::new(Config::default(), &size, listener.clone());
        let term = Arc::new(FairMutex::new(term));
        let event_loop = EventLoop::new(term.clone(), listener, pty, true, false)
            .context("starting the pty reader")?;
        let sender = event_loop.channel();
        event_loop.spawn();
        Ok((Pty { term, sender, size }, rx))
    }

    pub fn write(&self, bytes: impl Into<Cow<'static, [u8]>>) {
        let _ = self.sender.send(Msg::Input(bytes.into()));
    }

    pub fn resize(&mut self, size: Size) {
        if size != self.size && size.cols > 0 && size.lines > 0 {
            self.size = size;
            let _ = self.sender.send(Msg::Resize(size.window()));
            self.term.lock().resize(size);
        }
    }

    pub fn size(&self) -> Size {
        self.size
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        let _ = self.sender.send(Msg::Shutdown);
    }
}

// A dark palette. Programs can override any entry with OSC 4/10/11.
const ANSI: [Rgb; 16] = [
    rgb(0x282c34),
    rgb(0xe06c75),
    rgb(0x98c379),
    rgb(0xe5c07b),
    rgb(0x61afef),
    rgb(0xc678dd),
    rgb(0x56b6c2),
    rgb(0xabb2bf),
    rgb(0x5c6370),
    rgb(0xef7b85),
    rgb(0xa5d68a),
    rgb(0xf0cd8a),
    rgb(0x74bdf7),
    rgb(0xd38be8),
    rgb(0x67c9d6),
    rgb(0xe6e6e6),
];
pub const FOREGROUND: Rgb = rgb(0xd4d4d4);
pub const BACKGROUND: Rgb = rgb(0x1e1e1e);

const fn rgb(hex: u32) -> Rgb {
    Rgb {
        r: (hex >> 16) as u8,
        g: (hex >> 8) as u8,
        b: hex as u8,
    }
}

fn dim(c: Rgb) -> Rgb {
    Rgb {
        r: (c.r as u16 * 2 / 3) as u8,
        g: (c.g as u16 * 2 / 3) as u8,
        b: (c.b as u16 * 2 / 3) as u8,
    }
}

/// Resolves a cell color: program overrides first, then the palette.
pub fn resolve(color: Color, colors: &Colors) -> Rgb {
    match color {
        Color::Spec(rgb) => rgb,
        Color::Named(n) => indexed(n as usize, colors),
        Color::Indexed(i) => indexed(i as usize, colors),
    }
}

pub fn indexed(i: usize, colors: &Colors) -> Rgb {
    colors[i].unwrap_or_else(|| palette(i))
}

/// xterm's 256 colors, then alacritty's named slots (foreground = 256, ...).
fn palette(i: usize) -> Rgb {
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    match i {
        0..16 => ANSI[i],
        16..232 => {
            let i = i - 16;
            Rgb {
                r: LEVELS[i / 36],
                g: LEVELS[(i / 6) % 6],
                b: LEVELS[i % 6],
            }
        }
        232..=255 => {
            let v = 8 + 10 * (i - 232) as u8;
            Rgb { r: v, g: v, b: v }
        }
        257 => BACKGROUND,
        259..=266 => dim(ANSI[i - 259]),
        268 => dim(FOREGROUND),
        // Foreground (256), cursor (258) and bright foreground (267).
        _ => FOREGROUND,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alacritty_terminal::vte::ansi::NamedColor;
    use std::time::{Duration, Instant};

    #[test]
    fn palette_covers_the_xterm_ranges() {
        let colors = Colors::default();
        assert_eq!(
            resolve(Color::Indexed(16), &colors),
            Rgb { r: 0, g: 0, b: 0 }
        );
        assert_eq!(
            resolve(Color::Indexed(231), &colors),
            Rgb {
                r: 255,
                g: 255,
                b: 255
            }
        );
        assert_eq!(
            resolve(Color::Indexed(232), &colors),
            Rgb { r: 8, g: 8, b: 8 }
        );
        assert_eq!(
            resolve(Color::Named(NamedColor::Background), &colors),
            BACKGROUND
        );
        assert_eq!(resolve(Color::Named(NamedColor::Red), &colors), ANSI[1]);
    }

    fn screen_text(pty: &Pty) -> String {
        let term = pty.term.lock();
        let mut out = String::new();
        let mut last_line = None;
        for cell in term.grid().display_iter() {
            if last_line.is_some_and(|l| l != cell.point.line) {
                out.push('\n');
            }
            last_line = Some(cell.point.line);
            out.push(cell.c);
        }
        out
    }

    #[test]
    fn spawned_output_reaches_the_grid_and_input_reaches_the_program() {
        let size = Size {
            cols: 40,
            lines: 5,
            cell_width: 8,
            cell_height: 16,
        };
        let argv: Vec<String> = [
            "sh",
            "-c",
            "printf 'hello\\n'; read x; printf 'got %s' \"$x\"; sleep 5",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let (pty, _events) = Pty::spawn(&argv, size).unwrap();
        let wait_for = |needle: &str| {
            let deadline = Instant::now() + Duration::from_secs(5);
            while Instant::now() < deadline {
                if screen_text(&pty).contains(needle) {
                    return true;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            false
        };
        assert!(wait_for("hello"), "output: {:?}", screen_text(&pty));
        pty.write(b"abc\r".as_slice());
        assert!(wait_for("got abc"), "output: {:?}", screen_text(&pty));
    }
}
