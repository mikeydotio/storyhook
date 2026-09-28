"""A file's existence does not prove its PID has been published."""

from pathlib import Path
import tempfile
import unittest

from pid_record import read_pid


class PidRecord(unittest.TestCase):
    """Exercise missing, partial, complete, and malformed publication states."""

    def test_reads_only_complete_positive_records(self):
        """Never use a truncated PID to identify or signal a process."""
        with tempfile.TemporaryDirectory(dir="/tmp") as root:
            path = Path(root) / "pid"
            self.assertIsNone(read_pid(path))
            for partial in ("", "1", "12345"):
                path.write_text(partial)
                self.assertIsNone(read_pid(path), partial)
            path.write_text("12345\n")
            self.assertEqual(read_pid(path), 12345)

    def test_rejects_malformed_complete_records(self):
        """Completed invalid data fails with the file and contents as evidence."""
        with tempfile.TemporaryDirectory(dir="/tmp") as root:
            path = Path(root) / "pid"
            for malformed in ("\n", "0\n", "-1\n", "12x\n", "1\n2\n", "١٢\n"):
                path.write_text(malformed)
                with self.assertRaisesRegex(ValueError, str(path)):
                    read_pid(path)


if __name__ == "__main__":
    unittest.main()
