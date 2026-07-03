//! Token-savings tracking and reporting (port of `semble/stats.py`).

use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::Write;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use serde_json::json;

use crate::store::resolve_cache_folder;
use crate::types::{CallType, SearchResult};

fn stats_file() -> PathBuf {
    resolve_cache_folder().join("savings.jsonl")
}

/// Save stats about a search or find_related call to the stats file.
/// Failures are silent: stats must never break a search.
pub fn save_search_stats(
    results: &[SearchResult],
    call_type: CallType,
    file_sizes: &HashMap<String, usize>,
    max_snippet_lines: Option<usize>,
) {
    let snippet_chars: usize = results
        .iter()
        .map(|r| match max_snippet_lines {
            Some(0) => 0,
            Some(n) => r
                .chunk
                .content
                .lines()
                .take(n)
                .collect::<Vec<_>>()
                .join("\n")
                .len(),
            None => r.chunk.content.len(),
        })
        .sum();
    let unique_paths: std::collections::HashSet<&str> =
        results.iter().map(|r| r.chunk.file_path.as_str()).collect();
    let file_chars: usize = unique_paths.iter().filter_map(|p| file_sizes.get(*p)).sum();

    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    let record = json!({
        "ts": ts,
        "call": call_type.to_string(),
        "results": results.len(),
        "snippet_chars": snippet_chars,
        "file_chars": file_chars,
    });

    let path = stats_file();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&path) else {
        return;
    };
    // Advisory lock; skip the record if another process holds it.
    let fd = f.as_raw_fd();
    let locked = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) } == 0;
    if !locked {
        return;
    }
    let _ = writeln!(f, "{record}");
    unsafe { libc::flock(fd, libc::LOCK_UN) };
}

#[derive(Debug, Default, Clone)]
pub struct BucketStats {
    pub calls: u64,
    pub snippet_chars: u64,
    pub file_chars: u64,
    pub saved_chars: u64,
}

impl BucketStats {
    fn add(&mut self, snippet_chars: u64, file_chars: u64) {
        self.calls += 1;
        self.snippet_chars += snippet_chars;
        self.file_chars += file_chars;
        self.saved_chars += file_chars.saturating_sub(snippet_chars);
    }
}

#[derive(Debug, Default)]
pub struct SavingsSummary {
    pub today: BucketStats,
    pub last_7_days: BucketStats,
    pub all_time: BucketStats,
    pub call_type_counts: HashMap<String, u64>,
}

#[derive(Deserialize)]
struct StatsRecord {
    ts: f64,
    call: String,
    snippet_chars: u64,
    file_chars: u64,
}

/// Read savings.jsonl and return a summary.
pub fn build_savings_summary(path: &Path) -> SavingsSummary {
    let mut summary = SavingsSummary::default();
    let Ok(text) = std::fs::read_to_string(path) else {
        return summary;
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    const DAY: f64 = 86_400.0;
    let today_start = (now / DAY).floor() * DAY;
    let seven_days_ago = today_start - 6.0 * DAY;

    for line in text.lines() {
        let Ok(record) = serde_json::from_str::<StatsRecord>(line) else {
            continue;
        };
        *summary
            .call_type_counts
            .entry(record.call.clone())
            .or_insert(0) += 1;
        summary
            .all_time
            .add(record.snippet_chars, record.file_chars);
        if record.ts >= seven_days_ago {
            summary
                .last_7_days
                .add(record.snippet_chars, record.file_chars);
        }
        if record.ts >= today_start {
            summary.today.add(record.snippet_chars, record.file_chars);
        }
    }
    summary
}

fn format_token_count(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        format!("~{:.1}M", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("~{:.1}k", tokens as f64 / 1_000.0)
    } else {
        format!("~{tokens}")
    }
}

fn format_calls(calls: u64) -> String {
    if calls >= 1_000 {
        format!("{:.1}k", calls as f64 / 1_000.0)
    } else {
        calls.to_string()
    }
}

fn bucket_row(label: &str, bucket: &BucketStats) -> String {
    let saved_tokens = bucket.saved_chars / 4;
    let pct = if bucket.file_chars > 0 {
        (bucket.saved_chars as f64 / bucket.file_chars as f64 * 100.0).round() as u64
    } else {
        0
    };
    format!(
        "  {:<14}  {:>8}  {:>14}  {}%",
        label,
        format_calls(bucket.calls),
        format_token_count(saved_tokens) + " tokens",
        pct
    )
}

/// Return a formatted token-savings report.
pub fn format_savings_report() -> String {
    let path = stats_file();
    if !path.exists() {
        return "No stats yet. Run a search first.".to_string();
    }
    let summary = build_savings_summary(&path);
    let total_saved = summary.all_time.saved_chars / 4;
    let overall_pct = if summary.all_time.file_chars > 0 {
        (summary.all_time.saved_chars as f64 / summary.all_time.file_chars as f64 * 100.0).round()
            as u64
    } else {
        0
    };

    let mut lines = vec![
        String::new(),
        "  Semble Token Savings".to_string(),
        format!("  {}", "=".repeat(60)),
        String::new(),
        format!(
            "  Total saved:  {} tokens  ({overall_pct}%)",
            format_token_count(total_saved)
        ),
        format!("  Total calls:  {}", format_calls(summary.all_time.calls)),
        String::new(),
        "  By Period".to_string(),
        format!("  {}", "-".repeat(60)),
        format!("  {:<14}  {:>8}  {:>14}  Ratio", "Period", "Calls", "Saved"),
        format!("  {}", "-".repeat(60)),
        bucket_row("Today", &summary.today),
        bucket_row("Last 7 days", &summary.last_7_days),
        bucket_row("All time", &summary.all_time),
    ];

    if !summary.call_type_counts.is_empty() {
        lines.push(String::new());
        lines.push("  By Call Type".to_string());
        lines.push(format!("  {}", "-".repeat(60)));
        let mut sorted: Vec<(&String, &u64)> = summary.call_type_counts.iter().collect();
        sorted.sort_by(|a, b| b.1.cmp(a.1));
        let total: u64 = summary.call_type_counts.values().sum();
        for (i, (call_type, count)) in sorted.iter().enumerate() {
            lines.push(format!(
                "  {}.  {:<16}  {:>8}  {:>4.0}%",
                i + 1,
                call_type,
                format_calls(**count),
                **count as f64 / total as f64 * 100.0
            ));
        }
    }
    lines.push(String::new());
    lines.join("\n")
}

/// Delete the savings file. Returns true if one existed.
pub fn clear_savings() -> Option<PathBuf> {
    let path = stats_file();
    if path.exists() {
        std::fs::remove_file(&path).ok()?;
        Some(path)
    } else {
        None
    }
}
