# Branch remediation inventory — 2026-08-11

This is the durable, cumulative inventory for the branch review and its remediation audits. All 124 original findings and all 46 post-remediation audit findings are resolved in the current working tree: 170 resolved findings in total. References are workspace-relative and point to the current implementation or a focused regression test.

## Original review findings

| ID | Severity | Status | Finding | Current reference(s) |
|---:|:---:|:---:|---|---|
| #1 | P1 | resolved | Codex read-only native-tool policy permits unaudited network mutations. | `crates/trouve-agents/src/codex.rs:84`; `crates/trouve-core/src/modes.rs:41` |
| #2 | P1 | resolved | `spawn_output` can block cancellation for three minutes. | `crates/trouve-core/src/engine.rs:11136`; regression: `crates/trouve-core/src/engine.rs:13826` |
| #3 | P1 | resolved | Windows preference persistence fails after the first replacement write. | `crates/trouve-desktop-host/src/gateway.rs:1208`; regression: `crates/trouve-desktop-host/src/gateway.rs:3415` |
| #4 | P1 | resolved | Slint rollback was removed before the documented retirement gates. | `docs/adr/0023-lit-web-frontend-and-webview-host.md:3`; `docs/design/web-frontend-parity-ledger.md:3` |
| #5 | P1 | resolved | Linux releases omit Wry runtime prerequisites. | `.github/workflows/release.yml:229`; `.github/workflows/release.yml:254` |
| #6 | P1 | resolved | Required notices and SBOMs are not packaged in releases. | `.github/workflows/release.yml:95`; `.github/workflows/release.yml:100` |
| #7 | P1 | resolved | Collaborator and approval-only bridges did not retain the mutation lane. | `crates/trouve-core/src/engine.rs:10426`; `crates/trouve-core/src/engine.rs:11001` |
| #8 | P1 | resolved | MCP bridge handles remain usable after their active turn. | `crates/trouve-core/src/engine.rs:1719`; `crates/trouve-core/src/engine.rs:8786`; regression: `crates/trouve-server/tests/e2e_api.rs:2558` |
| #9 | P1 | resolved | Cursor overload cancellation is detached and unacknowledged. | `crates/trouve-agents/src/cursor.rs:905`; regression: `crates/trouve-agents/src/cursor.rs:2612` |
| #10 | P2 | resolved | Cancellation could permanently lose a queued prompt. | `crates/trouve-core/src/engine.rs:7218`; `crates/trouve-core/src/engine.rs:7378` |
| #11 | P2 | resolved | Removing queued attachments leaks database rows and files. | `crates/trouve-core/src/engine.rs:6931`; `crates/trouve-core/src/store.rs:4489`; regression: `crates/trouve-core/src/store.rs:10215` |
| #12 | P2 | resolved | Failed session creation can orphan a worktree, branch, and checkpoint. | `crates/trouve-core/src/engine.rs:5496`; `crates/trouve-core/src/git.rs:636` |
| #13 | P2 | resolved | MCP zero-tool and zero-approval settings filter discovery, not execution. | `crates/trouve-server/src/mcp.rs:48`; `crates/trouve-server/src/mcp.rs:116` |
| #14 | P2 | resolved | Live cancellation disagrees with canonical rebuilt snapshots. | `web/app-ui/src/state/thread-view-model.ts:827`; `crates/trouve-thread-view/src/lib.rs:432` |
| #15 | P2 | resolved | Live tool duration includes approval wait. | `crates/trouve-thread-view/src/lib.rs:334`; `web/app-ui/src/state/thread-view-model.ts:750` |
| #16 | P2 | resolved | Worker limits are bypassed on rejected inputs. | `web/app-ui/src/workers/content-worker-protocol.ts:42`; `web/app-ui/src/workers/content-worker.ts:20` |
| #17 | P2 | resolved | Notification bodies allow model-controlled freedesktop markup. | `crates/trouve-app/src/native_notification.rs:61`; regression: `crates/trouve-app/src/native_notification.rs:192` |
| #18 | P2 | resolved | A broken frontend can make the desktop impossible to close normally. | `crates/trouve-app/src/web_preview.rs:65`; `crates/trouve-app/src/web_preview.rs:458` |
| #19 | P2 | resolved | Window move/resize synchronously waits for preference serialization and `fsync`. | `crates/trouve-app/src/web_preview.rs:122`; `crates/trouve-app/src/web_preview.rs:359` |
| #20 | P2 | resolved | Full preference PUTs can overwrite newer native window geometry. | `crates/trouve-desktop-host/src/gateway.rs:666`; regression: `crates/trouve-desktop-host/src/gateway.rs:3434` |
| #21 | P2 | resolved | Rust SBOM generation can emit invalid SPDX and omit the product version. | `scripts/generate_rust_third_party_notices.py:198`; `scripts/generate_rust_third_party_notices.py:220`; regression: `scripts/test_generate_rust_third_party_notices.py:60` |
| #22 | P2 | resolved | The isolated Servo graph lacks separate license and security coverage. | `.github/workflows/web-frontend-qualification.yml:78`; `.github/workflows/web-frontend-qualification.yml:88` |
| #23 | P2 | resolved | Servo qualification misses internal dependency triggers and nested fmt/clippy. | `.github/workflows/web-frontend-qualification.yml:15`; `.github/workflows/web-frontend-qualification.yml:92` |
| #24 | P2 | resolved | Rust client inserts opaque tool-call IDs into URLs without encoding. | `crates/trouve-client-core/src/client.rs:280`; `crates/trouve-client-core/src/client.rs:286` |
| #25 | P2 | resolved | Queue prioritization commits before durable publication. | `crates/trouve-core/src/engine.rs:7124`; regression: `crates/trouve-core/src/engine.rs:14112` |
| #26 | P3 | resolved | PR tabs lack the expected keyboard tab pattern. | `web/app-ui/src/components/session-pr-panel.ts:644`; `web/app-ui/src/components/session-pr-panel.ts:653` |
| #27 | P3 | resolved | Packaged and runtime MIME maps have diverged. | `crates/trouve-app/build.rs:132`; `crates/trouve-desktop-host/src/lib.rs:1639` |
| #28 | P3 | resolved | Servo preview advertises persistent preferences in a temporary directory. | `crates/trouve-servo-embed-preview/src/web_preview_support.rs:76`; `crates/trouve-servo-embed-preview/src/web_preview_support.rs:85` |
| #29 | P3 | resolved | ADR index disagrees with ADR 0022’s status. | `docs/adr/0022-bounded-thread-view-pages.md:3`; `docs/adr/README.md:34` |
| #30 | P1 | resolved | Protocol evolution breaks advertised same-major compatibility. | `crates/trouve-client-core/src/protocol_compatibility.rs:11`; `crates/trouve-protocol/src/lib.rs:258`; `docs/adr/0036-exact-protocol-version-compatibility.md:20` |
| #31 | P1 | resolved | Provider metadata can widen a review parent into a writable collaborator. | `crates/trouve-core/src/engine.rs:6014`; regression: `crates/trouve-core/src/engine.rs:14343` |
| #32 | P1 | resolved | Background shell jobs escape the mutation lane. | `crates/trouve-core/src/tools/mod.rs:45`; `crates/trouve-core/src/tools/shell.rs:332`; `crates/trouve-core/src/engine.rs:11023` |
| #33 | P1 | resolved | Vendor cancellation could persist both completion and cancellation. | `crates/trouve-core/src/engine.rs:7369`; `crates/trouve-core/src/engine.rs:7409`; regression: `crates/trouve-server/tests/e2e_api.rs:2977` |
| #34 | P1 | resolved | Collaborator enrichment can stop draining the bounded stream for about 30 seconds. | `crates/trouve-agents/src/codex.rs:938`; `crates/trouve-agents/src/codex.rs:1097` |
| #35 | P1 | resolved | Concurrent MCP settings requests can discard changes. | `crates/trouve-core/src/mcp.rs:71`; `crates/trouve-core/src/mcp.rs:332` |
| #36 | P2 | resolved | Restart recovery marks every sibling thread failed. | `crates/trouve-core/src/store.rs:9145` |
| #37 | P2 | resolved | Stale status HTTP snapshots can delete newer SSE-only state. | `web/app-ui/src/state/app-store.ts:519`; `web/app-ui/src/state/thread-view-model.ts:304` |
| #38 | P2 | resolved | Auxiliary status failure prevents a conversation from opening. | `web/app-ui/src/services/thread-ingress.ts:180`; `web/app-ui/src/services/thread-ingress.ts:182` |
| #39 | P2 | resolved | Delayed message responses can resurrect dispatched queue rows. | `web/app-ui/src/state/thread-view-model.ts:476`; `web/app-ui/src/components/thread-screen.ts:6406` |
| #40 | P2 | resolved | Background-window completions are marked read unseen. | `web/app-ui/src/app/trouve-app.ts:621` |
| #41 | P2 | resolved | One TODO lifecycle row suppresses all TODO-like tool cards. | `web/app-ui/src/components/chat-layout.ts:82`; `web/app-ui/src/components/tool-presentation.ts:1014` |
| #42 | P2 | resolved | New-thread creation waits up to 48 seconds for title metadata. | `web/app-ui/src/components/thread-screen.ts:205`; `web/app-ui/src/components/thread-screen.ts:5467` |
| #43 | P2 | resolved | Turn pagination can expand across arbitrarily many cancelled turns. | `crates/trouve-thread-view/src/lib.rs:432`; regression: `crates/trouve-core/src/store.rs:8232` |
| #44 | P2 | resolved | Disabling MCP does not terminate its cached process. | `crates/trouve-core/src/mcp.rs:2228`; regression: `crates/trouve-core/src/mcp.rs:4171` |
| #45 | P2 | resolved | Enabling a pure MCP tombstone leaves an empty higher-priority definition. | `crates/trouve-core/src/mcp.rs:299`; regression: `crates/trouve-core/src/mcp.rs:3241` |
| #46 | P2 | resolved | MCP server enablement URLs do not encode server names. | `crates/trouve-client-core/src/client.rs:952`; `crates/trouve-client-core/src/client.rs:957` |
| #47 | P2 | resolved | Temporarily unavailable collaborator prompts freeze as empty. | `crates/trouve-agents/src/codex.rs:938`; `crates/trouve-agents/src/codex.rs:1090` |
| #48 | P2 | resolved | Timed-out login-shell discovery can orphan descendants. | `crates/trouve-agents/src/process_env.rs:555`; regression: `crates/trouve-agents/src/process_env.rs:734` |
| #49 | P3 | resolved | Stale MCP connections can overwrite replacement health. | `crates/trouve-core/src/mcp.rs:157`; regressions: `crates/trouve-core/src/mcp.rs:2787`, `crates/trouve-core/src/mcp.rs:2801` |
| #50 | P3 | resolved | Cancel becomes a no-op after every thread tab closes. | `web/app-ui/src/components/thread-screen.ts:1224`; `web/app-ui/src/components/thread-screen.ts:5414` |
| #51 | P3 | resolved | Closing a non-selected tab loses keyboard focus. | `web/app-ui/src/components/thread-screen.ts:5409` |
| #52 | P3 | resolved | Focusable close buttons violate the tablist composite model. | `web/app-ui/src/components/thread-screen.ts:1140`; `web/app-ui/src/components/thread-screen.ts:1162` |
| #53 | P3 | resolved | Hashline and patch-fallback cards misrepresent multi-file edits. | `web/app-ui/src/components/tool-presentation.ts:467`; `web/app-ui/src/components/tool-presentation.ts:1040` |
| #54 | P1 | resolved | Payload-only MCP correlation can execute a call under the wrong thread policy. | `crates/trouve-core/src/engine.rs:8313`; regression: `crates/trouve-core/src/engine.rs:13193`; `crates/trouve-server/src/mcp.rs:300` |
| #55 | P1 | resolved | Correlated collaborator tools lose root-turn cancellation. | `crates/trouve-core/src/engine.rs:8331`; `crates/trouve-core/src/engine.rs:8438` |
| #56 | P1 | resolved | Session-only collaborator lifecycle permits hangs and premature closure. | `crates/trouve-agents/src/codex.rs:537`; `crates/trouve-agents/src/codex.rs:583` |
| #57 | P1 | resolved | Cancellation after provisional root completion bypasses descendant cleanup. | `crates/trouve-agents/src/codex.rs:1418`; `crates/trouve-agents/src/codex.rs:1468` |
| #58 | P1 | resolved | Publishing a collaborator before claiming it can steal and erase a real dispatcher claim. | `crates/trouve-core/src/engine.rs:501`; regression: `crates/trouve-core/src/engine.rs:13369` |
| #59 | P1 | resolved | Register paste can expand a compact hashline edit into unbounded memory and disk use. | `crates/trouve-core/src/tools/hashline.rs:44`; `crates/trouve-core/src/tools/hashline.rs:1106`; regression: `crates/trouve-core/src/tools/hashline.rs:2108` |
| #60 | P1 | resolved | Every syntactic-block operation reparses the whole file without responsive cancellation. | `crates/trouve-core/src/tools/hashline.rs:1104`; `crates/trouve-core/src/tools/hashline.rs:1201` |
| #61 | P1 | resolved | Hashline `REM` and `MV` operate on a symlink target instead of the requested path. | `crates/trouve-core/src/tools/hashline.rs:376`; regressions: `crates/trouve-core/src/tools/hashline.rs:1855`, `crates/trouve-core/src/tools/hashline.rs:1871` |
| #62 | P2 | resolved | The one-second owner timeout rejects valid calls queued behind collaborator enrichment. | `crates/trouve-agents/src/codex.rs:608`; `crates/trouve-agents/src/codex.rs:938` |
| #63 | P2 | resolved | An abandoned owner tombstone can consume a later identical legitimate call. | `crates/trouve-core/src/engine.rs:231`; regression: `crates/trouve-core/src/engine.rs:13242` |
| #64 | P2 | resolved | Checkpoint errors are type-erased before retry classification. | `crates/trouve-core/src/engine.rs:11718`; regression: `crates/trouve-core/src/engine.rs:13150` |
| #65 | P2 | resolved | Provider-native descendants bypass depth and active-descendant bounds. | `crates/trouve-core/src/engine.rs:135`; `crates/trouve-core/src/engine.rs:9004` |
| #66 | P2 | resolved | A failed descendant can be reported as a successful collected subtree. | `crates/trouve-core/src/store.rs:4218`; `crates/trouve-core/src/engine.rs:11380` |
| #67 | P2 | resolved | Historical descendant collection performs unbounded N+1 scans. | `crates/trouve-core/src/store.rs:4162`; `crates/trouve-core/src/store.rs:4168` |
| #68 | P2 | resolved | Thinking blocks merge across intervening tools and steering, reordering the transcript. | `crates/trouve-thread-view/src/lib.rs:173`; `crates/trouve-thread-view/src/lib.rs:263` |
| #69 | P2 | resolved | Cross-session descendants lack statuses after cold load and appear idle. | `web/app-ui/src/services/thread-ingress.ts:180`; `web/app-ui/src/components/session-info-panel.ts:824` |
| #70 | P2 | resolved | Named-register storage is not effectively bounded. | `crates/trouve-core/src/tools/hashline.rs:48`; `crates/trouve-core/src/tools/hashline.rs:1358` |
| #71 | P2 | resolved | Enforced benchmark profiles do not isolate the selected editor. | `crates/trouve-core/src/tools/edit_strategy.rs:83`; `crates/trouve-core/src/tools/mod.rs:428` |
| #72 | P2 | resolved | Invalid benchmark strategy silently runs the normal catalog. | `crates/trouve-core/src/tools/edit_strategy.rs:48` |
| #73 | P2 | resolved | Unmatched benchmark rows are silently discarded. | `benchmarks/hashline/analyze.py:122`; `benchmarks/hashline/analyze.py:147` |
| #74 | P2 | resolved | The analyzer does not enforce its required corpus shape. | `benchmarks/hashline/analyze.py:174`; regression: `benchmarks/hashline/test_analyze.py:64` |
| #75 | P2 | resolved | Relaxed benchmark thresholds are omitted from archived reports. | `benchmarks/hashline/analyze.py:218`; `benchmarks/hashline/analyze.py:249` |
| #76 | P2 | resolved | Markdown block resolution mistakes fenced-code content for headings. | `crates/trouve-core/src/tools/hashline.rs:1268`; regression: `crates/trouve-core/src/tools/hashline.rs:2077` |
| #77 | P2 | resolved | Hashline `MV` can overwrite a destination created after preflight. | `crates/trouve-core/src/tools/hashline.rs:775`; regression: `crates/trouve-core/src/tools/hashline.rs:1989` |
| #78 | P2 | resolved | Missing benchmark origin metadata is trusted as local evidence. | `benchmarks/hashline/analyze.py:71`; regression: `benchmarks/hashline/test_analyze.py:50` |
| #79 | P2 | resolved | Restoring an older checkpoint leaves no route back to the newest checkpoint. | `web/app-ui/src/components/turn-checkpoint-actions.ts:70`; `web/app-ui/src/components/thread-screen.ts:2262` |
| #80 | P3 | resolved | External MCP tools with built-in basenames are mislabeled as native tools. | `web/app-ui/src/components/tool-presentation.ts:217`; `web/app-ui/src/components/tool-presentation.ts:246` |
| #81 | P3 | resolved | The analyzer advertises an `edit_file` arm the strict harness cannot select. | `benchmarks/hashline/analyze.py:15`; `crates/trouve-core/src/tools/edit_strategy.rs:68` |
| #82 | P3 | resolved | Stale-retry data is computed but omitted from reports. | `benchmarks/hashline/analyze.py:165`; `benchmarks/hashline/analyze.py:239` |
| #83 | P3 | resolved | Case-only hashline `MV` renames fail on case-insensitive filesystems. | `crates/trouve-core/src/tools/hashline.rs:282`; regression: `crates/trouve-core/src/tools/hashline.rs:1965` |
| #84 | P3 | resolved | Runtime hashline schema does not explain the new grammar. | `crates/trouve-core/src/tools/hashline.rs:110` |
| #85 | P3 | resolved | Maintained UX documentation still claims diff-pane Undo/Redo. | `docs/design/web-frontend-parity-ledger.md:287`; `docs/design/ux-screen-map.md:174` |
| #86 | P2 | resolved | Normal spawned threads can remain cached without `parent_thread_id`. | `crates/trouve-core/src/store.rs:7940`; regression: `crates/trouve-core/src/store.rs:10277` |
| #87 | P2 | resolved | Expanded hashline reads cannot decode real server output. | `web/app-ui/src/components/tool-presentation.ts:736`; `web/app-ui/src/components/tool-presentation.ts:747` |
| #88 | P2 | resolved | Expanded turn transcripts silently discard messages after the first 100. | `web/app-ui/src/components/tool-presentation.ts:803`; `web/app-ui/src/components/tool-presentation.ts:831` |
| #89 | P2 | resolved | Unconditional `spawn_output` suppression hides failed and unrelated MCP calls. | `web/app-ui/src/components/chat-layout.ts:89` |
| #90 | P2 | resolved | Thread-switcher traversal is quadratic and recursively stack-unsafe. | `web/app-ui/src/components/thread-switcher-model.ts:32`; `web/app-ui/src/components/thread-switcher-model.ts:82` |
| #91 | P2 | resolved | A selected provisional New Thread tab can be clipped at one-tab capacity. | `web/app-ui/src/components/thread-switcher-model.ts:110`; `web/app-ui/src/components/thread-screen.ts:1004` |
| #92 | P2 | resolved | Switcher Home/End handling steals search-input caret navigation. | `web/app-ui/src/components/thread-screen.ts:5293` |
| #93 | P3 | resolved | A transient tool-detail import failure permanently leaves cards blank. | `web/app-ui/src/components/thread-screen.ts:5017`; `web/app-ui/src/components/thread-screen.ts:5026` |
| #94 | P3 | resolved | Parity documentation advertises host bridge v8 while implementation is v12. | `crates/trouve-desktop-host/src/lib.rs:33`; `docs/design/web-frontend-parity-ledger.md:303`; `docs/design/web-frontend-parity-ledger.md:1206` |
| #95 | P1 | resolved | Wry close deadline not cleared after cancel/quit_when_idle | `crates/trouve-app/src/web_preview.rs:65`; regressions: `crates/trouve-app/src/web_preview.rs:655`, `crates/trouve-app/src/web_preview.rs:667` |
| #96 | P1 | resolved | hashline case-only MV symlink clobber | `crates/trouve-core/src/tools/hashline.rs:282`; regression: `crates/trouve-core/src/tools/hashline.rs:1928` |
| #97 | P2 | resolved | Markdown fence scanner wrong syntax | `crates/trouve-core/src/tools/hashline.rs:1316`; regressions: `crates/trouve-core/src/tools/hashline.rs:2084`, `crates/trouve-core/src/tools/hashline.rs:2095` |
| #98 | P2 | resolved | checkpoint busy sticks across session navigation | `web/app-ui/src/components/turn-checkpoint-actions.ts:16`; `web/app-ui/src/components/thread-screen.ts:2347` |
| #99 | P2 | resolved | external MCP generic wrappers lose server identity | `web/app-ui/src/components/tool-presentation.ts:217`; `web/app-ui/src/components/tool-presentation.ts:230` |
| #100 | P2 | resolved | external todo_write can become authoritative/hide UI | `web/app-ui/src/components/chat-layout.ts:82`; `web/app-ui/src/components/tool-presentation.ts:1014` |
| #101 | P1 | resolved | bridge capability flags client-controlled/unbound | `crates/trouve-core/src/engine.rs:1113`; `crates/trouve-core/src/engine.rs:1719`; `crates/trouve-server/src/mcp.rs:61` |
| #102 | P1 | resolved | Cursor closed oneshot mistaken as cancellation ack | `crates/trouve-agents/src/cursor.rs:963`; regression: `crates/trouve-agents/src/cursor.rs:2077` |
| #103 | P1 | resolved | Cursor/MCP kill direct child not tree | `crates/trouve-agents/src/process_env.rs:91`; `crates/trouve-agents/src/cursor.rs:1493`; `crates/trouve-core/src/mcp.rs:1088` |
| #104 | P1 | resolved | MCP write outside timeout | `crates/trouve-core/src/mcp.rs:1130`; `crates/trouve-core/src/mcp.rs:1152` |
| #105 | P2 | resolved | equivalent concurrent connection attempts all fail | `crates/trouve-core/src/mcp.rs:1926`; regression: `crates/trouve-core/src/mcp.rs:4343` |
| #106 | P1 | resolved | atomic config replace drops security metadata | `crates/trouve-core/src/mcp.rs:515`; regression: `crates/trouve-core/src/mcp.rs:3329` |
| #107 | P2 | resolved | directory fsync postcommit misleading failure | `crates/trouve-core/src/mcp.rs:480`; `crates/trouve-core/src/mcp.rs:492`; regression: `crates/trouve-core/src/mcp.rs:3311` |
| #108 | P2 | resolved | tools=0 launches external MCP discovery | `crates/trouve-core/src/tools/mod.rs:193`; regression: `crates/trouve-core/src/tools/mod.rs:823` |
| #109 | P1 | resolved | async Codex prompt recovery dropped at root completion | `crates/trouve-agents/src/codex.rs:938`; `crates/trouve-agents/src/codex.rs:1090` |
| #110 | P1 | resolved | daemon/background child escapes mutation lease | `crates/trouve-core/src/tools/shell.rs:332`; regression: `crates/trouve-core/src/tools/shell.rs:883` |
| #111 | P1 | resolved | native_specs default fail-open | `crates/trouve-core/src/tools/mod.rs:193`; regression: `crates/trouve-core/src/tools/mod.rs:823` |
| #112 | P1 | resolved | failed worktree setup rollback destructive TOCTOU | `crates/trouve-core/src/git.rs:636`; regression: `crates/trouve-core/src/git.rs:1028` |
| #113 | P1 | resolved | MCP queued cancel kills active call | `crates/trouve-core/src/mcp.rs:1140`; regression: `crates/trouve-core/src/mcp.rs:4829` |
| #114 | P1 | resolved | Codex descendant cleanup | `crates/trouve-agents/src/codex.rs:537`; `crates/trouve-agents/src/codex.rs:1269`; `crates/trouve-agents/src/codex.rs:1468` |
| #115 | P1 | resolved | Windows job assignment race | `crates/trouve-agents/src/process_env.rs:230`; regression: `crates/trouve-agents/src/process_env.rs:699` |
| #116 | P1 | resolved | Codex steer nondurable after cancel | `crates/trouve-agents/src/codex.rs:253`; regression: `crates/trouve-agents/src/codex.rs:5543` |
| #117 | P1 | resolved | early/stale-authority close autoquit | `web/app-ui/src/services/desktop-host-coordinator.ts:206`; `web/app-ui/src/services/desktop-host-coordinator.ts:349` |
| #118 | P1 | resolved | Cursor setup/config unfenced | `crates/trouve-agents/src/cursor.rs:742`; regression: `crates/trouve-agents/src/cursor.rs:2432` |
| #119 | P2 | resolved | qualified MCP shell command omitted | `web/app-ui/src/components/tool-presentation.ts:301` |
| #120 | P2 | resolved | identical/ghost queued prompt response race | `web/app-ui/src/state/thread-view-model.ts:476`; `web/app-ui/src/components/thread-screen.ts:6406` |
| #121 | P2 | resolved | inspection checkpoint busy state | `web/app-ui/src/components/inspection-workspace.ts:63`; `web/app-ui/src/components/inspection-workspace.ts:393` |
| #122 | P3 | resolved | hashline analyzer CI absent | `.github/workflows/lint.yml:48` |
| #123 | P3 | resolved | Servo notice commands/link | `.github/workflows/web-frontend-qualification.yml:78`; `crates/trouve-servo-embed-preview/THIRD_PARTY_NOTICES.md:5` |
| #124 | P3 | resolved | stale parity ledger paths/claims | `docs/design/web-frontend-parity-ledger.md:138`; `docs/design/web-frontend-parity-ledger.md:303`; `docs/design/web-frontend-parity-ledger.md:1147` |

## Post-remediation audit findings

These identifiers are intentionally stable and separate from the original review numbering.

| ID | Status | Finding | Current reference(s) |
|:---:|:---:|---|---|
| R1 | resolved | MCP framed JSON-RPC application errors dirty/evict | `crates/trouve-core/src/mcp.rs:1174`; regression: `crates/trouve-core/src/mcp.rs:5000` |
| R2 | resolved | Codex effect response timeout reused transport | `crates/trouve-agents/src/codex.rs:3539`; regression: `crates/trouve-agents/src/codex.rs:6056` |
| R3 | resolved | Cursor apply_model_config cancellation misclassified Protocol | `crates/trouve-agents/src/cursor.rs:742`; regression: `crates/trouve-agents/src/cursor.rs:2509` |
| R4 | resolved | BSD/Hurd ACL preservation false claim | fail-closed branch: `crates/trouve-core/src/mcp.rs:560`; ACL probe: `crates/trouve-core/src/mcp.rs:633`; regression: `crates/trouve-core/src/mcp.rs:3353` |
| R5 | resolved | Codex handshake initialize error unreaped | `crates/trouve-agents/src/codex.rs:186`; regression: `crates/trouve-agents/src/codex.rs:5658` |
| R6 | resolved | queue 256 tombstone resurrection | `web/app-ui/src/state/thread-view-model.ts:22`; `web/app-ui/src/state/thread-view-model.ts:476`; regression: `web/app-ui/src/components/queue-controls.test.ts:125` |
| R7 | resolved | detached geometry write can overwrite final save | `crates/trouve-app/src/web_preview.rs:122`; regression: `crates/trouve-app/src/web_preview.rs:757` |
| R8 | resolved | >4MiB Markdown stale prior content/unhandled rejection | `web/app-ui/src/components/markdown-view.ts:20`; `web/app-ui/src/services/content-worker-client.ts:16` |
| R9 | resolved | healthy close prompt force-exits after 5 sec | `crates/trouve-app/src/web_preview.rs:105`; regression: `crates/trouve-app/src/web_preview.rs:687` |
| R10 | resolved | fresh auto close failure stuck | `web/app-ui/src/services/desktop-host-coordinator.ts:349`; regression: `web/app-ui/src/services/desktop-host-coordinator.test.ts:277` |
| R11 | resolved | Codex steer missing/wrong ID reusable | `crates/trouve-agents/src/codex.rs:872`; regression: `crates/trouve-agents/src/codex.rs:7477` |
| R12 | resolved | Codex thread/start/resume missing ID reusable | `crates/trouve-agents/src/codex.rs:850`; regression: `crates/trouve-agents/src/codex.rs:7562` |
| R13 | resolved | fresh session cancellation before SessionStarted untracked | regression: `crates/trouve-agents/src/codex.rs:7618` |
| R14 | resolved | Codex resume fallback dead server | regression: `crates/trouve-agents/src/codex.rs:7738` |
| R15 | resolved | Codex startup future drop/unreaped | regression: `crates/trouve-agents/src/codex.rs:7414` |
| R16 | resolved | Cursor startup future drop/unreaped | regression: `crates/trouve-agents/src/cursor.rs:2770` |
| R17 | resolved | Codex unexpected stdout EOF replacement before reap | `crates/trouve-agents/src/codex.rs:3247`; regression: `crates/trouve-agents/src/codex.rs:5745` |
| R18 | resolved | MCP leader exits/descendant survives termination acknowledgement | termination contract: `crates/trouve-core/src/mcp.rs:1066`; regression: `crates/trouve-core/src/mcp.rs:3627` |
| R19 | resolved | cleanup failure fail-open replacement across Codex/Cursor/MCP | regressions: `crates/trouve-agents/src/codex.rs:7824`, `crates/trouve-agents/src/cursor.rs:2940`, `crates/trouve-core/src/mcp.rs:3920` |
| R20 | resolved | MCP last-waiter/eviction cleanup gap | `crates/trouve-core/src/mcp.rs:1490`; regressions: `crates/trouve-core/src/mcp.rs:4534`, `crates/trouve-core/src/mcp.rs:4630` |
| R21 | resolved | Claude cancellation only direct-child kill/removes pool entry | `crates/trouve-agents/src/claude.rs:109`; regression: `crates/trouve-agents/src/claude.rs:1349` |
| R22 | resolved | Claude usage query cleanup failure permits later probe spawn | `crates/trouve-agents/src/claude.rs:626`; regression: `crates/trouve-agents/src/claude.rs:1557` |
| R23 | resolved | stale MCP config snapshots can respawn an invalidated/updated/deleted server | `crates/trouve-core/src/mcp.rs:1850`; invalidation: `crates/trouve-core/src/mcp.rs:2107`, `crates/trouve-core/src/mcp.rs:2166`; regressions: `crates/trouve-core/src/mcp.rs:2578`, `crates/trouve-core/src/mcp.rs:2623` |
| R24 | resolved | ProcessTreeChild::try_wait mistakes kill signaling for full-tree cleanup acknowledgement | `crates/trouve-agents/src/process_env.rs:155`; regression: `crates/trouve-agents/src/process_env.rs:916` |
| R25 | resolved | server bridge E2E fixtures omitted the new exact active-turn/call capability metadata and expected obsolete success | regressions: `crates/trouve-server/tests/e2e_api.rs:2558`, `crates/trouve-server/tests/e2e_api.rs:5464`; validation: `crates/trouve-server/src/mcp.rs:83` |
| R26 | resolved | desktop total-JS budget was already 111,597 B below clean baseline and branch added only 9,410 B, so the obsolete gate failed all builds | JS baseline/artifacts and 3,125,000 B limit: `web/app-ui/scripts/check-bundle-budget.mjs:28`, `web/app-ui/scripts/check-bundle-budget.mjs:33`; CSS baseline/artifact and 180,000 B limit: `web/app-ui/scripts/check-bundle-budget.mjs:34`, `web/app-ui/scripts/check-bundle-budget.mjs:36`; unchanged entry/worker/chunk caps: `web/app-ui/scripts/check-bundle-budget.mjs:27`, `web/app-ui/scripts/check-bundle-budget.mjs:39` |
| R27 | resolved | MCP `specs()` reads the trusted catalog without a pre-read epoch, so a stale catalog can resume after update/delete, overwrite `trusted_configs`, and spawn an old command. | ordered catalog generation: `crates/trouve-core/src/mcp.rs:1330`; reconcile guard: `crates/trouve-core/src/mcp.rs:2228`, `crates/trouve-core/src/mcp.rs:2237`; pre-read ticket: `crates/trouve-core/src/mcp.rs:2333`, `crates/trouve-core/src/mcp.rs:2344`; regressions: `crates/trouve-core/src/mcp.rs:2668`, `crates/trouve-core/src/mcp.rs:2718` |
| R28 | resolved | Browser protocol fixtures still advertised 3.14 despite exact 4.0 compatibility. | `web/app-ui/e2e/app-shell.spec.ts:90`; `web/app-ui/e2e/chat-session.spec.ts:481`; validation: `web/app-ui/src/services/protocol-client.ts:422` |
| R29 | resolved | TODO lifecycle presentation suppressed the low-level audit tool card. | `web/app-ui/src/components/chat-layout.ts:82` |
| R30 | resolved | An MCP fixture with `health: unknown` incorrectly expected an active server instead of Enabled / Unknown / zero active. | regression: `web/app-ui/e2e/chat-session.spec.ts:981`; classification: `web/app-ui/src/components/session-info-panel.ts:80` |
| R31 | resolved | Vulnerable transitive nanoid remained in the app-ui and review-ui lockfiles. | `web/review-ui/package-lock.json:1052`; `web/app-ui/package-lock.json:3059`; notice: `web/app-ui/THIRD_PARTY_NOTICES.md:216` |
| R32 | resolved | Checkpoint Playwright used `.last()`, which retargeted after a later checkpoint appeared. | regression: `web/app-ui/e2e/chat-session.spec.ts:1168`; action markup: `web/app-ui/src/components/thread-screen.ts:2318` |
| R33 | resolved | Active-turn browser fixtures omitted `turn.capacity_acquired`, invalidating running-state expectations. | representative regression: `web/app-ui/e2e/chat-session.spec.ts:5265`; fold: `web/app-ui/src/state/thread-view-model.ts:494` |
| R34 | resolved | app-ui retained a vulnerable js-yaml dependency through Redocly. | `web/app-ui/package-lock.json:543`; `web/app-ui/package-lock.json:1652`; notice: `web/app-ui/THIRD_PARTY_NOTICES.md:153` |
| R35 | resolved | A focusable checkpoint action group was nested inside `role=separator`. | `web/app-ui/src/components/thread-screen.ts:2318`; `web/app-ui/src/styles/app.css:319` |
| R36 | resolved | Browser expectations disagreed with the transparent tool-disclosure visual contract. | `web/app-ui/src/styles/app.css:491` |
| R37 | resolved | Exact-4.0 queued-message fixture omitted canonical `queued_prompt`, creating a duplicate optimistic row. | regression: `web/app-ui/e2e/chat-session.spec.ts:5308`; canonical response: `web/app-ui/e2e/chat-session.spec.ts:5330` |
| R38 | resolved | Native undo E2E treated a multi-character synthesized sequence as one browser edit unit. | regression: `web/app-ui/e2e/chat-session.spec.ts:1740`; compatibility shim: `web/app-ui/src/services/native-text-history.ts:60`; unit test: `web/app-ui/src/services/native-text-history.test.ts:5` |
| R39 | resolved | Cancellation browser fixtures expected a cancelled status row that canonical snapshots intentionally remove. | canonical fold: `crates/trouve-thread-view/src/lib.rs:432`; regression flow: `web/app-ui/e2e/chat-session.spec.ts:5308` |
| R40 | resolved | DOM-anchor capture could select the persistent start spacer instead of visible history. | `web/app-ui/src/components/thread-screen.ts:3507`; hit-test guard: `web/app-ui/src/components/thread-screen.ts:3530`; fallback guard: `web/app-ui/src/components/thread-screen.ts:3575` |
| R41 | resolved | The prepended-row scroll E2E did not atomically establish changed geometry and anchor preservation. | regression: `web/app-ui/e2e/chat-session.spec.ts:4703`; persistent growth: `web/app-ui/e2e/chat-session.spec.ts:4749`; atomic predicate: `web/app-ui/e2e/chat-session.spec.ts:4791` |
| R42 | resolved | First-measure calibration deltas could reverse native momentum by roughly 310px. | `web/app-ui/src/components/thread-screen.ts:766`; retained-delta application: `web/app-ui/src/components/thread-screen.ts:780`; regression: `web/app-ui/e2e/chat-session.spec.ts:4703` |
| R43 | resolved | Cancellation acknowledgement could remain stuck at `Stopping…` after a compacted cancellation and replacement turn. | `web/app-ui/src/components/thread-screen.ts:6284`; replacement detection: `web/app-ui/src/components/thread-screen.ts:6302`; regression flow: `web/app-ui/e2e/chat-session.spec.ts:5308` |
| R44 | resolved | Cancelling while waiting for capacity recorded no active turn and cleared the acknowledgement prematurely. | active rendering: `web/app-ui/src/components/thread-screen.ts:2067`; reconciliation: `web/app-ui/src/components/thread-screen.ts:6297`; active-turn lookup: `web/app-ui/src/components/thread-screen.ts:6325`; regression: `web/app-ui/e2e/chat-session.spec.ts:5519` |
| R45 | resolved | The rewritten secured bridge E2E no longer proved vendor writes were confined to the session worktree. | permission gate: `crates/trouve-core/src/engine.rs:10464`; escape denial: `crates/trouve-core/src/engine.rs:10475`; regression: `crates/trouve-server/tests/e2e_api.rs:2558`; allow assertion: `crates/trouve-server/tests/e2e_api.rs:2722`; deny assertion: `crates/trouve-server/tests/e2e_api.rs:2759` |
| R46 | resolved | Mixed ResizeObserver batches added first-measure estimate deltas to genuine late-layout correction. | helper: `web/app-ui/src/components/history-scroll-correction.ts:7`; integration: `web/app-ui/src/components/thread-screen.ts:780`; regression: `web/app-ui/src/components/history-scroll-correction.test.ts:5` |

## Verification notes

- Earlier in remediation, before the final UI/test-only tail edits, the full root Rust matrix was green: `cargo fmt --all -- --check`, workspace/all-target `cargo check`, workspace/all-target Clippy with `-D warnings`, `cargo test --workspace`, and locked/offline Cargo metadata. The isolated Servo workspace also passed its fmt, check, Clippy, and test matrix.
- The earlier compliance and generated-artifact pass was green for the hashline Python analyzer and tests, Rust notice/SBOM generation and freshness, version synchronization, OpenAPI snapshots, runtime schemas and validators, and Cargo metadata. The review UI passed `npm ci`, its build, all 26 tests, and an audit with zero high-severity vulnerabilities. After the nanoid/js-yaml lock refresh, all three committed Node workspaces audited with zero high-severity vulnerabilities; the final app-ui audit below reports zero vulnerabilities at every severity.
- Fresh affected Rust verification after R44–R46: the focused secured-bridge test passed 1/1; `cargo test -p trouve-server --test e2e_api` passed 37/37; `cargo check -p trouve-server --tests`, `cargo clippy -p trouve-server --tests -- -D warnings`, and `cargo fmt --all -- --check` passed.
- Fresh app-ui verification after R44–R46: source format passed for 261 files, source-policy lint passed for 241 files, type checking passed, and Vitest passed 105 files / 697 tests. The runnable Playwright matrix passed 140 tests with 61 configured Firefox skips, zero failures, and retries disabled. The high-risk scroll/queue set passed 40/40 across five desktop/mobile Chromium repetitions, and the focused cancellation-before-capacity case passed 2/2 across desktop/mobile Chromium.
- Desktop and PWA builds, validators, bundle budgets, and build-mode verification passed. Desktop sizes were entry 608,473 B, worker 335,446 B, total JavaScript 3,121,665 B, CSS 179,000 B, and fonts 119,488 B. PWA sizes were entry 597,817 B, worker 335,446 B, total JavaScript 3,111,009 B, CSS 179,000 B, and fonts 119,488 B. `npm audit` reported zero vulnerabilities; notice and validator freshness checks passed.
- Final `git diff --check` passed.
- Platform limitations: local WebKit launch is unavailable because `libicu74`, `libxml2`, and `libflite1` are missing. The 61 configured Firefox skips cover stateful chat/scrolling cases and are not semantic proof of those workflows. Windows Job Object behavior and BSD/Hurd ACL branches require their platform CI jobs.
