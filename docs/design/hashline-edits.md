# Hashline edits

Hashline is a model-facing edit strategy for compact line-oriented changes.
The executor selects and enforces a model profile; it does not rely on prompt
instructions to make a required strategy reliable.
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
CUT 70* @handler
PUT <100 @handler
MV "src/new location.rs"
```

Supported operations are:

- `PUT N.=M:` or `PUT N:`: replace an inclusive original line range.
- `PUT <N:`: insert before original line N; `<1` inserts at the beginning.
- `PUT >N:`: insert after original line N.
- `PUT >$:`: append at the end of the file.
- `PUT N*:`: replace the complete multi-line syntactic construct beginning on N.
- `PUT >N*:`: insert after the complete construct beginning on N.
- `CUT N.=M`, `CUT N`, or `CUT N*`: delete and capture a range or block.
- `CUT ... @name`: capture into a named register; an unlabeled CUT uses the
  call-local anonymous register.
- Bodyless `PUT <N`, `PUT >N`, or `PUT >$` pastes the anonymous register;
  append `@name` to paste a named register. Range/block pastes require a name.
- `REM`: remove the whole section file.
- `MV DEST`: move the section file after applying edits above it. JSON-style
  double quotes and simple single quotes support destinations with spaces.

Every PUT body line begins with `+`; an empty replacement is deliberately
spelled as CUT. Line coordinates always refer to the snapshot named by the
section header, not to lines shifted by an earlier operation in the same call.
A single call may contain multiple file sections.

Named registers are scoped to one thread and persist across edit calls for the
life of the process. Anonymous registers exist only during one call. A register
is at most 1 MiB; each thread retains at most 16 named registers and 4 MiB;
the process retains at most 64 thread scopes and 32 MiB, evicting least-recently
used scopes as necessary.

Syntactic block targets use trouve-search's bundled tree-sitter grammars.
Markdown headings resolve through the next heading of equal or higher level.
Fenced-code content is excluded from Markdown heading detection. All block
targets in one file share one parse, and block targeting is limited to 1 MiB
files so the non-interruptible portion of a tree-sitter parse stays bounded;
larger files remain editable with explicit ranges.
If a language or opening line cannot be resolved unambiguously, the tool
rejects the operation and requests an explicit `N.=M` range.

## Snapshot and transaction semantics

The tag is the first 48 bits of SHA-256 over the complete logical file text,
represented as 12 uppercase hexadecimal characters. UTF-8 BOMs and CRLF/LF
transport differences are normalized; all other content, including trailing
horizontal whitespace, remains significant.

`hashline_edit` is classified as mutating. Consequently the engine acquires
the session mutation lane before the tool reads or validates any file. The tool
then:

1. resolves every path inside the session worktree;
2. reads every complete file and validates every tag, range, block, register,
   file action, overlap, and gap;
3. computes every result without changing the worktree;
4. stages every result beside its destination;
5. revalidates the exact preimages to catch external-editor races;
6. promotes the staged files only after all preflight work succeeds.

The call is limited to 64 sections and 128 MiB of aggregate source and output.
Each resulting file remains limited to 32 MiB, including register expansion.
`REM` removes the requested final path rather than a symlink target. `MV`
uses no-clobber destination promotion; final-component symlinks are rejected
instead of being followed, and case-only renames are handled as direct renames.

A stale tag changes no files. Its error has `code: "stale_snapshot"` and
returns the current tag plus a bounded numbered excerpt around the first
requested operation, so the model can recover without another broad read.
Promotion is atomic per update or move destination. If a later update, remove,
or move cannot be committed, already committed files are restored from their
retained preimages and moved destinations are removed.

The tool preserves the source file's UTF-8 BOM, CRLF/LF convention, final
newline shape, and permissions.

### Compatibility boundary

The operation language covers the current Oh My Pi forms used for edits,
syntactic blocks, cut/paste registers, removal, and moves. Trouve deliberately
keeps two stricter differences:

- tags use 12 hexadecimal characters rather than four, reducing accidental
  collisions while remaining visually compact;
- a stale snapshot is rejected with refreshed bounded context instead of
  attempting an automatic merge whose result could change model intent.

Those differences do not change how a model authors operations. Automatic
stale recovery can be considered separately once it has adversarial merge and
concurrent-editor coverage.

## Strategy selection and enforcement

`ToolCtx` carries one of five model edit profiles. `ToolExecutor::specs`
applies it to both raw-provider tool schemas and the vendor MCP bridge:

- `auto` advertises all normal edit strategies without preferring one.
- `prefer_apply_patch` keeps alternatives available but marks `apply_patch`
  as preferred. `codex/*` uses this profile because V4A is in distribution.
- `prefer_hashline` keeps alternatives available but marks hashline as
  preferred for existing files.
- `enforce_apply_patch` and `enforce_hashline` are benchmark-only profiles.
  Each advertises read-only inspection tools plus its selected editor, and
  denies every other mutation path before dispatch. Shell, MCP, direct
  write/delete, network, and fallback tools are unavailable, so one benchmark
  arm cannot silently mutate through an unmeasured editor.

Execution applies the same policy, so a model cannot bypass it by emitting a
hidden tool name. No production model is assigned a hashline-enforced profile
until it passes the benchmark gate below.

A model must never invent a tag or reuse one after the target file changes.

## Benchmark gate

Hashline enforcement must remain opt-in until a representative per-model
benchmark shows a benefit. Compare it with that model's current preferred
strategy and record:

- output tokens used to express edits;
- edit retries and stale-snapshot retries;
- final patch correctness and test pass rate;
- monotonic tool executor latency, measured separately from durable event-log timestamps;
- concurrent-edit outcomes under the session mutation lane.

Edit-strategy execution logs record the selected profile, tool, executor
latency, outcome, and current hashline-failure count. Correlate those records
with the turn's durable token-usage events to compare output tokens, then add a
passing model to the benchmark-owned profile table.

Use the same tasks, prompts, repository state, temperature, and model version.
Report medians and tail latency, not only a single best run. A model-specific
default may change only when hashline reduces tokens or retries without
regressing correctness or concurrency safety.

The reproducible input format and analyzer live in `benchmarks/hashline/`.
External benchmark results may be recorded as `origin: "external"`; the
analyzer reports them separately as candidate-selection evidence and never
uses them to enable an enforced production profile.
