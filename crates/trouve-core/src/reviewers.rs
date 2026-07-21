use trouve_protocol::ReviewerProfile;

pub const DEFAULT_REVIEWER_IDS: &[&str] =
    &["correctness", "security", "api-compatibility", "testing"];

fn built_in(id: &str, name: &str, prompt: &str) -> ReviewerProfile {
    ReviewerProfile {
        id: id.into(),
        name: name.into(),
        prompt: prompt.into(),
        model: None,
        built_in: true,
    }
}

/// Stable, built-in reviewers. Keep ids durable: repository policies
/// and queued jobs persist them.
pub fn built_in_reviewers() -> Vec<ReviewerProfile> {
    vec![
        built_in(
            "correctness",
            "Correctness & Edge Cases",
            "Find behavior that is incorrect for reachable inputs or states. Trace changed control flow, invariants, boundary conditions, null/empty/error cases, and interactions with unchanged callers. Prefer concrete failures over speculative concerns.",
        ),
        built_in(
            "security",
            "Security & Privacy",
            "Look for authorization or authentication bypasses, injection, unsafe deserialization, secret or personal-data exposure, path and command traversal, cryptographic misuse, insecure defaults, and trust-boundary violations introduced by the change.",
        ),
        built_in(
            "reliability",
            "Reliability & Error Handling",
            "Review failure paths, retries, timeouts, cancellation, cleanup, partial writes, idempotency, resource lifetime, and recovery after interruption. Identify failures that can corrupt state, leak resources, hang, or hide actionable errors.",
        ),
        built_in(
            "performance",
            "Performance & Scalability",
            "Find algorithmic regressions, unbounded work or memory, avoidable network or database round trips, blocking work on async paths, missing batching or pagination, cache invalidation problems, and hot-path allocations with material impact.",
        ),
        built_in(
            "concurrency",
            "Concurrency & Async",
            "Analyze races, deadlocks, lock ordering, cancellation races, task and process lifetime, atomicity, lost wakeups, duplicate work, and unsafe assumptions about serialization across threads, workers, or replicas.",
        ),
        built_in(
            "api-compatibility",
            "API & Compatibility",
            "Check public APIs, wire formats, schemas, migrations, configuration, persisted data, CLI behavior, and downstream callers for breaking or ambiguous changes. Verify backward/forward compatibility and safe rollout behavior.",
        ),
        built_in(
            "data-integrity",
            "Data Integrity & Migrations",
            "Review database and state transitions for transactional safety, constraints, migration compatibility, precision or encoding loss, ordering assumptions, duplicate handling, rollback safety, and consistency between durable and in-memory state.",
        ),
        built_in(
            "testing",
            "Tests & Verification",
            "Identify changed behavior that lacks meaningful coverage, tests that can pass while the implementation is broken, missing negative or boundary cases, nondeterministic tests, and validation that does not exercise the real integration path.",
        ),
        built_in(
            "maintainability",
            "Maintainability & Architecture",
            "Look for unnecessary coupling, duplicated sources of truth, violated module boundaries, misleading abstractions, brittle control flow, unreachable or obsolete code, and complexity that is likely to cause future correctness defects.",
        ),
        built_in(
            "dependencies",
            "Dependencies & Supply Chain",
            "Inspect dependency, lockfile, build, packaging, and CI changes for unsafe sources, accidental upgrades or downgrades, feature mismatches, license or provenance concerns, non-reproducible builds, and deployment incompatibilities.",
        ),
        built_in(
            "accessibility",
            "Frontend UX & Accessibility",
            "Review user-facing changes for keyboard and screen-reader access, focus and state management, semantic structure, contrast and motion concerns, responsive behavior, localization, destructive-action safety, and confusing failure states.",
        ),
        built_in(
            "operations",
            "Observability & Operations",
            "Check logging, metrics, tracing, health behavior, configuration, deployment, rate limiting, backpressure, alertability, and operational failure modes. Flag changes that make incidents harder to detect, diagnose, contain, or recover from.",
        ),
    ]
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
    }
}
