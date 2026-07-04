//! Trouve: fast and accurate code search for agents.
//!
//! Rust port of [MinishLab/semble](https://github.com/MinishLab/semble) with a
//! content-addressed chunk store that makes indexing incremental, multithreaded,
//! and shared across git branches and worktrees.

pub mod bm25;
pub mod chunk;
pub mod cli;
pub mod clone_cache;
pub mod dense;
pub mod embed;
pub mod index;
pub mod languages;
pub mod manifest;
pub mod mcp;
pub mod ranking;
pub mod search;
pub mod snapshot;
pub mod stats;
pub mod store;
pub mod tokens;
pub mod types;
pub mod utils;
pub mod walker;
