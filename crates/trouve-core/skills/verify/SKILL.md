---
name: verify
description: Verify that an implementation satisfies its request using concrete tests and inspection.
argument-hint: "[claim or scope]"
---

# Verify

Verify the requested outcome independently.

1. Translate the request into observable acceptance criteria.
2. Inspect the implementation paths that are supposed to satisfy each criterion.
3. Run the smallest relevant tests, builds, linters, or manual checks, expanding only when risk warrants it.
4. Treat skipped, flaky, unavailable, or unrelated checks explicitly; never present an unrun check as passing.
5. Report the result criterion by criterion with the command or evidence that supports it.
6. Call out failures, coverage gaps, and residual risk before any summary.

Do not modify the implementation unless the user also asks you to fix discovered problems.
