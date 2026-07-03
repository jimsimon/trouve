//! Identifier tokenization for BM25 indexing (port of `semble/tokens.py`).

/// Emulates Python's `[A-Z]+(?=[A-Z][a-z])|[A-Z]?[a-z]+|[A-Z]+|[0-9]+` findall,
/// invoking `push` for each (not yet lowercased) part.
///
/// The Rust `regex` crate has no lookahead, so acronym-followed-by-word
/// boundaries ("getHTTPResponse" -> "HTTP" + "Response") are handled manually.
fn camel_scan(token: &str, push: &mut impl FnMut(&str)) {
    let bytes = token.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c.is_ascii_uppercase() {
            // Run of uppercase letters.
            let mut j = i + 1;
            while j < bytes.len() && bytes[j].is_ascii_uppercase() {
                j += 1;
            }
            let upper_run = j - i;
            if j < bytes.len() && bytes[j].is_ascii_lowercase() {
                if upper_run > 1 {
                    // Acronym followed by a Word: split before the last upper.
                    push(&token[i..j - 1]);
                    i = j - 1;
                }
                // Single upper followed by lowers: [A-Z]?[a-z]+
                let mut k = i + 1;
                while k < bytes.len() && bytes[k].is_ascii_lowercase() {
                    k += 1;
                }
                push(&token[i..k]);
                i = k;
            } else {
                push(&token[i..j]);
                i = j;
            }
        } else if c.is_ascii_lowercase() {
            let mut j = i + 1;
            while j < bytes.len() && bytes[j].is_ascii_lowercase() {
                j += 1;
            }
            push(&token[i..j]);
            i = j;
        } else if c.is_ascii_digit() {
            let mut j = i + 1;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                j += 1;
            }
            push(&token[i..j]);
            i = j;
        } else {
            i += 1;
        }
    }
}

/// Appends lowercased camel parts to `out`, returning the number of parts.
fn camel_findall_lower_into(token: &str, out: &mut Vec<String>) -> usize {
    let mut n = 0;
    camel_scan(token, &mut |s| {
        out.push(s.to_ascii_lowercase());
        n += 1;
    });
    n
}

/// Append the split of one identifier to `out` (see [`split_identifier`]).
///
/// Tokens from [`tokenize`] are ASCII by construction; ranking also feeds
/// file stems here, which may not be, so the full-token lowering falls back
/// to Unicode lowercasing in that case (camel parts are ASCII runs either way).
fn split_identifier_into(token: &str, out: &mut Vec<String>) {
    let lower = || {
        if token.is_ascii() {
            token.to_ascii_lowercase()
        } else {
            token.to_lowercase()
        }
    };
    let mark = out.len();
    if token.contains('_') {
        let lower = lower();
        out.extend(
            lower
                .split('_')
                .filter(|p| !p.is_empty())
                .map(str::to_string),
        );
        if out.len() - mark >= 2 {
            out.insert(mark, lower);
        } else {
            out.truncate(mark);
            out.push(lower);
        }
    } else {
        let n = camel_findall_lower_into(token, out);
        if n >= 2 {
            out.insert(mark, lower());
        } else {
            out.truncate(mark);
            out.push(lower());
        }
    }
}

/// Split a single identifier into sub-tokens via camelCase/snake_case.
///
/// Returns the original token (lowered) plus any sub-tokens.
/// E.g. "HandlerStack" -> ["handlerstack", "handler", "stack"]
///      "my_func" -> ["my_func", "my", "func"]
///      "simple" -> ["simple"]
pub fn split_identifier(token: &str) -> Vec<String> {
    let mut out = Vec::new();
    split_identifier_into(token, &mut out);
    out
}

/// Split text into lowercase identifier-like tokens for BM25 indexing.
///
/// Compound identifiers (camelCase, PascalCase, snake_case) are expanded into
/// sub-tokens so that partial matches work. The original compound token is
/// preserved for exact-match boosting. Matches Python's
/// `[a-zA-Z_][a-zA-Z0-9_]*` findall followed by `split_identifier`.
pub fn tokenize(text: &str) -> Vec<String> {
    let mut result = Vec::new();
    for_each_identifier(text, |ident| split_identifier_into(ident, &mut result));
    result
}

/// Invoke `f` for every `[a-zA-Z_][a-zA-Z0-9_]*` match in `text`.
fn for_each_identifier(text: &str, mut f: impl FnMut(&str)) {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b.is_ascii_alphabetic() || b == b'_' {
            let start = i;
            i += 1;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            f(&text[start..i]);
        } else {
            i += 1;
        }
    }
}

/// Flat token storage: all token bytes in one blob, delimited by cumulative
/// end offsets, grouped into documents by cumulative token counts.
///
/// Semantically identical to `Vec<Vec<String>>` (same tokens, same order) but
/// with three allocations total instead of one per token, which matters when
/// a large repo produces tens of millions of tokens.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct TokenDocs {
    /// Concatenated token bytes (ASCII lowercase by construction).
    pub blob: Vec<u8>,
    /// End offset of token `i` in `blob` (token `i` starts at `token_ends[i-1]`).
    pub token_ends: Vec<u32>,
    /// End index of doc `d` in `token_ends` (doc `d` starts at `doc_ends[d-1]`).
    pub doc_ends: Vec<u32>,
}

impl TokenDocs {
    pub fn n_docs(&self) -> usize {
        self.doc_ends.len()
    }

    pub fn n_tokens(&self) -> usize {
        self.token_ends.len()
    }

    fn doc_token_range(&self, doc: usize) -> std::ops::Range<usize> {
        let start = if doc == 0 {
            0
        } else {
            self.doc_ends[doc - 1] as usize
        };
        start..self.doc_ends[doc] as usize
    }

    pub fn doc_len(&self, doc: usize) -> usize {
        self.doc_token_range(doc).len()
    }

    pub fn token(&self, i: usize) -> &[u8] {
        let start = if i == 0 {
            0
        } else {
            self.token_ends[i - 1] as usize
        };
        &self.blob[start..self.token_ends[i] as usize]
    }

    /// Iterate over the tokens of one document.
    pub fn doc_tokens(&self, doc: usize) -> impl Iterator<Item = &[u8]> + '_ {
        self.doc_token_range(doc).map(move |i| self.token(i))
    }

    /// Append one token (must be valid UTF-8 bytes; tokens are ASCII here).
    pub fn push_token_bytes(&mut self, tok: &[u8]) {
        self.blob.extend_from_slice(tok);
        self.token_ends.push(self.blob.len() as u32);
    }

    /// Tokenize `text` (same output as [`tokenize`]) into the current doc.
    pub fn push_text(&mut self, text: &str) {
        for_each_identifier(text, |ident| split_identifier_flat(ident, self));
    }

    /// Close the current document.
    pub fn finish_doc(&mut self) {
        self.doc_ends.push(self.token_ends.len() as u32);
    }

    /// Append another `TokenDocs`, preserving document boundaries.
    pub fn append(&mut self, other: &TokenDocs) {
        let blob_base = self.blob.len() as u32;
        let tok_base = self.token_ends.len() as u32;
        self.blob.extend_from_slice(&other.blob);
        self.token_ends
            .extend(other.token_ends.iter().map(|e| e + blob_base));
        self.doc_ends
            .extend(other.doc_ends.iter().map(|e| e + tok_base));
    }

    /// Build from nested per-document token lists.
    pub fn from_nested(docs: &[Vec<String>]) -> TokenDocs {
        let mut out = TokenDocs::default();
        for doc in docs {
            for tok in doc {
                out.push_token_bytes(tok.as_bytes());
            }
            out.finish_doc();
        }
        out
    }
}

/// Flat-arena version of [`split_identifier_into`]: same tokens in the same
/// order, appended to `docs` without per-token allocations. `token` is ASCII
/// (guaranteed by [`for_each_identifier`]).
fn split_identifier_flat(token: &str, docs: &mut TokenDocs) {
    debug_assert!(token.is_ascii());
    let push_lower = |docs: &mut TokenDocs, s: &str| {
        docs.blob.extend(s.bytes().map(|b| b.to_ascii_lowercase()));
        docs.token_ends.push(docs.blob.len() as u32);
    };
    if token.contains('_') {
        let n = token.split('_').filter(|p| !p.is_empty()).count();
        push_lower(docs, token);
        if n >= 2 {
            for part in token.split('_').filter(|p| !p.is_empty()) {
                push_lower(docs, part);
            }
        }
    } else {
        push_lower(docs, token);
        if camel_count(token) >= 2 {
            camel_scan(token, &mut |s| push_lower(docs, s));
        }
    }
}

/// Number of parts [`camel_scan`] would produce.
fn camel_count(token: &str) -> usize {
    let mut n = 0;
    camel_scan(token, &mut |_| n += 1);
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_snake_case() {
        assert_eq!(split_identifier("my_func"), vec!["my_func", "my", "func"]);
        // Single sub-part after splitting: only the lowered original survives.
        assert_eq!(split_identifier("_private"), vec!["_private"]);
    }

    #[test]
    fn splits_camel_case() {
        assert_eq!(
            split_identifier("HandlerStack"),
            vec!["handlerstack", "handler", "stack"]
        );
        assert_eq!(
            split_identifier("getHTTPResponse"),
            vec!["gethttpresponse", "get", "http", "response"]
        );
        assert_eq!(
            split_identifier("XMLParser"),
            vec!["xmlparser", "xml", "parser"]
        );
    }

    #[test]
    fn simple_token_unchanged() {
        assert_eq!(split_identifier("simple"), vec!["simple"]);
        assert_eq!(split_identifier("UPPER"), vec!["upper"]);
    }

    #[test]
    fn tokenize_extracts_identifiers() {
        let toks = tokenize("def saveModel(path): return None");
        assert!(toks.contains(&"savemodel".to_string()));
        assert!(toks.contains(&"save".to_string()));
        assert!(toks.contains(&"model".to_string()));
        assert!(toks.contains(&"path".to_string()));
        assert!(!toks.contains(&"(".to_string()));
    }

    #[test]
    fn tokenize_numbers_in_identifiers() {
        assert_eq!(
            split_identifier("utf8Decode"),
            vec!["utf8decode", "utf", "8", "decode"]
        );
    }

    #[test]
    fn flat_tokenize_matches_nested() {
        let samples = [
            "def saveModel(path): return None",
            "getHTTPResponse XMLParser my_func _private __init__ UPPER",
            "utf8Decode a1b2C3 x _ __ a_b_c camelCaseID99",
            "",
            "no-idents-here 123 456",
        ];
        for text in samples {
            let nested = tokenize(text);
            let mut flat = TokenDocs::default();
            flat.push_text(text);
            flat.finish_doc();
            let flat_tokens: Vec<&[u8]> = flat.doc_tokens(0).collect();
            let nested_bytes: Vec<&[u8]> = nested.iter().map(|t| t.as_bytes()).collect();
            assert_eq!(flat_tokens, nested_bytes, "mismatch for {text:?}");
            assert_eq!(flat.doc_len(0), nested.len());
        }
    }

    #[test]
    fn token_docs_append_preserves_boundaries() {
        let mut a = TokenDocs::default();
        a.push_text("alpha beta");
        a.finish_doc();
        let mut b = TokenDocs::default();
        b.push_text("gamma");
        b.finish_doc();
        b.push_text("delta epsilon");
        b.finish_doc();
        a.append(&b);
        assert_eq!(a.n_docs(), 3);
        assert_eq!(a.doc_tokens(1).collect::<Vec<_>>(), vec![b"gamma".as_ref()]);
        assert_eq!(a.doc_len(2), 2);
        assert_eq!(a.doc_tokens(2).next().unwrap(), b"delta");
    }
}
