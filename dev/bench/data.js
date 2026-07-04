window.BENCHMARK_DATA = {
  "lastUpdate": 1783128625638,
  "repoUrl": "https://github.com/jimsimon/trouve",
  "entries": {
    "e2e-benchmarks": [
      {
        "commit": {
          "author": {
            "email": "jim.j.simon@gmail.com",
            "name": "Jim Simon",
            "username": "jimsimon"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "c54a63d163682009cd91a59851bd623c93a9f52a",
          "message": "Benchmark git vs non-git roots; gate CI on benchmark regressions (#1)\n\n* Benchmark git vs non-git roots on kubernetes\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Guard TOUCH_REL pipeline against SIGPIPE under pipefail\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Update git vs non-git numbers to committed-script run\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Gate CI on benchmark regressions\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Fix racy shared model dir in embed parity tests\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Persist benchmark data to gh-pages instead of the actions cache\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Address review: harden bench scripts and workflow\n\n- Resolve caller-supplied repo paths before cd; error instead of cloning over a missing user path\n- Shell-escape all values interpolated into hyperfine command strings; drop eval in favor of direct invocations\n- Restore the incremental-scenario file via EXIT trap so failures leave the tree clean\n- Recursive criterion glob (grouped/parameterized bench IDs) and a loud duplicate-name guard in the converter\n- SHA-pin all actions, set persist-credentials: false on checkouts\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n---------\n\nCo-authored-by: Cursor Agent <cursoragent@cursor.com>\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>",
          "timestamp": "2026-07-03T13:51:39-04:00",
          "tree_id": "099ec6b183f6cb426cc60fe16491d57ba3cdda2a",
          "url": "https://github.com/jimsimon/trouve/commit/c54a63d163682009cd91a59851bd623c93a9f52a"
        },
        "date": 1783101231400,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "cold index + query",
            "value": 117.3390725,
            "range": "± 8.8",
            "unit": "ms"
          },
          {
            "name": "warm query",
            "value": 58.690117480000005,
            "range": "± 1.7",
            "unit": "ms"
          },
          {
            "name": "incremental (1 file modified)",
            "value": 68.84425028000001,
            "range": "± 1.6",
            "unit": "ms"
          },
          {
            "name": "non-git warm query",
            "value": 57.074566960000006,
            "range": "± 1.0",
            "unit": "ms"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "jim.j.simon@gmail.com",
            "name": "Jim Simon",
            "username": "jimsimon"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "22d3e560019dc5635491b0b20caf7c55b621f2b1",
          "message": "Add tree-sitter grammars for 23 more languages (#5)\n\nBundle maintained crates.io grammars for CMake, D, Dart, Elm, ERB/EJS\ntemplates, Erlang, Fortran, Gleam, GraphQL, Groovy, HCL/Terraform,\nJulia, Make, Nix, Objective-C, Perl, PowerShell, Protocol Buffers, R,\nSolidity, SQL, Svelte, and XML (incl. DTD), bringing syntax-aware\nchunking to ~50 languages. Document the native language list and the\nline-based fallback tiers in the README.\n\nConsidered but excluded: tree-sitter-clojure (pins tree-sitter 0.25,\nconflicts with 0.26) and tree-sitter-dockerfile (pins tree-sitter 0.20).\n\nStripped x86_64 Linux release binary grows 50.9 MB -> 83.7 MB\n(gzipped: 7.5 MB -> 10.1 MB), dominated by the Fortran, Julia,\nObjective-C, and D parser tables.\n\nCo-authored-by: Cursor Agent <cursoragent@cursor.com>\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>",
          "timestamp": "2026-07-03T13:53:25-04:00",
          "tree_id": "aa7d8e8413c072d49db8b39aba53029798d7fba4",
          "url": "https://github.com/jimsimon/trouve/commit/22d3e560019dc5635491b0b20caf7c55b621f2b1"
        },
        "date": 1783101389139,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "cold index + query",
            "value": 118.07405020000002,
            "range": "± 8.7",
            "unit": "ms"
          },
          {
            "name": "warm query",
            "value": 57.729071319999996,
            "range": "± 1.7",
            "unit": "ms"
          },
          {
            "name": "incremental (1 file modified)",
            "value": 68.33090696000001,
            "range": "± 1.8",
            "unit": "ms"
          },
          {
            "name": "non-git warm query",
            "value": 57.008611140000006,
            "range": "± 1",
            "unit": "ms"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "jim.j.simon@gmail.com",
            "name": "Jim Simon",
            "username": "jimsimon"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "7117bbc09c7dc497bac1c19bffd1d206b5411395",
          "message": "Fix new stable clippy lint manual_is_multiple_of (#3)\n\n* Fix new stable clippy lint manual_is_multiple_of\n\nCurrent stable clippy (-D warnings in lint CI) flags the manual modulo\nchecks in embed.rs and tests/embed_parity.rs. Use usize::is_multiple_of\nand raise the advertised MSRV to 1.87, where it was stabilized.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Remove clone_cache.rs committed by mistake\n\nThe file belongs to the separate clone-caching branch; it was untracked\nand slipped into the previous commit via git add -A.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n---------\n\nCo-authored-by: Cursor Agent <cursoragent@cursor.com>\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>",
          "timestamp": "2026-07-03T13:55:24-04:00",
          "tree_id": "0ea316d52639cfd8ec54abe1489580c9c927c3eb",
          "url": "https://github.com/jimsimon/trouve/commit/7117bbc09c7dc497bac1c19bffd1d206b5411395"
        },
        "date": 1783101517085,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "cold index + query",
            "value": 122.62990428,
            "range": "± 1.2",
            "unit": "ms"
          },
          {
            "name": "warm query",
            "value": 58.91164150000001,
            "range": "± 1.6",
            "unit": "ms"
          },
          {
            "name": "incremental (1 file modified)",
            "value": 69.04898406,
            "range": "± 1.4",
            "unit": "ms"
          },
          {
            "name": "non-git warm query",
            "value": 57.970883900000004,
            "range": "± 0.8",
            "unit": "ms"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "jim.j.simon@gmail.com",
            "name": "Jim Simon",
            "username": "jimsimon"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "c941ab5bb6338796215d329e0389cbee2a09852b",
          "message": "Update LICENSE copyright holder to James Simon (#13)\n\nCo-authored-by: Cursor Agent <cursoragent@cursor.com>\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>",
          "timestamp": "2026-07-03T15:03:48-04:00",
          "tree_id": "71414dea1f4009dd7ff10bb72b0ab2f69d867881",
          "url": "https://github.com/jimsimon/trouve/commit/c941ab5bb6338796215d329e0389cbee2a09852b"
        },
        "date": 1783105521579,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "cold index + query",
            "value": 120.06920018000001,
            "range": "± 4.1",
            "unit": "ms"
          },
          {
            "name": "warm query",
            "value": 58.48141726,
            "range": "± 2.2",
            "unit": "ms"
          },
          {
            "name": "incremental (1 file modified)",
            "value": 70.33421738000001,
            "range": "± 1.4",
            "unit": "ms"
          },
          {
            "name": "non-git warm query",
            "value": 59.79942134000001,
            "range": "± 1.8",
            "unit": "ms"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "jim.j.simon@gmail.com",
            "name": "Jim Simon",
            "username": "jimsimon"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "01d6d607afdfba9532dfc392fa8ff390f1703010",
          "message": "Add trouve-native config names with deprecated semble fallbacks (#14)\n\n- .trouveignore is now honoured per directory (same gitignore semantics),\n  taking precedence over the deprecated .sembleignore where patterns\n  conflict; .sembleignore still works but logs a one-time deprecation\n  warning pointing at .trouveignore.\n- SEMBLE_CACHE_LOCATION, SEMBLE_MODEL_NAME, and SEMBLE_CLONE_TIMEOUT are\n  honoured as fallbacks when the TROUVE_* equivalent is unset, with the\n  same one-time deprecation warning.\n- .semble/ directories are skipped during walks alongside .trouve/,\n  matching upstream's default ignore list.\n\nCo-authored-by: Cursor Agent <cursoragent@cursor.com>\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>",
          "timestamp": "2026-07-03T16:46:36-04:00",
          "tree_id": "77de7c46fec7e1fb22775691563517390b8f6154",
          "url": "https://github.com/jimsimon/trouve/commit/01d6d607afdfba9532dfc392fa8ff390f1703010"
        },
        "date": 1783111670646,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "cold index + query",
            "value": 117.79910108,
            "range": "± 9.4",
            "unit": "ms"
          },
          {
            "name": "warm query",
            "value": 57.135625100000006,
            "range": "± 1.3",
            "unit": "ms"
          },
          {
            "name": "incremental (1 file modified)",
            "value": 67.80243886000001,
            "range": "± 1.7",
            "unit": "ms"
          },
          {
            "name": "non-git warm query",
            "value": 56.297090620000006,
            "range": "± 1.2",
            "unit": "ms"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "jim.j.simon@gmail.com",
            "name": "Jim Simon",
            "username": "jimsimon"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "93ea488b5d6edce49a23796bb4f7286707ef6472",
          "message": "Consolidate all agent plugins into one trouve-plugin package (#12)\n\n* Consolidate all agent plugins into one trouve-plugin package\n\nplugins/trouve is simultaneously the npm package trouve-plugin for\nOpenCode and Kilo Code (native tools backed by one persistent trouve\nstdio server per session), the Claude Code plugin bundle (MCP server,\nsub-agent, workflow skill, SessionStart index-warming hook; marketplace\nat .claude-plugin/marketplace.json), and the Codex plugin bundle (MCP\nserver + skill; marketplace at .agents/plugins/marketplace.json).\n\nThe OpenCode/Kilo plugin warms the project index at load and on\nsession.idle (throttled; warm:false disables). README gains an Agent\nintegrations feature grid comparing every install route.\n\nRebased onto main as a single commit, folding in all review fixes.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Address review: request timeouts, stderr capture, visible failures\n\n- request() now takes a per-request timeout (30s for the initialize\n  handshake, 10 minutes for tools/call to cover cold index builds of\n  huge repos). On timeout the pending request is rejected with an\n  actionable message and the server is killed so the next call starts\n  fresh — a stalled-but-alive server can no longer hang an agent turn.\n- The server's stderr is captured (last 2KB) and included in the\n  rejection message when the process exits unexpectedly.\n- The Claude SessionStart hook now fails visibly with an install hint\n  when the trouve binary is missing, instead of masking it with exit 0;\n  the warm itself still runs backgrounded via nohup.\n- Invalid content plugin-option values are reported via console.warn\n  instead of being silently dropped.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n---------\n\nCo-authored-by: Cursor Agent <cursoragent@cursor.com>\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>",
          "timestamp": "2026-07-03T17:21:03-04:00",
          "tree_id": "06ce1231852c1c9a4feede5fe15470ec8978f9f6",
          "url": "https://github.com/jimsimon/trouve/commit/93ea488b5d6edce49a23796bb4f7286707ef6472"
        },
        "date": 1783113744197,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "cold index + query",
            "value": 123.25124344000001,
            "range": "± 1.6",
            "unit": "ms"
          },
          {
            "name": "warm query",
            "value": 59.95674306000001,
            "range": "± 1.7",
            "unit": "ms"
          },
          {
            "name": "incremental (1 file modified)",
            "value": 69.85672328000001,
            "range": "± 1.6",
            "unit": "ms"
          },
          {
            "name": "non-git warm query",
            "value": 58.375656920000004,
            "range": "± 1.5",
            "unit": "ms"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "jim.j.simon@gmail.com",
            "name": "Jim Simon",
            "username": "jimsimon"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "bb36045e50014ccd97a4e394d3552b90cb557f5c",
          "message": "Keep crate, plugin, and package versions in sync — enforced in CI and releases (#11)\n\nscripts/sync_versions.py treats the crate version in Cargo.toml\n(parsed with tomllib) as the single source of truth and rewrites every\npublished manifest to match: plugins/*/package.json, package-lock.json\n(both version records), and Claude Code / Codex plugin.json manifests.\nLint CI runs it with --check so any drift fails the build.\n\nThe release workflow gains a verify-versions job (sync check + tag ==\ncrate version assertion, before any build) and a publish-npm job that\npublishes every plugins/*/package.json package at the same version\nafter the GitHub release — idempotent, and skipped cleanly when no npm\npackages exist or NPM_TOKEN is not configured. New checkout steps set\npersist-credentials: false.\n\nWith the unified plugin (#12) on main, the sync check now covers\nplugins/trouve. Rebased onto main as a single commit with all review\nfixes.\n\nCo-authored-by: Cursor Agent <cursoragent@cursor.com>\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>",
          "timestamp": "2026-07-03T17:38:25-04:00",
          "tree_id": "8f1adfb5bd3765e3db2792f5b4cd9abda4d2b44f",
          "url": "https://github.com/jimsimon/trouve/commit/bb36045e50014ccd97a4e394d3552b90cb557f5c"
        },
        "date": 1783114785817,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "cold index + query",
            "value": 116.92929120000001,
            "range": "± 5",
            "unit": "ms"
          },
          {
            "name": "warm query",
            "value": 57.500692560000005,
            "range": "± 2.9",
            "unit": "ms"
          },
          {
            "name": "incremental (1 file modified)",
            "value": 67.6127314,
            "range": "± 1.6",
            "unit": "ms"
          },
          {
            "name": "non-git warm query",
            "value": 56.352596240000004,
            "range": "± 1.4",
            "unit": "ms"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "jim.j.simon@gmail.com",
            "name": "Jim Simon",
            "username": "jimsimon"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "6b52916408d70e9c364e43cb3e7933573b43ccad",
          "message": "Cache shallow clones of remote repositories persistently (#4)\n\n* Cache shallow clones of remote repositories persistently\n\nfrom_git used to clone into a throwaway temp directory on every call,\nmaking the network-bound clone the dominant repeated cost of querying a\nremote repo (chunks and embeddings were already cached by the store).\n\nClones now persist under <cache>/clones keyed by URL (and optional\nref): refreshed via git fetch --depth 1 + hard reset at most once per\nfreshness window (TROUVE_CLONE_TTL seconds, default 300; the stamp\nadvances even on failed fetches so unreachable remotes are retried once\nper window), guarded by advisory file locks held for the whole index\nbuild, with stale clones served (with a warning) when the remote is\nunreachable. Idle clones and orphaned partials are evicted after a\nweek; trouve clear index reclaims per key while honouring locks and\nreports skipped in-use clones. Refs pass after --end-of-options.\n\nThe MCP server now re-validates git URLs after the same cooldown as\nlocal paths. Clone timeout honours the deprecated SEMBLE_CLONE_TIMEOUT\nfallback. MSRV rises to 1.89 for std file locking.\n\nRebased onto main (post-#12) as a single commit with all review fixes.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Retrigger review of the rebased head\n\nCodeRabbit's rate limiter skipped the review of the previous push;\nall feedback from its last review round is addressed in that commit\n(clear_clones honours locks, failed refreshes advance the TTL stamp,\nchangelog conflict markers removed, test isolated to its own clone).\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n---------\n\nCo-authored-by: Cursor Agent <cursoragent@cursor.com>\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>",
          "timestamp": "2026-07-03T18:11:53-04:00",
          "tree_id": "c2f2c758dfec558d69085f35c7e8466d9e7d10b0",
          "url": "https://github.com/jimsimon/trouve/commit/6b52916408d70e9c364e43cb3e7933573b43ccad"
        },
        "date": 1783116794755,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "cold index + query",
            "value": 120.89273136000001,
            "range": "± 8.1",
            "unit": "ms"
          },
          {
            "name": "warm query",
            "value": 61.346233659999996,
            "range": "± 1.6",
            "unit": "ms"
          },
          {
            "name": "incremental (1 file modified)",
            "value": 72.709371,
            "range": "± 0.8",
            "unit": "ms"
          },
          {
            "name": "non-git warm query",
            "value": 61.28082774,
            "range": "± 1.6",
            "unit": "ms"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "jim.j.simon@gmail.com",
            "name": "Jim Simon",
            "username": "jimsimon"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "134c8e48abbdbec99793f0c76e327ed03cf5cd60",
          "message": "Add native OpenCode custom tools as an opt-in alternative to MCP (#6)\n\ntrouve install gains a fourth, opt-in integration (\"Native tool\")\nthat writes ~/.config/opencode/tools/trouve.ts: exports surface to the\nmodel as trouve_search and trouve_find_related, run the trouve CLI via\nBun.spawn with a 10-minute watchdog (SIGTERM, then SIGKILL after 5s)\nand a catch on the stream await so every failure path returns tool\noutput, default repo to the session worktree, and support a content\nargument. MCP remains the default integration and is never touched by\nthe tool file; instruction blocks render whichever tool names the\nselected integrations expose. Documented in the README's Agent\nintegrations grid.\n\nRebased onto main (post-#4/#11) as a single commit with all review\nfixes.\n\nCo-authored-by: Cursor Agent <cursoragent@cursor.com>\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>",
          "timestamp": "2026-07-03T18:21:46-04:00",
          "tree_id": "2e99cc8b211f50fd466ffb86c3ae3f15c336ee77",
          "url": "https://github.com/jimsimon/trouve/commit/134c8e48abbdbec99793f0c76e327ed03cf5cd60"
        },
        "date": 1783117378958,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "cold index + query",
            "value": 121.01258385999999,
            "range": "± 1.7",
            "unit": "ms"
          },
          {
            "name": "warm query",
            "value": 59.2790148,
            "range": "± 2",
            "unit": "ms"
          },
          {
            "name": "incremental (1 file modified)",
            "value": 68.65951584,
            "range": "± 0.9",
            "unit": "ms"
          },
          {
            "name": "non-git warm query",
            "value": 56.927228420000006,
            "range": "± 1.7",
            "unit": "ms"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "jim.j.simon@gmail.com",
            "name": "Jim Simon",
            "username": "jimsimon"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "3ffd6848b3649fe9c20668def17396487e7ad869",
          "message": "Validate store parameters on snapshot open; verify existing files in save (#16)\n\nThe patch fast path opens the newest compatible snapshot regardless of\nmanifest hash (open_latest), but only validated SNAPSHOT_VERSION,\nmodel_id, and content types. STORE_VERSION and DESIRED_CHUNK_LENGTH are\nmixed into the manifest hash (so exact-match loads were safe) but were\nnot recorded in the snapshot itself — a future STORE_VERSION bump\nwithout a matching SNAPSHOT_VERSION bump would have silently spliced\nrows chunked under the old rules into patched indexes, breaking the\npatched-equals-full-rebuild guarantee.\n\nRecord store_version and chunk_len in SnapshotMeta and reject\nmismatches in RawSnapshot::open. Bump the snapshot format to v4\n(SMBLSNP4) since the meta layout changed; old snapshots are discarded\non magic mismatch and rebuilt.\n\nAlso fix save()'s early exit: the snapshot filename truncates the\nmanifest hash to 128 bits, and save() trusted any pre-existing file at\nthat path without verifying its embedded full hash — a partial or\nforeign file would be kept forever and miss on every load. Verify the\nexisting file and rewrite it if it is not actually this snapshot.\n\nThe module doc still described the v2 magic; it now matches the code.\n\nCo-authored-by: Cursor Agent <cursoragent@cursor.com>\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>",
          "timestamp": "2026-07-03T20:14:54-04:00",
          "tree_id": "aa2fd2a41ad500b180574b14dcf106846ca7b565",
          "url": "https://github.com/jimsimon/trouve/commit/3ffd6848b3649fe9c20668def17396487e7ad869"
        },
        "date": 1783124169285,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "cold index + query",
            "value": 123.83666352000002,
            "range": "± 11.8",
            "unit": "ms"
          },
          {
            "name": "warm query",
            "value": 61.753597860000006,
            "range": "± 2",
            "unit": "ms"
          },
          {
            "name": "incremental (1 file modified)",
            "value": 73.96610446,
            "range": "± 1",
            "unit": "ms"
          },
          {
            "name": "non-git warm query",
            "value": 60.8970364,
            "range": "± 1.2",
            "unit": "ms"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "jim.j.simon@gmail.com",
            "name": "Jim Simon",
            "username": "jimsimon"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "600ffaa2e2445d200c7048ae841b2093619dfdb2",
          "message": "Report snapshot reuse separately and add the documented cache hit rate (#19)\n\nBuildStats.files_from_store counted every non-recomputed file,\nincluding rows spliced zero-copy out of a previous snapshot — after a\npatch build the stats implied store reads that never happened, and the\nexact-match snapshot load reported the whole manifest as store hits.\nTrack files_from_snapshot separately and only count real store reads\nin files_from_store.\n\nThe stats subcommand help, README, and DIFFERENCES.md all promised a\ncache hit rate that the output never included; trouve stats now emits\ncache_hit_rate (files reused from any cache layer over files_total)\nalongside the per-layer counts.\n\nCo-authored-by: Cursor Agent <cursoragent@cursor.com>\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>",
          "timestamp": "2026-07-03T20:15:20-04:00",
          "tree_id": "4651f6562da6d13513c1e11e24fcf6b6b588ec85",
          "url": "https://github.com/jimsimon/trouve/commit/600ffaa2e2445d200c7048ae841b2093619dfdb2"
        },
        "date": 1783124299698,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "cold index + query",
            "value": 115.83488864,
            "range": "± 7.3",
            "unit": "ms"
          },
          {
            "name": "warm query",
            "value": 58.6285584,
            "range": "± 0.9",
            "unit": "ms"
          },
          {
            "name": "incremental (1 file modified)",
            "value": 70.10730076,
            "range": "± 0.7",
            "unit": "ms"
          },
          {
            "name": "non-git warm query",
            "value": 57.67109850000001,
            "range": "± 2.6",
            "unit": "ms"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "jim.j.simon@gmail.com",
            "name": "Jim Simon",
            "username": "jimsimon"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "81f99b6338ff3eed4521065ee09c7d80d040741e",
          "message": "Docs cleanup: fix inaccuracies found in review (#23)\n\n- README: Go was fully wired (tree-sitter-go dependency and chunk.rs\n  match arm) but missing from the natively-supported language table.\n- README: the cache-location section paired SEMBLE_CLONE_TIMEOUT with\n  TROUVE_CLONE_TTL, but the TTL is trouve-only; the actual pair is\n  TROUVE_CLONE_TIMEOUT (git network timeout, default 60s), which was\n  undocumented. Document both correctly.\n- CHANGELOG: the 1.0.0 installer entry said eleven coding agents; there\n  are 14 (matching the README).\n- BENCHMARKS: the kubernetes warm-query time appeared as both 0.55s and\n  0.54s; use the headline 0.55s consistently.\n- plugin README: the Claude Code section listed raw MCP tool names\n  while Codex showed the harness-prefixed ones; Claude Code also\n  prefixes (mcp__trouve__*).\n- SearchResult/search module docs now state that reranking changes the\n  score scale, so scores are only comparable within one result list.\n- bm25.rs module doc now notes production indexing tokenizes content\n  and path enrichment separately (index::path_enrichment_tokens) and\n  keeps enrich_for_bm25 as the upstream-reference form.\n- manifest.rs documents the mtime+size fast-path staleness caveat for\n  non-git roots (same trade-off as git's stat-based detection).\n\nCo-authored-by: Cursor Agent <cursoragent@cursor.com>\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>",
          "timestamp": "2026-07-03T21:29:10-04:00",
          "tree_id": "8fb7a48d27a0c8e395dbdd851e9d94168ed0ff78",
          "url": "https://github.com/jimsimon/trouve/commit/81f99b6338ff3eed4521065ee09c7d80d040741e"
        },
        "date": 1783128625402,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "cold index + query",
            "value": 124.91506824,
            "range": "± 6.8",
            "unit": "ms"
          },
          {
            "name": "warm query",
            "value": 59.50724926000001,
            "range": "± 1.3",
            "unit": "ms"
          },
          {
            "name": "incremental (1 file modified)",
            "value": 70.64415818,
            "range": "± 1.2",
            "unit": "ms"
          },
          {
            "name": "non-git warm query",
            "value": 57.94205618,
            "range": "± 1.2",
            "unit": "ms"
          }
        ]
      }
    ],
    "micro-benchmarks": [
      {
        "commit": {
          "author": {
            "email": "jim.j.simon@gmail.com",
            "name": "Jim Simon",
            "username": "jimsimon"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "c54a63d163682009cd91a59851bd623c93a9f52a",
          "message": "Benchmark git vs non-git roots; gate CI on benchmark regressions (#1)\n\n* Benchmark git vs non-git roots on kubernetes\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Guard TOUCH_REL pipeline against SIGPIPE under pipefail\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Update git vs non-git numbers to committed-script run\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Gate CI on benchmark regressions\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Fix racy shared model dir in embed parity tests\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Persist benchmark data to gh-pages instead of the actions cache\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Address review: harden bench scripts and workflow\n\n- Resolve caller-supplied repo paths before cd; error instead of cloning over a missing user path\n- Shell-escape all values interpolated into hyperfine command strings; drop eval in favor of direct invocations\n- Restore the incremental-scenario file via EXIT trap so failures leave the tree clean\n- Recursive criterion glob (grouped/parameterized bench IDs) and a loud duplicate-name guard in the converter\n- SHA-pin all actions, set persist-credentials: false on checkouts\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n---------\n\nCo-authored-by: Cursor Agent <cursoragent@cursor.com>\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>",
          "timestamp": "2026-07-03T13:51:39-04:00",
          "tree_id": "099ec6b183f6cb426cc60fe16491d57ba3cdda2a",
          "url": "https://github.com/jimsimon/trouve/commit/c54a63d163682009cd91a59851bd623c93a9f52a"
        },
        "date": 1783101292746,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "bm25_build_5k_docs",
            "value": 4836295.05,
            "range": "± 7795",
            "unit": "ns"
          },
          {
            "name": "bm25_query_5k_docs",
            "value": 36837.23729543497,
            "range": "± 33",
            "unit": "ns"
          },
          {
            "name": "chunk_python_200_functions",
            "value": 2963367.852941177,
            "range": "± 1289",
            "unit": "ns"
          },
          {
            "name": "dense_query_20k_rows",
            "value": 1285569.2848324515,
            "range": "± 2454",
            "unit": "ns"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "jim.j.simon@gmail.com",
            "name": "Jim Simon",
            "username": "jimsimon"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "22d3e560019dc5635491b0b20caf7c55b621f2b1",
          "message": "Add tree-sitter grammars for 23 more languages (#5)\n\nBundle maintained crates.io grammars for CMake, D, Dart, Elm, ERB/EJS\ntemplates, Erlang, Fortran, Gleam, GraphQL, Groovy, HCL/Terraform,\nJulia, Make, Nix, Objective-C, Perl, PowerShell, Protocol Buffers, R,\nSolidity, SQL, Svelte, and XML (incl. DTD), bringing syntax-aware\nchunking to ~50 languages. Document the native language list and the\nline-based fallback tiers in the README.\n\nConsidered but excluded: tree-sitter-clojure (pins tree-sitter 0.25,\nconflicts with 0.26) and tree-sitter-dockerfile (pins tree-sitter 0.20).\n\nStripped x86_64 Linux release binary grows 50.9 MB -> 83.7 MB\n(gzipped: 7.5 MB -> 10.1 MB), dominated by the Fortran, Julia,\nObjective-C, and D parser tables.\n\nCo-authored-by: Cursor Agent <cursoragent@cursor.com>\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>",
          "timestamp": "2026-07-03T13:53:25-04:00",
          "tree_id": "aa7d8e8413c072d49db8b39aba53029798d7fba4",
          "url": "https://github.com/jimsimon/trouve/commit/22d3e560019dc5635491b0b20caf7c55b621f2b1"
        },
        "date": 1783101430500,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "bm25_build_5k_docs",
            "value": 5052508.45,
            "range": "± 10489",
            "unit": "ns"
          },
          {
            "name": "bm25_query_5k_docs",
            "value": 35875.32666630482,
            "range": "± 6",
            "unit": "ns"
          },
          {
            "name": "chunk_python_200_functions",
            "value": 2804065.6944444445,
            "range": "± 919",
            "unit": "ns"
          },
          {
            "name": "dense_query_20k_rows",
            "value": 1529569.7038398692,
            "range": "± 11718",
            "unit": "ns"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "jim.j.simon@gmail.com",
            "name": "Jim Simon",
            "username": "jimsimon"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "7117bbc09c7dc497bac1c19bffd1d206b5411395",
          "message": "Fix new stable clippy lint manual_is_multiple_of (#3)\n\n* Fix new stable clippy lint manual_is_multiple_of\n\nCurrent stable clippy (-D warnings in lint CI) flags the manual modulo\nchecks in embed.rs and tests/embed_parity.rs. Use usize::is_multiple_of\nand raise the advertised MSRV to 1.87, where it was stabilized.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Remove clone_cache.rs committed by mistake\n\nThe file belongs to the separate clone-caching branch; it was untracked\nand slipped into the previous commit via git add -A.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n---------\n\nCo-authored-by: Cursor Agent <cursoragent@cursor.com>\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>",
          "timestamp": "2026-07-03T13:55:24-04:00",
          "tree_id": "0ea316d52639cfd8ec54abe1489580c9c927c3eb",
          "url": "https://github.com/jimsimon/trouve/commit/7117bbc09c7dc497bac1c19bffd1d206b5411395"
        },
        "date": 1783101558748,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "bm25_build_5k_docs",
            "value": 4824395.35,
            "range": "± 9922",
            "unit": "ns"
          },
          {
            "name": "bm25_query_5k_docs",
            "value": 37213.31794380587,
            "range": "± 29",
            "unit": "ns"
          },
          {
            "name": "chunk_python_200_functions",
            "value": 3056787.029411765,
            "range": "± 1658",
            "unit": "ns"
          },
          {
            "name": "dense_query_20k_rows",
            "value": 1374890.5306856188,
            "range": "± 8512",
            "unit": "ns"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "jim.j.simon@gmail.com",
            "name": "Jim Simon",
            "username": "jimsimon"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "c941ab5bb6338796215d329e0389cbee2a09852b",
          "message": "Update LICENSE copyright holder to James Simon (#13)\n\nCo-authored-by: Cursor Agent <cursoragent@cursor.com>\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>",
          "timestamp": "2026-07-03T15:03:48-04:00",
          "tree_id": "71414dea1f4009dd7ff10bb72b0ab2f69d867881",
          "url": "https://github.com/jimsimon/trouve/commit/c941ab5bb6338796215d329e0389cbee2a09852b"
        },
        "date": 1783105546979,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "bm25_build_5k_docs",
            "value": 4799090.090909091,
            "range": "± 10963",
            "unit": "ns"
          },
          {
            "name": "bm25_query_5k_docs",
            "value": 36713.62142038946,
            "range": "± 24",
            "unit": "ns"
          },
          {
            "name": "chunk_python_200_functions",
            "value": 2964038.970588235,
            "range": "± 1655",
            "unit": "ns"
          },
          {
            "name": "dense_query_20k_rows",
            "value": 1355977.4707052442,
            "range": "± 18106",
            "unit": "ns"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "jim.j.simon@gmail.com",
            "name": "Jim Simon",
            "username": "jimsimon"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "01d6d607afdfba9532dfc392fa8ff390f1703010",
          "message": "Add trouve-native config names with deprecated semble fallbacks (#14)\n\n- .trouveignore is now honoured per directory (same gitignore semantics),\n  taking precedence over the deprecated .sembleignore where patterns\n  conflict; .sembleignore still works but logs a one-time deprecation\n  warning pointing at .trouveignore.\n- SEMBLE_CACHE_LOCATION, SEMBLE_MODEL_NAME, and SEMBLE_CLONE_TIMEOUT are\n  honoured as fallbacks when the TROUVE_* equivalent is unset, with the\n  same one-time deprecation warning.\n- .semble/ directories are skipped during walks alongside .trouve/,\n  matching upstream's default ignore list.\n\nCo-authored-by: Cursor Agent <cursoragent@cursor.com>\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>",
          "timestamp": "2026-07-03T16:46:36-04:00",
          "tree_id": "77de7c46fec7e1fb22775691563517390b8f6154",
          "url": "https://github.com/jimsimon/trouve/commit/01d6d607afdfba9532dfc392fa8ff390f1703010"
        },
        "date": 1783111717330,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "bm25_build_5k_docs",
            "value": 5054409.15,
            "range": "± 10088",
            "unit": "ns"
          },
          {
            "name": "bm25_query_5k_docs",
            "value": 35675.966458333336,
            "range": "± 5",
            "unit": "ns"
          },
          {
            "name": "chunk_python_200_functions",
            "value": 2820735.472222222,
            "range": "± 1123",
            "unit": "ns"
          },
          {
            "name": "dense_query_20k_rows",
            "value": 1490320.6433483372,
            "range": "± 10255",
            "unit": "ns"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "jim.j.simon@gmail.com",
            "name": "Jim Simon",
            "username": "jimsimon"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "93ea488b5d6edce49a23796bb4f7286707ef6472",
          "message": "Consolidate all agent plugins into one trouve-plugin package (#12)\n\n* Consolidate all agent plugins into one trouve-plugin package\n\nplugins/trouve is simultaneously the npm package trouve-plugin for\nOpenCode and Kilo Code (native tools backed by one persistent trouve\nstdio server per session), the Claude Code plugin bundle (MCP server,\nsub-agent, workflow skill, SessionStart index-warming hook; marketplace\nat .claude-plugin/marketplace.json), and the Codex plugin bundle (MCP\nserver + skill; marketplace at .agents/plugins/marketplace.json).\n\nThe OpenCode/Kilo plugin warms the project index at load and on\nsession.idle (throttled; warm:false disables). README gains an Agent\nintegrations feature grid comparing every install route.\n\nRebased onto main as a single commit, folding in all review fixes.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Address review: request timeouts, stderr capture, visible failures\n\n- request() now takes a per-request timeout (30s for the initialize\n  handshake, 10 minutes for tools/call to cover cold index builds of\n  huge repos). On timeout the pending request is rejected with an\n  actionable message and the server is killed so the next call starts\n  fresh — a stalled-but-alive server can no longer hang an agent turn.\n- The server's stderr is captured (last 2KB) and included in the\n  rejection message when the process exits unexpectedly.\n- The Claude SessionStart hook now fails visibly with an install hint\n  when the trouve binary is missing, instead of masking it with exit 0;\n  the warm itself still runs backgrounded via nohup.\n- Invalid content plugin-option values are reported via console.warn\n  instead of being silently dropped.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n---------\n\nCo-authored-by: Cursor Agent <cursoragent@cursor.com>\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>",
          "timestamp": "2026-07-03T17:21:03-04:00",
          "tree_id": "06ce1231852c1c9a4feede5fe15470ec8978f9f6",
          "url": "https://github.com/jimsimon/trouve/commit/93ea488b5d6edce49a23796bb4f7286707ef6472"
        },
        "date": 1783113781933,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "bm25_build_5k_docs",
            "value": 4773579.954545455,
            "range": "± 7855",
            "unit": "ns"
          },
          {
            "name": "bm25_query_5k_docs",
            "value": 36758.75185185185,
            "range": "± 29",
            "unit": "ns"
          },
          {
            "name": "chunk_python_200_functions",
            "value": 2935443.794117647,
            "range": "± 1741",
            "unit": "ns"
          },
          {
            "name": "dense_query_20k_rows",
            "value": 1353331.826388889,
            "range": "± 7278",
            "unit": "ns"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "jim.j.simon@gmail.com",
            "name": "Jim Simon",
            "username": "jimsimon"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "bb36045e50014ccd97a4e394d3552b90cb557f5c",
          "message": "Keep crate, plugin, and package versions in sync — enforced in CI and releases (#11)\n\nscripts/sync_versions.py treats the crate version in Cargo.toml\n(parsed with tomllib) as the single source of truth and rewrites every\npublished manifest to match: plugins/*/package.json, package-lock.json\n(both version records), and Claude Code / Codex plugin.json manifests.\nLint CI runs it with --check so any drift fails the build.\n\nThe release workflow gains a verify-versions job (sync check + tag ==\ncrate version assertion, before any build) and a publish-npm job that\npublishes every plugins/*/package.json package at the same version\nafter the GitHub release — idempotent, and skipped cleanly when no npm\npackages exist or NPM_TOKEN is not configured. New checkout steps set\npersist-credentials: false.\n\nWith the unified plugin (#12) on main, the sync check now covers\nplugins/trouve. Rebased onto main as a single commit with all review\nfixes.\n\nCo-authored-by: Cursor Agent <cursoragent@cursor.com>\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>",
          "timestamp": "2026-07-03T17:38:25-04:00",
          "tree_id": "8f1adfb5bd3765e3db2792f5b4cd9abda4d2b44f",
          "url": "https://github.com/jimsimon/trouve/commit/bb36045e50014ccd97a4e394d3552b90cb557f5c"
        },
        "date": 1783114830680,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "bm25_build_5k_docs",
            "value": 4821277.318181818,
            "range": "± 10204",
            "unit": "ns"
          },
          {
            "name": "bm25_query_5k_docs",
            "value": 36889.59164292498,
            "range": "± 30",
            "unit": "ns"
          },
          {
            "name": "chunk_python_200_functions",
            "value": 3080285.6176470593,
            "range": "± 1212",
            "unit": "ns"
          },
          {
            "name": "dense_query_20k_rows",
            "value": 1348077.005952381,
            "range": "± 7466",
            "unit": "ns"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "jim.j.simon@gmail.com",
            "name": "Jim Simon",
            "username": "jimsimon"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "6b52916408d70e9c364e43cb3e7933573b43ccad",
          "message": "Cache shallow clones of remote repositories persistently (#4)\n\n* Cache shallow clones of remote repositories persistently\n\nfrom_git used to clone into a throwaway temp directory on every call,\nmaking the network-bound clone the dominant repeated cost of querying a\nremote repo (chunks and embeddings were already cached by the store).\n\nClones now persist under <cache>/clones keyed by URL (and optional\nref): refreshed via git fetch --depth 1 + hard reset at most once per\nfreshness window (TROUVE_CLONE_TTL seconds, default 300; the stamp\nadvances even on failed fetches so unreachable remotes are retried once\nper window), guarded by advisory file locks held for the whole index\nbuild, with stale clones served (with a warning) when the remote is\nunreachable. Idle clones and orphaned partials are evicted after a\nweek; trouve clear index reclaims per key while honouring locks and\nreports skipped in-use clones. Refs pass after --end-of-options.\n\nThe MCP server now re-validates git URLs after the same cooldown as\nlocal paths. Clone timeout honours the deprecated SEMBLE_CLONE_TIMEOUT\nfallback. MSRV rises to 1.89 for std file locking.\n\nRebased onto main (post-#12) as a single commit with all review fixes.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Retrigger review of the rebased head\n\nCodeRabbit's rate limiter skipped the review of the previous push;\nall feedback from its last review round is addressed in that commit\n(clear_clones honours locks, failed refreshes advance the TTL stamp,\nchangelog conflict markers removed, test isolated to its own clone).\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n---------\n\nCo-authored-by: Cursor Agent <cursoragent@cursor.com>\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>",
          "timestamp": "2026-07-03T18:11:53-04:00",
          "tree_id": "c2f2c758dfec558d69085f35c7e8466d9e7d10b0",
          "url": "https://github.com/jimsimon/trouve/commit/6b52916408d70e9c364e43cb3e7933573b43ccad"
        },
        "date": 1783116837545,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "bm25_build_5k_docs",
            "value": 4853027.090909091,
            "range": "± 7578",
            "unit": "ns"
          },
          {
            "name": "bm25_query_5k_docs",
            "value": 37115.97875948237,
            "range": "± 32",
            "unit": "ns"
          },
          {
            "name": "chunk_python_200_functions",
            "value": 2951606.0588235296,
            "range": "± 1081",
            "unit": "ns"
          },
          {
            "name": "dense_query_20k_rows",
            "value": 1342083.738310709,
            "range": "± 7993",
            "unit": "ns"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "jim.j.simon@gmail.com",
            "name": "Jim Simon",
            "username": "jimsimon"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "134c8e48abbdbec99793f0c76e327ed03cf5cd60",
          "message": "Add native OpenCode custom tools as an opt-in alternative to MCP (#6)\n\ntrouve install gains a fourth, opt-in integration (\"Native tool\")\nthat writes ~/.config/opencode/tools/trouve.ts: exports surface to the\nmodel as trouve_search and trouve_find_related, run the trouve CLI via\nBun.spawn with a 10-minute watchdog (SIGTERM, then SIGKILL after 5s)\nand a catch on the stream await so every failure path returns tool\noutput, default repo to the session worktree, and support a content\nargument. MCP remains the default integration and is never touched by\nthe tool file; instruction blocks render whichever tool names the\nselected integrations expose. Documented in the README's Agent\nintegrations grid.\n\nRebased onto main (post-#4/#11) as a single commit with all review\nfixes.\n\nCo-authored-by: Cursor Agent <cursoragent@cursor.com>\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>",
          "timestamp": "2026-07-03T18:21:46-04:00",
          "tree_id": "2e99cc8b211f50fd466ffb86c3ae3f15c336ee77",
          "url": "https://github.com/jimsimon/trouve/commit/134c8e48abbdbec99793f0c76e327ed03cf5cd60"
        },
        "date": 1783117430922,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "bm25_build_5k_docs",
            "value": 5167545.75,
            "range": "± 13863",
            "unit": "ns"
          },
          {
            "name": "bm25_query_5k_docs",
            "value": 36375.539863445374,
            "range": "± 6",
            "unit": "ns"
          },
          {
            "name": "chunk_python_200_functions",
            "value": 2805324.25,
            "range": "± 3210",
            "unit": "ns"
          },
          {
            "name": "dense_query_20k_rows",
            "value": 1528145.2828407225,
            "range": "± 8369",
            "unit": "ns"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "jim.j.simon@gmail.com",
            "name": "Jim Simon",
            "username": "jimsimon"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "3ffd6848b3649fe9c20668def17396487e7ad869",
          "message": "Validate store parameters on snapshot open; verify existing files in save (#16)\n\nThe patch fast path opens the newest compatible snapshot regardless of\nmanifest hash (open_latest), but only validated SNAPSHOT_VERSION,\nmodel_id, and content types. STORE_VERSION and DESIRED_CHUNK_LENGTH are\nmixed into the manifest hash (so exact-match loads were safe) but were\nnot recorded in the snapshot itself — a future STORE_VERSION bump\nwithout a matching SNAPSHOT_VERSION bump would have silently spliced\nrows chunked under the old rules into patched indexes, breaking the\npatched-equals-full-rebuild guarantee.\n\nRecord store_version and chunk_len in SnapshotMeta and reject\nmismatches in RawSnapshot::open. Bump the snapshot format to v4\n(SMBLSNP4) since the meta layout changed; old snapshots are discarded\non magic mismatch and rebuilt.\n\nAlso fix save()'s early exit: the snapshot filename truncates the\nmanifest hash to 128 bits, and save() trusted any pre-existing file at\nthat path without verifying its embedded full hash — a partial or\nforeign file would be kept forever and miss on every load. Verify the\nexisting file and rewrite it if it is not actually this snapshot.\n\nThe module doc still described the v2 magic; it now matches the code.\n\nCo-authored-by: Cursor Agent <cursoragent@cursor.com>\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>",
          "timestamp": "2026-07-03T20:14:54-04:00",
          "tree_id": "aa2fd2a41ad500b180574b14dcf106846ca7b565",
          "url": "https://github.com/jimsimon/trouve/commit/3ffd6848b3649fe9c20668def17396487e7ad869"
        },
        "date": 1783124219207,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "bm25_build_5k_docs",
            "value": 4888265.909090909,
            "range": "± 6690",
            "unit": "ns"
          },
          {
            "name": "bm25_query_5k_docs",
            "value": 36957.28734228734,
            "range": "± 32",
            "unit": "ns"
          },
          {
            "name": "chunk_python_200_functions",
            "value": 3105329.09375,
            "range": "± 1884",
            "unit": "ns"
          },
          {
            "name": "dense_query_20k_rows",
            "value": 1386176.0267737617,
            "range": "± 8925",
            "unit": "ns"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "jim.j.simon@gmail.com",
            "name": "Jim Simon",
            "username": "jimsimon"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "600ffaa2e2445d200c7048ae841b2093619dfdb2",
          "message": "Report snapshot reuse separately and add the documented cache hit rate (#19)\n\nBuildStats.files_from_store counted every non-recomputed file,\nincluding rows spliced zero-copy out of a previous snapshot — after a\npatch build the stats implied store reads that never happened, and the\nexact-match snapshot load reported the whole manifest as store hits.\nTrack files_from_snapshot separately and only count real store reads\nin files_from_store.\n\nThe stats subcommand help, README, and DIFFERENCES.md all promised a\ncache hit rate that the output never included; trouve stats now emits\ncache_hit_rate (files reused from any cache layer over files_total)\nalongside the per-layer counts.\n\nCo-authored-by: Cursor Agent <cursoragent@cursor.com>\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>",
          "timestamp": "2026-07-03T20:15:20-04:00",
          "tree_id": "4651f6562da6d13513c1e11e24fcf6b6b588ec85",
          "url": "https://github.com/jimsimon/trouve/commit/600ffaa2e2445d200c7048ae841b2093619dfdb2"
        },
        "date": 1783124342333,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "bm25_build_5k_docs",
            "value": 5000658.75,
            "range": "± 7997",
            "unit": "ns"
          },
          {
            "name": "bm25_query_5k_docs",
            "value": 37199.88940329218,
            "range": "± 39",
            "unit": "ns"
          },
          {
            "name": "chunk_python_200_functions",
            "value": 3038219.2352941176,
            "range": "± 2485",
            "unit": "ns"
          },
          {
            "name": "dense_query_20k_rows",
            "value": 1382237.3854166667,
            "range": "± 10563",
            "unit": "ns"
          }
        ]
      }
    ]
  }
}