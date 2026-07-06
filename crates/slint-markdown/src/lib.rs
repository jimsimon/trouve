//! Streaming markdown for Slint.
//!
//! [`parse_blocks`] converts markdown text into flat blocks (headings,
//! paragraphs, bullets, code fences) rendered by the `MarkdownView`
//! component. [`StreamingMarkdown`] keeps a live block model in sync while
//! text streams in token-by-token: each append only touches the trailing
//! blocks that actually changed, so long documents never re-layout from
//! scratch.

slint::include_modules!();

use std::rc::Rc;

use slint::{Model, ModelRc, SharedString, VecModel};

/// Path to the crate's `.slint` sources.
pub const UI_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/ui");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockKind {
    Paragraph,
    H1,
    H2,
    H3,
    Bullet,
    Code,
    /// Ordered-list item; the marker ("1.", "2)") stays in the text.
    Numbered,
}

impl BlockKind {
    fn as_int(self) -> i32 {
        match self {
            BlockKind::Paragraph => 0,
            BlockKind::H1 => 1,
            BlockKind::H2 => 2,
            BlockKind::H3 => 3,
            BlockKind::Bullet => 4,
            BlockKind::Code => 5,
            BlockKind::Numbered => 6,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    pub kind: BlockKind,
    pub text: String,
    pub language: String,
    /// Nesting level for list items (0 = top level), from leading spaces.
    pub indent: i32,
}

impl Block {
    fn new(kind: BlockKind, text: impl Into<String>) -> Self {
        Self {
            kind,
            text: text.into(),
            language: String::new(),
            indent: 0,
        }
    }
}

/// Split an ordered-list line ("3. item", "3) item") into marker and body.
fn split_ordered(s: &str) -> Option<(&str, &str)> {
    let digits = s.bytes().take_while(|b| b.is_ascii_digit()).count();
    if digits == 0 || digits > 3 {
        return None;
    }
    if !matches!(s.as_bytes().get(digits), Some(b'.' | b')')) {
        return None;
    }
    let (marker, rest) = s.split_at(digits + 1);
    Some((marker, rest.strip_prefix(' ')?))
}

/// Opening code fence: three or more backticks plus an optional info
/// string (which, per CommonMark, may not itself contain backticks).
/// Returns the fence length and the language tag.
fn split_fence(s: &str) -> Option<(usize, &str)> {
    let ticks = s.bytes().take_while(|b| *b == b'`').count();
    let info = &s[ticks..];
    (ticks >= 3 && !info.contains('`')).then(|| (ticks, info.trim()))
}

/// Heading level from a run of '#' followed by a space; #### and deeper
/// render like ###.
fn split_heading(s: &str) -> Option<(BlockKind, &str)> {
    let hashes = s.bytes().take_while(|b| *b == b'#').count();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    let kind = match hashes {
        1 => BlockKind::H1,
        2 => BlockKind::H2,
        _ => BlockKind::H3,
    };
    Some((kind, s[hashes..].strip_prefix(' ')?))
}

/// Parse markdown into flat blocks. Supports headings (# through ######),
/// bullets (-/*/+) and ordered lists (1. / 1)) with nesting from
/// indentation, fenced code blocks (``` with optional language), and
/// paragraphs. Indented lines directly under a list item continue that
/// item. Inline styling is left in the text for the renderer.
pub fn parse_blocks(text: &str) -> Vec<Block> {
    let mut blocks: Vec<Block> = Vec::new();
    let mut paragraph: Vec<&str> = Vec::new();
    // Open fence: (fence length, language, content lines). The length
    // matters for nesting — a ```` fence can contain ``` fences as content
    // and only closes on a fence at least as long.
    let mut code: Option<(usize, String, Vec<&str>)> = None;
    // True while the last block is a list item with no blank line after it
    // yet; indented lines then continue that item.
    let mut list_open = false;

    let flush_paragraph = |blocks: &mut Vec<Block>, paragraph: &mut Vec<&str>| {
        if !paragraph.is_empty() {
            blocks.push(Block::new(BlockKind::Paragraph, paragraph.join(" ")));
            paragraph.clear();
        }
    };

    for line in text.lines() {
        if let Some((ticks, lang, lines)) = code.as_mut() {
            // Only a closing fence at least as long as the opener (and
            // nothing but backticks) ends the block; shorter fences are
            // content (raw-markdown examples).
            let trimmed = line.trim();
            let closing = trimmed.len() >= *ticks && trimmed.bytes().all(|b| b == b'`');
            if closing {
                let mut block = Block::new(BlockKind::Code, lines.join("\n"));
                block.language = std::mem::take(lang);
                blocks.push(block);
                code = None;
            } else {
                lines.push(line);
            }
            continue;
        }
        let trimmed = line.trim_start();
        let leading = line.len() - trimmed.len();
        let indent = ((leading as i32) / 2).min(4);
        if let Some((ticks, lang)) = split_fence(trimmed) {
            flush_paragraph(&mut blocks, &mut paragraph);
            code = Some((ticks, lang.to_string(), Vec::new()));
            list_open = false;
        } else if let Some((kind, h)) = split_heading(trimmed) {
            flush_paragraph(&mut blocks, &mut paragraph);
            blocks.push(Block::new(kind, h));
            list_open = false;
        } else if let Some(item) = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
            .or_else(|| trimmed.strip_prefix("+ "))
        {
            flush_paragraph(&mut blocks, &mut paragraph);
            let mut block = Block::new(BlockKind::Bullet, item);
            block.indent = indent;
            blocks.push(block);
            list_open = true;
        } else if let Some((marker, item)) = split_ordered(trimmed) {
            flush_paragraph(&mut blocks, &mut paragraph);
            let mut block = Block::new(BlockKind::Numbered, format!("{marker} {item}"));
            block.indent = indent;
            blocks.push(block);
            list_open = true;
        } else if trimmed.is_empty() {
            flush_paragraph(&mut blocks, &mut paragraph);
            list_open = false;
        } else if list_open && leading >= 2 && paragraph.is_empty() {
            // Indented text directly under a list item is a continuation of
            // that item (wrapped or multi-line list entries).
            let last = blocks.last_mut().unwrap();
            last.text.push(' ');
            last.text.push_str(trimmed);
        } else {
            paragraph.push(trimmed);
        }
    }
    // An unterminated fence is still rendered as code (it is mid-stream).
    if let Some((_, lang, lines)) = code {
        let mut block = Block::new(BlockKind::Code, lines.join("\n"));
        block.language = lang;
        blocks.push(block);
    }
    flush_paragraph(&mut blocks, &mut paragraph);
    blocks
}

fn to_widget_block(b: &Block) -> MarkdownBlock {
    MarkdownBlock {
        kind: b.kind.as_int(),
        text: SharedString::from(b.text.as_str()),
        language: SharedString::from(b.language.as_str()),
        indent: b.indent,
    }
}

/// Live block model for a streaming message.
pub struct StreamingMarkdown {
    text: String,
    blocks: Vec<Block>,
    model: Rc<VecModel<MarkdownBlock>>,
}

impl Default for StreamingMarkdown {
    fn default() -> Self {
        Self::new()
    }
}

impl StreamingMarkdown {
    pub fn new() -> Self {
        Self {
            text: String::new(),
            blocks: Vec::new(),
            model: Rc::new(VecModel::default()),
        }
    }

    /// The model to hand to `MarkdownView.blocks`.
    pub fn model(&self) -> ModelRc<MarkdownBlock> {
        ModelRc::from(self.model.clone())
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    /// Append streamed text and sync the model. Returns how many model rows
    /// were touched — appends normally touch only the trailing block.
    pub fn push(&mut self, delta: &str) -> usize {
        self.text.push_str(delta);
        let new_blocks = parse_blocks(&self.text);

        // First index where old and new disagree; everything before it is
        // untouched (stable prefix).
        let mut same = 0;
        while same < self.blocks.len()
            && same < new_blocks.len()
            && self.blocks[same] == new_blocks[same]
        {
            same += 1;
        }
        let mut touched = 0;
        // Update rows that changed in place.
        let in_place = new_blocks.len().min(self.model.row_count());
        for (i, block) in new_blocks.iter().enumerate().take(in_place).skip(same) {
            self.model.set_row_data(i, to_widget_block(block));
            touched += 1;
        }
        // Append new rows.
        for block in new_blocks.iter().skip(self.model.row_count()) {
            self.model.push(to_widget_block(block));
            touched += 1;
        }
        // Remove surplus rows (rare: a paragraph merged into a fence).
        while self.model.row_count() > new_blocks.len() {
            self.model.remove(self.model.row_count() - 1);
            touched += 1;
        }
        self.blocks = new_blocks;
        touched
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_mixed_document() {
        let blocks = parse_blocks(
            "# Title\n\nSome intro text\nacross two lines.\n\n- first\n- second\n\n```rust\nfn x() {}\n```\ntail",
        );
        let kinds: Vec<_> = blocks.iter().map(|b| b.kind).collect();
        assert_eq!(
            kinds,
            vec![
                BlockKind::H1,
                BlockKind::Paragraph,
                BlockKind::Bullet,
                BlockKind::Bullet,
                BlockKind::Code,
                BlockKind::Paragraph
            ]
        );
        assert_eq!(blocks[1].text, "Some intro text across two lines.");
        assert_eq!(blocks[4].language, "rust");
        assert_eq!(blocks[4].text, "fn x() {}");
    }

    #[test]
    fn unterminated_fence_renders_as_code() {
        let blocks = parse_blocks("```py\nprint(1)");
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].kind, BlockKind::Code);
    }

    #[test]
    fn longer_fences_contain_shorter_ones_as_content() {
        // Raw-markdown example: a ```` fence showing a ``` fence.
        let blocks = parse_blocks("intro\n\n````markdown\n```rust\nfn main() {}\n```\n````\ntail");
        let kinds: Vec<_> = blocks.iter().map(|b| b.kind).collect();
        assert_eq!(
            kinds,
            vec![BlockKind::Paragraph, BlockKind::Code, BlockKind::Paragraph]
        );
        assert_eq!(blocks[1].text, "```rust\nfn main() {}\n```");
        assert_eq!(blocks[1].language, "markdown");
    }

    #[test]
    fn closing_fence_must_be_backticks_only() {
        // "``` trailing words" inside a block is content, not a closer.
        let blocks = parse_blocks("```\ncode\n``` not a closer\nstill code\n```");
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].text, "code\n``` not a closer\nstill code");
    }

    #[test]
    fn inline_code_span_is_not_a_fence() {
        let blocks = parse_blocks("```not a fence``` because the info string has backticks");
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].kind, BlockKind::Paragraph);
    }

    #[test]
    fn ordered_lists_keep_their_markers_and_stay_separate_items() {
        // Tight list: no blank lines between items — these must not merge
        // into one paragraph.
        let blocks = parse_blocks("1. first\n2. second\n10) tenth");
        let kinds: Vec<_> = blocks.iter().map(|b| b.kind).collect();
        assert_eq!(
            kinds,
            vec![
                BlockKind::Numbered,
                BlockKind::Numbered,
                BlockKind::Numbered
            ]
        );
        assert_eq!(blocks[0].text, "1. first");
        assert_eq!(blocks[2].text, "10) tenth");
        // Not a list: version strings, years.
        let blocks = parse_blocks("2026 was the year.\n1.5x speedup");
        assert!(blocks.iter().all(|b| b.kind == BlockKind::Paragraph));
    }

    #[test]
    fn nested_lists_carry_indent_levels() {
        let blocks = parse_blocks("1. top\n  - nested\n    - deeper\n- flat");
        assert_eq!(blocks[0].indent, 0);
        assert_eq!(blocks[1].indent, 1);
        assert_eq!((blocks[1].kind, blocks[1].text.as_str()), (BlockKind::Bullet, "nested"));
        assert_eq!(blocks[2].indent, 2);
        assert_eq!(blocks[3].indent, 0);
    }

    #[test]
    fn indented_continuation_joins_its_list_item() {
        let blocks = parse_blocks("- item text\n  wraps onto a second line\n\nplain paragraph");
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].text, "item text wraps onto a second line");
        assert_eq!(blocks[1].kind, BlockKind::Paragraph);
    }

    #[test]
    fn deep_headings_render_as_h3() {
        let blocks = parse_blocks("#### deep\n##### deeper");
        assert_eq!(blocks[0].kind, BlockKind::H3);
        assert_eq!(blocks[1].kind, BlockKind::H3);
        assert_eq!(blocks[0].text, "deep");
    }

    #[test]
    fn streaming_matches_batch_and_touches_only_the_tail() {
        let full = "# Title\n\nA paragraph that grows over time.\n\n- item one\n- item two\n\n```rust\nfn main() {}\n```\n";
        let mut streaming = StreamingMarkdown::new();
        // Feed in small chunks.
        let mut max_touched_after_warmup = 0;
        for (i, chunk) in full.as_bytes().chunks(7).enumerate() {
            let touched = streaming.push(std::str::from_utf8(chunk).unwrap_or(""));
            if i > 3 {
                max_touched_after_warmup = max_touched_after_warmup.max(touched);
            }
        }
        let batch = parse_blocks(full);
        assert_eq!(streaming.blocks, batch);
        assert_eq!(streaming.model.row_count(), batch.len());
        // Incremental appends only touch the trailing block or two.
        assert!(
            max_touched_after_warmup <= 2,
            "appends touched {max_touched_after_warmup} rows"
        );
    }
}
