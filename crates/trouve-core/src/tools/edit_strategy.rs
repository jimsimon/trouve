//! Model-specific edit-tool catalog policy.
//!
//! Preferences are enforced where tools are advertised and executed rather
//! than relying on prompt wording. Strict hashline profiles are intentionally
//! opt-in: only models backed by representative benchmark evidence belong in
//! `BENCHMARKED_HASHLINE_PROFILES`.

use trouve_providers::ToolSpec;

pub const HASHLINE_FALLBACK_FAILURES: u8 = 2;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum EditStrategy {
    /// Advertise every normal edit strategy without favoring one.
    #[default]
    Auto,
    /// Keep every normal strategy available, with `apply_patch` identified as
    /// the model's preferred existing-file editor.
    PreferApplyPatch,
    /// Keep every normal strategy available, with hashline identified as the
    /// preferred existing-file editor.
    PreferHashline,
    /// Require the model's trained apply-patch format for existing files.
    /// Used only by isolated benchmark processes; every mutation path except
    /// the selected editor is hidden and denied.
    EnforceApplyPatch,
    /// Require hashline in an isolated benchmark process. Every mutation path
    /// except hashline is hidden and denied.
    EnforceHashline,
}

#[derive(Debug, Clone, Copy)]
struct ModelEditProfile {
    prefix: &'static str,
    strategy: EditStrategy,
}

const TRAINED_FORMAT_PROFILES: &[ModelEditProfile] = &[ModelEditProfile {
    prefix: "codex/",
    strategy: EditStrategy::PreferApplyPatch,
}];

// Add a model here only after the benchmark in docs/design/hashline-edits.md
// demonstrates lower token/retry cost without a correctness regression.
const BENCHMARKED_HASHLINE_PROFILES: &[ModelEditProfile] = &[];

pub fn for_model(model: &str) -> EditStrategy {
    match std::env::var("TROUVE_EDIT_BENCHMARK_STRATEGY") {
        Ok(value) => {
            return benchmark_override(&value).unwrap_or_else(|| {
                panic!(
                    "invalid TROUVE_EDIT_BENCHMARK_STRATEGY={value:?}; expected apply_patch or hashline"
                )
            });
        }
        Err(std::env::VarError::NotPresent) => {}
        Err(std::env::VarError::NotUnicode(_)) => {
            panic!("TROUVE_EDIT_BENCHMARK_STRATEGY must be valid UTF-8")
        }
    }
    BENCHMARKED_HASHLINE_PROFILES
        .iter()
        .chain(TRAINED_FORMAT_PROFILES)
        .find(|profile| model.starts_with(profile.prefix))
        .map_or(EditStrategy::Auto, |profile| profile.strategy)
}

fn benchmark_override(value: &str) -> Option<EditStrategy> {
    match value.trim().to_ascii_lowercase().as_str() {
        "apply_patch" => Some(EditStrategy::EnforceApplyPatch),
        "hashline" => Some(EditStrategy::EnforceHashline),
        _ => None,
    }
}

pub(super) fn is_enforced_benchmark(strategy: EditStrategy) -> bool {
    matches!(
        strategy,
        EditStrategy::EnforceApplyPatch | EditStrategy::EnforceHashline
    )
}

pub(super) fn benchmark_tool_allowed(strategy: EditStrategy, name: &str) -> bool {
    let selected_editor = match strategy {
        EditStrategy::EnforceApplyPatch => "apply_patch",
        EditStrategy::EnforceHashline => "hashline_edit",
        _ => return true,
    };
    name == selected_editor
        || matches!(
            name,
            "read_file" | "list_dir" | "git_diff" | "glob" | "grep" | "search" | "find_related"
        )
}

pub(super) fn advertise(strategy: EditStrategy, mut spec: ToolSpec) -> Option<ToolSpec> {
    let name = spec.name.as_str();
    if !benchmark_tool_allowed(strategy, name) {
        return None;
    }
    match strategy {
        EditStrategy::Auto => {
            if name == "apply_patch_fallback" {
                return None;
            }
        }
        EditStrategy::PreferApplyPatch => {
            if name == "apply_patch_fallback" {
                return None;
            }
            if name == "apply_patch" {
                spec.description = format!(
                    "Preferred existing-file edit strategy for this model. {}",
                    spec.description
                );
            } else if matches!(name, "edit_file" | "hashline_edit") {
                spec.description = format!(
                    "Alternative edit strategy; prefer apply_patch unless this operation is a better fit. {}",
                    spec.description
                );
            }
        }
        EditStrategy::PreferHashline => {
            if name == "apply_patch_fallback" {
                return None;
            }
            if name == "hashline_edit" {
                spec.description = format!(
                    "Preferred existing-file edit strategy for this model. {}",
                    spec.description
                );
            } else if matches!(name, "edit_file" | "apply_patch") {
                spec.description = format!(
                    "Alternative edit strategy; prefer hashline_edit after a hashline read. {}",
                    spec.description
                );
            }
        }
        EditStrategy::EnforceApplyPatch => {
            if name == "apply_patch" {
                spec.description = format!(
                    "Required existing-file edit strategy for this benchmark run. {}",
                    spec.description
                );
            }
        }
        EditStrategy::EnforceHashline => {
            if name == "read_file" {
                spec.description = format!(
                    "For existing-file edits, read with format=\"hashline\" before calling hashline_edit. {}",
                    spec.description
                );
            } else if name == "hashline_edit" {
                spec.description = format!(
                    "Required existing-file edit strategy for this model. {}",
                    spec.description
                );
            }
        }
    }
    Some(spec)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_prefers_its_trained_patch_format() {
        assert_eq!(
            for_model("codex/gpt-5.6-sol"),
            EditStrategy::PreferApplyPatch
        );
        assert_eq!(for_model("openai/gpt-5.6-sol"), EditStrategy::Auto);
    }

    #[test]
    fn benchmark_overrides_are_strict_and_explicit() {
        assert_eq!(
            benchmark_override("apply_patch"),
            Some(EditStrategy::EnforceApplyPatch)
        );
        assert_eq!(
            benchmark_override("HASHLINE"),
            Some(EditStrategy::EnforceHashline)
        );
        assert_eq!(benchmark_override("automatic"), None);
    }

    #[test]
    fn benchmark_catalog_allows_only_read_tools_and_selected_editor() {
        assert!(benchmark_tool_allowed(
            EditStrategy::EnforceApplyPatch,
            "apply_patch"
        ));
        assert!(benchmark_tool_allowed(
            EditStrategy::EnforceHashline,
            "hashline_edit"
        ));
        for name in [
            "write_file",
            "delete_file",
            "shell",
            "web_fetch",
            "mcp__x__edit",
        ] {
            assert!(!benchmark_tool_allowed(
                EditStrategy::EnforceApplyPatch,
                name
            ));
            assert!(!benchmark_tool_allowed(EditStrategy::EnforceHashline, name));
        }
    }
}
