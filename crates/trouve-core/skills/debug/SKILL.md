---
name: debug
description: Diagnose a reproducible failure, isolate its cause, and verify a minimal correction.
argument-hint: "<failure or symptom>"
---

# Debug

Debug from evidence rather than symptoms.

1. Restate the observed failure and the expected behavior.
2. Reproduce it with the smallest relevant command or test when possible.
3. Trace the failing path and form falsifiable hypotheses.
4. Run focused checks that distinguish those hypotheses; avoid unrelated rewrites.
5. Identify the root cause and explain why it produces the symptom.
6. If the user requested a fix, make the smallest coherent correction and add or update a regression test.
7. Re-run the reproduction plus proportionate surrounding tests, then report the evidence and any remaining uncertainty.
