#!/usr/bin/env python3

import json
import pathlib
import sys
import tempfile
import unittest
from contextlib import redirect_stderr
from io import StringIO
from subprocess import TimeoutExpired
from unittest import mock

sys.path.insert(0, str(pathlib.Path(__file__).parent))

from run_search_comparison import (  # noqa: E402
    SearchCase,
    command_for,
    estimated_input_tokens,
    first_line,
    git_value,
    load_cases,
    parse_args,
    parse_extensions,
    summarize,
)


class SearchComparisonTests(unittest.TestCase):
    def test_estimated_tokens_rounds_up(self):
        self.assertEqual(estimated_input_tokens(b""), 0)
        self.assertEqual(estimated_input_tokens(b"abcd"), 1)
        self.assertEqual(estimated_input_tokens(b"abcde"), 2)

    def test_load_cases_rejects_duplicate_names(self):
        with tempfile.TemporaryDirectory() as directory:
            path = pathlib.Path(directory) / "cases.json"
            path.write_text(
                json.dumps(
                    [
                        {"name": "same", "intent": "first", "pattern": "one"},
                        {"name": "same", "intent": "second", "pattern": "two"},
                    ]
                ),
                encoding="utf-8",
            )
            with self.assertRaisesRegex(ValueError, "duplicate case name"):
                load_cases(path)

    def test_commands_use_intent_for_trouve_and_pattern_for_lexical_tools(self):
        case = SearchCase("case", "semantic intent", "literal|names")
        extensions = (".ex",)
        trouve = command_for(
            "trouve-search", case, pathlib.Path("/bin/trouve"), extensions, 5, 10
        )
        grep = command_for("grep", case, pathlib.Path("/bin/trouve"), extensions, 5, 10)
        ripgrep = command_for(
            "ripgrep", case, pathlib.Path("/bin/trouve"), extensions, 5, 10
        )
        self.assertIn("semantic intent", trouve)
        self.assertNotIn("literal|names", trouve)
        self.assertIn("literal|names", grep)
        self.assertIn("literal|names", ripgrep)
        self.assertIn("--include=*.ex", grep)
        self.assertIn("*.ex", ripgrep)

    def test_parses_authoritative_extensions_including_less_common_code(self):
        self.assertEqual(parse_extensions('[".ex", ".rs"]'), (".ex", ".rs"))

    @mock.patch("run_search_comparison.subprocess.run", side_effect=OSError)
    def test_first_line_falls_back_when_command_is_unavailable(self, _run):
        self.assertEqual(first_line(["missing"]), "unknown")

    @mock.patch("run_search_comparison.subprocess.run", side_effect=TimeoutExpired("git", 5))
    def test_git_value_falls_back_when_command_times_out(self, _run):
        self.assertIsNone(git_value(pathlib.Path("."), "rev-parse", "HEAD"))

    def test_rejects_non_finite_input_costs(self):
        for value in ("nan", "inf", "-inf"):
            with self.subTest(value=value), redirect_stderr(StringIO()):
                with self.assertRaises(SystemExit):
                    parse_args(["--input-cost-per-million", value])

    def test_summary_converts_tokens_to_provider_cost(self):
        cases = [
            {
                "tools": {
                    tool: {"timings_ms": [1.0, 3.0], "estimated_input_tokens": tokens}
                    for tool, tokens in (
                        ("trouve-search", 100),
                        ("grep", 400),
                        ("ripgrep", 400),
                    )
                }
            }
        ]
        summary = summarize(cases, 2.0)
        self.assertEqual(summary["trouve-search"]["median_latency_ms"], 2.0)
        self.assertEqual(summary["grep"]["tokens_relative_to_trouve"], 4.0)
        self.assertEqual(summary["grep"]["cost_per_1000_searches_usd"], 0.8)


if __name__ == "__main__":
    unittest.main()
