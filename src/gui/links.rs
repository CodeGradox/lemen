//! Links in the terminal, found where the pointer is: an OSC 8 hyperlink's
//! URI, or else a URL written out in the text.

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::term::Term;
use alacritty_terminal::term::cell::Flags;

/// A link on screen and where it goes.
#[derive(Clone, Debug, PartialEq)]
pub struct Link {
    pub url: String,
    /// Where it's drawn: one (screen line, first column, end column) a row.
    pub spans: Vec<(usize, usize, usize)>,
}

/// The link at screen column `col` of screen line `line`, if any. A URL
/// can run on across rows the terminal wrapped, though not across rows
/// a program broke itself.
pub fn link_at<T>(term: &Term<T>, col: usize, line: usize) -> Option<Link> {
    let grid = term.grid();
    let offset = grid.display_offset() as i32;
    let last = Column(grid.columns().checked_sub(1)?);
    let wrapped = |line: i32| grid[Line(line)][last].flags.contains(Flags::WRAPLINE);
    let at = Line(line as i32 - offset);
    if at < grid.topmost_line() || at > grid.bottommost_line() {
        return None;
    }
    // The row's whole logical line, as the terminal wrapped it.
    let mut first = at.0;
    while first > grid.topmost_line().0 && wrapped(first - 1) {
        first -= 1;
    }
    let mut end = at.0;
    while end < grid.bottommost_line().0 && wrapped(end) {
        end += 1;
    }
    // Its characters and where each is; the spacer after a wide one isn't one.
    let mut cells: Vec<(char, Point, usize)> = vec![];
    for row in first..=end {
        for column in 0..grid.columns() {
            let point = Point::new(Line(row), Column(column));
            let cell = &grid[point];
            if cell
                .flags
                .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
            {
                continue;
            }
            let width = if cell.flags.contains(Flags::WIDE_CHAR) {
                2
            } else {
                1
            };
            let c = if cell.c == '\0' { ' ' } else { cell.c };
            cells.push((c, point, width));
        }
    }
    let pointer = Point::new(at, Column(col));
    let here = cells.iter().rposition(|(_, point, _)| *point <= pointer)?;
    let (start, stop, url) = match grid[cells[here].1].hyperlink() {
        // Any program's output sets these, with text that needn't match:
        // only web links open, as for a URL written out.
        Some(link) if !is_web(link.uri()) => return None,
        Some(link) => {
            let same = |i: &usize| grid[cells[*i].1].hyperlink().as_ref() == Some(&link);
            let start = (0..here).rev().take_while(same).last().unwrap_or(here);
            let stop = (here..cells.len()).take_while(same).last()? + 1;
            (start, stop, link.uri().to_string())
        }
        None => {
            let text: Vec<char> = cells.iter().map(|(c, _, _)| *c).collect();
            let (start, stop) = url_at(&text, here)?;
            (start, stop, text[start..stop].iter().collect())
        }
    };
    let mut spans: Vec<(usize, usize, usize)> = vec![];
    for &(_, point, width) in &cells[start..stop] {
        let Ok(screen) = usize::try_from(point.line.0 + offset) else {
            continue;
        };
        if screen >= grid.screen_lines() {
            continue;
        }
        let (column, until) = (point.column.0, point.column.0 + width);
        match spans.last_mut() {
            Some((line, _, end)) if *line == screen => *end = until,
            _ => spans.push((screen, column, until)),
        }
    }
    Some(Link { url, spans })
}

const SCHEMES: [&str; 2] = ["https://", "http://"];

/// Whether `uri` is a web link, whatever the case of its scheme.
fn is_web(uri: &str) -> bool {
    SCHEMES.iter().any(|scheme| {
        uri.get(..scheme.len())
            .is_some_and(|start| start.eq_ignore_ascii_case(scheme))
    })
}

/// The URL written out in `text` over the character at `at`: its first
/// character and the one after its last.
pub(super) fn url_at(text: &[char], at: usize) -> Option<(usize, usize)> {
    let mut from = 0;
    while let Some((start, scheme)) = next_scheme(text, from) {
        let mut end = start + scheme;
        while end < text.len() && !ends_url(text[end]) {
            end += 1;
        }
        let end = start + trimmed(&text[start..end]);
        if end > start + scheme && (start..end).contains(&at) {
            return Some((start, end));
        }
        from = end.max(start + 1);
    }
    None
}

/// Where the next `http://` or `https://` from `from` starts, and its length.
fn next_scheme(text: &[char], from: usize) -> Option<(usize, usize)> {
    (from..text.len()).find_map(|i| {
        SCHEMES.iter().find_map(|scheme| {
            let len = scheme.chars().count();
            let matches =
                text.len() >= i + len && text[i..i + len].iter().copied().eq(scheme.chars());
            matches.then_some((i, len))
        })
    })
}

/// Whether `c` can't be in a URL written out: spaces, quotes, and the
/// lines programs draw boxes with.
fn ends_url(c: char) -> bool {
    c.is_whitespace()
        || c.is_control()
        || "<>\"'`{}|\\^".contains(c)
        || ('\u{2500}'..='\u{257f}').contains(&c)
}

/// How much of `url` is left without what trails it in prose: a full stop
/// or comma, or a closing bracket that opens before it, as in Markdown's
/// `[text](url)`.
fn trimmed(url: &[char]) -> usize {
    let mut end = url.len();
    while let Some(&last) = url[..end].last() {
        let count = |c: char| url[..end].iter().filter(|&&x| x == c).count();
        let unopened = |open: char, close: char| last == close && count(open) < count(close);
        if ".,;:!?".contains(last) || unopened('(', ')') || unopened('[', ']') {
            end -= 1;
        } else {
            break;
        }
    }
    end
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gui::terminal::{Pty, Size};
    use crate::test_support::wait_until;
    use std::time::Duration;

    /// The URL in `text` at the first character of `at`.
    fn url(text: &str, at: &str) -> Option<String> {
        let chars: Vec<char> = text.chars().collect();
        let at = text[..text.find(at).expect(at)].chars().count();
        url_at(&chars, at).map(|(start, end)| chars[start..end].iter().collect())
    }

    #[test]
    fn urls_end_where_prose_does() {
        let text = "See https://example.com/a?b=c. Or (http://x.io/y), or [docs](https://d.dev/p).";
        assert_eq!(
            url(text, "example").as_deref(),
            Some("https://example.com/a?b=c")
        );
        assert_eq!(url(text, "x.io").as_deref(), Some("http://x.io/y"));
        assert_eq!(url(text, "d.dev").as_deref(), Some("https://d.dev/p"));
        assert_eq!(url(text, "See"), None);
        assert_eq!(url(text, "docs"), None);
        // Brackets a URL opens are its own.
        let wiki = "https://en.wikipedia.org/wiki/Rust_(programming_language)";
        assert_eq!(url(wiki, "wiki").as_deref(), Some(wiki));
        assert_eq!(
            url("│ https://a.b/c│", "a.b").as_deref(),
            Some("https://a.b/c")
        );
        assert_eq!(url("https:// nothing", "https"), None);
    }

    /// A shell printing `script` in a 30-column terminal, once `shown` shows.
    fn printed(script: &str, shown: &str) -> Pty {
        let argv: Vec<String> = ["sh", "-c", script].iter().map(|s| s.to_string()).collect();
        let size = Size {
            cols: 30,
            lines: 6,
            cell_width: 8,
            cell_height: 16,
        };
        let pty = Pty::spawn(&argv, size).unwrap().0;
        let text = |pty: &Pty| {
            let term = pty.term.lock();
            let grid = term.grid();
            (0..grid.screen_lines())
                .map(|line| {
                    (0..grid.columns())
                        .map(|col| grid[Line(line as i32)][Column(col)].c)
                        .collect::<String>()
                })
                .collect::<String>()
        };
        assert!(
            wait_until(Duration::from_secs(5), || text(&pty).contains(shown)),
            "{:?}",
            text(&pty)
        );
        pty
    }

    #[test]
    fn a_url_runs_on_across_wrapped_rows() {
        let pty = printed(
            "printf 'go https://example.com/a/long/path/here now'; sleep 5",
            "now",
        );
        let term = pty.term.lock();
        let link = link_at(&term, 2, 1).expect("on the wrapped row");
        assert_eq!(link.url, "https://example.com/a/long/path/here");
        assert_eq!(link.spans, [(0, 3, 30), (1, 0, 9)]);
        assert_eq!(link_at(&term, 0, 0), None);
    }

    #[test]
    fn an_osc_8_link_goes_to_its_uri() {
        let pty = printed(
            r"printf 'read \033]8;;https://lemur.dev/docs\033\\the docs\033]8;;\033\\ now'; sleep 5",
            "now",
        );
        let term = pty.term.lock();
        let link = link_at(&term, 7, 0).expect("on the text");
        assert_eq!(link.url, "https://lemur.dev/docs");
        assert_eq!(link.spans, [(0, 5, 13)]);
        assert_eq!(link_at(&term, 14, 0), None);
    }

    /// Text can hide any URI; only a web link's opens.
    #[test]
    fn an_osc_8_link_elsewhere_doesnt_open() {
        let pty = printed(
            r"printf '\033]8;;file:///etc/passwd\033\\safe\033]8;;\033\\ \033]8;;HTTPS://x.dev\033\\web\033]8;;\033\\ end'; sleep 5",
            "end",
        );
        let term = pty.term.lock();
        assert_eq!(link_at(&term, 1, 0), None);
        let web = link_at(&term, 6, 0).expect("on the web link");
        assert_eq!(web.url, "HTTPS://x.dev");
        assert!(is_web("http://a") && !is_web("javascript:alert(1)") && !is_web("ht"));
    }
}
