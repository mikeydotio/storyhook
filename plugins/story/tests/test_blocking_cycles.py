"""SH-687: exact cycle membership, including graphs Kahn cannot distinguish."""

import itertools
from pathlib import Path
import subprocess
import sys
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "lib"))
from blocking_cycles import cycle_members


class CycleTests(unittest.TestCase):
    def test_every_three_node_graph_matches_positive_length_reachability(self):
        possibilities = list(itertools.product(range(3), repeat=2))
        for mask in range(1 << len(possibilities)):
            edges = [edge for bit, edge in enumerate(possibilities) if mask & (1 << bit)]
            reachable = [[(a, b) in edges for b in range(3)] for a in range(3)]
            for via in range(3):
                for a in range(3):
                    for b in range(3):
                        reachable[a][b] |= reachable[a][via] and reachable[via][b]
            expected = [n for n in range(3) if reachable[n][n]]
            self.assertEqual(cycle_members(edges), expected, edges)

    def test_long_chain_into_and_out_of_cycle(self):
        edges = [(n, n + 1) for n in range(10000)] + [(5001, 5000)]
        self.assertEqual(cycle_members(edges), [5000, 5001])

    def test_disconnected_cycles_duplicate_edges_and_self_loop(self):
        edges = [("A", "B"), ("B", "A"), ("B", "C"), ("D", "E"),
                 ("E", "D"), ("F", "F"), ("G", "A"), ("A", "B")]
        self.assertEqual(cycle_members(edges), ["A", "B", "D", "E", "F"])
        self.assertEqual(cycle_members(reversed(edges)), ["A", "B", "D", "E", "F"])

    def test_cli_rejects_malformed_edges(self):
        script = Path(__file__).resolve().parents[1] / "lib" / "blocking_cycles.py"
        result = subprocess.run([sys.executable, str(script)], input="A\tB\ninvalid\n",
                                text=True, capture_output=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("invalid blocking edge on line 2", result.stderr)
        self.assertEqual(result.stdout, "")


if __name__ == "__main__":
    unittest.main()
