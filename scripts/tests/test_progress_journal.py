"""Exercise semantic observation and the real machine-lock watchdog."""

import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import threading
import unittest

SCRIPTS = Path(__file__).resolve().parents[1]

# Bounds a hung watchdog run only, as a multiple of the idle ceiling the run
# enforces; a correct run ends at that ceiling (SH-698).
WATCHDOG_RUN_ALLOWANCE = 30
sys.path.insert(0, str(SCRIPTS))
from progress_journal import observe


class ObserverTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(dir="/tmp", prefix="progress-observation-")
        self.addCleanup(self.tmp.cleanup)
        self.path = Path(self.tmp.name) / "journal"
        self.path.touch()

    def append(self, data):
        with self.path.open("ab") as output:
            output.write(data)

    def test_complete_resource_records_and_fragments_are_not_progress(self):
        cursor, changed = observe(self.path)
        self.assertFalse(changed)
        self.append(b'{"extra":1,"kind":"resour\\u0063e"}\n{"kind":')
        cursor, changed = observe(self.path, cursor)
        self.assertFalse(changed)
        self.append(b'"resource"}\n{"kind":"case"}')
        cursor, changed = observe(self.path, cursor)
        self.assertFalse(changed)
        self.append(b'\n')
        cursor, changed = observe(self.path, cursor)
        self.assertTrue(changed)
        self.append(b'legacy progress\n')
        cursor, changed = observe(self.path, cursor)
        self.assertTrue(changed)
        self.assertFalse(observe(self.path, cursor)[1])

    def test_replacement_truncation_and_missing_journals_refuse(self):
        cursor, _ = observe(self.path)
        self.append(b'partial')
        cursor, _ = observe(self.path, cursor)
        self.path.write_bytes(b'')
        with self.assertRaisesRegex(ValueError, "shrank"):
            observe(self.path, cursor)
        cursor, _ = observe(self.path)
        moved = self.path.with_name("moved")
        self.path.rename(moved)
        self.path.write_bytes(b'{}\n')
        with self.assertRaisesRegex(ValueError, "replaced"):
            observe(self.path, cursor)
        self.path.unlink()
        with self.assertRaises(OSError):
            observe(self.path, cursor)
        self.path.symlink_to(moved)
        with self.assertRaises(OSError):
            observe(self.path, cursor)
        self.path.unlink(); os.mkfifo(self.path)
        with self.assertRaisesRegex(ValueError, "regular"):
            observe(self.path)

    def test_reads_are_bounded_and_a_large_record_cannot_be_progress(self):
        cursor, _ = observe(self.path)
        self.append(b'x' * 1_048_577)
        with self.assertRaisesRegex(ValueError, "oversized"):
            observe(self.path, cursor)
        self.path.write_bytes(b'')
        cursor, _ = observe(self.path)
        self.append(b'{"kind":"resource"}\n' * 100_000 + b'{}\n')
        next_cursor, changed = observe(self.path, cursor)
        self.assertFalse(changed)
        self.assertLess(next_cursor[3], self.path.stat().st_size)
        self.assertTrue(observe(self.path, next_cursor)[1])


class ShellTests(unittest.TestCase):
    def test_resource_only_journal_growth_does_not_keep_a_gate_alive(self):
        source = (SCRIPTS / "machine-lock.sh").read_text()
        poll = int(re.search(r"readonly LOCK_POLL_SECS=(\d+)", source)[1])
        ceiling = 2 * poll
        with tempfile.TemporaryDirectory(dir="/tmp", prefix="progress-shell-") as root:
            root = Path(root)
            journal, ready = root / "journal", root / "ready"
            journal.touch()
            stop = threading.Event()
            def feed_startup():
                while not ready.exists() and not stop.is_set():
                    with journal.open("a") as output:
                        output.write('{"kind":"activity","label":"fixture startup"}\n')
                    stop.wait(poll / 4)
            feeder = threading.Thread(target=feed_startup)
            feeder.start()
            code = ("import sys,time;from pathlib import Path;"
                    "journal=Path(sys.argv[1]);Path(sys.argv[2]).touch();"
                    "interval=float(sys.argv[3]);\n"
                    "for n in range(20):\n"
                    " with journal.open('a') as f:f.write('{\\\"kind\\\":\\\"resource\\\"}\\n')\n"
                    " time.sleep(interval)\n")
            env = dict(os.environ, STORYHOOK_LOCK_DIR=str(root / "locks"), STORYHOOK_GATE_PROGRESS=str(journal))
            env.pop("STORYHOOK_MACHINE_LOCKS", None)
            try:
                out = subprocess.run(["bash", str(SCRIPTS / "machine-lock.sh"), "--max-idle", str(ceiling),
                                      "resource-probe", "--", sys.executable, "-c", code,
                                      str(journal), str(ready), str(poll / 4)], env=env,
                                     stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=WATCHDOG_RUN_ALLOWANCE * ceiling)
            finally:
                stop.set(); feeder.join()
            self.assertTrue(ready.exists(), out.stderr.decode())
            self.assertIn('"kind":"resource"', journal.read_text())
            self.assertEqual(out.returncode, 124, out.stderr.decode())
            self.assertIn(b"made no progress", out.stderr)


if __name__ == "__main__":
    unittest.main()
