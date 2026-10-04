"""Native-format observations drive the real sensor calculations."""

from pathlib import Path
import sys
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from host_admission.policy import Refusal
from host_admission.sensors import Sensor, linux_snapshot, runnable_states


class SensorTests(unittest.TestCase):
    def test_native_ps_states_count_running_processes_and_refuse_malformed_data(self):
        self.assertEqual(runnable_states("S\nR+\nRs\nI\nZ\n?\n?Es\n"), 4)
        for text in ("", "not a state", "??", "S R"):
            with self.assertRaises(Refusal):
                runnable_states(text)

    def test_first_cpu_sample_is_unknown_then_delta_is_measured(self):
        rows = iter([dict(cpu_ticks=(100, 100), available=123, memory=5, runnable=2),
                     dict(cpu_ticks=(120, 200), available=122, memory=10, runnable=3)])
        sensor = Sensor(lambda: next(rows), lambda: 500)
        self.assertIsNone(sensor())
        self.assertEqual(sensor(), dict(at=500, available=122, memory=10, cpu=200, runnable=3))

    def test_reversed_or_static_cpu_counters_fail_closed(self):
        for next_ticks in [(99, 101), (100, 100), (110, 105)]:
            with self.subTest(ticks=next_ticks):
                rows = iter([dict(cpu_ticks=(100, 100), available=123, memory=5, runnable=2),
                             dict(cpu_ticks=next_ticks, available=123, memory=5, runnable=2)])
                sensor = Sensor(lambda: next(rows), lambda: 500)
                sensor()
                with self.assertRaises(Refusal):
                    sensor()

    def test_linux_observes_memory_cpu_ticks_and_stalls(self):
        files = {"meminfo": "MemTotal: 1000 kB\nMemAvailable: 200 kB\n",
                 "stat": "cpu 10 5 20 60 5 2 3 0 0 0\nprocs_running 4\n",
                 "pressure/memory": "some avg10=12.50 avg60=2.0 avg300=1.0 total=50\nfull avg10=1.0 avg60=0 avg300=0 total=1\n",
                 "pressure/cpu": "some avg10=15.00 avg60=0 avg300=0 total=5\n"}
        row = linux_snapshot(files.__getitem__)
        self.assertEqual(row["available"], 204800)
        self.assertEqual(row["cpu_ticks"], (40, 105))
        self.assertEqual(row["cpu_stall"], 150)
        self.assertEqual(row["memory"], 125)
        self.assertEqual(row["runnable"], 4)
        files["pressure/memory"] = "malformed"
        with self.assertRaises(Refusal):
            linux_snapshot(files.__getitem__)


if __name__ == "__main__":
    unittest.main()
