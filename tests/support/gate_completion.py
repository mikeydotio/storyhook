"""Execute production Make recipes, leg reuse and aggregation with controlled leaves."""

import json
import os
import shutil
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


class GateCompletion(unittest.TestCase):
    """Each invocation owns its Git state, control files, and progress journal."""

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="sh701-gate-", dir="/tmp")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.env = {k: v for k, v in os.environ.items() if not k.startswith("STORYHOOK_")}
        self.env.update(SH701_REAL_CARGO=shutil.which("cargo"), SH701_TRACE=str(self.root / "trace"),
                        STORYHOOK_GATE_PROGRESS=str(self.root / "progress"))
        self.write("Makefile", (ROOT / "Makefile").read_text())
        for name in ["gate-legs.sh", "leg.sh", "gate-leg-fingerprint.sh", "rust-test-targets.sh",
                     "gate-progress.sh", "activity-log.sh", "with-orphan-postlude.sh",
                     "cargo_diagnostics.py"]:
            source = ROOT / "scripts" / name
            if source.exists():
                self.write("scripts/" + name, source.read_text())
        self.write("Cargo.toml", '[package]\nname="fixture"\nversion="0.1.0"\n')
        self.write("src/lib.rs", "")
        self.write("tests/core.rs", "#[test] fn core() {}\n")
        self.write("tests/contract.rs", 'const ROOT: &str = env!("CARGO_MANIFEST_DIR");\n')
        self.write("leaf", '''#!/usr/bin/env bash
label="$1"
echo "$label" >> "$SH701_TRACE"
case ",${SH701_FAILURES:-}," in
(*,"$label",*) exit 7 ;;
esac
if [ "${SH701_SIGNAL:-}" = "$label" ]; then kill -TERM $$; fi
exit 0
''')
        self.write("bin/cargo", '''#!/usr/bin/env bash
if [ "${SH701_REAL_RUST:-}" = 1 ]; then
 case "$1" in
 (test) exec "$SH701_REAL_CARGO" "$@" ;;
 (build) ./leaf build; exec "$SH701_REAL_CARGO" "$@" ;;
 esac
fi
case "$1" in
(metadata) printf '%s\n' '{"packages":[{"targets":[{"name":"core","kind":["test"],"src_path":"tests/core.rs"},{"name":"contract","kind":["test"],"src_path":"tests/contract.rs"}]}]}'; exit 0 ;;
(fmt) exec ./leaf fmt ;;
(clippy|build)
 ./leaf "$1"; status=$?
 printf '{"reason":"build-finished","success":%s}\n' "$([ "$status" = 0 ] && echo true || echo false)"
 exit "$status" ;;
esac
exit 99
''')
        self.env["PATH"] = str(self.root / "bin") + os.pathsep + self.env["PATH"]
        for name in ["release-status", "browser-status"]:
            self.write(f"scripts/{name}.sh", "#!/bin/sh\nexit 0\n")
        self.write("scripts/run-rust-battery.sh", '#!/bin/sh\ncase "$1" in core) exec ./leaf rust-suite;; *) exec ./leaf rust-contracts;; esac\n')
        self.write("scripts/run-changed.sh", '#!/bin/sh\nexec ./leaf rust-suite\n')
        self.write("plugins/story/tests/run-tests.sh", '#!/bin/sh\nexec ./leaf plugin\n')
        self.write("scripts/run-e2e.sh", '#!/bin/sh\nexec ./leaf e2e\n')
        self.write("scripts/check-no-orphan-servers.sh", '#!/bin/sh\nexec ./leaf "orphan-$1"\n')
        self.write("scripts/gate-receipt.sh", '#!/bin/sh\nexec ./leaf "receipt-$1"\n')
        for args in [["init", "-q"], ["config", "user.email", "gate@example.test"],
                     ["config", "user.name", "Gate"], ["add", "."], ["commit", "-qm", "fixture"]]:
            subprocess.run(["git", *args], cwd=self.root, env=self.env, check=True,
                           capture_output=True)

    def write(self, path, text):
        destination = self.root / path
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_text(text)
        destination.chmod(0o755)

    def run_gate(self, target="test", failures="", signal="", flags=()):
        for name in ["trace", "progress"]:
            (self.root / name).unlink(missing_ok=True)
        return subprocess.run(["make", "--no-print-directory", *flags, target], cwd=self.root,
                              env=dict(self.env, SH701_FAILURES=failures, SH701_SIGNAL=signal),
                              capture_output=True, text=True, timeout=60)

    def trace(self):
        path = self.root / "trace"
        return path.read_text().splitlines() if path.exists() else []

    def test_every_tier_collects_multiple_failures_and_never_certifies_red(self):
        for target in ["test", "test-full", "test-changed"]:
            with self.subTest(target=target):
                result = self.run_gate(target, "fmt,rust-suite,plugin")
                self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                for leg in ["fmt", "rust-suite", "rust-contracts", "build", "plugin"]:
                    # Prior green legs may be reused across fixture invocations.
                    self.assertTrue(leg in self.trace() or f"leg {leg}: REUSED" in result.stderr,
                                    (leg, self.trace(), result.stderr))
                self.assertIn("orphan-postlude", self.trace())
                self.assertNotIn("receipt-postlude", self.trace())
                if target == "test-full":
                    self.assertIn("e2e", self.trace())

    def test_green_order_receipt_and_reuse(self):
        result = self.run_gate("test-full")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.trace(), ["orphan-preflight", "receipt-preflight", "fmt", "clippy",
                                      "rust-suite", "rust-contracts", "build", "plugin", "e2e",
                                      "orphan-postlude", "receipt-postlude"])
        second = self.run_gate("test-full")
        self.assertEqual(second.returncode, 0, second.stderr)
        self.assertEqual(self.trace(), ["orphan-preflight", "receipt-preflight",
                                      "orphan-postlude", "receipt-postlude"])

    def test_build_failure_skips_consumers_with_a_reason(self):
        result = self.run_gate("test-full", "build")
        self.assertNotEqual(result.returncode, 0)
        for leg in ["plugin", "e2e"]:
            self.assertNotIn(leg, self.trace())
            self.assertIn(f"leg {leg}: SKIPPED — dependency build failed", result.stderr)
        events = [json.loads(x) for x in (self.root / "progress").read_text().splitlines()]
        self.assertTrue(any(x.get("status") == "skipped" and x.get("dependency") == "build"
                            for x in events))
        self.assertIn("orphan-postlude", self.trace())
        self.assertNotIn("receipt-postlude", self.trace())

    def test_signal_stops_continuation_but_cleanup_still_runs(self):
        result = self.run_gate(signal="rust-suite")
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn("rust-contracts", self.trace())
        self.assertIn("orphan-postlude", self.trace())
        self.assertNotIn("receipt-postlude", self.trace())

    def test_shared_compilation_skips_dependents_but_cfg_test_failure_does_not(self):
        self.env["SH701_REAL_RUST"] = "1"
        self.write("scripts/run-rust-battery.sh", "#!/bin/sh\ncase \"$1\" in core) ./leaf rust-suite;; *) ./leaf rust-contracts;; esac\nexec python3 scripts/cargo_diagnostics.py -- cargo test --offline --no-run\n")
        for source, shared in [("pub fn broken() { missing; }", True),
                               ("#[cfg(test)] fn broken() { missing; }", False)]:
            with self.subTest(shared=shared):
                self.write("src/lib.rs", source)
                result = self.run_gate("test-full")
                self.assertNotEqual(result.returncode, 0, result.stderr)
                self.assertEqual("dependency rust-suite failed" in result.stderr, shared, result.stderr)
                self.assertEqual("plugin" in self.trace(), not shared, result.stderr)
                self.assertEqual("rust-contracts" in self.trace(), not shared, result.stderr)
                self.assertNotIn("receipt-postlude", self.trace())

    def test_accumulator_preserves_first_status_and_rejects_damaged_evidence(self):
        helper = str(self.root / "scripts/gate-legs.sh")
        for body, expected in [
            ('gate_run fmt bash -c "exit 7"; gate_run clippy bash -c "exit 9"; gate_finish', 7),
            ("gate_run rust-suite bash -c 'printf broken > \"$STORYHOOK_GATE_BUILD_OUTCOME\"'; gate_finish", 125),
        ]:
            with self.subTest(body=body):
                result = subprocess.run(["bash", "-c", '. "$1"; gate_init; ' + body, "probe", helper],
                                        cwd=self.root, env=self.env, capture_output=True, timeout=60)
                self.assertEqual(result.returncode, expected, result.stderr)

    def test_red_cleanup_prevents_receipt_even_when_all_legs_pass(self):
        result = self.run_gate(failures="orphan-postlude")
        self.assertNotEqual(result.returncode, 0, result.stderr)
        self.assertIn("plugin", self.trace())
        self.assertNotIn("receipt-postlude", self.trace())

    def test_dry_modes_never_execute_a_leaf(self):
        for flag in ["-n", "-t", "-q"]:
            for target in ["test", "test-full", "test-changed"]:
                result = self.run_gate(target, flags=[flag])
                self.assertEqual(self.trace(), [], (flag, target, result.stderr))
                if flag == "-n":
                    self.assertIn("scripts/run-changed.sh" if target == "test-changed"
                                  else "scripts/run-rust-battery.sh core", result.stdout)
                    self.assertIn("scripts/run-rust-battery.sh contracts", result.stdout)


if __name__ == "__main__":
    unittest.main()
