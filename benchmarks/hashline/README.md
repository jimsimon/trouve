# Hashline model benchmark

This benchmark determines whether a specific model should prefer or be
restricted to `hashline_edit`. It compares paired runs from identical repository
states and prompts; it is not a microbenchmark of the Rust parser.

## Task set

Use at least ten representative tasks and at least two independent runs of
each task/strategy (20 paired runs minimum). Include:

- a small single-line edit;
- several non-adjacent changes in one file;
- a complete function/class replacement;
- a cross-file code move using a register;
- a file rename and a file removal;
- a large-file edit with a narrow read window;
- a deliberately stale snapshot and retry;
- two disjoint concurrent edits in one session;
- a malformed edit that must fail without changing files;
- a task whose acceptance test catches a plausible but incorrect patch.

For each pair, reset to the same commit and use the same model revision,
provider, prompt, mode, thinking level, temperature, context, and acceptance
tests. Randomize strategy order to reduce warm-cache and service-load bias.
Record the full transcript and resulting Git diff for auditability.

Run each arm in its own process and isolated benchmark database/worktree. The
benchmark-only environment override makes the advertised and executable tool
catalog strict:

```sh
TROUVE_DATA_DIR=/tmp/trouve-edit-bench-apply \
  TROUVE_EDIT_BENCHMARK_STRATEGY=apply_patch cargo run -p trouve-app
TROUVE_DATA_DIR=/tmp/trouve-edit-bench-hashline \
  TROUVE_EDIT_BENCHMARK_STRATEGY=hashline cargo run -p trouve-app
```

The override accepts only `apply_patch` or `hashline`. It is intentionally
process-wide so one concurrent turn cannot contaminate another arm.

## JSON Lines format

Write one object per model run:

```json
{"model":"provider/model@revision","strategy":"apply_patch","task":"rename-symbol","run":1,"origin":"local","output_tokens":812,"edit_retries":1,"stale_retries":0,"executor_ms":42.5,"correct":true,"tests_passed":true,"concurrency_safe":true}
{"model":"provider/model@revision","strategy":"hashline","task":"rename-symbol","run":1,"origin":"local","output_tokens":431,"edit_retries":0,"stale_retries":0,"executor_ms":38.0,"correct":true,"tests_passed":true,"concurrency_safe":true}
```

`output_tokens` is the complete turn output, not only the tool argument.
`edit_retries` counts failed or superseded edit attempts. `stale_retries` is
the subset caused by stale snapshots. `executor_ms` is monotonic executor time,
excluding durable event-log wait. `correct`, `tests_passed`, and
`concurrency_safe` are separate so a passing unit test cannot conceal an
incorrect or unsafe patch.

Run the analyzer:

```sh
python3 benchmarks/hashline/analyze.py results.jsonl \
  --model provider/model@revision \
  --baseline apply_patch \
  --candidate hashline
```

The default gate requires 20 paired local runs, no candidate correctness/test/
concurrency failures, no correctness regression, and either at least a 5%
median token reduction or no token increase plus fewer retries. It also reports
median and p95 executor latency. A passing report is evidence for adding a
model to `BENCHMARKED_HASHLINE_PROFILES`; it is not an automatic source edit.

Run the analyzer tests with:

```sh
python3 -m unittest discover -s benchmarks/hashline -p 'test_*.py'
```

## Using someone else's benchmark

Third-party results are useful for choosing which model to test first. Import
raw paired rows with `origin: "external"` only when the exact model revision,
task corpus, prompts/tool schemas, repetitions, acceptance criteria, and token
accounting are available. The analyzer keeps those results separate.

Do not enforce a trouve profile from an aggregate claim alone. Tool names,
schemas, system prompts, snapshot semantics, repository tasks, and mutation
serialization all affect model behavior. In particular, Oh My Pi's published
model-specific token reduction is strong evidence that the idea is worth
testing, but it is not an apples-to-apples trouve result.
