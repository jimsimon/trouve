window.BENCHMARK_DATA = {
  "lastUpdate": 1783101292997,
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
      }
    ]
  }
}