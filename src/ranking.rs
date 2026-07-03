//! Code-tuned reranking: query boosts and path penalties.
//!
//! Port of `semble/ranking/boosting.py`, `penalties.py`, and `weighting.py`.
//! These heuristics are small but load-bearing for retrieval quality.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::OnceLock;

use regex::Regex;

use crate::tokens::split_identifier;
use crate::types::Chunk;

const ALPHA_SYMBOL: f64 = 0.3; // lean BM25 for exact keyword matching
const ALPHA_NL: f64 = 0.5; // balanced semantic + BM25

const EMBEDDED_STEM_MIN_LEN: usize = 4;
const EMBEDDED_SYMBOL_BOOST_SCALE: f64 = 0.5;
const DEFINITION_BOOST_MULTIPLIER: f64 = 3.0;
const STEM_BOOST_MULTIPLIER: f64 = 1.0;
const FILE_COHERENCE_BOOST_FRAC: f64 = 0.2;

const STRONG_PENALTY: f64 = 0.3; // test files, compat shims, example/doc code
const MODERATE_PENALTY: f64 = 0.5; // re-export / metadata files
const MILD_PENALTY: f64 = 0.7; // .d.ts declaration stubs

const FILE_SATURATION_THRESHOLD: usize = 1;
const FILE_SATURATION_DECAY: f64 = 0.5;

static STOPWORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "by", "do", "does", "for", "from", "has", "have",
    "how", "if", "in", "is", "it", "not", "of", "on", "or", "the", "to", "was", "what", "when",
    "where", "which", "who", "why", "with",
];

static DEFINITION_KEYWORDS: &[&str] = &[
    "class",
    "module",
    "defmodule",
    "def",
    "interface",
    "struct",
    "enum",
    "trait",
    "type",
    "func",
    "function",
    "object",
    "abstract class",
    "data class",
    "fn",
    "fun",
    "package",
    "namespace",
    "protocol",
    "record",
    "typedef",
];

static SQL_DEFINITION_KEYWORDS: &[&str] = &[
    "CREATE TABLE",
    "CREATE VIEW",
    "CREATE PROCEDURE",
    "CREATE FUNCTION",
];

fn symbol_query_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"^(?:[A-Za-z_][A-Za-z0-9_]*(?:(?:::|\\|->|\.)[A-Za-z_][A-Za-z0-9_]*)+|_[A-Za-z0-9_]*|[A-Za-z][A-Za-z0-9]*[A-Z_][A-Za-z0-9_]*|[A-Z][A-Za-z0-9]*)$",
        )
        .unwrap()
    })
}

fn embedded_symbol_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"\b(?:[A-Z][a-z][a-zA-Z0-9]*[A-Z][a-zA-Z0-9]*|[a-z][a-zA-Z0-9]*[A-Z][a-zA-Z0-9]+)\b",
        )
        .unwrap()
    })
}

fn keyword_word_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"[a-zA-Z_][a-zA-Z0-9_]*").unwrap())
}

/// Return true if the query looks like a bare symbol or namespace-qualified identifier.
pub fn is_symbol_query(query: &str) -> bool {
    symbol_query_re().is_match(query.trim())
}

/// Return the blending weight for semantic scores, auto-detecting from query type.
pub fn resolve_alpha(query: &str, alpha: Option<f64>) -> f64 {
    if let Some(a) = alpha {
        return a;
    }
    if is_symbol_query(query) {
        ALPHA_SYMBOL
    } else {
        ALPHA_NL
    }
}

/// Extract the final identifier from a possibly namespace-qualified query.
fn extract_symbol_name(query: &str) -> String {
    for separator in ["::", "\\", "->", "."] {
        if let Some(idx) = query.rfind(separator) {
            return query[idx + separator.len()..].to_string();
        }
    }
    query.trim().to_string()
}

/// Compile the (general, SQL) definition patterns for one symbol name.
///
/// Upstream uses `(?:^|(?<=\s))` — the `regex` crate has no lookbehind, so
/// `(?:^|\s)` is used instead; only match existence is tested, never spans.
fn definition_patterns(symbol_name: &str) -> (Regex, Regex) {
    let escaped = regex::escape(symbol_name);
    let ns_prefix = r"(?:[A-Za-z_][A-Za-z0-9_]*(?:\.|::))*";
    let keyword_body = DEFINITION_KEYWORDS
        .iter()
        .map(|k| regex::escape(k))
        .collect::<Vec<_>>()
        .join("|");
    let sql_body = SQL_DEFINITION_KEYWORDS
        .iter()
        .map(|k| regex::escape(k))
        .collect::<Vec<_>>()
        .join("|");
    let suffix = format!(r")\s+{ns_prefix}{escaped}(?:\s|[<({{:\[;]|$)");
    let general = Regex::new(&format!(r"(?m)(?:^|\s)(?:{keyword_body}{suffix}")).unwrap();
    let sql = Regex::new(&format!(r"(?mi)(?:^|\s)(?:{sql_body}{suffix}")).unwrap();
    (general, sql)
}

struct DefinitionMatcher {
    patterns: Vec<(Regex, Regex)>,
    names: Vec<String>,
}

impl DefinitionMatcher {
    fn new(names: &[String]) -> DefinitionMatcher {
        DefinitionMatcher {
            patterns: names.iter().map(|n| definition_patterns(n)).collect(),
            names: names.to_vec(),
        }
    }

    fn chunk_defines_any(&self, chunk: &Chunk) -> bool {
        self.patterns
            .iter()
            .any(|(general, sql)| general.is_match(&chunk.content) || sql.is_match(&chunk.content))
    }
}

fn file_stem(path: &str) -> String {
    Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_lowercase()
}

/// Return true if `stem` matches `name` (exact, snake_case-normalised, or plural).
fn stem_matches(stem: &str, name: &str) -> bool {
    let stem_norm = stem.replace('_', "");
    stem == name
        || stem_norm == name
        || stem.trim_end_matches('s') == name
        || stem_norm.trim_end_matches('s') == name
}

/// Return the boost amount for a chunk that defines one of the names (0.0 if none match).
fn definition_tier(chunk: &Chunk, matcher: &DefinitionMatcher, boost_unit: f64) -> f64 {
    if !matcher.chunk_defines_any(chunk) {
        return 0.0;
    }
    let stem = file_stem(&chunk.file_path);
    let stem_hit = matcher
        .names
        .iter()
        .any(|name| stem_matches(&stem, &name.to_lowercase()));
    boost_unit * if stem_hit { 1.5 } else { 1.0 }
}

/// Scores keyed by chunk index into the shared chunk list.
pub type ScoreMap = HashMap<usize, f64>;

/// Promote files with multiple high-scoring chunks by boosting their top chunk (in-place).
pub fn boost_multi_chunk_files(scores: &mut ScoreMap, chunks: &[Chunk]) {
    if scores.is_empty() {
        return;
    }
    let max_score = scores.values().cloned().fold(f64::MIN, f64::max);
    if max_score == 0.0 {
        return;
    }
    let mut file_sum: HashMap<&str, f64> = HashMap::new();
    let mut best_chunk: HashMap<&str, usize> = HashMap::new();
    // Iterate in index order for deterministic tie-breaks.
    let mut indices: Vec<usize> = scores.keys().copied().collect();
    indices.sort_unstable();
    for i in indices {
        let score = scores[&i];
        let file_path = chunks[i].file_path.as_str();
        *file_sum.entry(file_path).or_insert(0.0) += score;
        match best_chunk.get(file_path) {
            Some(existing) if scores[existing] >= score => {}
            _ => {
                best_chunk.insert(file_path, i);
            }
        }
    }
    let max_file_sum = file_sum.values().cloned().fold(f64::MIN, f64::max);
    let boost_unit = max_score * FILE_COHERENCE_BOOST_FRAC;
    for (file_path, idx) in best_chunk {
        *scores.get_mut(&idx).unwrap() += boost_unit * file_sum[file_path] / max_file_sum;
    }
}

/// Apply query-type boosts to candidate scores (port of `apply_query_boost`).
pub fn apply_query_boost(scores: &mut ScoreMap, query: &str, chunks: &[Chunk]) {
    if scores.is_empty() {
        return;
    }
    let max_score = scores.values().cloned().fold(f64::MIN, f64::max);
    if is_symbol_query(query) {
        boost_symbol_definitions(scores, query, max_score, chunks);
    } else {
        boost_stem_matches(scores, query, max_score, chunks);
        boost_embedded_symbols(scores, query, max_score, chunks);
    }
}

fn boost_symbol_definitions(scores: &mut ScoreMap, query: &str, max_score: f64, chunks: &[Chunk]) {
    let symbol_name = extract_symbol_name(query);
    let mut names = vec![symbol_name.clone()];
    let trimmed = query.trim().to_string();
    if symbol_name != trimmed {
        names.push(trimmed);
    }
    let matcher = DefinitionMatcher::new(&names);
    let boost_unit = max_score * DEFINITION_BOOST_MULTIPLIER;

    let candidate_indices: Vec<usize> = scores.keys().copied().collect();
    for i in candidate_indices {
        let tier = definition_tier(&chunks[i], &matcher, boost_unit);
        if tier != 0.0 {
            *scores.get_mut(&i).unwrap() += tier;
        }
    }

    let symbol_lower = symbol_name.to_lowercase();
    for (i, chunk) in chunks.iter().enumerate() {
        if scores.contains_key(&i) {
            continue;
        }
        if !stem_matches(&file_stem(&chunk.file_path), &symbol_lower) {
            continue;
        }
        let tier = definition_tier(chunk, &matcher, boost_unit);
        if tier != 0.0 {
            scores.insert(i, tier);
        }
    }
}

fn boost_embedded_symbols(scores: &mut ScoreMap, query: &str, max_score: f64, chunks: &[Chunk]) {
    let names: Vec<String> = {
        let set: HashSet<String> = embedded_symbol_re()
            .find_iter(query)
            .map(|m| m.as_str().to_string())
            .collect();
        let mut v: Vec<String> = set.into_iter().collect();
        v.sort();
        v
    };
    if names.is_empty() {
        return;
    }
    let matcher = DefinitionMatcher::new(&names);
    let boost_unit = max_score * DEFINITION_BOOST_MULTIPLIER * EMBEDDED_SYMBOL_BOOST_SCALE;

    let candidate_indices: Vec<usize> = scores.keys().copied().collect();
    for i in candidate_indices {
        let tier = definition_tier(&chunks[i], &matcher, boost_unit);
        if tier != 0.0 {
            *scores.get_mut(&i).unwrap() += tier;
        }
    }

    let symbols_lower: Vec<String> = names.iter().map(|s| s.to_lowercase()).collect();
    for (i, chunk) in chunks.iter().enumerate() {
        if scores.contains_key(&i) {
            continue;
        }
        let stem = file_stem(&chunk.file_path);
        let stem_norm = stem.replace('_', "");
        let hit = symbols_lower.iter().any(|symbol_lower| {
            stem == *symbol_lower
                || stem_norm == *symbol_lower
                || (stem.len() >= EMBEDDED_STEM_MIN_LEN && symbol_lower.starts_with(&stem))
                || (stem_norm.len() >= EMBEDDED_STEM_MIN_LEN
                    && symbol_lower.starts_with(&stem_norm))
        });
        if !hit {
            continue;
        }
        let tier = definition_tier(chunk, &matcher, boost_unit);
        if tier != 0.0 {
            scores.insert(i, tier);
        }
    }
}

/// Count query keywords that match path parts, allowing prefix overlap (min 3 chars).
fn count_keyword_matches(keywords: &HashSet<String>, parts: &HashSet<String>) -> usize {
    let exact: usize = keywords.iter().filter(|k| parts.contains(*k)).count();
    if exact == keywords.len() {
        return exact;
    }
    let mut n_matches = exact;
    for keyword in keywords.iter().filter(|k| !parts.contains(*k)) {
        for part in parts {
            let (shorter, longer) = if keyword.len() <= part.len() {
                (keyword.as_str(), part.as_str())
            } else {
                (part.as_str(), keyword.as_str())
            };
            if shorter.len() >= 3 && longer.starts_with(shorter) {
                n_matches += 1;
                break;
            }
        }
    }
    n_matches
}

fn boost_stem_matches(scores: &mut ScoreMap, query: &str, max_score: f64, chunks: &[Chunk]) {
    let stopwords: HashSet<&str> = STOPWORDS.iter().copied().collect();
    let keywords: HashSet<String> = keyword_word_re()
        .find_iter(query)
        .map(|m| m.as_str().to_lowercase())
        .filter(|w| w.len() > 2 && !stopwords.contains(w.as_str()))
        .collect();
    if keywords.is_empty() {
        return;
    }
    let boost = max_score * STEM_BOOST_MULTIPLIER;
    let mut path_cache: HashMap<&str, HashSet<String>> = HashMap::new();
    let candidate_indices: Vec<usize> = scores.keys().copied().collect();
    for i in candidate_indices {
        let file_path = chunks[i].file_path.as_str();
        let parts = path_cache.entry(file_path).or_insert_with(|| {
            let path = Path::new(file_path);
            let mut parts: HashSet<String> = path
                .file_stem()
                .and_then(|s| s.to_str())
                .map(|stem| split_identifier(stem).into_iter().collect())
                .unwrap_or_default();
            if let Some(parent) = path
                .parent()
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str())
            {
                if parent != "." && parent != "/" && parent != ".." {
                    parts.extend(split_identifier(parent));
                }
            }
            parts
        });
        let n_matches = count_keyword_matches(&keywords, parts);
        if n_matches > 0 {
            let match_ratio = n_matches as f64 / keywords.len() as f64;
            if match_ratio >= 0.10 {
                *scores.get_mut(&i).unwrap() += boost * match_ratio;
            }
        }
    }
}

fn test_file_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?:^|/)(?:test_[^/]*\.py|[^/]*_test\.py|[^/]*_test\.go|[^/]*Tests?\.java|[^/]*Test\.php|[^/]*_spec\.rb|[^/]*_test\.rb|[^/]*\.test\.[jt]sx?|[^/]*\.spec\.[jt]sx?|[^/]*Tests?\.kt|[^/]*Spec\.kt|[^/]*Tests?\.swift|[^/]*Spec\.swift|[^/]*Tests?\.cs|test_[^/]*\.cpp|[^/]*_test\.cpp|test_[^/]*\.c|[^/]*_test\.c|[^/]*Spec\.scala|[^/]*Suite\.scala|[^/]*Test\.scala|[^/]*_test\.dart|test_[^/]*\.dart|[^/]*_spec\.lua|[^/]*_test\.lua|test_[^/]*\.lua|test_helpers?[^/]*\.\w+)$",
        )
        .unwrap()
    })
}

fn test_dir_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?:^|/)(?:tests?|__tests__|spec|testing)(?:/|$)").unwrap())
}

fn compat_dir_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?:^|/)(?:compat|_compat|legacy)(?:/|$)").unwrap())
}

fn examples_dir_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?:^|/)(?:_?examples?|docs?_src)(?:/|$)").unwrap())
}

fn type_defs_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\.d\.ts$").unwrap())
}

static REEXPORT_FILENAMES: &[&str] = &["__init__.py", "package-info.java"];

/// Return a combined multiplicative penalty for all applicable path patterns.
pub fn file_path_penalty(file_path: &str) -> f64 {
    let normalised = file_path.replace('\\', "/");
    let mut penalty = 1.0;
    if test_file_re().is_match(&normalised) || test_dir_re().is_match(&normalised) {
        penalty *= STRONG_PENALTY;
    }
    let name = Path::new(file_path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    if REEXPORT_FILENAMES.contains(&name) {
        penalty *= MODERATE_PENALTY;
    }
    if compat_dir_re().is_match(&normalised) {
        penalty *= STRONG_PENALTY;
    }
    if examples_dir_re().is_match(&normalised) {
        penalty *= STRONG_PENALTY;
    }
    if type_defs_re().is_match(&normalised) {
        penalty *= MILD_PENALTY;
    }
    penalty
}

/// Select top-k results with optional file-path penalties and file-saturation
/// decay (port of `rerank_topk`). Returns `(chunk index, score)` pairs.
pub fn rerank_topk(
    scores: &ScoreMap,
    chunks: &[Chunk],
    top_k: usize,
    penalise_paths: bool,
) -> Vec<(usize, f64)> {
    if scores.is_empty() {
        return Vec::new();
    }

    let mut penalty_cache: HashMap<&str, f64> = HashMap::new();
    let mut penalised: Vec<(usize, f64)> = scores
        .iter()
        .map(|(i, score)| {
            let score = if penalise_paths {
                let file_path = chunks[*i].file_path.as_str();
                let penalty = *penalty_cache
                    .entry(file_path)
                    .or_insert_with(|| file_path_penalty(file_path));
                score * penalty
            } else {
                *score
            };
            (*i, score)
        })
        .collect();

    // Sort by penalised score (highest first); tie-break on index for determinism.
    penalised.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });

    let mut file_selected: HashMap<&str, usize> = HashMap::new();
    let mut selected: Vec<(f64, usize)> = Vec::new();
    let mut min_selected = f64::INFINITY;

    for (i, pen_score) in penalised {
        if selected.len() >= top_k && pen_score <= min_selected {
            break;
        }
        let file_path = chunks[i].file_path.as_str();
        let already = *file_selected.get(file_path).unwrap_or(&0);
        let mut eff_score = pen_score;
        if already >= FILE_SATURATION_THRESHOLD {
            let excess = already - FILE_SATURATION_THRESHOLD + 1;
            eff_score *= FILE_SATURATION_DECAY.powi(excess as i32);
        }
        selected.push((eff_score, i));
        file_selected.insert(file_path, already + 1);
        if selected.len() >= top_k {
            min_selected = selected
                .iter()
                .map(|(s, _)| *s)
                .fold(f64::INFINITY, f64::min);
        }
    }

    selected.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.1.cmp(&b.1))
    });
    selected.truncate(top_k);
    selected.into_iter().map(|(score, i)| (i, score)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(content: &str, path: &str) -> Chunk {
        Chunk {
            content: content.into(),
            file_path: path.into(),
            start_line: 1,
            end_line: 1,
            language: Some("python".into()),
        }
    }

    #[test]
    fn symbol_query_detection() {
        assert!(is_symbol_query("HandlerStack"));
        assert!(is_symbol_query("get_user"));
        assert!(is_symbol_query("Sinatra::Base"));
        assert!(is_symbol_query("_private"));
        assert!(is_symbol_query("Client"));
        assert!(!is_symbol_query("session"));
        assert!(!is_symbol_query("how does auth work"));
    }

    #[test]
    fn alpha_resolution() {
        assert_eq!(resolve_alpha("HandlerStack", None), 0.3);
        assert_eq!(resolve_alpha("how does auth work", None), 0.5);
        assert_eq!(resolve_alpha("anything", Some(0.9)), 0.9);
    }

    #[test]
    fn extracts_symbol_names() {
        assert_eq!(extract_symbol_name("Sinatra::Base"), "Base");
        assert_eq!(extract_symbol_name("Client"), "Client");
        assert_eq!(extract_symbol_name("a.b.c"), "c");
        assert_eq!(extract_symbol_name("Ns\\Klass"), "Klass");
    }

    #[test]
    fn definition_matching() {
        let matcher = DefinitionMatcher::new(&["Handler".to_string()]);
        assert!(matcher.chunk_defines_any(&chunk("class Handler:", "h.py")));
        assert!(matcher.chunk_defines_any(&chunk("pub struct Handler {", "h.rs")));
        assert!(matcher.chunk_defines_any(&chunk("defmodule Phoenix.Handler do", "h.ex")));
        assert!(matcher.chunk_defines_any(&chunk("CREATE TABLE Handler (", "h.sql")));
        assert!(matcher.chunk_defines_any(&chunk("create table Handler (", "h.sql")));
        assert!(!matcher.chunk_defines_any(&chunk("handler = Handler()", "h.py")));
        // Case-sensitive for general keywords.
        assert!(!matcher.chunk_defines_any(&chunk("Class Handler", "h.rb")));
    }

    #[test]
    fn symbol_boost_promotes_definition() {
        let chunks = vec![
            chunk("x = HandlerStack()", "usage.py"),
            chunk("class HandlerStack:\n    pass", "handler_stack.py"),
        ];
        let mut scores: ScoreMap = HashMap::new();
        scores.insert(0, 1.0);
        scores.insert(1, 0.5);
        apply_query_boost(&mut scores, "HandlerStack", &chunks);
        assert!(scores[&1] > scores[&0]);
    }

    #[test]
    fn symbol_boost_scans_non_candidates() {
        let chunks = vec![
            chunk("x = 1", "misc.py"),
            chunk("class Router:\n    pass", "router.py"),
        ];
        let mut scores: ScoreMap = HashMap::new();
        scores.insert(0, 1.0);
        apply_query_boost(&mut scores, "Router", &chunks);
        assert!(scores.contains_key(&1));
        assert!(scores[&1] > scores[&0]);
    }

    #[test]
    fn path_penalties() {
        assert_eq!(file_path_penalty("src/handler.py"), 1.0);
        assert!(file_path_penalty("tests/test_handler.py") < 1.0);
        assert!(file_path_penalty("src/__init__.py") < 1.0);
        assert!(file_path_penalty("examples/demo.py") < 1.0);
        assert!(file_path_penalty("types/index.d.ts") < 1.0);
        assert!(file_path_penalty("legacy/old.py") < 1.0);
    }

    #[test]
    fn rerank_applies_saturation() {
        let chunks = vec![
            chunk("a", "same.py"),
            chunk("b", "same.py"),
            chunk("c", "other.py"),
        ];
        let mut scores: ScoreMap = HashMap::new();
        scores.insert(0, 1.0);
        scores.insert(1, 0.9);
        scores.insert(2, 0.6);
        let ranked = rerank_topk(&scores, &chunks, 3, false);
        assert_eq!(ranked[0].0, 0);
        // Second chunk from same.py decays 0.9 -> 0.45, dropping below other.py's 0.6.
        assert_eq!(ranked[1].0, 2);
        assert_eq!(ranked[2].0, 1);
    }

    #[test]
    fn multi_chunk_file_boost() {
        let chunks = vec![
            chunk("a", "multi.py"),
            chunk("b", "multi.py"),
            chunk("c", "single.py"),
        ];
        let mut scores: ScoreMap = HashMap::new();
        scores.insert(0, 0.8);
        scores.insert(1, 0.7);
        scores.insert(2, 0.8);
        boost_multi_chunk_files(&mut scores, &chunks);
        // multi.py's top chunk gets a bigger coherence boost than single.py's.
        assert!(scores[&0] > scores[&2]);
        assert_eq!(scores[&1], 0.7);
    }

    #[test]
    fn stem_boost_for_nl_queries() {
        let chunks = vec![
            chunk("def connect(): pass", "database_connection.py"),
            chunk("def connect(): pass", "misc.py"),
        ];
        let mut scores: ScoreMap = HashMap::new();
        scores.insert(0, 0.5);
        scores.insert(1, 0.5);
        apply_query_boost(&mut scores, "database connection setup", &chunks);
        assert!(scores[&0] > scores[&1]);
    }
}
