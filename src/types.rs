//! Core data types shared across the index, search, and serialization layers.

use std::collections::HashMap;
use std::fmt;

use serde::{Deserialize, Serialize};

/// Call type for token-savings tracking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallType {
    Search,
    FindRelated,
}

impl fmt::Display for CallType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CallType::Search => write!(f, "search"),
            CallType::FindRelated => write!(f, "find_related"),
        }
    }
}

/// Content type for indexing and search pipeline selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentType {
    Code,
    Docs,
    Config,
}

impl ContentType {
    pub const ALL: [ContentType; 3] = [ContentType::Code, ContentType::Docs, ContentType::Config];

    pub fn as_str(&self) -> &'static str {
        match self {
            ContentType::Code => "code",
            ContentType::Docs => "docs",
            ContentType::Config => "config",
        }
    }

    pub fn parse(s: &str) -> Option<ContentType> {
        match s {
            "code" => Some(ContentType::Code),
            "docs" => Some(ContentType::Docs),
            "config" => Some(ContentType::Config),
            _ => None,
        }
    }
}

impl fmt::Display for ContentType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A single indexable unit of code.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Chunk {
    pub content: String,
    pub file_path: String,
    pub start_line: u32,
    pub end_line: u32,
    #[serde(default)]
    pub language: Option<String>,
}

impl Chunk {
    /// File path and line range as a string.
    pub fn location(&self) -> String {
        format!("{}:{}-{}", self.file_path, self.start_line, self.end_line)
    }
}

/// A single search result with score and source chunk index.
#[derive(Debug, Clone)]
pub struct SearchResult {
    pub chunk: Chunk,
    pub score: f64,
}

/// Statistics about the current index state.
#[derive(Debug, Clone, Default, Serialize)]
pub struct IndexStats {
    pub indexed_files: usize,
    pub total_chunks: usize,
    pub languages: HashMap<String, usize>,
}
