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

/// Terminal colors for one appearance. Programs can override any entry
/// with OSC 4/10/11.
pub struct Palette {
    ansi: [Rgb; 16],
    pub foreground: Rgb,
    pub background: Rgb,
    cursor: Rgb,
}

pub const DARK: Palette = Palette {
    ansi: [
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
    ],
    foreground: rgb(0xd4d4d4),
    background: rgb(0x1e1e1e),
    cursor: rgb(0xd4d4d4),
};

pub const LIGHT: Palette = Palette {
    ansi: [
        rgb(0x383a42),
        rgb(0xe45649),
        rgb(0x50a14f),
        rgb(0xc18401),
        rgb(0x4078f2),
        rgb(0xa626a4),
        rgb(0x0184bc),
        rgb(0xa0a1a7),
        rgb(0x696c77),
        rgb(0xca1243),
        rgb(0x3d8f3c),
        rgb(0x986801),
        rgb(0x2a63d9),
        rgb(0x8a1d8a),
        rgb(0x0f7493),
        rgb(0xd4d4d4),
    ],
    foreground: rgb(0x383a42),
    background: rgb(0xfafafa),
    cursor: rgb(0x526fff),
};

const fn rgb(hex: u32) -> Rgb {
    Rgb {
        r: (hex >> 16) as u8,
        g: (hex >> 8) as u8,
        b: hex as u8,
    }
}

impl Palette {
    pub fn for_appearance(dark: bool) -> &'static Palette {
        if dark { &DARK } else { &LIGHT }
    }

    /// Faint text: a third of the way toward the background, so it works on
    /// light and dark alike.
    pub fn dim(&self, c: Rgb) -> Rgb {
        let mix = |a: u8, b: u8| ((a as u16 * 2 + b as u16) / 3) as u8;
        let bg = self.background;
        Rgb {
            r: mix(c.r, bg.r),
            g: mix(c.g, bg.g),
            b: mix(c.b, bg.b),
        }
    }

    /// Resolves a cell color: program overrides first, then the palette.
    pub fn resolve(&self, color: Color, colors: &Colors) -> Rgb {
        match color {
            Color::Spec(rgb) => rgb,
            Color::Named(n) => self.indexed(n as usize, colors),
            Color::Indexed(i) => self.indexed(i as usize, colors),
        }
    }

    pub fn indexed(&self, i: usize, colors: &Colors) -> Rgb {
        colors[i].unwrap_or_else(|| self.slot(i))
    }

    /// xterm's 256 colors, then alacritty's named slots (foreground = 256, ...).
    fn slot(&self, i: usize) -> Rgb {
        const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
        match i {
            0..16 => self.ansi[i],
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
            257 => self.background,
            258 => self.cursor,
            259..=266 => self.dim(self.ansi[i - 259]),
            268 => self.dim(self.foreground),
            // Foreground (256) and bright foreground (267).
            _ => self.foreground,
        }
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
        let p = &DARK;
        assert_eq!(
            p.resolve(Color::Indexed(16), &colors),
            Rgb { r: 0, g: 0, b: 0 }
        );
        assert_eq!(
            p.resolve(Color::Indexed(231), &colors),
            Rgb {
                r: 255,
                g: 255,
                b: 255
            }
        );
        assert_eq!(
            p.resolve(Color::Indexed(232), &colors),
            Rgb { r: 8, g: 8, b: 8 }
        );
        assert_eq!(
            p.resolve(Color::Named(NamedColor::Background), &colors),
            DARK.background
        );
        assert_eq!(
            p.resolve(Color::Named(NamedColor::Red), &colors),
            DARK.ansi[1]
        );
    }

    #[test]
    fn light_palette_and_dimming() {
        let colors = Colors::default();
        assert_eq!(
            Palette::for_appearance(false).resolve(Color::Named(NamedColor::Background), &colors),
            LIGHT.background
        );
        // Dimming moves toward the background in both appearances.
        assert!(LIGHT.dim(rgb(0x000000)).r > 0);
        assert!(DARK.dim(rgb(0xffffff)).r < 255);
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

    #[test]
    fn a_running_program_sees_resizes() {
        let size = Size {
            cols: 40,
            lines: 5,
            cell_width: 8,
            cell_height: 16,
        };
        // Prints its size at start and on every SIGWINCH.
        let argv: Vec<String> = [
            "sh",
            "-c",
            "trap 'stty size' WINCH; stty size; while :; do sleep 0.05; done",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let (mut pty, _events) = Pty::spawn(&argv, size).unwrap();
        let wait_for = |pty: &Pty, needle: &str| {
            let deadline = Instant::now() + Duration::from_secs(5);
            while Instant::now() < deadline {
                if screen_text(pty).contains(needle) {
                    return true;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            false
        };
        assert!(wait_for(&pty, "5 40"), "output: {:?}", screen_text(&pty));
        pty.resize(Size {
            cols: 60,
            lines: 8,
            ..size
        });
        assert!(wait_for(&pty, "8 60"), "output: {:?}", screen_text(&pty));
    }
}
