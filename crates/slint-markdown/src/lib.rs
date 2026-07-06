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
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    pub kind: BlockKind,
    pub text: String,
    pub language: String,
}

/// Parse markdown into flat blocks. Supports headings (#/##/###), bullets
/// (-/*), fenced code blocks (``` with optional language), and paragraphs.
/// Inline styling is intentionally out of scope for the spike.
pub fn parse_blocks(text: &str) -> Vec<Block> {
    let mut blocks: Vec<Block> = Vec::new();
    let mut paragraph: Vec<&str> = Vec::new();
    let mut code: Option<(String, Vec<&str>)> = None;

    let flush_paragraph = |blocks: &mut Vec<Block>, paragraph: &mut Vec<&str>| {
        if !paragraph.is_empty() {
            blocks.push(Block {
                kind: BlockKind::Paragraph,
                text: paragraph.join(" "),
                language: String::new(),
            });
            paragraph.clear();
        }
    };

    for line in text.lines() {
        if let Some((lang, lines)) = code.as_mut() {
            if line.trim_start().starts_with("```") {
                blocks.push(Block {
                    kind: BlockKind::Code,
                    text: lines.join("\n"),
                    language: std::mem::take(lang),
                });
                code = None;
            } else {
                lines.push(line);
            }
            continue;
        }
        let trimmed = line.trim_start();
        if let Some(lang) = trimmed.strip_prefix("```") {
            flush_paragraph(&mut blocks, &mut paragraph);
            code = Some((lang.trim().to_string(), Vec::new()));
        } else if let Some(h) = trimmed.strip_prefix("### ") {
            flush_paragraph(&mut blocks, &mut paragraph);
            blocks.push(Block {
                kind: BlockKind::H3,
                text: h.into(),
                language: String::new(),
            });
        } else if let Some(h) = trimmed.strip_prefix("## ") {
            flush_paragraph(&mut blocks, &mut paragraph);
            blocks.push(Block {
                kind: BlockKind::H2,
                text: h.into(),
                language: String::new(),
            });
        } else if let Some(h) = trimmed.strip_prefix("# ") {
            flush_paragraph(&mut blocks, &mut paragraph);
            blocks.push(Block {
                kind: BlockKind::H1,
                text: h.into(),
                language: String::new(),
            });
        } else if let Some(item) = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
        {
            flush_paragraph(&mut blocks, &mut paragraph);
            blocks.push(Block {
                kind: BlockKind::Bullet,
                text: item.into(),
                language: String::new(),
            });
        } else if trimmed.is_empty() {
            flush_paragraph(&mut blocks, &mut paragraph);
        } else {
            paragraph.push(trimmed);
        }
    }
    // An unterminated fence is still rendered as code (it is mid-stream).
    if let Some((lang, lines)) = code {
        blocks.push(Block {
            kind: BlockKind::Code,
            text: lines.join("\n"),
            language: lang,
        });
    }
    flush_paragraph(&mut blocks, &mut paragraph);
    blocks
}

fn to_widget_block(b: &Block) -> MarkdownBlock {
    MarkdownBlock {
        kind: b.kind.as_int(),
        text: SharedString::from(b.text.as_str()),
        language: SharedString::from(b.language.as_str()),
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
