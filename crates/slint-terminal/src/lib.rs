//! Terminal scrollback view for Slint: virtualized monospace output with
//! basic ANSI SGR color support (16 colors + reset; other escape sequences
//! are stripped).

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
}
