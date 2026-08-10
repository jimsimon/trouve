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
    /// Require hashline for existing-file edits. File creation and deletion
    /// remain available; a separately named patch fallback is gated until
    /// repeated hashline failures.
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
    BENCHMARKED_HASHLINE_PROFILES
        .iter()
        .chain(TRAINED_FORMAT_PROFILES)
        .find(|profile| model.starts_with(profile.prefix))
        .map_or(EditStrategy::Auto, |profile| profile.strategy)
}

pub(super) fn advertise(strategy: EditStrategy, mut spec: ToolSpec) -> Option<ToolSpec> {
    let name = spec.name.as_str();
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
        EditStrategy::EnforceHashline => {
            if matches!(name, "edit_file" | "apply_patch") {
                return None;
            }
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
}
