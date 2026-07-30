---
name: code-review
description: Review code changes for correctness, regressions, maintainability, and missing tests.
argument-hint: "[scope or concern]"
---

# Code review

Review the requested change as an independent engineer.

1. Establish the intended behavior from the request, surrounding code, and tests.
2. Inspect the actual diff and the callers or contracts it affects.
3. Look for correctness bugs, regressions, unsafe assumptions, error-handling gaps, concurrency issues, and missing coverage.
4. Verify material claims with focused read-only checks or tests when practical.
5. Report findings first, ordered by severity. Include precise file and line references, impact, and a concrete remediation.
6. Keep summaries brief. If there are no findings, say so and identify any residual testing risk.

Do not modify code unless the user explicitly asks for fixes.
