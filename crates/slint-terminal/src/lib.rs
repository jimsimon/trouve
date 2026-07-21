//! Terminal widgets for Slint.
//!
//! Two components with matching Rust-side state helpers:
//! - `TerminalView` + [`Scrollback`]: append-only scrollback for command
//!   output / build logs (basic SGR colors, everything else stripped).
//! - `TerminalGrid` + [`GridState`]: a full interactive terminal screen
//!   backed by a vt100 parser — SGR styling, cursor modes, alternate screen,
//!   scrollback search, selection/copy, IME/touch-friendly input, mouse
//!   tracking, and host callbacks — plus [`encode_key`], [`encode_mouse`],
//!   and [`encode_paste`] to turn UI events into the bytes a PTY expects.

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

/// Longest partial (newline-free) line buffered before it is force-flushed
/// as its own row, so pathological output can't grow it without bound.
const MAX_PARTIAL_BYTES: usize = 64 * 1024;

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
        // Cap the pending partial line: a process emitting megabytes with no
        // newline (progress bars using \r, or binary output) would otherwise
        // grow it without bound. Flush an over-long partial as its own row.
        if self.partial.len() > MAX_PARTIAL_BYTES {
            let flushed = std::mem::take(&mut self.partial);
            let segments: Vec<TermSegment> =
                parse_ansi_line(flushed.trim_end_matches(['\n', '\r']))
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
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
    /// Whether `bg` differs from the default background (lets the widget
    /// skip painting cell backgrounds for the common case).
    pub has_bg: bool,
}

/// Side effects and host requests emitted by escape sequences that do not
/// directly mutate terminal cells.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalEvent {
    Bell,
    Title(String),
    Resize {
        rows: u16,
        cols: u16,
    },
    ClipboardCopy {
        selection: String,
        data: Vec<u8>,
    },
    ClipboardPaste {
        selection: String,
    },
    CursorStyle {
        shape: CursorShape,
        blinking: bool,
    },
    #[doc(hidden)]
    Hyperlink {
        uri: Option<String>,
        position: (u16, u16),
    },
    Reply(Vec<u8>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorShape {
    Block,
    Underline,
    Bar,
}

#[derive(Default)]
struct GridCallbacks {
    events: Vec<TerminalEvent>,
}

fn callback_text(bytes: &[u8], max_bytes: usize) -> String {
    String::from_utf8_lossy(&bytes[..bytes.len().min(max_bytes)])
        .chars()
        .filter(|c| !c.is_control())
        .collect()
}

impl vt100::Callbacks for GridCallbacks {
    fn audible_bell(&mut self, _: &mut vt100::Screen) {
        self.events.push(TerminalEvent::Bell);
    }

    fn visual_bell(&mut self, _: &mut vt100::Screen) {
        self.events.push(TerminalEvent::Bell);
    }

    fn resize(&mut self, _: &mut vt100::Screen, request: (u16, u16)) {
        self.events.push(TerminalEvent::Resize {
            rows: request.0,
            cols: request.1,
        });
    }

    fn set_window_icon_name(&mut self, _: &mut vt100::Screen, icon_name: &[u8]) {
        self.events
            .push(TerminalEvent::Title(callback_text(icon_name, 512)));
    }

    fn set_window_title(&mut self, _: &mut vt100::Screen, title: &[u8]) {
        self.events
            .push(TerminalEvent::Title(callback_text(title, 512)));
    }

    fn copy_to_clipboard(&mut self, _: &mut vt100::Screen, ty: &[u8], data: &[u8]) {
        self.events.push(TerminalEvent::ClipboardCopy {
            selection: callback_text(ty, 32),
            data: data[..data.len().min(1024 * 1024)].to_vec(),
        });
    }

    fn paste_from_clipboard(&mut self, _: &mut vt100::Screen, ty: &[u8]) {
        self.events.push(TerminalEvent::ClipboardPaste {
            selection: callback_text(ty, 32),
        });
    }

    fn unhandled_csi(
        &mut self,
        screen: &mut vt100::Screen,
        i1: Option<u8>,
        _: Option<u8>,
        params: &[&[u16]],
        c: char,
    ) {
        let first = params
            .first()
            .and_then(|values| values.first())
            .copied()
            .unwrap_or(0);
        let reply = match (i1, first, c) {
            (None, 0, 'c') => Some(b"\x1b[?1;2c".to_vec()),
            (Some(b'>'), 0, 'c') => Some(b"\x1b[>0;10;1c".to_vec()),
            (None, 5, 'n') => Some(b"\x1b[0n".to_vec()),
            (None, 6, 'n') => {
                let (row, col) = screen.cursor_position();
                Some(format!("\x1b[{};{}R", row + 1, col + 1).into_bytes())
            }
            (Some(b'?'), 6, 'n') => {
                let (row, col) = screen.cursor_position();
                Some(format!("\x1b[?{};{}R", row + 1, col + 1).into_bytes())
            }
            _ => None,
        };
        if i1 == Some(b' ') && c == 'q' {
            let (shape, blinking) = match first {
                3 => (CursorShape::Underline, true),
                4 => (CursorShape::Underline, false),
                5 => (CursorShape::Bar, true),
                6 => (CursorShape::Bar, false),
                2 => (CursorShape::Block, false),
                _ => (CursorShape::Block, true),
            };
            self.events
                .push(TerminalEvent::CursorStyle { shape, blinking });
        }
        if let Some(reply) = reply {
            self.events.push(TerminalEvent::Reply(reply));
        }
    }

    fn unhandled_osc(&mut self, screen: &mut vt100::Screen, params: &[&[u8]]) {
        if params.first().copied() != Some(b"8".as_slice()) {
            return;
        }
        let uri = params
            .get(2)
            .filter(|uri| !uri.is_empty())
            .map(|uri| callback_text(uri, 4096));
        self.events.push(TerminalEvent::Hyperlink {
            uri,
            position: screen.cursor_position(),
        });
    }
}

/// Screen state for `TerminalGrid`: a vt100 parser plus scrollback paging.
pub struct GridState {
    parser: vt100::Parser<GridCallbacks>,
    hyperlinks: Vec<HyperlinkRange>,
    active_hyperlink: Option<(String, (u16, u16))>,
}

struct HyperlinkRange {
    uri: String,
    start: (u16, u16),
    end: (u16, u16),
    label: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchMatch {
    pub scrollback: usize,
    pub start: (u16, u16),
    pub end: (u16, u16),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchDirection {
    Older,
    Newer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionUnit {
    Word,
    Line,
}

impl GridState {
    pub fn new(rows: u16, cols: u16, scrollback: usize) -> Self {
        Self {
            parser: vt100::Parser::new_with_callbacks(
                rows.max(1),
                cols.max(1),
                scrollback,
                GridCallbacks::default(),
            ),
            hyperlinks: Vec::new(),
            active_hyperlink: None,
        }
    }

    /// Feed raw PTY output (bytes, possibly mid-escape or mid-UTF-8).
    pub fn process(&mut self, bytes: &[u8]) {
        self.parser.process(bytes);
        let events = std::mem::take(&mut self.parser.callbacks_mut().events);
        let mut host_events = Vec::with_capacity(events.len());
        for event in events {
            match event {
                TerminalEvent::Hyperlink {
                    uri: Some(uri),
                    position,
                } => self.active_hyperlink = Some((uri, position)),
                TerminalEvent::Hyperlink {
                    uri: None,
                    position: end,
                } => {
                    if let Some((uri, start)) = self.active_hyperlink.take()
                        && start < end
                    {
                        let label = self
                            .parser
                            .screen()
                            .contents_between(start.0, start.1, end.0, end.1);
                        if !label.is_empty() {
                            self.hyperlinks.push(HyperlinkRange {
                                uri,
                                start,
                                end,
                                label,
                            });
                            if self.hyperlinks.len() > 2048 {
                                self.hyperlinks.remove(0);
                            }
                        }
                    }
                }
                event => host_events.push(event),
            }
        }
        self.parser.callbacks_mut().events = host_events;
    }

    /// Drain side effects emitted while processing output. Clipboard events
    /// are requests only; the host decides whether to grant them.
    pub fn drain_events(&mut self) -> Vec<TerminalEvent> {
        std::mem::take(&mut self.parser.callbacks_mut().events)
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        self.parser.screen_mut().set_size(rows.max(1), cols.max(1));
        self.hyperlinks.clear();
        self.active_hyperlink = None;
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

    pub fn application_keypad(&self) -> bool {
        self.parser.screen().application_keypad()
    }

    pub fn bracketed_paste(&self) -> bool {
        self.parser.screen().bracketed_paste()
    }

    /// Plain text of the visible screen (for copy).
    pub fn contents(&self) -> String {
        self.parser.screen().contents()
    }

    /// Plain text logically between two visible cells.
    ///
    /// The start is inclusive and the end is exclusive. Points are clamped
    /// to the visible grid and may be supplied in either order. `vt100`
    /// preserves soft-wrapped rows here, so selecting across a visual wrap
    /// does not introduce a newline that was not present in the PTY output.
    pub fn selection_text(&self, start: (u16, u16), end: (u16, u16)) -> String {
        let screen = self.parser.screen();
        let (rows, cols) = screen.size();
        let clamp = |(row, col): (u16, u16)| (row.min(rows.saturating_sub(1)), col.min(cols));
        let (start, end) = (clamp(start), clamp(end));
        let (start, end) = if start <= end {
            (start, end)
        } else {
            (end, start)
        };
        screen.contents_between(start.0, start.1, end.0, end.1)
    }

    /// Find text across the visible screen and retained scrollback, moving
    /// the viewport to the matching page. Search wraps at either end.
    pub fn search(
        &mut self,
        query: &str,
        direction: SearchDirection,
        skip_current: bool,
    ) -> Option<SearchMatch> {
        if query.is_empty() {
            return None;
        }
        let current = self.parser.screen().scrollback();
        self.parser.screen_mut().set_scrollback(usize::MAX);
        let maximum = self.parser.screen().scrollback();
        self.parser.screen_mut().set_scrollback(current);

        let mut offsets = Vec::with_capacity(maximum + 1);
        match direction {
            SearchDirection::Older => {
                offsets.extend((current..=maximum).skip(usize::from(skip_current)));
                offsets.extend(0..current);
            }
            SearchDirection::Newer => {
                offsets.extend((0..=current).rev().skip(usize::from(skip_current)));
                offsets.extend(((current + 1)..=maximum).rev());
            }
        }
        for offset in offsets {
            self.parser.screen_mut().set_scrollback(offset);
            if let Some((start, end)) = visible_search(self.parser.screen(), query) {
                return Some(SearchMatch {
                    scrollback: offset,
                    start,
                    end,
                });
            }
        }
        self.parser.screen_mut().set_scrollback(current);
        None
    }

    /// Cell range for a conventional double-click word or triple-click
    /// logical line selection.
    pub fn selection_unit(
        &self,
        row: u16,
        col: u16,
        unit: SelectionUnit,
    ) -> Option<((u16, u16), (u16, u16))> {
        let screen = self.parser.screen();
        let (rows, cols) = screen.size();
        if row >= rows || col >= cols {
            return None;
        }
        if unit == SelectionUnit::Line {
            let mut start_row = row;
            while start_row > 0 && screen.row_wrapped(start_row - 1) {
                start_row -= 1;
            }
            let mut end_row = row;
            while end_row + 1 < rows && screen.row_wrapped(end_row) {
                end_row += 1;
            }
            return Some(((start_row, 0), (end_row, cols)));
        }

        let class = |cell: Option<&vt100::Cell>| {
            let c = cell.and_then(|cell| cell.contents().chars().next());
            match c {
                None | Some(' ' | '\t') => 0,
                Some(c) if c.is_alphanumeric() || "_-./:@?&=%+#~".contains(c) => 1,
                Some(_) => 2,
            }
        };
        let selected = class(screen.cell(row, col));
        let mut start_col = col;
        while start_col > 0 && class(screen.cell(row, start_col - 1)) == selected {
            start_col -= 1;
        }
        let mut end_col = col + 1;
        while end_col < cols && class(screen.cell(row, end_col)) == selected {
            end_col += 1;
        }
        Some(((row, start_col), (row, end_col)))
    }

    /// An http(s) URL occupying the requested visible cell, if any.
    pub fn url_at(&self, row: u16, col: u16) -> Option<String> {
        let screen = self.parser.screen();
        let (rows, cols) = screen.size();
        if row >= rows || col >= cols {
            return None;
        }
        let point = (row, col);
        if let Some(link) = self
            .hyperlinks
            .iter()
            .rev()
            .find(|link| point >= link.start && point < link.end)
            && screen.contents_between(link.start.0, link.start.1, link.end.0, link.end.1)
                == link.label
        {
            return Some(link.uri.clone());
        }
        let mut text = String::new();
        let mut byte_for_col = Vec::with_capacity(cols as usize + 1);
        for current_col in 0..cols {
            byte_for_col.push(text.len());
            let cell = screen.cell(row, current_col)?;
            if !cell.is_wide_continuation() {
                text.push_str(match cell.contents() {
                    "" => " ",
                    value => value,
                });
            }
        }
        byte_for_col.push(text.len());
        let byte = *byte_for_col.get(col as usize)?;
        let start = text[..byte]
            .rfind(char::is_whitespace)
            .map_or(0, |index| index + 1);
        let end = text[byte..]
            .find(char::is_whitespace)
            .map_or(text.len(), |index| byte + index);
        let candidate = text[start..end]
            .trim_matches(['(', ')', '[', ']', '{', '}', '<', '>', '\'', '"', ',', ';'])
            .trim_end_matches(['.', ':', '!', '?']);
        (candidate.starts_with("https://") || candidate.starts_with("http://"))
            .then(|| candidate.to_string())
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
                let bold = cell.bold();
                let dim = cell.dim();
                let italic = cell.italic();
                let underline = cell.underline();
                let text = match cell.contents() {
                    "" => " ",
                    t => t,
                };
                match spans.last_mut() {
                    Some(last)
                        if last.fg == fg
                            && last.bg == bg
                            && last.bold == bold
                            && last.dim == dim
                            && last.italic == italic
                            && last.underline == underline =>
                    {
                        last.text.push_str(text);
                    }
                    _ => spans.push(GridSpan {
                        text: text.to_string(),
                        fg,
                        bg,
                        bold,
                        dim,
                        italic,
                        underline,
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
                    bold: false,
                    dim: false,
                    italic: false,
                    underline: false,
                    has_bg: false,
                });
            }
            out.push(spans);
        }
        out
    }
}

fn visible_search(screen: &vt100::Screen, query: &str) -> Option<((u16, u16), (u16, u16))> {
    let (rows, cols) = screen.size();
    let query_lower = query.to_lowercase();
    for row in 0..rows {
        let mut text = String::new();
        let mut cells = Vec::new();
        for col in 0..cols {
            let Some(cell) = screen.cell(row, col) else {
                continue;
            };
            if cell.is_wide_continuation() {
                continue;
            }
            let start = text.len();
            text.push_str(match cell.contents() {
                "" => " ",
                value => value,
            });
            let width = if screen
                .cell(row, col.saturating_add(1))
                .is_some_and(vt100::Cell::is_wide_continuation)
            {
                2
            } else {
                1
            };
            cells.push((start, text.len(), col, col.saturating_add(width)));
        }
        let text_lower = text.to_lowercase();
        let Some(found) = text_lower.find(&query_lower) else {
            continue;
        };
        let found_end = found + query_lower.len();
        let start_col = cells
            .iter()
            .find(|(start, end, _, _)| found >= *start && found < *end)
            .map(|(_, _, col, _)| *col)
            .unwrap_or(0);
        let end_col = cells
            .iter()
            .take_while(|(start, _, _, _)| *start < found_end)
            .last()
            .map(|(_, _, _, end)| *end)
            .unwrap_or(start_col);
        return Some(((row, start_col), (row, end_col)));
    }
    None
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
pub fn encode_key(
    text: &str,
    ctrl: bool,
    alt: bool,
    shift: bool,
    app_cursor: bool,
) -> Option<Vec<u8>> {
    let mut chars = text.chars();
    let c = chars.next()?;
    // Multi-char text is IME/paste-like input: send as-is.
    if chars.next().is_some() {
        return Some(text.as_bytes().to_vec());
    }

    let modifier = 1 + u8::from(shift) + 2 * u8::from(alt) + 4 * u8::from(ctrl);
    let mut modifier_encoded = false;
    let mut bytes: Vec<u8> = match c {
        K_RETURN => vec![b'\r'],
        K_BACKSPACE => vec![0x7f],
        K_TAB if shift => {
            modifier_encoded = true;
            if modifier == 2 {
                b"\x1b[Z".to_vec()
            } else {
                format!("\x1b[1;{modifier}Z").into_bytes()
            }
        }
        K_TAB => vec![b'\t'],
        K_ESCAPE => vec![0x1b],
        K_UP | K_DOWN | K_RIGHT | K_LEFT => {
            let dir = match c {
                K_UP => b'A',
                K_DOWN => b'B',
                K_RIGHT => b'C',
                _ => b'D',
            };
            if modifier > 1 {
                modifier_encoded = true;
                format!("\x1b[1;{modifier}{}", dir as char).into_bytes()
            } else if app_cursor {
                vec![0x1b, b'O', dir]
            } else {
                vec![0x1b, b'[', dir]
            }
        }
        K_HOME | K_END => {
            let final_byte = if c == K_HOME { 'H' } else { 'F' };
            if modifier > 1 {
                modifier_encoded = true;
                format!("\x1b[1;{modifier}{final_byte}").into_bytes()
            } else {
                format!("\x1b[{final_byte}").into_bytes()
            }
        }
        K_PAGE_UP | K_PAGE_DOWN | K_INSERT | K_DELETE_FWD => {
            let code = match c {
                K_INSERT => 2,
                K_DELETE_FWD => 3,
                K_PAGE_UP => 5,
                _ => 6,
            };
            if modifier > 1 {
                modifier_encoded = true;
                format!("\x1b[{code};{modifier}~").into_bytes()
            } else {
                format!("\x1b[{code}~").into_bytes()
            }
        }
        c if (K_F1..=K_F12).contains(&c) => {
            let n = c as u32 - K_F1 as u32 + 1;
            let code = match n {
                5 => 15,
                6..=10 => n + 11,
                11..=12 => n + 12,
                _ => 0,
            };
            if modifier > 1 {
                modifier_encoded = true;
                if n <= 4 {
                    format!("\x1b[1;{modifier}{}", (b'P' + (n - 1) as u8) as char).into_bytes()
                } else {
                    format!("\x1b[{code};{modifier}~").into_bytes()
                }
            } else if n <= 4 {
                vec![0x1b, b'O', b'P' + (n - 1) as u8]
            } else {
                format!("\x1b[{code}~").into_bytes()
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
    if alt && !modifier_encoded {
        bytes.insert(0, 0x1b);
    }
    Some(bytes)
}

/// Mouse tracking requested by the foreground terminal application.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseMode {
    None,
    Press,
    PressRelease,
    ButtonMotion,
    AnyMotion,
}

/// Coordinate encoding requested by the foreground terminal application.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseEncoding {
    Default,
    Utf8,
    Sgr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseEventKind {
    Press,
    Release,
    Move,
    WheelUp,
    WheelDown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    None,
    Left,
    Middle,
    Right,
}

impl GridState {
    pub fn mouse_mode(&self) -> MouseMode {
        match self.parser.screen().mouse_protocol_mode() {
            vt100::MouseProtocolMode::None => MouseMode::None,
            vt100::MouseProtocolMode::Press => MouseMode::Press,
            vt100::MouseProtocolMode::PressRelease => MouseMode::PressRelease,
            vt100::MouseProtocolMode::ButtonMotion => MouseMode::ButtonMotion,
            vt100::MouseProtocolMode::AnyMotion => MouseMode::AnyMotion,
        }
    }

    pub fn mouse_encoding(&self) -> MouseEncoding {
        match self.parser.screen().mouse_protocol_encoding() {
            vt100::MouseProtocolEncoding::Default => MouseEncoding::Default,
            vt100::MouseProtocolEncoding::Utf8 => MouseEncoding::Utf8,
            vt100::MouseProtocolEncoding::Sgr => MouseEncoding::Sgr,
        }
    }
}

/// Encode a pointer event using the tracking mode selected by the terminal
/// application. Rows and columns are zero-based widget cell coordinates.
#[allow(clippy::too_many_arguments)]
pub fn encode_mouse(
    kind: MouseEventKind,
    button: MouseButton,
    row: u16,
    col: u16,
    shift: bool,
    alt: bool,
    ctrl: bool,
    mode: MouseMode,
    encoding: MouseEncoding,
) -> Option<Vec<u8>> {
    if mode == MouseMode::None
        || (kind == MouseEventKind::Release && mode == MouseMode::Press)
        || (kind == MouseEventKind::Move
            && !matches!(mode, MouseMode::ButtonMotion | MouseMode::AnyMotion))
        || (kind == MouseEventKind::Move
            && button == MouseButton::None
            && mode != MouseMode::AnyMotion)
    {
        return None;
    }

    let base = match kind {
        MouseEventKind::WheelUp => 64,
        MouseEventKind::WheelDown => 65,
        MouseEventKind::Release if encoding != MouseEncoding::Sgr => 3,
        _ => match button {
            MouseButton::Left => 0,
            MouseButton::Middle => 1,
            MouseButton::Right => 2,
            MouseButton::None => 3,
        },
    };
    let code = base
        + if kind == MouseEventKind::Move { 32 } else { 0 }
        + if shift { 4 } else { 0 }
        + if alt { 8 } else { 0 }
        + if ctrl { 16 } else { 0 };
    let x = u32::from(col) + 1;
    let y = u32::from(row) + 1;

    match encoding {
        MouseEncoding::Sgr => Some(
            format!(
                "\x1b[<{code};{x};{y}{}",
                if kind == MouseEventKind::Release {
                    'm'
                } else {
                    'M'
                }
            )
            .into_bytes(),
        ),
        MouseEncoding::Default | MouseEncoding::Utf8 => {
            let mut bytes = b"\x1b[M".to_vec();
            bytes.push(code + 32);
            let mut push_coord = |value: u32| -> Option<()> {
                let value = value + 32;
                if encoding == MouseEncoding::Default {
                    bytes.push(u8::try_from(value).ok()?);
                } else {
                    let ch = char::from_u32(value)?;
                    let mut utf8 = [0; 4];
                    bytes.extend_from_slice(ch.encode_utf8(&mut utf8).as_bytes());
                }
                Some(())
            };
            push_coord(x)?;
            push_coord(y)?;
            Some(bytes)
        }
    }
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
    fn grid_preserves_text_attributes() {
        let mut grid = GridState::new(1, 10, 0);
        grid.process(b"\x1b[1;3;4mX\x1b[0;2mY\x1b[0mZ");
        let rows = grid.rows(FG, BG);
        let styled = &rows[0][0];
        assert!(styled.bold);
        assert!(styled.italic);
        assert!(styled.underline);
        assert!(rows[0][1].dim);
        assert!(!rows[0][2].bold);
    }

    #[test]
    fn grid_reports_host_events_and_query_replies() {
        let mut grid = GridState::new(2, 10, 0);
        grid.process(b"\x07\x1b]2;build shell\x07\x1b[5n\x1b[5 q");
        let events = grid.drain_events();
        assert!(events.contains(&TerminalEvent::Bell));
        assert!(events.contains(&TerminalEvent::Title("build shell".into())));
        assert!(events.contains(&TerminalEvent::Reply(b"\x1b[0n".to_vec())));
        assert!(events.contains(&TerminalEvent::CursorStyle {
            shape: CursorShape::Bar,
            blinking: true,
        }));
    }

    #[test]
    fn grid_selection_extracts_explicit_and_wrapped_lines() {
        let mut grid = GridState::new(3, 5, 0);
        grid.process(b"abcdeFG\r\nnext");

        // The first two visual rows are one soft-wrapped logical line; the
        // explicit CRLF before "next" remains a newline in copied text.
        assert_eq!(grid.selection_text((0, 1), (2, 4)), "bcdeFG\nnext");
        assert_eq!(grid.selection_text((2, 4), (0, 1)), "bcdeFG\nnext");
        assert_eq!(grid.selection_text((1, 1), (1, 1)), "");
    }

    #[test]
    fn grid_selection_handles_wide_cells() {
        let mut grid = GridState::new(1, 5, 0);
        grid.process("界x".as_bytes());
        assert_eq!(grid.selection_text((0, 0), (0, 2)), "界");
        assert_eq!(grid.selection_text((0, 0), (0, 3)), "界x");
    }

    #[test]
    fn grid_searches_history_and_selects_words_and_links() {
        let mut grid = GridState::new(2, 40, 20);
        grid.process(b"old needle\r\nnext\r\nvisit https://example.com/path now\r\nlive");
        let found = grid
            .search("needle", SearchDirection::Older, false)
            .expect("history match");
        assert!(found.scrollback > 0);
        assert_eq!(grid.selection_text(found.start, found.end), "needle");

        grid.scroll_to_live();
        assert_eq!(
            grid.url_at(0, 10).as_deref(),
            Some("https://example.com/path")
        );
        let range = grid
            .selection_unit(0, 10, SelectionUnit::Word)
            .expect("word range");
        assert_eq!(
            grid.selection_text(range.0, range.1),
            "https://example.com/path"
        );

        let mut osc8 = GridState::new(1, 20, 0);
        osc8.process(b"\x1b]8;;https://example.com/docs\x1b\\open docs\x1b]8;;\x1b\\");
        assert_eq!(
            osc8.url_at(0, 2).as_deref(),
            Some("https://example.com/docs")
        );
    }

    #[test]
    fn encode_key_basics() {
        assert_eq!(encode_key("a", false, false, false, false).unwrap(), b"a");
        assert_eq!(encode_key("\n", false, false, false, false).unwrap(), b"\r");
        assert_eq!(
            encode_key("\u{8}", false, false, false, false).unwrap(),
            [0x7f]
        );
        assert_eq!(encode_key("c", true, false, false, false).unwrap(), [0x03]);
        assert_eq!(
            encode_key("\u{F700}", false, false, false, false).unwrap(),
            b"\x1b[A"
        );
        assert_eq!(
            encode_key("\u{F700}", false, false, false, true).unwrap(),
            b"\x1bOA"
        );
        assert_eq!(
            encode_key("\u{F72C}", false, false, false, false).unwrap(),
            b"\x1b[5~"
        );
        assert_eq!(
            encode_key("\u{F700}", true, false, true, false).unwrap(),
            b"\x1b[1;6A"
        );
        assert_eq!(
            encode_key("\t", false, false, true, false).unwrap(),
            b"\x1b[Z"
        );
        assert_eq!(
            encode_key("x", false, true, false, false).unwrap(),
            b"\x1bx"
        );
        // Bare modifier presses produce nothing.
        assert_eq!(encode_key("\u{11}", false, false, false, false), None);
    }

    #[test]
    fn function_keys_use_xterm_tilde_codes() {
        for (number, expected) in [
            (9, b"\x1b[20~".as_slice()),
            (10, b"\x1b[21~".as_slice()),
            (11, b"\x1b[23~".as_slice()),
            (12, b"\x1b[24~".as_slice()),
        ] {
            let key = char::from_u32(K_F1 as u32 + number - 1)
                .unwrap()
                .to_string();
            assert_eq!(
                encode_key(&key, false, false, false, false).unwrap(),
                expected
            );
        }
    }

    #[test]
    fn encode_mouse_sgr_and_tracking_modes() {
        assert_eq!(
            encode_mouse(
                MouseEventKind::Press,
                MouseButton::Left,
                2,
                4,
                false,
                false,
                false,
                MouseMode::PressRelease,
                MouseEncoding::Sgr,
            )
            .unwrap(),
            b"\x1b[<0;5;3M"
        );
        assert_eq!(
            encode_mouse(
                MouseEventKind::Release,
                MouseButton::Left,
                2,
                4,
                false,
                false,
                false,
                MouseMode::PressRelease,
                MouseEncoding::Sgr,
            )
            .unwrap(),
            b"\x1b[<0;5;3m"
        );
        assert!(
            encode_mouse(
                MouseEventKind::Press,
                MouseButton::Left,
                0,
                0,
                false,
                false,
                false,
                MouseMode::None,
                MouseEncoding::Sgr,
            )
            .is_none()
        );
    }

    #[test]
    fn encode_paste_modes() {
        assert_eq!(encode_paste("a\nb", false), b"a\rb");
        assert_eq!(encode_paste("hi", true), b"\x1b[200~hi\x1b[201~");
    }
}
