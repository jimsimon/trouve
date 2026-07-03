window.BENCHMARK_DATA = {
  "lastUpdate": 1783113782196,
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
      }
    ]
  }
}