//! Property tests for the chunker: invariants that must hold for any input.

use proptest::prelude::*;

use trouve::chunk::{chunk_lines, chunk_source, DESIRED_CHUNK_LENGTH};

fn assert_boundary_invariants(boundaries: &[trouve::chunk::ChunkBoundary], text_len: usize) {
    for b in boundaries {
        assert!(b.start <= b.end, "start must not exceed end");
        assert!(b.end <= text_len, "boundary must stay inside the text");
    }
    for w in boundaries.windows(2) {
        assert!(w[0].end <= w[1].start, "boundaries must not overlap");
    }
}

proptest! {
    #[test]
    fn line_chunks_cover_all_content(text in "[ -~\n]{0,2000}") {
        let boundaries = chunk_lines(&text, DESIRED_CHUNK_LENGTH);
        assert_boundary_invariants(&boundaries, text.len());
        if text.trim().is_empty() {
            prop_assert!(boundaries.is_empty());
        } else {
            // Line chunking is gapless: concatenated boundaries equal the text.
            let combined: String = boundaries
                .iter()
                .map(|b| &text[b.start..b.end])
                .collect();
            prop_assert_eq!(combined, text);
        }
    }

    #[test]
    fn line_chunks_respect_length_where_possible(
        text in "[a-z ]{1,60}(\n[a-z ]{1,60}){0,50}",
        desired in 80usize..400,
    ) {
        let boundaries = chunk_lines(&text, desired);
        // Any boundary longer than `desired` must be a single unsplittable line.
        for b in &boundaries {
            let segment = &text[b.start..b.end];
            if segment.len() > desired {
                prop_assert_eq!(segment.trim_end_matches('\n').lines().count(), 1);
            }
        }
    }

    #[test]
    fn python_chunks_are_ordered_and_in_bounds(
        names in proptest::collection::vec("[a-z_][a-z0-9_]{0,12}", 1..30),
    ) {
        let mut source = String::new();
        for (i, name) in names.iter().enumerate() {
            source.push_str(&format!("def {name}_{i}(value):\n    return value + {i}\n\n"));
        }
        let total_lines = source.lines().count() as u32;
        let chunks = chunk_source(&source, "gen.py", Some("python"));
        prop_assert!(!chunks.is_empty());
        for c in &chunks {
            prop_assert!(c.start_line >= 1);
            prop_assert!(c.start_line <= c.end_line);
            prop_assert!(c.end_line <= total_lines);
            prop_assert!(!c.content.is_empty());
        }
        for w in chunks.windows(2) {
            prop_assert!(w[0].start_line <= w[1].start_line);
        }
        // Every function definition appears in some chunk.
        let combined: String = chunks.iter().map(|c| c.content.as_str()).collect();
        for (i, name) in names.iter().enumerate() {
            let needle = format!("def {name}_{i}");
            prop_assert!(combined.contains(&needle));
        }
    }

    #[test]
    fn arbitrary_utf8_never_panics(text in "\\PC{0,500}") {
        // Fallback line chunking plus python parsing must never panic or
        // produce out-of-bounds slices on arbitrary unicode input.
        let _ = chunk_source(&text, "any.py", Some("python"));
        let _ = chunk_source(&text, "any.txt", None);
    }

    #[test]
    fn tokenize_roundtrip_is_lowercase(text in "[A-Za-z0-9_ ]{0,200}") {
        for token in trouve::tokens::tokenize(&text) {
            prop_assert_eq!(token.to_lowercase(), token.clone());
            prop_assert!(!token.is_empty());
        }
    }
}
