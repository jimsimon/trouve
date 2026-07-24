#!/usr/bin/env python3

import json
import pathlib
import sys
import tempfile
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).parent))

from compare_benchmarks import Benchmark, BenchmarkDataError, compare, load_suite


def suite(value: float, *, name: str = "query", unit: str = "ns"):
    benchmark = Benchmark(name, unit, value)
    return {name: benchmark}


class CompareBenchmarksTests(unittest.TestCase):
    def test_below_threshold_passes(self):
        status, report = compare(suite(120), [suite(100)], 1.5, 1, "Test")

        self.assertEqual(status, 0)
        self.assertIn("1.20× | Pass", report)

    def test_regression_fails(self):
        status, report = compare(suite(151), [suite(100)], 1.5, 1, "Test")

        self.assertEqual(status, 1)
        self.assertIn("Regression", report)

    def test_median_ignores_one_fast_outlier(self):
        status, report = compare(
            suite(140),
            [suite(50), suite(100), suite(101), suite(102), suite(103)],
            1.5,
            3,
            "Test",
        )

        self.assertEqual(status, 0)
        self.assertIn("1.39× | Pass", report)

    def test_insufficient_per_benchmark_history_requests_confirmation(self):
        status, report = compare(suite(100), [suite(100)], 1.5, 3, "Test")

        self.assertEqual(status, 2)
        self.assertIn("Insufficient history (1/3)", report)

    def test_new_benchmark_is_not_a_regression(self):
        status, report = compare(
            suite(100, name="new"), [suite(100, name="old")], 1.5, 1, "Test"
        )

        self.assertEqual(status, 0)
        self.assertIn("New benchmark", report)

    def test_unit_mismatch_is_rejected(self):
        with self.assertRaises(BenchmarkDataError):
            compare(suite(100, unit="ms"), [suite(100)], 1.5, 1, "Test")

    def test_invalid_file_is_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            path = pathlib.Path(directory) / "benchmarks.json"
            path.write_text(json.dumps([{"name": "query", "unit": "ns", "value": 0}]))

            with self.assertRaises(BenchmarkDataError):
                load_suite(path)


if __name__ == "__main__":
    unittest.main()
