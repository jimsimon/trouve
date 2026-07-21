---
name: simplify
description: Simplify recently changed code without changing its behavior or public contract.
argument-hint: "[scope]"
disable-model-invocation: true
---

# Simplify

Simplify the requested code while preserving behavior.

1. Read the change, its tests, and the surrounding conventions before editing.
2. Remove accidental complexity: redundant branches, duplicated logic, needless indirection, stale comments, and overly broad abstractions.
3. Preserve public APIs, observable behavior, error semantics, and performance characteristics unless the user says otherwise.
4. Prefer small, idiomatic changes that make the intent more obvious. Do not turn the task into a broad refactor.
5. Run focused tests and formatting after the edit, and summarize what became simpler.
