"""rustc-slot.py chooses slots or a host admission root (SH-869, decision D7)."""

import importlib.util
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest import mock

SCRIPTS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPTS))
from host_admission import namespace

GRANT = {"STORYHOOK_HOST_GRANT": "token", "STORYHOOK_HOST_REQUEST": "rustc:1"}


def wrapper():
    spec = importlib.util.spec_from_file_location("rustc_slot", SCRIPTS / "rustc-slot.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class Exec(Exception):
    """Raised by the patched exec calls, so a test sees the argv instead of a new image."""


class Modes(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(dir="/tmp", prefix="rs-")
        self.addCleanup(self.tmp.cleanup)
        self.slot = wrapper()
        self.policy = Path(self.tmp.name) / "policy.json"
        self.slot.HOST_POLICY = str(self.policy)
        clean = {k: v for k, v in os.environ.items() if not k.startswith("STORYHOOK_HOST_")}
        clean["STORYHOOK_LOCK_DIR"] = str(Path(self.tmp.name) / "locks")
        patcher = mock.patch.dict(os.environ, clean, clear=True)
        patcher.start()
        self.addCleanup(patcher.stop)

    def run_main(self, argv):
        """main()'s exec target: ('admit', argv) for a root, ('rustc', argv) otherwise."""
        def admit(_program, args):
            raise Exec(("admit", args))

        def rustc(_program, args):
            raise Exec(("rustc", args))

        with mock.patch.object(sys, "argv", ["rustc-slot.py", *argv]), \
                mock.patch("os.execv", admit), mock.patch("os.execvp", rustc), \
                self.assertRaises(Exec) as done:
            self.slot.main()
        return done.exception.args[0]

    def test_the_policy_path_is_the_authority_namespace(self):
        self.assertEqual(wrapper().HOST_POLICY, str(namespace.ROOT / "policy.json"))

    def test_disabled_authority_keeps_the_slot_path(self):
        self.assertFalse(self.slot.admitted_as_root())
        kind, argv = self.run_main(["rustc", "--crate-name", "demo", "lib.rs"])
        self.assertEqual((kind, argv), ("rustc", ["rustc", "--crate-name", "demo", "lib.rs"]))
        self.assertTrue(any(Path(self.tmp.name, "locks", "build-slots").iterdir()),
                        "the disabled path still takes a slot")

    def test_an_enabled_authority_admits_an_unenclosed_compile_as_a_root(self):
        self.policy.write_text("{}")
        self.assertTrue(self.slot.admitted_as_root())
        kind, argv = self.run_main(["rustc", "--crate-name", "demo", "lib.rs"])
        self.assertEqual(kind, "admit")
        self.assertEqual(argv[1:], ["-B", str(SCRIPTS / "host-admit.py"), "--entry", "rustc", "--",
                                    "rustc", "--crate-name", "demo", "lib.rs"])
        self.assertFalse(Path(self.tmp.name, "locks").exists(), "a root takes no slot")

    def test_a_compile_inside_a_grant_keeps_the_slot_path(self):
        self.policy.write_text("{}")
        os.environ.update(GRANT)
        self.assertFalse(self.slot.admitted_as_root())
        kind, _ = self.run_main(["rustc", "--crate-name", "demo", "lib.rs"])
        self.assertEqual(kind, "rustc")

    def test_an_incomplete_inherited_grant_is_refused(self):
        self.policy.write_text("{}")
        os.environ["STORYHOOK_HOST_GRANT"] = "token"
        with self.assertRaises(SystemExit) as refused:
            self.slot.admitted_as_root()
        self.assertEqual(refused.exception.code, 125)

    def test_an_older_interpreter_is_refused_by_name_when_admission_applies(self):
        self.policy.write_text("{}")
        with mock.patch.object(sys, "version_info", (3, 9, 6)), \
                mock.patch.object(sys, "version", "3.9.6 (default)"), \
                self.assertRaises(SystemExit) as refused:
            self.slot.admitted_as_root()
        self.assertEqual(refused.exception.code, 125)

    def test_a_probe_passes_straight_through_in_every_mode(self):
        for enabled in (False, True):
            if enabled:
                self.policy.write_text("{}")
            kind, argv = self.run_main(["rustc", "-vV"])
            self.assertEqual((kind, argv), ("rustc", ["rustc", "-vV"]), f"enabled={enabled}")


if __name__ == "__main__":
    unittest.main()
