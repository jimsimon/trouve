import json
import pathlib
import tempfile
import unittest

import analyze


def row(strategy: str, run: int, *, tokens: int, origin: str = "local", correct: bool = True):
    return {
        "model": "example/model",
        "strategy": strategy,
        "task": f"task-{run}",
        "run": run,
        "origin": origin,
        "output_tokens": tokens,
        "edit_retries": 0,
        "stale_retries": 0,
        "executor_ms": 10,
        "correct": correct,
        "tests_passed": True,
        "concurrency_safe": True,
    }


class AnalyzeTests(unittest.TestCase):
    def test_safe_token_reduction_passes(self):
        pairs = [
            (analyze.Run(**row("apply_patch", i, tokens=100)), analyze.Run(**row("hashline", i, tokens=60)))
            for i in range(1, 4)
        ]
        outcome, _ = analyze.decision(analyze.summarize(pairs), 3, 0.05)
        self.assertEqual(outcome, "pass")

    def test_correctness_regression_rejects(self):
        pairs = [
            (analyze.Run(**row("apply_patch", 1, tokens=100)), analyze.Run(**row("hashline", 1, tokens=50, correct=False)))
        ]
        outcome, _ = analyze.decision(analyze.summarize(pairs), 1, 0.05)
        self.assertEqual(outcome, "reject")

    def test_external_rows_are_kept_separate(self):
        with tempfile.TemporaryDirectory() as directory:
            path = pathlib.Path(directory, "runs.jsonl")
            path.write_text("\n".join(json.dumps(row(strategy, 1, tokens=tokens, origin="external")) for strategy, tokens in (("apply_patch", 100), ("hashline", 50))))
            runs = analyze.load(path)
        self.assertEqual(analyze.paired_runs(runs, "example/model", "apply_patch", "hashline", "local"), [])
        self.assertEqual(len(analyze.paired_runs(runs, "example/model", "apply_patch", "hashline", "external")), 1)

    def test_missing_origin_is_rejected(self):
        candidate = row("hashline", 1, tokens=50)
        candidate.pop("origin")
        with tempfile.TemporaryDirectory() as directory:
            path = pathlib.Path(directory, "runs.jsonl")
            path.write_text(json.dumps(candidate))
            with self.assertRaisesRegex(analyze.DataError, "origin must be str"):
                analyze.load(path)

    def test_unmatched_rows_are_rejected(self):
        runs = [analyze.Run(**row("hashline", 1, tokens=50))]
        with self.assertRaisesRegex(analyze.DataError, "not fully paired"):
            analyze.paired_runs(runs, "example/model", "apply_patch", "hashline", "local")

    def test_corpus_shape_requires_tasks_and_repetitions(self):
        pairs = [
            (
                analyze.Run(**row("apply_patch", run, tokens=100)),
                analyze.Run(**row("hashline", run, tokens=50)),
            )
            for run in range(1, 3)
        ]
        self.assertIn("distinct tasks", analyze.corpus_shape_error(pairs, 10, 2))

        repeated = []
        for task in range(10):
            baseline = row("apply_patch", 1, tokens=100)
            candidate = row("hashline", 1, tokens=50)
            baseline["task"] = candidate["task"] = f"task-{task}"
            repeated.append((analyze.Run(**baseline), analyze.Run(**candidate)))
        self.assertIn("independent runs", analyze.corpus_shape_error(repeated, 10, 2))

    def test_report_records_thresholds_and_stale_retries(self):
        pair = (
            analyze.Run(**row("apply_patch", 1, tokens=100)),
            analyze.Run(**{**row("hashline", 1, tokens=50), "stale_retries": 2}),
        )
        summary = analyze.summarize([pair])
        rendered = analyze.report(
            "example/model",
            "apply_patch",
            "hashline",
            summary,
            None,
            "pass",
            "test",
            20,
            10,
            2,
            0.05,
        )
        self.assertIn("Minimum paired local runs: 20", rendered)
        self.assertIn("Mean stale retries", rendered)
        self.assertIn("2.00", rendered)


if __name__ == "__main__":
    unittest.main()
