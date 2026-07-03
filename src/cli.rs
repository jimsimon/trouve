//! Command-line interface (port of `semble/cli.py`).
//!
//! `semble search|find-related|clear|savings|install|uninstall`, with the
//! bare `semble [--content ...]` invocation starting the MCP stdio server,
//! matching upstream dispatch behaviour.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand, ValueEnum};

use crate::index::SembleIndex;
use crate::stats::{clear_savings, format_savings_report};
use crate::store::{clear_all_stores, resolve_cache_folder};
use crate::types::ContentType;
use crate::utils::{format_results, is_git_url, resolve_chunk};

#[derive(Parser)]
#[command(
    name = "semble",
    version,
    about = "Instant local code search for agents.",
    long_about = "Fast and accurate code search for agents. Runs as an MCP stdio server when \
                  invoked without a subcommand."
)]
struct Cli {
    #[command(subcommand)]
    command: Option<CliCommand>,
    #[command(flatten)]
    content: ContentArgs,
}

#[derive(Args)]
struct ContentArgs {
    /// Content types to index (space-separated, e.g. --content code docs).
    #[arg(long, num_args = 1.., value_name = "TYPE", default_values = ["code"])]
    content: Vec<ContentChoice>,
}

#[derive(Clone, Copy, PartialEq, ValueEnum)]
enum ContentChoice {
    Code,
    Docs,
    Config,
    All,
}

impl ContentArgs {
    fn resolve(&self) -> Vec<ContentType> {
        if self.content.iter().any(|c| matches!(c, ContentChoice::All)) {
            return ContentType::ALL.to_vec();
        }
        self.content
            .iter()
            .map(|c| match c {
                ContentChoice::Code => ContentType::Code,
                ContentChoice::Docs => ContentType::Docs,
                ContentChoice::Config => ContentType::Config,
                ContentChoice::All => unreachable!(),
            })
            .collect()
    }
}

#[derive(Subcommand)]
enum CliCommand {
    /// Search a codebase.
    Search {
        /// Natural language or code query.
        query: String,
        /// Local path or git URL (default: current directory).
        #[arg(default_value = ".")]
        path: String,
        /// Number of results.
        #[arg(short = 'k', long = "top-k", default_value_t = 5)]
        top_k: usize,
        /// Lines of source per result (default: full chunk). 10 = signature + body, 0 = no code.
        #[arg(long, value_name = "N")]
        max_snippet_lines: Option<usize>,
        #[command(flatten)]
        content: ContentArgs,
    },
    /// Find code similar to a specific location.
    FindRelated {
        /// File path as shown in search results.
        file_path: String,
        /// Line number (1-indexed).
        line: u32,
        /// Local path or git URL (default: current directory).
        #[arg(default_value = ".")]
        path: String,
        /// Number of results.
        #[arg(short = 'k', long = "top-k", default_value_t = 5)]
        top_k: usize,
        /// Lines of source per result (default: full chunk).
        #[arg(long, value_name = "N")]
        max_snippet_lines: Option<usize>,
        #[command(flatten)]
        content: ContentArgs,
    },
    /// Clear the index cache.
    Clear {
        /// Type of cache to clear.
        #[arg(value_parser = ["all", "index", "savings"])]
        r#type: String,
    },
    /// Show token savings and usage stats.
    Savings,
    /// Show index stats for a path (files, chunks, cache hit rate).
    Stats {
        /// Local path (default: current directory).
        #[arg(default_value = ".")]
        path: String,
        #[command(flatten)]
        content: ContentArgs,
    },
    /// Interactively configure semble across coding agents.
    Install,
    /// Interactively remove semble configuration from coding agents.
    Uninstall,
}

fn build_index(path: &str, content: &[ContentType]) -> anyhow::Result<SembleIndex> {
    if is_git_url(path) {
        SembleIndex::from_git(path, None, content, None)
    } else {
        SembleIndex::from_path(&PathBuf::from(path), content, None)
    }
}

fn load_index_or_exit(path: &str, content: &[ContentType]) -> Result<SembleIndex, ExitCode> {
    build_index(path, content).map_err(|e| {
        eprintln!("{e}");
        ExitCode::FAILURE
    })
}

fn run_search(
    path: &str,
    query: &str,
    top_k: usize,
    content: &[ContentType],
    max_snippet_lines: Option<usize>,
) -> ExitCode {
    let index = match load_index_or_exit(path, content) {
        Ok(i) => i,
        Err(code) => return code,
    };
    let results = index.search(query, top_k, None, None, None, None, max_snippet_lines);
    let out = if results.is_empty() {
        serde_json::json!({"error": "No results found."})
    } else {
        format_results(query, &results, max_snippet_lines)
    };
    println!("{out}");
    ExitCode::SUCCESS
}

fn run_find_related(
    path: &str,
    file_path: &str,
    line: u32,
    top_k: usize,
    content: &[ContentType],
    max_snippet_lines: Option<usize>,
) -> ExitCode {
    let index = match load_index_or_exit(path, content) {
        Ok(i) => i,
        Err(code) => return code,
    };
    let Some(chunk) = resolve_chunk(&index.chunks, file_path, line).cloned() else {
        eprintln!("No chunk found at {file_path}:{line}.");
        return ExitCode::FAILURE;
    };
    let results = index.find_related(&chunk, top_k, max_snippet_lines);
    let label = format!("Chunks related to {file_path}:{line}");
    let out = if results.is_empty() {
        serde_json::json!({"error": format!("No related chunks found for {file_path}:{line}.")})
    } else {
        format_results(&label, &results, max_snippet_lines)
    };
    println!("{out}");
    ExitCode::SUCCESS
}

fn run_clear(clear_type: &str) -> ExitCode {
    if clear_type == "index" || clear_type == "all" {
        let removed = clear_all_stores();
        if removed.is_empty() {
            println!(
                "No indexes found to clear in `{}`",
                resolve_cache_folder().display()
            );
        } else {
            for path in removed {
                println!("Cleared index store at `{}`", path.display());
            }
        }
    }
    if clear_type == "savings" || clear_type == "all" {
        match clear_savings() {
            Some(path) => println!("Cleared savings at `{}`", path.display()),
            None => println!(
                "No savings file found at `{}`",
                resolve_cache_folder().join("savings.jsonl").display()
            ),
        }
    }
    ExitCode::SUCCESS
}

fn run_stats(path: &str, content: &[ContentType]) -> ExitCode {
    let index = match load_index_or_exit(path, content) {
        Ok(i) => i,
        Err(code) => return code,
    };
    let stats = index.stats();
    let build = &index.build_stats;
    let out = serde_json::json!({
        "indexed_files": stats.indexed_files,
        "total_chunks": stats.total_chunks,
        "languages": stats.languages,
        "build": {
            "files_total": build.files_total,
            "files_from_store": build.files_from_store,
            "files_computed": build.files_computed,
        },
    });
    println!("{}", serde_json::to_string_pretty(&out).unwrap());
    ExitCode::SUCCESS
}

/// Entry point for the semble command-line tool.
pub fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        None => crate::mcp::serve(&cli.content.resolve()),
        Some(CliCommand::Search {
            query,
            path,
            top_k,
            max_snippet_lines,
            content,
        }) => run_search(&path, &query, top_k, &content.resolve(), max_snippet_lines),
        Some(CliCommand::FindRelated {
            file_path,
            line,
            path,
            top_k,
            max_snippet_lines,
            content,
        }) => run_find_related(
            &path,
            &file_path,
            line,
            top_k,
            &content.resolve(),
            max_snippet_lines,
        ),
        Some(CliCommand::Clear { r#type }) => run_clear(&r#type),
        Some(CliCommand::Savings) => {
            println!("{}", format_savings_report());
            ExitCode::SUCCESS
        }
        Some(CliCommand::Stats { path, content }) => run_stats(&path, &content.resolve()),
        Some(CliCommand::Install) => crate::installer::run(crate::installer::Mode::Install),
        Some(CliCommand::Uninstall) => crate::installer::run(crate::installer::Mode::Uninstall),
    }
}
