//! Command-line interface (port of `semble/cli.py`).
//!
//! `trouve search|find-related|clear|savings|install|uninstall`, with the
//! bare `trouve [--content ...]` invocation starting the MCP stdio server,
//! matching upstream dispatch behaviour.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand, ValueEnum};

use crate::index::TrouveIndex;
use crate::stats::{clear_savings, format_savings_report};
use crate::store::{clear_all_stores, resolve_cache_folder};
use crate::types::ContentType;
use crate::utils::{format_results, is_git_url, resolve_chunk};

#[derive(Parser)]
#[command(
    name = "trouve",
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
    /// Interactively configure trouve across coding agents.
    Install,
    /// Interactively remove trouve configuration from coding agents.
    Uninstall,
    /// Internal debug helpers used by the parity harness.
    #[command(hide = true)]
    Debug {
        #[command(subcommand)]
        command: DebugCommand,
    },
}

#[derive(Subcommand)]
enum DebugCommand {
    /// Chunk one file and print the chunks as JSON.
    Chunk { file: PathBuf },
    /// Tokenize text (BM25 tokenizer) and print the tokens as JSON.
    Tokenize { text: String },
    /// Print BM25 scores for a query over a JSON list of documents on stdin.
    Bm25 { query: String },
}

fn build_index(path: &str, content: &[ContentType]) -> anyhow::Result<TrouveIndex> {
    if is_git_url(path) {
        TrouveIndex::from_git(path, None, content, None)
    } else {
        TrouveIndex::from_path(&PathBuf::from(path), content, None)
    }
}

fn load_index_or_exit(path: &str, content: &[ContentType]) -> Result<TrouveIndex, ExitCode> {
    build_index(path, content).map_err(|e| {
        eprintln!("{e:#}");
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
        if let Some(report) = crate::clone_cache::clear_clones() {
            println!(
                "Cleared {} cached clone(s) at `{}`",
                report.removed,
                report.root.display()
            );
            if report.skipped_locked > 0 {
                println!(
                    "Skipped {} clone(s) in use by another trouve process",
                    report.skipped_locked
                );
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
            "files_from_snapshot": build.files_from_snapshot,
            "files_from_store": build.files_from_store,
            "files_computed": build.files_computed,
            // Documented in the subcommand help: fraction of files served
            // from a cache layer (snapshot splice or store) this build.
            "cache_hit_rate": (build.cache_hit_rate() * 1000.0).round() / 1000.0,
        },
    });
    println!("{}", serde_json::to_string_pretty(&out).unwrap());
    ExitCode::SUCCESS
}

/// Entry point for the trouve command-line tool.
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
        Some(CliCommand::Debug { command }) => run_debug(command),
    }
}

fn run_debug(command: DebugCommand) -> ExitCode {
    match command {
        DebugCommand::Chunk { file } => {
            let Ok(source) = crate::languages::read_file_text(&file) else {
                eprintln!("Cannot read {}", file.display());
                return ExitCode::FAILURE;
            };
            let language = crate::languages::detect_language(&file);
            let chunks = crate::chunk::chunk_source(&source, &file.to_string_lossy(), language);
            let out: Vec<serde_json::Value> = chunks
                .iter()
                .map(|c| {
                    serde_json::json!({
                        "start_line": c.start_line,
                        "end_line": c.end_line,
                        "content": c.content,
                        "language": c.language,
                    })
                })
                .collect();
            println!("{}", serde_json::Value::Array(out));
            ExitCode::SUCCESS
        }
        DebugCommand::Tokenize { text } => {
            println!(
                "{}",
                serde_json::to_string(&crate::tokens::tokenize(&text)).unwrap()
            );
            ExitCode::SUCCESS
        }
        DebugCommand::Bm25 { query } => {
            let mut input = String::new();
            use std::io::Read;
            if std::io::stdin().read_to_string(&mut input).is_err() {
                return ExitCode::FAILURE;
            }
            let Ok(docs) = serde_json::from_str::<Vec<String>>(&input) else {
                eprintln!("stdin must be a JSON array of document strings");
                return ExitCode::FAILURE;
            };
            let tokenized: Vec<Vec<String>> =
                docs.iter().map(|d| crate::tokens::tokenize(d)).collect();
            let index = crate::bm25::Bm25Index::build(&tokenized);
            let scores = index.get_scores(&crate::tokens::tokenize(&query), None);
            println!("{}", serde_json::to_string(&scores).unwrap());
            ExitCode::SUCCESS
        }
    }
}
