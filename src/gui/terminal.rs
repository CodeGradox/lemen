//! A PTY running a command (`tmux attach`, usually over SSH) whose output is
//! parsed by alacritty_terminal. Knows nothing about drawing.

use crate::config::{Terminal, TerminalColors, parse_hex};
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
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Palette {
    ansi: [Rgb; 16],
    pub foreground: Rgb,
    pub background: Rgb,
    cursor: Rgb,
}

/// One color a palette sets.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Slot {
    Foreground,
    Background,
    Cursor,
    /// 0-7 normal, 8-15 bright: black, red, green, yellow, blue, magenta,
    /// cyan, white.
    Ansi(usize),
}

/// Every slot with its name, as Settings lists them.
pub const SLOTS: [(Slot, &str); 19] = [
    (Slot::Background, "Background"),
    (Slot::Foreground, "Foreground"),
    (Slot::Cursor, "Cursor"),
    (Slot::Ansi(0), "Black"),
    (Slot::Ansi(1), "Red"),
    (Slot::Ansi(2), "Green"),
    (Slot::Ansi(3), "Yellow"),
    (Slot::Ansi(4), "Blue"),
    (Slot::Ansi(5), "Magenta"),
    (Slot::Ansi(6), "Cyan"),
    (Slot::Ansi(7), "White"),
    (Slot::Ansi(8), "Bright black"),
    (Slot::Ansi(9), "Bright red"),
    (Slot::Ansi(10), "Bright green"),
    (Slot::Ansi(11), "Bright yellow"),
    (Slot::Ansi(12), "Bright blue"),
    (Slot::Ansi(13), "Bright magenta"),
    (Slot::Ansi(14), "Bright cyan"),
    (Slot::Ansi(15), "Bright white"),
];

/// One Half Dark, as the WezTerm scheme of that name.
pub const DARK: Palette = Palette {
    ansi: [
        rgb(0x282c34),
        rgb(0xe17079),
        rgb(0x98c379),
        rgb(0xe5c07b),
        rgb(0x61afef),
        rgb(0xc678dd),
        rgb(0x56b6c2),
        rgb(0xdcdfe4),
        rgb(0x8c939f),
        rgb(0xe17079),
        rgb(0x98c379),
        rgb(0xe5c07b),
        rgb(0x61afef),
        rgb(0xc678dd),
        rgb(0x56b6c2),
        rgb(0xffffff),
    ],
    foreground: rgb(0xdcdfe4),
    background: rgb(0x282c34),
    cursor: rgb(0x979db4),
};

/// One Half Light, as the WezTerm scheme of that name.
pub const LIGHT: Palette = Palette {
    ansi: [
        rgb(0x383a42),
        rgb(0xce4136),
        rgb(0x318332),
        rgb(0x986801),
        rgb(0x356ce5),
        rgb(0xa626a4),
        rgb(0x007ab2),
        rgb(0xa0a1a7),
        rgb(0x696c77),
        rgb(0xce4136),
        rgb(0x318332),
        rgb(0x986801),
        rgb(0x356ce5),
        rgb(0xa626a4),
        rgb(0x007ab2),
        rgb(0xfafafa),
    ],
    foreground: rgb(0x383a42),
    background: rgb(0xfafafa),
    cursor: rgb(0x4f525e),
};

/// Sets one color of the dark or light palette, or with `None` puts lemen's
/// own back; the config keeps only what differs from lemen's own.
pub fn set_color(terminal: &mut Terminal, dark: bool, slot: Slot, color: Option<Rgb>) {
    let defaults = Palette::default_for(dark);
    let mut palette = Palette::themed(dark, terminal);
    palette.set(slot, color.unwrap_or(defaults.get(slot)));
    let colors = palette.overrides(defaults);
    if dark {
        terminal.dark = colors;
    } else {
        terminal.light = colors;
    }
}

/// `#rrggbb`.
pub fn hex(c: Rgb) -> String {
    format!("#{:02x}{:02x}{:02x}", c.r, c.g, c.b)
}

const fn rgb(hex: u32) -> Rgb {
    Rgb {
        r: (hex >> 16) as u8,
        g: (hex >> 8) as u8,
        b: hex as u8,
    }
}

impl Palette {
    /// lemen's own colors for dark or light.
    pub fn default_for(dark: bool) -> &'static Palette {
        if dark { &DARK } else { &LIGHT }
    }

    /// The colors for dark or light: Settings' over lemen's own.
    pub fn themed(dark: bool, terminal: &Terminal) -> Palette {
        let mut palette = *Palette::default_for(dark);
        let colors = if dark {
            &terminal.dark
        } else {
            &terminal.light
        };
        let parse = |hex: &Option<String>| hex.as_deref().and_then(parse_hex).map(rgb);
        let slots = [
            (Slot::Foreground, &colors.foreground),
            (Slot::Background, &colors.background),
            (Slot::Cursor, &colors.cursor),
        ];
        for (slot, hex) in slots {
            if let Some(c) = parse(hex) {
                palette.set(slot, c);
            }
        }
        for (offset, list) in [(0, &colors.ansi), (8, &colors.brights)] {
            for (i, hex) in list.iter().flatten().take(8).enumerate() {
                if let Some(c) = parse_hex(hex).map(rgb) {
                    palette.set(Slot::Ansi(offset + i), c);
                }
            }
        }
        palette
    }

    pub fn get(&self, slot: Slot) -> Rgb {
        match slot {
            Slot::Foreground => self.foreground,
            Slot::Background => self.background,
            Slot::Cursor => self.cursor,
            Slot::Ansi(i) => self.ansi[i],
        }
    }

    pub fn set(&mut self, slot: Slot, color: Rgb) {
        match slot {
            Slot::Foreground => self.foreground = color,
            Slot::Background => self.background = color,
            Slot::Cursor => self.cursor = color,
            Slot::Ansi(i) => self.ansi[i] = color,
        }
    }

    /// What differs from `defaults`, as the config writes it.
    fn overrides(&self, defaults: &Palette) -> TerminalColors {
        let differs = |slot| (self.get(slot) != defaults.get(slot)).then(|| hex(self.get(slot)));
        let eight = |offset: usize| {
            (offset..offset + 8)
                .any(|i| self.ansi[i] != defaults.ansi[i])
                .then(|| {
                    self.ansi[offset..offset + 8]
                        .iter()
                        .map(|c| hex(*c))
                        .collect()
                })
        };
        TerminalColors {
            foreground: differs(Slot::Foreground),
            background: differs(Slot::Background),
            cursor: differs(Slot::Cursor),
            ansi: eight(0),
            brights: eight(8),
        }
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
    use crate::test_support::wait_until;
    use alacritty_terminal::vte::ansi::NamedColor;
    use std::time::Duration;

    const SIZE: Size = Size {
        cols: 40,
        lines: 5,
        cell_width: 8,
        cell_height: 16,
    };

    /// Starts `sh -c script` in a PTY.
    fn spawn(script: &str) -> Pty {
        let argv: Vec<String> = ["sh", "-c", script].iter().map(|s| s.to_string()).collect();
        Pty::spawn(&argv, SIZE).unwrap().0
    }

    fn shows(pty: &Pty, needle: &str) -> bool {
        wait_until(Duration::from_secs(5), || screen_text(pty).contains(needle))
    }

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
            Palette::default_for(false).resolve(Color::Named(NamedColor::Background), &colors),
            LIGHT.background
        );
        // Dimming moves toward the background in both appearances.
        assert!(LIGHT.dim(rgb(0x000000)).r > 0);
        assert!(DARK.dim(rgb(0xffffff)).r < 255);
    }

    #[test]
    fn settings_colors_go_over_lemens_own() {
        let mut terminal = Terminal::default();
        assert_eq!(Palette::themed(true, &terminal), DARK);
        set_color(&mut terminal, true, Slot::Background, Some(rgb(0x000000)));
        set_color(&mut terminal, true, Slot::Ansi(9), Some(rgb(0xff0000)));
        let dark = Palette::themed(true, &terminal);
        assert_eq!(dark.background, rgb(0x000000));
        assert_eq!(dark.get(Slot::Ansi(9)), rgb(0xff0000));
        assert_eq!(dark.get(Slot::Ansi(1)), DARK.get(Slot::Ansi(1)));
        // Only what differs is kept, and light is left alone.
        assert_eq!(terminal.dark.background.as_deref(), Some("#000000"));
        assert_eq!(
            (
                terminal.dark.foreground.as_ref(),
                terminal.dark.ansi.as_ref()
            ),
            (None, None)
        );
        assert_eq!(
            terminal.dark.brights.as_ref().map(|b| b[1].as_str()),
            Some("#ff0000")
        );
        assert_eq!(Palette::themed(false, &terminal), LIGHT);
        // Back to lemen's own empties it again.
        set_color(&mut terminal, true, Slot::Background, None);
        set_color(&mut terminal, true, Slot::Ansi(9), None);
        assert_eq!(terminal.dark, TerminalColors::default());
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
        let pty = spawn("printf 'hello\\n'; read x; printf 'got %s' \"$x\"; sleep 5");
        assert!(shows(&pty, "hello"), "output: {:?}", screen_text(&pty));
        pty.write(b"abc\r".as_slice());
        assert!(shows(&pty, "got abc"), "output: {:?}", screen_text(&pty));
    }

    #[test]
    fn a_running_program_sees_resizes() {
        // Prints its size at start and on every SIGWINCH.
        let mut pty = spawn("trap 'stty size' WINCH; stty size; while :; do sleep 0.05; done");
        assert!(shows(&pty, "5 40"), "output: {:?}", screen_text(&pty));
        pty.resize(Size {
            cols: 60,
            lines: 8,
            ..SIZE
        });
        assert!(shows(&pty, "8 60"), "output: {:?}", screen_text(&pty));
    }
}
