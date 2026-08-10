# Hashline edits

Hashline is an additive, model-facing edit strategy for compact line-oriented
changes. It does not replace `apply_patch`, `edit_file`, or `write_file`.
Every mutation still executes through `ToolExecutor` and therefore inherits
the permission gate, cancellation contract, durable audit events, and the
session mutation lane described by ADR 0030.

## Wire format

Request hashline output explicitly:

```json
{"path":"src/lib.rs","offset":40,"limit":20,"format":"hashline"}
```

The response contains a whole-file snapshot tag and numbered lines:

```text
[src/lib.rs#A1B2C3D4E5F6]
40:fn old_name() {
41:    work();
42:}
```

Pass the returned tag unchanged to `hashline_edit`:

```text
[src/lib.rs#A1B2C3D4E5F6]
PUT 40.=42:
+fn new_name() {
+    better_work();
+}
PUT <50:
+// Inserted before original line 50.
PUT >$:
+// Appended at end of file.
CUT 60.=62
```

Supported operations are:

- `PUT N.=M:` or `PUT N:`: replace an inclusive original line range.
- `PUT <N:`: insert before original line N; `<1` inserts at the beginning.
- `PUT >N:`: insert after original line N.
- `PUT >$:`: append at the end of the file.
- `CUT N.=M` or `CUT N`: delete an inclusive original line range.

Every PUT body line begins with `+`; an empty replacement is deliberately
spelled as CUT. Line coordinates always refer to the snapshot named by the
section header, not to lines shifted by an earlier operation in the same call.
A single call may contain multiple file sections.

## Snapshot and transaction semantics

The tag is the first 48 bits of SHA-256 over the complete logical file text,
represented as 12 uppercase hexadecimal characters. UTF-8 BOMs and CRLF/LF
transport differences are normalized; all other content, including trailing
horizontal whitespace, remains significant.

`hashline_edit` is classified as mutating. Consequently the engine acquires
the session mutation lane before the tool reads or validates any file. The tool
then:

1. resolves every path inside the session worktree;
2. reads every complete file and validates every tag, range, overlap, and gap;
3. computes every result without changing the worktree;
4. stages every result beside its destination;
5. revalidates the exact preimages to catch external-editor races;
6. promotes the staged files only after all preflight work succeeds.

A stale tag changes no files. Its error has `code: "stale_snapshot"` and
returns the current tag plus a bounded numbered excerpt around the first
requested operation, so the model can recover without another broad read.
Promotion is atomic per file. If a later file cannot be promoted, already
promoted files are restored from their retained preimages.

The tool preserves the source file's UTF-8 BOM, CRLF/LF convention, final
newline shape, and permissions.

## Strategy selection

No provider is switched to hashline by default.

- Codex-family models should normally keep using `apply_patch`, whose V4A
  format is part of their trained tool distribution.
- Models that reliably follow line-anchored formats may opt into
  `read_file(format="hashline")` plus `hashline_edit`, especially when an
  exact replacement would repeat a large preimage.
- `edit_file` remains appropriate for small, unique exact substitutions.
- `write_file` remains appropriate for new files or complete regeneration.

A model must never invent a tag or reuse one after the target file changes.

## Benchmark gate

Hashline must remain opt-in until a representative per-model benchmark shows a
benefit. Compare it with that model's current preferred strategy and record:

- output tokens used to express edits;
- edit retries and stale-snapshot retries;
- final patch correctness and test pass rate;
- tool executor latency (the durable tool event already records this);
- concurrent-edit outcomes under the session mutation lane.

Use the same tasks, prompts, repository state, temperature, and model version.
Report medians and tail latency, not only a single best run. A model-specific
default may change only when hashline reduces tokens or retries without
regressing correctness or concurrency safety.
