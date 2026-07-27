---
name: security-review
description: Audit a change or subsystem for exploitable security and trust-boundary failures.
argument-hint: "[scope or threat]"
---

# Security review

Perform an evidence-based security review of the requested scope.

1. Identify assets, actors, entry points, trust boundaries, and privileged side effects.
2. Trace untrusted data through authentication, authorization, parsing, storage, process execution, network access, and output encoding.
3. Check for injection, path traversal, secret exposure, confused-deputy behavior, insecure defaults, race conditions, unsafe deserialization, denial of service, and dependency misuse.
4. Distinguish exploitable findings from hardening suggestions. Do not inflate speculative risks.
5. Report findings first, ordered by severity, with exact evidence, an attack scenario, affected code locations, and a practical fix.
6. Note meaningful coverage gaps and residual risks when no vulnerability is confirmed.

Do not change code unless the user asks for remediation.
