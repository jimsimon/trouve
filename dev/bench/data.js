window.BENCHMARK_DATA = {
  "lastUpdate": 1783101559588,
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
      }
    ]
  }
}