"""Scripted counter intervals establish measurement semantics without workload sleeps."""

from pathlib import Path
import sys
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from host_admission.usage import Monitor
from host_admission.policy import Refusal


class UsageTests(unittest.TestCase):
    def setUp(self):
        self.now = 1_000_000_000
        self.rows = {10: {"pid:1": dict(cpu_ns=1000, memory=100)}}
        self.monitor = Monitor("boot", read=lambda session: self.rows[session], clock=lambda: self.now)

    def test_cpu_interval_is_unknown_until_counters_span_the_same_incarnations(self):
        self.assertEqual(self.monitor.sample("root", [10]), dict(cpu=None, memory=100))
        self.now += 1_000_000_000
        self.rows[10]["pid:1"]["cpu_ns"] += 500_000_000
        self.assertEqual(self.monitor.sample("root", [10]), dict(cpu=500, memory=100))
        self.now += 1_000_000_000
        self.rows[10] = {"pid:2": dict(cpu_ns=900, memory=50)}
        self.assertEqual(self.monitor.sample("root", [10]), dict(cpu=None, memory=50))

    def test_nested_sessions_count_once_and_bad_counters_refuse(self):
        self.rows[20] = {"child:1": dict(cpu_ns=0, memory=200)}
        self.assertEqual(self.monitor.sample("root", [10, 20, 20])["memory"], 300)
        self.now += 1000
        self.rows[10]["pid:1"]["cpu_ns"] = 0
        with self.assertRaisesRegex(Refusal, "counter"):
            self.monitor.sample("root", [10, 20])

    def test_disappearing_members_and_nonadvancing_clock_are_not_zero_cpu(self):
        self.monitor.sample("root", [10])
        with self.assertRaises(Refusal):
            self.monitor.sample("root", [10])
        self.now += 1000; self.rows[10] = {}
        self.assertIsNone(self.monitor.sample("root", [10])["cpu"])


if __name__ == "__main__":
    unittest.main()
