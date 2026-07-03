//! Code-aware chunking.
//!
//! Port of `semble/chunking/core.py` and `semble/chunking/chunking.py`: a
//! recursive tree-sitter node merge produces chunk boundaries close to a
//! desired length, with a line-based fallback for languages without a grammar.

use std::cell::RefCell;
use std::collections::HashMap;

use tree_sitter::{Language, Node, Parser};

use crate::types::Chunk;

/// The desired length of chunks, in bytes of source text.
pub const DESIRED_CHUNK_LENGTH: usize = 750;

const RECURSION_DEPTH: usize = 500;
const MIN_CHUNK_SIZE: usize = 50;

/// The output of the internal chunking algorithm (byte offsets).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkBoundary {
    pub start: usize,
    pub end: usize,
}

/// Return the tree-sitter language for an upstream language name, if bundled.
fn language_for(name: &str) -> Option<Language> {
    let lang = match name {
        "bash" => tree_sitter_bash::LANGUAGE,
        "c" => tree_sitter_c::LANGUAGE,
        "cpp" => tree_sitter_cpp::LANGUAGE,
        "csharp" => tree_sitter_c_sharp::LANGUAGE,
        "css" => tree_sitter_css::LANGUAGE,
        "elixir" => tree_sitter_elixir::LANGUAGE,
        "go" => tree_sitter_go::LANGUAGE,
        "haskell" => tree_sitter_haskell::LANGUAGE,
        "html" => tree_sitter_html::LANGUAGE,
        "java" => tree_sitter_java::LANGUAGE,
        "javascript" => tree_sitter_javascript::LANGUAGE,
        "json" => tree_sitter_json::LANGUAGE,
        "kotlin" => tree_sitter_kotlin_ng::LANGUAGE,
        "lua" => tree_sitter_lua::LANGUAGE,
        "markdown" => tree_sitter_md::LANGUAGE,
        "ocaml" => tree_sitter_ocaml::LANGUAGE_OCAML,
        "ocaml_interface" => tree_sitter_ocaml::LANGUAGE_OCAML_INTERFACE,
        "php" => tree_sitter_php::LANGUAGE_PHP,
        "python" => tree_sitter_python::LANGUAGE,
        "ruby" => tree_sitter_ruby::LANGUAGE,
        "rust" => tree_sitter_rust::LANGUAGE,
        "scala" => tree_sitter_scala::LANGUAGE,
        "swift" => tree_sitter_swift::LANGUAGE,
        "toml" => tree_sitter_toml_ng::LANGUAGE,
        "tsx" => tree_sitter_typescript::LANGUAGE_TSX,
        "typescript" => tree_sitter_typescript::LANGUAGE_TYPESCRIPT,
        "yaml" => tree_sitter_yaml::LANGUAGE,
        "zig" => tree_sitter_zig::LANGUAGE,
        _ => return None,
    };
    Some(lang.into())
}

/// True if a tree-sitter grammar is bundled for this language name.
pub fn is_supported_language(name: &str) -> bool {
    language_for(name).is_some()
}

thread_local! {
    static PARSERS: RefCell<HashMap<&'static str, Option<Parser>>> = RefCell::new(HashMap::new());
}

fn with_parser<R>(language: &str, f: impl FnOnce(&mut Parser) -> R) -> Option<R> {
    // Intern via the static language table so the thread-local key is 'static.
    let key: &'static str = match language_for(language) {
        Some(_) => crate::languages::EXTENSION_TO_LANGUAGE
            .iter()
            .map(|(_, l)| *l)
            .find(|l| *l == language)?,
        None => return None,
    };
    PARSERS.with(|cell| {
        let mut map = cell.borrow_mut();
        let entry = map.entry(key).or_insert_with(|| {
            let lang = language_for(key)?;
            let mut parser = Parser::new();
            parser.set_language(&lang).ok()?;
            Some(parser)
        });
        entry.as_mut().map(f)
    })
}

/// Merge adjacent chunks up to the desired length.
fn merge_adjacent_chunks(chunks: &[ChunkBoundary], desired_length: usize) -> Vec<ChunkBoundary> {
    let mut merged = Vec::new();
    let mut current_start = chunks[0].start;
    let mut current_end = chunks[0].end;
    let mut current_length = current_end - current_start;

    for group in &chunks[1..] {
        let (start, end) = (group.start, group.end);
        let length = end - start;
        if current_length + length > desired_length {
            merged.push(ChunkBoundary {
                start: current_start,
                end: current_end,
            });
            current_start = start;
            current_end = end;
            current_length = length;
            continue;
        }
        current_end = end;
        current_length += length;
    }
    merged.push(ChunkBoundary {
        start: current_start,
        end: current_end,
    });
    merged
}

/// Recursively merge and split nodes.
fn merge_node_inner(node: Node<'_>, desired_length: usize, depth: usize) -> Vec<ChunkBoundary> {
    if node.child_count() == 0 {
        return vec![ChunkBoundary {
            start: node.start_byte(),
            end: node.end_byte(),
        }];
    }

    let length = node.end_byte() - node.start_byte();
    if depth > RECURSION_DEPTH || length < MIN_CHUNK_SIZE {
        return vec![ChunkBoundary {
            start: node.start_byte(),
            end: node.end_byte(),
        }];
    }

    let children: Vec<Node<'_>> = {
        let mut cursor = node.walk();
        node.children(&mut cursor).collect()
    };

    let mut groups: Vec<ChunkBoundary> = Vec::new();
    let mut index = 0;
    while index < children.len() {
        let child = children[index];
        let start = child.start_byte();
        let mut end = child.end_byte();
        let mut length = end - start;
        index += 1;

        // If this single chunk is longer than the desired length, split it again.
        if length > desired_length {
            groups.extend(merge_node_inner(child, desired_length, depth + 1));
            continue;
        }

        while index < children.len() {
            let child = children[index];
            let child_length = child.end_byte() - child.start_byte();
            if length + child_length > desired_length {
                break;
            }
            end = child.end_byte();
            length += child_length;
            index += 1;
        }

        groups.push(ChunkBoundary { start, end });
    }
    groups
}

fn merge_node(node: Node<'_>, desired_length: usize) -> Vec<ChunkBoundary> {
    let raw = merge_node_inner(node, desired_length, 0);
    merge_adjacent_chunks(&raw, desired_length)
}

/// Chunk source code by line (fallback when no grammar is available).
pub fn chunk_lines(text: &str, desired_length: usize) -> Vec<ChunkBoundary> {
    if text.trim().is_empty() {
        return Vec::new();
    }
    let mut groups = Vec::new();
    let bytes = text.as_bytes();
    let mut line_start = 0;
    for (i, b) in bytes.iter().enumerate() {
        if *b == b'\n' {
            groups.push(ChunkBoundary {
                start: line_start,
                end: i + 1,
            });
            line_start = i + 1;
        }
    }
    if line_start < bytes.len() {
        groups.push(ChunkBoundary {
            start: line_start,
            end: bytes.len(),
        });
    }
    merge_adjacent_chunks(&groups, desired_length)
}

/// Chunk source code using tree-sitter. Returns None when the language has no
/// bundled grammar (callers fall back to line chunking).
pub fn chunk(text: &str, language: &str, desired_length: usize) -> Option<Vec<ChunkBoundary>> {
    if text.trim().is_empty() {
        return Some(Vec::new());
    }
    with_parser(language, |parser| {
        let tree = parser.parse(text.as_bytes(), None)?;
        Some(merge_node(tree.root_node(), desired_length))
    })
    .flatten()
}

/// Chunk pre-read source text into [`Chunk`]s (port of `chunk_source`).
pub fn chunk_source(source: &str, file_path: &str, language: Option<&str>) -> Vec<Chunk> {
    if source.trim().is_empty() {
        return Vec::new();
    }
    let boundaries = match language {
        Some(lang) if is_supported_language(lang) => chunk(source, lang, DESIRED_CHUNK_LENGTH)
            .unwrap_or_else(|| chunk_lines(source, DESIRED_CHUNK_LENGTH)),
        _ => chunk_lines(source, DESIRED_CHUNK_LENGTH),
    };

    let bytes = source.as_bytes();
    let mut chunks = Vec::with_capacity(boundaries.len());
    // Boundaries are sorted and non-overlapping, so newlines are counted with
    // a single forward cursor instead of rescanning from 0 per boundary.
    let mut cursor = 0usize;
    let mut newlines_at_cursor = 0usize;
    let count_to = |pos: usize, cursor: &mut usize, newlines: &mut usize| {
        debug_assert!(pos >= *cursor);
        *newlines += count_newlines(&bytes[*cursor..pos]);
        *cursor = pos;
        *newlines
    };
    for boundary in boundaries {
        let (start, end) = (boundary.start, boundary.end.min(bytes.len()));
        // Clamp so zero-length boundaries take a single character (upstream
        // slices source[start : max(end-1, start)+1]).
        let text: &str = if end > start {
            &source[start..end]
        } else {
            let mut char_end = start + 1;
            while char_end < bytes.len() && !source.is_char_boundary(char_end) {
                char_end += 1;
            }
            if start >= bytes.len() {
                ""
            } else {
                &source[start..char_end.min(bytes.len())]
            }
        };
        let start_line = count_to(start, &mut cursor, &mut newlines_at_cursor) as u32 + 1;
        let newlines_to_end = count_to(end, &mut cursor, &mut newlines_at_cursor);
        let last_is_newline = end > start && bytes[end - 1] == b'\n';
        let end_line = (newlines_to_end - usize::from(last_is_newline)) as u32 + 1;
        chunks.push(Chunk {
            content: text.to_string(),
            file_path: file_path.to_string(),
            start_line,
            end_line: end_line.max(start_line),
            language: language.map(|l| l.to_string()),
        });
    }
    chunks
}

fn count_newlines(bytes: &[u8]) -> usize {
    bytes.iter().filter(|b| **b == b'\n').count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_source_yields_no_chunks() {
        assert!(chunk_source("", "a.py", Some("python")).is_empty());
        assert!(chunk_source("  \n \t ", "a.py", Some("python")).is_empty());
    }

    #[test]
    fn line_chunking_merges_lines() {
        let text = "line one\nline two\nline three\n";
        let boundaries = chunk_lines(text, 750);
        assert_eq!(boundaries.len(), 1);
        assert_eq!(boundaries[0].start, 0);
        assert_eq!(boundaries[0].end, text.len());
    }

    #[test]
    fn line_chunking_splits_at_desired_length() {
        let text = "a".repeat(100) + "\n" + &"b".repeat(100) + "\n";
        let boundaries = chunk_lines(&text, 150);
        assert_eq!(boundaries.len(), 2);
    }

    #[test]
    fn python_chunking_covers_source() {
        let source = "def foo():\n    return 1\n\n\ndef bar():\n    return 2\n";
        let chunks = chunk_source(source, "a.py", Some("python"));
        assert!(!chunks.is_empty());
        let combined: String = chunks.iter().map(|c| c.content.as_str()).collect();
        assert!(combined.contains("def foo()"));
        assert!(combined.contains("def bar()"));
        assert_eq!(chunks[0].start_line, 1);
    }

    #[test]
    fn rust_chunking_splits_large_functions() {
        let mut source = String::new();
        for i in 0..30 {
            source.push_str(&format!(
                "fn function_number_{i}() -> u64 {{\n    let value = {i} * 42;\n    value + 7\n}}\n\n"
            ));
        }
        let chunks = chunk_source(&source, "a.rs", Some("rust"));
        assert!(chunks.len() > 1);
        for c in &chunks {
            assert!(c.start_line <= c.end_line);
        }
        // Chunks are sorted and non-overlapping by construction.
        for w in chunks.windows(2) {
            assert!(w[0].start_line <= w[1].start_line);
        }
    }

    #[test]
    fn unknown_language_falls_back_to_lines() {
        let source = "some content here\nthat is plain text\n".repeat(5);
        let chunks = chunk_source(&source, "a.xyz", Some("nonexistent_lang"));
        assert!(!chunks.is_empty());
    }

    #[test]
    fn unicode_content_is_handled() {
        let source = "def foo():\n    return \"héllo wörld 🎉\"\n".repeat(30);
        let chunks = chunk_source(&source, "a.py", Some("python"));
        assert!(!chunks.is_empty());
        for c in &chunks {
            // All content must be valid UTF-8 slices (would panic otherwise).
            assert!(!c.content.is_empty());
        }
    }

    #[test]
    fn supported_language_names() {
        assert!(is_supported_language("python"));
        assert!(is_supported_language("rust"));
        assert!(is_supported_language("tsx"));
        assert!(!is_supported_language("cobol"));
    }
}
