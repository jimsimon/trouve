//! Terminal widgets for Slint.
//!
//! Two components with matching Rust-side state helpers:
//! - `TerminalView` + [`Scrollback`]: append-only scrollback for command
//!   output / build logs (basic SGR colors, everything else stripped).
//! - `TerminalGrid` + [`GridState`]: a full interactive terminal screen
//!   backed by a vt100 parser — colors, cursor, alternate screen,
//!   scrollback paging — plus [`encode_key`]/[`encode_paste`] to turn UI
//!   key events into the byte sequences a PTY expects.

slint::include_modules!();

use std::rc::Rc;

use slint::{Color, Model, ModelRc, SharedString, VecModel};

/// Path to the crate's `.slint` sources.
pub const UI_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/ui");

const PALETTE: [u32; 16] = [
    0x000000, 0xcd3131, 0x0dbc79, 0xe5e510, 0x2472c8, 0xbc3fbc, 0x11a8cd, 0xe5e5e5, // normal
    0x666666, 0xf14c4c, 0x23d18b, 0xf5f543, 0x3b8eea, 0xd670d6, 0x29b8db, 0xffffff, // bright
];

/// One parsed scrollback line: (text, rgb-or-zero) segments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnsiSegment {
    pub text: String,
    /// 0 = default foreground.
    pub color: u32,
}

/// Parse one line of terminal output, honouring SGR color codes (30-37,
/// 90-97, 39, 0) and stripping every other CSI sequence.
pub fn parse_ansi_line(line: &str) -> Vec<AnsiSegment> {
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut color = 0u32;
    let mut chars = line.chars().peekable();

    let flush = |segments: &mut Vec<AnsiSegment>, text: &mut String, color: u32| {
        if !text.is_empty() {
            segments.push(AnsiSegment {
                text: std::mem::take(text),
                color,
            });
        }
    };

    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            current.push(c);
            continue;
        }
        // ESC [ params letter
        if chars.peek() != Some(&'[') {
            continue;
        }
        chars.next();
        let mut params = String::new();
        let mut command = ' ';
        for c in chars.by_ref() {
            if c.is_ascii_alphabetic() {
                command = c;
                break;
            }
            params.push(c);
        }
        if command != 'm' {
            continue; // strip cursor movement etc.
        }
        flush(&mut segments, &mut current, color);
        for code in params.split(';') {
            match code.parse::<u32>().unwrap_or(0) {
                0 | 39 => color = 0,
                n @ 30..=37 => color = PALETTE[(n - 30) as usize],
                n @ 90..=97 => color = PALETTE[(n - 90 + 8) as usize],
                _ => {} // bold/underline/background: ignored in the scrollback view
            }
        }
    }
    flush(&mut segments, &mut current, color);
    if segments.is_empty() {
        segments.push(AnsiSegment {
            text: String::new(),
            color: 0,
        });
    }
    segments
}

fn to_widget(seg: &AnsiSegment) -> TermSegment {
    TermSegment {
        text: SharedString::from(seg.text.as_str()),
        color: Color::from_argb_encoded(0xff00_0000 | seg.color),
    }
}

/// Growable scrollback buffer backing the widget's model, with a line cap.
pub struct Scrollback {
    model: Rc<VecModel<ModelRc<TermSegment>>>,
    partial: String,
    max_lines: usize,
}

impl Scrollback {
    pub fn new(max_lines: usize) -> Self {
        Self {
            model: Rc::new(VecModel::default()),
            partial: String::new(),
            max_lines,
        }
    }

    pub fn model(&self) -> ModelRc<ModelRc<TermSegment>> {
        ModelRc::from(self.model.clone())
    }

    pub fn line_count(&self) -> usize {
        self.model.row_count()
    }

    /// Feed raw output (possibly mid-line). Complete lines are appended to
    /// the model; the trailing partial line is buffered until its newline.
    pub fn push(&mut self, output: &str) {
        self.partial.push_str(output);
        while let Some(pos) = self.partial.find('\n') {
            let line: String = self.partial.drain(..=pos).collect();
            let segments: Vec<TermSegment> = parse_ansi_line(line.trim_end_matches(['\n', '\r']))
                .iter()
                .map(to_widget)
                .collect();
            self.model.push(ModelRc::new(VecModel::from(segments)));
            while self.model.row_count() > self.max_lines {
                self.model.remove(0);
            }
        }
    }
}

// --- interactive grid (TerminalGrid) ---------------------------------------

/// One run of same-styled cells on a grid row. Colors are resolved RGB
/// (0xRRGGBB); the defaults passed to [`GridState::rows`] are already baked
/// in, so the widget just paints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GridSpan {
    pub text: String,
    pub fg: u32,
    pub bg: u32,
    /// Whether `bg` differs from the default background (lets the widget
    /// skip painting cell backgrounds for the common case).
    pub has_bg: bool,
}

/// Screen state for `TerminalGrid`: a vt100 parser plus scrollback paging.
pub struct GridState {
    parser: vt100::Parser,
}

impl GridState {
    pub fn new(rows: u16, cols: u16, scrollback: usize) -> Self {
        Self {
            parser: vt100::Parser::new(rows.max(1), cols.max(1), scrollback),
        }
    }

    /// Feed raw PTY output (bytes, possibly mid-escape or mid-UTF-8).
    pub fn process(&mut self, bytes: &[u8]) {
        self.parser.process(bytes);
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        self.parser.screen_mut().set_size(rows.max(1), cols.max(1));
    }

    pub fn size(&self) -> (u16, u16) {
        self.parser.screen().size()
    }

    /// Page through history: positive `lines` scrolls back (older output),
    /// negative towards live. Clamped by the parser.
    pub fn scroll_lines(&mut self, lines: i32) {
        let cur = self.parser.screen().scrollback() as i64;
        let next = (cur + lines as i64).max(0) as usize;
        self.parser.screen_mut().set_scrollback(next);
    }

    /// Back to the live view (offset 0).
    pub fn scroll_to_live(&mut self) {
        self.parser.screen_mut().set_scrollback(0);
    }

    pub fn scrollback_offset(&self) -> usize {
        self.parser.screen().scrollback()
    }

    /// Cursor (row, col), or `None` while hidden or scrolled into history.
    pub fn cursor(&self) -> Option<(u16, u16)> {
        let screen = self.parser.screen();
        if screen.hide_cursor() || screen.scrollback() > 0 {
            return None;
        }
        Some(screen.cursor_position())
    }

    pub fn application_cursor(&self) -> bool {
        self.parser.screen().application_cursor()
    }

    pub fn bracketed_paste(&self) -> bool {
        self.parser.screen().bracketed_paste()
    }

    /// Plain text of the visible screen (for copy).
    pub fn contents(&self) -> String {
        self.parser.screen().contents()
    }

    /// The visible screen as styled spans, one `Vec` per row. Default
    /// foreground/background (`0xRRGGBB`) are baked in so the widget needs
    /// no theme knowledge.
    pub fn rows(&self, default_fg: u32, default_bg: u32) -> Vec<Vec<GridSpan>> {
        let screen = self.parser.screen();
        let (rows, cols) = screen.size();
        let mut out = Vec::with_capacity(rows as usize);
        for r in 0..rows {
            let mut spans: Vec<GridSpan> = Vec::new();
            for c in 0..cols {
                let Some(cell) = screen.cell(r, c) else { break };
                if cell.is_wide_continuation() {
                    continue;
                }
                let mut fg = resolve_color(cell.fgcolor(), cell.bold(), default_fg);
                let mut bg = resolve_color(cell.bgcolor(), false, default_bg);
                if cell.inverse() {
                    std::mem::swap(&mut fg, &mut bg);
                }
                let text = match cell.contents() {
                    "" => " ",
                    t => t,
                };
                match spans.last_mut() {
                    Some(last) if last.fg == fg && last.bg == bg => last.text.push_str(text),
                    _ => spans.push(GridSpan {
                        text: text.to_string(),
                        fg,
                        bg,
                        has_bg: bg != default_bg,
                    }),
                }
            }
            // Trailing default-styled blanks are noise: strip them (and any
            // spans they empty out), keeping at least one span per row.
            while let Some(last) = spans.last_mut() {
                if last.fg != default_fg || last.has_bg {
                    break;
                }
                last.text.truncate(last.text.trim_end_matches(' ').len());
                if last.text.is_empty() && spans.len() > 1 {
                    spans.pop();
                } else {
                    break;
                }
            }
            if spans.is_empty() {
                spans.push(GridSpan {
                    text: String::new(),
                    fg: default_fg,
                    bg: default_bg,
                    has_bg: false,
                });
            }
            out.push(spans);
        }
        out
    }
}

/// vt100 color → resolved RGB, brightening the low 8 indexed colors under
/// bold (classic xterm behavior).
fn resolve_color(color: vt100::Color, bold: bool, default: u32) -> u32 {
    match color {
        vt100::Color::Default => default,
        vt100::Color::Idx(i) => {
            let i = if bold && i < 8 { i + 8 } else { i };
            match i {
                0..=15 => PALETTE[i as usize],
                16..=231 => {
                    // 6x6x6 color cube.
                    let i = i - 16;
                    let comp = |v: u8| if v == 0 { 0u32 } else { 55 + v as u32 * 40 };
                    let (r, g, b) = (i / 36, (i / 6) % 6, i % 6);
                    (comp(r) << 16) | (comp(g) << 8) | comp(b)
                }
                _ => {
                    let v = 8 + (i as u32 - 232) * 10;
                    (v << 16) | (v << 8) | v
                }
            }
        }
        vt100::Color::Rgb(r, g, b) => ((r as u32) << 16) | ((g as u32) << 8) | b as u32,
    }
}

// Slint's special keys arrive as private-use-area code points (see
// i-slint-common's key_codes.rs / macOS NSEvent function keys).
const K_BACKSPACE: char = '\u{0008}';
const K_TAB: char = '\u{0009}';
const K_RETURN: char = '\u{000a}';
const K_ESCAPE: char = '\u{001b}';
const K_DELETE_FWD: char = '\u{007f}';
const K_UP: char = '\u{F700}';
const K_DOWN: char = '\u{F701}';
const K_LEFT: char = '\u{F702}';
const K_RIGHT: char = '\u{F703}';
const K_F1: char = '\u{F704}';
const K_F12: char = '\u{F70F}';
const K_INSERT: char = '\u{F727}';
const K_HOME: char = '\u{F729}';
const K_END: char = '\u{F72B}';
const K_PAGE_UP: char = '\u{F72C}';
const K_PAGE_DOWN: char = '\u{F72D}';

/// Translate a Slint key event (`text` + modifiers) into the bytes a PTY
/// expects, or `None` for keys the terminal has no encoding for (bare
/// modifiers, F-keys we don't map, shortcuts the app should keep).
///
/// `app_cursor` selects the application cursor-key encodings (`\x1bOA`
/// style) that full-screen programs switch on.
pub fn encode_key(text: &str, ctrl: bool, alt: bool, app_cursor: bool) -> Option<Vec<u8>> {
    let mut chars = text.chars();
    let c = chars.next()?;
    // Multi-char text is IME/paste-like input: send as-is.
    if chars.next().is_some() {
        return Some(text.as_bytes().to_vec());
    }

    let mut bytes: Vec<u8> = match c {
        K_RETURN => vec![b'\r'],
        K_BACKSPACE => vec![0x7f],
        K_TAB => vec![b'\t'],
        K_ESCAPE => vec![0x1b],
        K_DELETE_FWD => b"\x1b[3~".to_vec(),
        K_UP | K_DOWN | K_RIGHT | K_LEFT => {
            let dir = match c {
                K_UP => b'A',
                K_DOWN => b'B',
                K_RIGHT => b'C',
                _ => b'D',
            };
            if app_cursor {
                vec![0x1b, b'O', dir]
            } else {
                vec![0x1b, b'[', dir]
            }
        }
        K_HOME => b"\x1b[H".to_vec(),
        K_END => b"\x1b[F".to_vec(),
        K_PAGE_UP => b"\x1b[5~".to_vec(),
        K_PAGE_DOWN => b"\x1b[6~".to_vec(),
        K_INSERT => b"\x1b[2~".to_vec(),
        c if (K_F1..=K_F12).contains(&c) => {
            let n = c as u32 - K_F1 as u32 + 1;
            match n {
                1..=4 => vec![0x1b, b'O', (b'P' + (n - 1) as u8)],
                5 => b"\x1b[15~".to_vec(),
                6..=8 => format!("\x1b[{}~", n + 11).into_bytes(),
                _ => format!("\x1b[{}~", n + 12).into_bytes(),
            }
        }
        // Remaining PUA/control code points are bare modifiers etc.
        c if ('\u{F700}'..='\u{F8FF}').contains(&c) || (c as u32) < 0x20 => return None,
        c if ctrl => {
            // Ctrl+letter → C0 control byte (Ctrl+C = 0x03, ...).
            let lower = c.to_ascii_lowercase();
            match lower {
                'a'..='z' => vec![(lower as u8) - b'a' + 1],
                ' ' | '@' => vec![0x00],
                '[' => vec![0x1b],
                '\\' => vec![0x1c],
                ']' => vec![0x1d],
                _ => return None,
            }
        }
        c => c.to_string().into_bytes(),
    };
    if alt {
        bytes.insert(0, 0x1b);
    }
    Some(bytes)
}

/// Wrap pasted text for the PTY, honouring bracketed-paste mode when the
/// foreground program requested it.
pub fn encode_paste(text: &str, bracketed: bool) -> Vec<u8> {
    // Newlines must arrive as carriage returns or shells treat them oddly.
    let text = text.replace("\r\n", "\r").replace('\n', "\r");
    if bracketed {
        format!("\x1b[200~{text}\x1b[201~").into_bytes()
    } else {
        text.into_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_passthrough() {
        let segs = parse_ansi_line("hello world");
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].text, "hello world");
        assert_eq!(segs[0].color, 0);
    }

    #[test]
    fn sgr_colors_split_segments() {
        let segs =
            parse_ansi_line("ok \u{1b}[32mgreen\u{1b}[0m done \u{1b}[91mbright red\u{1b}[m.");
        let texts: Vec<&str> = segs.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(texts, vec!["ok ", "green", " done ", "bright red", "."]);
        assert_eq!(segs[1].color, PALETTE[2]);
        assert_eq!(segs[3].color, PALETTE[9]);
        assert_eq!(segs[4].color, 0);
    }

    #[test]
    fn non_sgr_sequences_are_stripped() {
        let segs = parse_ansi_line("a\u{1b}[2Kb\u{1b}[1;31mc");
        let texts: Vec<&str> = segs.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(texts, vec!["ab", "c"]);
        assert_eq!(segs[1].color, PALETTE[1]);
    }

    #[test]
    fn scrollback_caps_and_buffers_partial_lines() {
        let mut sb = Scrollback::new(3);
        sb.push("one\ntwo\nthr");
        assert_eq!(sb.line_count(), 2);
        sb.push("ee\nfour\nfive\n");
        assert_eq!(sb.line_count(), 3, "capped at max_lines");
    }

    const FG: u32 = 0xd8d8d8;
    const BG: u32 = 0x101010;

    #[test]
    fn grid_renders_colored_spans_and_cursor() {
        let mut grid = GridState::new(4, 20, 100);
        grid.process(b"ok \x1b[31mred\x1b[0m end");
        let rows = grid.rows(FG, BG);
        assert_eq!(rows.len(), 4);
        let texts: Vec<&str> = rows[0].iter().map(|s| s.text.as_str()).collect();
        assert_eq!(texts, vec!["ok ", "red", " end"]);
        assert_eq!(rows[0][1].fg, PALETTE[1]);
        assert_eq!(rows[0][0].fg, FG);
        assert!(!rows[0][0].has_bg);
        assert_eq!(grid.cursor(), Some((0, 10)));
    }

    #[test]
    fn grid_scrollback_paging_hides_cursor() {
        let mut grid = GridState::new(2, 10, 100);
        grid.process(b"one\r\ntwo\r\nthree\r\nfour");
        assert_eq!(grid.rows(FG, BG)[0][0].text.trim_end(), "three");
        grid.scroll_lines(2);
        assert_eq!(grid.rows(FG, BG)[0][0].text.trim_end(), "one");
        assert_eq!(grid.cursor(), None, "cursor hidden while in history");
        grid.scroll_to_live();
        assert_eq!(grid.cursor(), Some((1, 4)));
    }

    #[test]
    fn grid_resolves_256_and_rgb_colors() {
        let mut grid = GridState::new(1, 10, 0);
        // Index 196 = cube (5,0,0) = #ff0000; then a truecolor cell.
        grid.process(b"\x1b[38;5;196mX\x1b[38;2;1;2;3mY");
        let rows = grid.rows(FG, BG);
        assert_eq!(rows[0][0].fg, 0xff0000);
        assert_eq!(rows[0][1].fg, 0x010203);
    }

    #[test]
    fn encode_key_basics() {
        assert_eq!(encode_key("a", false, false, false).unwrap(), b"a");
        assert_eq!(encode_key("\n", false, false, false).unwrap(), b"\r");
        assert_eq!(encode_key("\u{8}", false, false, false).unwrap(), [0x7f]);
        assert_eq!(encode_key("c", true, false, false).unwrap(), [0x03]);
        assert_eq!(
            encode_key("\u{F700}", false, false, false).unwrap(),
            b"\x1b[A"
        );
        assert_eq!(
            encode_key("\u{F700}", false, false, true).unwrap(),
            b"\x1bOA"
        );
        assert_eq!(
            encode_key("\u{F72C}", false, false, false).unwrap(),
            b"\x1b[5~"
        );
        assert_eq!(encode_key("x", false, true, false).unwrap(), b"\x1bx");
        // Bare modifier presses produce nothing.
        assert_eq!(encode_key("\u{11}", false, false, false), None);
    }

    #[test]
    fn encode_paste_modes() {
        assert_eq!(encode_paste("a\nb", false), b"a\rb");
        assert_eq!(encode_paste("hi", true), b"\x1b[200~hi\x1b[201~");
    }
}
