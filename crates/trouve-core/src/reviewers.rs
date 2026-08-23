use trouve_protocol::{AgentPersona, PersonaGroup, ReviewerProfile};

pub const DEFAULT_REVIEWER_IDS: &[&str] = &[
    "correctness",
    "security",
    "concurrency",
    "api-compatibility",
    "testing",
];

pub const AUTO_BASELINE_REVIEWER_IDS: &[&str] = &["correctness", "security", "testing"];

fn built_in(id: &str, name: &str, prompt: &str) -> ReviewerProfile {
    ReviewerProfile {
        id: id.into(),
        name: name.into(),
        prompt: prompt.into(),
        model: None,
        default_thinking_level: None,
        built_in: true,
    }
}

/// Stable, built-in reviewers. Keep ids durable: repository policies
/// and queued jobs persist them.
pub fn built_in_reviewers() -> Vec<ReviewerProfile> {
    vec![
        built_in(
            "maintainability",
            "Software Architect",
            "Look for unnecessary coupling, duplicated sources of truth, violated module boundaries, misleading abstractions, brittle control flow, unreachable or obsolete code, and complexity that is likely to cause future correctness defects.",
        ),
        built_in(
            "correctness",
            "Correctness Analyst",
            "Find behavior that is incorrect for reachable inputs or states. Trace changed control flow, invariants, boundary conditions, null/empty/error cases, and interactions with unchanged callers. Prefer concrete failures over speculative concerns.",
        ),
        built_in(
            "security",
            "Security Engineer",
            "Look for authorization or authentication bypasses, injection, unsafe deserialization, secret or personal-data exposure, path and command traversal, cryptographic misuse, insecure defaults, and trust-boundary violations introduced by the change.",
        ),
        built_in(
            "reliability",
            "Application Reliability Engineer",
            "Review failure paths, retries, timeouts, cancellation, cleanup, partial writes, idempotency, resource lifetime, and recovery after interruption. Identify failures that can corrupt state, leak resources, hang, or hide actionable errors.",
        ),
        built_in(
            "performance",
            "Performance Engineer",
            "Find algorithmic regressions, unbounded work or memory, avoidable network or database round trips, blocking work on async paths, missing batching or pagination, cache invalidation problems, and hot-path allocations with material impact.",
        ),
        built_in(
            "concurrency",
            "Concurrency Specialist",
            "Analyze races, deadlocks, lock ordering and scope, cancellation races, task and process lifetime, atomicity, lost wakeups, duplicate work, and unsafe assumptions about serialization across threads, workers, or replicas. Trace synchronization guards through their full lifetime, especially across awaits, I/O, durable writes, callbacks, and state publication or removal.",
        ),
        built_in(
            "api-compatibility",
            "API Steward",
            "Check public APIs, wire formats, schemas, migrations, configuration, persisted data, CLI behavior, direct dependency API transitions, and downstream callers for breaking or ambiguous changes. Treat 0.x minor dependency upgrades as potentially breaking. Verify backward/forward compatibility and safe rollout behavior.",
        ),
        built_in(
            "data-integrity",
            "Data Integrity Specialist",
            "Review database and state transitions for transactional safety, constraints, migration compatibility, precision or encoding loss, ordering assumptions, duplicate handling, rollback safety, and consistency between durable and in-memory state.",
        ),
        built_in(
            "testing",
            "Test Engineer",
            "Identify changed behavior that lacks meaningful coverage, tests that can pass while the implementation is broken, missing negative or boundary cases, nondeterministic tests, and validation that does not exercise the real integration path.",
        ),
        built_in(
            "dependencies",
            "Supply Chain Analyst",
            "Inspect dependency, lockfile, build, packaging, and CI changes for unsafe sources, accidental upgrades or downgrades, feature mismatches, license or provenance concerns, non-reproducible builds, and deployment incompatibilities. For direct version or feature transitions, including 0.x minor and crypto, parser, or runtime upgrades, trace affected APIs into changed and unchanged call sites; verify required trait imports, output types, formatting, and other API changes, and prioritize concrete compile or runtime failures over speculative coverage concerns.",
        ),
        built_in(
            "accessibility",
            "Accessibility Specialist",
            "Review user-facing changes for keyboard and screen-reader access, focus and state management, semantic structure, contrast and motion concerns, responsive behavior, localization, destructive-action safety, and confusing failure states.",
        ),
        built_in(
            "operations",
            "Site Reliability Engineer",
            "Check logging, metrics, tracing, health behavior, configuration, deployment, rate limiting, backpressure, alertability, and operational failure modes. Flag changes that make incidents harder to detect, diagnose, contain, or recover from.",
        ),
    ]
}

/// Expose a review persona through the general agent-persona catalog. Review
/// personas are deliberately read-only and receive the same inspection tools
/// as the built-in Review persona when selected for an interactive thread.
pub fn reviewer_as_persona(reviewer: &ReviewerProfile) -> AgentPersona {
    AgentPersona {
        id: reviewer.id.clone(),
        display_name: reviewer.name.clone(),
        group: PersonaGroup::Reviewer,
        system_prompt: reviewer.prompt.clone(),
        allowed_tools: crate::personas::review_inspection_tools(),
        read_only: true,
        default_permission_mode: None,
        default_model: reviewer.model.clone(),
        default_thinking_level: reviewer.default_thinking_level.clone(),
    }
}

pub fn persona_as_reviewer(persona: &AgentPersona, built_in: bool) -> ReviewerProfile {
    ReviewerProfile {
        id: persona.id.clone(),
        name: persona.display_name.clone(),
        prompt: persona.system_prompt.clone(),
        model: persona.default_model.clone(),
        default_thinking_level: persona.default_thinking_level.clone(),
        built_in,
    }
}

pub fn default_reviewer_ids() -> Vec<String> {
    DEFAULT_REVIEWER_IDS
        .iter()
        .map(|reviewer| (*reviewer).to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn built_in_reviewer_ids_are_unique_and_defaults_exist() {
        let reviewers = built_in_reviewers();
        let ids: HashSet<_> = reviewers
            .iter()
            .map(|reviewer| reviewer.id.as_str())
            .collect();
        assert_eq!(ids.len(), reviewers.len());
        for default in DEFAULT_REVIEWER_IDS {
            assert!(ids.contains(default));
        }
        for baseline in AUTO_BASELINE_REVIEWER_IDS {
            assert!(
                ids.contains(baseline),
                "unknown baseline reviewer {baseline}"
            );
        }
        assert!(DEFAULT_REVIEWER_IDS.contains(&"concurrency"));
    }

    #[test]
    fn built_in_reviewer_names_describe_person_roles() {
        let reviewers = built_in_reviewers();
        for (id, name) in [
            ("correctness", "Correctness Analyst"),
            ("security", "Security Engineer"),
            ("reliability", "Application Reliability Engineer"),
            ("performance", "Performance Engineer"),
            ("concurrency", "Concurrency Specialist"),
            ("api-compatibility", "API Steward"),
            ("data-integrity", "Data Integrity Specialist"),
            ("testing", "Test Engineer"),
            ("maintainability", "Software Architect"),
            ("dependencies", "Supply Chain Analyst"),
            ("accessibility", "Accessibility Specialist"),
            ("operations", "Site Reliability Engineer"),
        ] {
            assert_eq!(
                reviewers
                    .iter()
                    .find(|reviewer| reviewer.id == id)
                    .unwrap()
                    .name,
                name
            );
        }
    }

    #[test]
    fn dependency_reviewers_cover_consumed_api_transitions() {
        let reviewers = built_in_reviewers();
        let api = reviewers
            .iter()
            .find(|reviewer| reviewer.id == "api-compatibility")
            .unwrap();
        let dependencies = reviewers
            .iter()
            .find(|reviewer| reviewer.id == "dependencies")
            .unwrap();

        assert!(api.prompt.contains("direct dependency API transitions"));
        assert!(api.prompt.contains("0.x minor dependency upgrades"));
        assert!(dependencies.prompt.contains("trace affected APIs"));
        assert!(dependencies.prompt.contains("required trait imports"));
        assert!(dependencies.prompt.contains("output types"));
        assert!(dependencies.prompt.contains("formatting"));
        assert!(
            dependencies
                .prompt
                .contains("concrete compile or runtime failures")
        );
    }

    #[test]
    fn reviewer_personas_inherit_the_builtin_review_policy() {
        let reviewer = built_in_reviewers().remove(0);
        let persona = reviewer_as_persona(&reviewer);
        let review = crate::personas::builtin_personas()
            .into_iter()
            .find(|candidate| candidate.id == "review")
            .unwrap();

        assert_eq!(persona.allowed_tools, review.allowed_tools);
        assert_eq!(persona.read_only, review.read_only);
        assert_eq!(
            persona.default_permission_mode,
            review.default_permission_mode
        );
        assert_eq!(persona.system_prompt, reviewer.prompt);
        assert_eq!(persona.group, PersonaGroup::Reviewer);
    }

    #[test]
    fn persona_conversion_preserves_canonical_values() {
        let persona = AgentPersona {
            id: "correctness".into(),
            display_name: "Correctness".into(),
            group: PersonaGroup::Reviewer,
            system_prompt: "Canonical prompt".into(),
            allowed_tools: Vec::new(),
            read_only: true,
            default_permission_mode: Some(trouve_protocol::PermissionMode::Ask),
            default_model: Some("provider/default".into()),
            default_thinking_level: Some("medium".into()),
        };
        let merged = persona_as_reviewer(&persona, true);
        assert_eq!(merged.name, "Correctness");
        assert_eq!(merged.prompt, "Canonical prompt");
        assert_eq!(merged.model.as_deref(), Some("provider/default"));
        assert_eq!(merged.default_thinking_level.as_deref(), Some("medium"));
        assert!(merged.built_in);
    }
}
