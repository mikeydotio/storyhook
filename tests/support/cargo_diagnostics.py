"""Provoke SH-685's production collector with wire data and real Cargo."""

import importlib.util
import io
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time
import unittest

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts/cargo_diagnostics.py"
ENV_KEY = "STORYHOOK_COMPILER_DIAGNOSTICS"
sys.dont_write_bytecode = True


def message(text, level="error", code="E0425"):
    """Make Cargo wire data, never a replacement collector."""
    return {"reason": "compiler-message", "message": {
        "level": level, "message": text, "code": {"code": code} if code else None,
        "rendered": f"{level}[{code}]: {text}\n" if code else f"{level}: {text}\n",
    }}


def wire(record):
    """Encode a Cargo record with its real line delimiter."""
    return (json.dumps(record, ensure_ascii=False) + "\n").encode()


class CollectorContracts(unittest.TestCase):
    """Use isolated artifacts for every collector invocation."""

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="sh685-", dir="/tmp")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.artifact = self.root / "diagnostics.jsonl"
        self.artifact.touch()
        self.env = dict(os.environ, **{ENV_KEY: str(self.artifact)})
        spec = importlib.util.spec_from_file_location("cargo_diagnostics", SCRIPT)
        self.module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(self.module)

    def records(self):
        return [json.loads(line) for line in self.artifact.read_text().splitlines()]

    def invoke(self, command, **kwargs):
        return subprocess.run([sys.executable, "-B", str(SCRIPT), *command],
                              cwd=self.root, env=self.env, capture_output=True,
                              timeout=60, **kwargs)

    def test_fragmented_records_and_test_owned_json(self):
        output = io.BytesIO()
        collector = self.module.Collector(self.artifact, output)
        real = message("missing café " + "x" * 20000)
        data = (b"macro chatter\xff\n" + wire(real)
                + wire({"reason": "compiler-artifact", "fresh": True})
                + wire({"reason": "build-finished", "success": False})
                + b"error: intentional test error\n" + wire(message("test-owned JSON")))
        for start in range(0, len(data), 7):
            collector.feed(data[start:start + 7])
        collector.finish()
        self.assertEqual(self.records(), [real])
        self.assertIn(real["message"]["rendered"].encode(), output.getvalue())
        self.assertIn(b"macro chatter\xff", output.getvalue())
        self.assertIn(wire(message("test-owned JSON")), output.getvalue())

    def test_each_invocation_restarts_the_boundary_and_summary_deduplicates(self):
        for _ in range(2):
            collector = self.module.Collector(self.artifact, io.BytesIO())
            collector.feed(wire(message("genuine")) + wire(message("warning", "warning"))
                           + wire({"reason": "build-finished", "success": False}))
            collector.finish()
        result = self.invoke(["--summarize", str(self.artifact)])
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.decode().splitlines(), ["error[E0425]: genuine"])

    def test_malformed_known_records_fail_loudly_and_unknown_output_survives(self):
        for record in [{"reason": "compiler-message", "message": None},
                       {"reason": "compiler-message", "message": {"level": "error"}},
                       {"reason": "build-finished", "success": "false"}]:
            with self.subTest(record=record):
                collector = self.module.Collector(self.artifact, io.BytesIO())
                with self.assertRaises(ValueError):
                    collector.feed(wire(record))
        output = io.BytesIO()
        collector = self.module.Collector(self.artifact, output)
        data = b'{not JSON}\n[1,2]\n{"reason":"future-cargo-record"}\nfinal'
        collector.feed(data)
        collector.finish()
        self.assertEqual(output.getvalue(), data)

    def test_corrupt_or_missing_artifact_is_not_an_empty_success(self):
        self.artifact.write_text('{"reason":"compiler-message","message":null}\n')
        for path in [self.artifact, self.root / "absent"]:
            result = self.invoke(["--summarize", str(path)])
            self.assertNotEqual(result.returncode, 0)
            self.assertIn(b"compiler diagnostics", result.stderr)

    def build_probe(self, code, artifact=None):
        driver = ("import sys; sys.path.insert(0, sys.argv[1]); "
                  "from cargo_diagnostics import run_build; "
                  "sys.exit(run_build([sys.executable, '-c', sys.argv[3]], sys.argv[2]))")
        return subprocess.run([sys.executable, "-B", "-c", driver, str(SCRIPT.parent),
                               str(artifact or self.artifact), code],
                              env=self.env, capture_output=True, timeout=15)

    def test_only_stdout_is_collected_and_child_environment_is_isolated(self):
        genuine = wire(message("stdout compiler"))
        fake = wire(message("stderr test impostor"))
        code = (f"import os,sys; assert {ENV_KEY!r} not in os.environ; "
                f"sys.stdout.buffer.write({genuine!r}); sys.stderr.buffer.write({fake!r}); "
                "print('{\"reason\":\"build-finished\",\"success\":false}'); sys.exit(7)")
        result = self.build_probe(code)
        self.assertEqual(result.returncode, 7, result.stderr)
        self.assertEqual(self.records(), [message("stdout compiler")])
        self.assertIn(fake, result.stderr)

    def test_unavailable_artifact_prevents_command_start(self):
        marker = self.root / "started"
        result = self.build_probe(f"open({str(marker)!r}, 'w').close()", self.root / "missing/a")
        self.assertEqual(result.returncode, 125, result.stderr)
        self.assertFalse(marker.exists())
        self.assertIn(b"compiler diagnostics", result.stderr)

    def test_signals_and_failures_without_json_preserve_status(self):
        result = self.build_probe("import os,signal; os.kill(os.getpid(), signal.SIGTERM)")
        self.assertEqual(result.returncode, 128 + signal.SIGTERM, result.stderr)
        result = self.build_probe("import sys; print('error: resolver failed', file=sys.stderr); sys.exit(9)")
        self.assertEqual(result.returncode, 9, result.stderr)
        self.assertEqual(self.records(), [])
        self.assertIn(b"resolver failed", result.stderr)
        result = self.build_probe("pass")
        self.assertEqual(result.returncode, 125, result.stderr)
        self.assertIn(b"build-finished", result.stderr)

    def test_detached_stdout_holder_does_not_delay_completion(self):
        release = self.root / "release"
        finished = self.root / "finished"
        pidfile = self.root / "child.pid"
        code = f'''import os,time
pid = os.fork()
if pid == 0:
    os.close(2)
    while not os.path.exists({str(release)!r}): time.sleep(0.01)
    open({str(finished)!r}, 'w').close()
    os._exit(0)
open({str(pidfile)!r}, 'w').write(str(pid))
print('{{"reason":"build-finished","success":true}}', flush=True)
'''
        try:
            result = self.build_probe(code)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertFalse(finished.exists())
        finally:
            release.touch()
            deadline = time.monotonic() + 5
            while not finished.exists() and time.monotonic() < deadline:
                time.sleep(0.01)
            if not finished.exists() and pidfile.exists():
                os.kill(int(pidfile.read_text()), signal.SIGKILL)
            self.assertTrue(finished.exists(), "fixture child did not acknowledge release")

    def package(self, name, source):
        project = self.root / name
        (project / "src").mkdir(parents=True)
        (project / "Cargo.toml").write_text(
            f'[package]\nname="{name}"\nversion="0.1.0"\nedition="2021"\n[workspace]\n')
        (project / "src/lib.rs").write_text(source)
        return project / "Cargo.toml"

    def test_real_cargo_errors_are_collected_on_repeated_invocations(self):
        manifest = self.package("broken", "pub fn broken() { absent_value; }\n")
        for subcommand in ["check", "test", "build", "clippy"]:
            before = len(self.records())
            result = self.invoke(["--", "cargo", subcommand, "--offline", "--manifest-path", str(manifest)])
            self.assertEqual(result.returncode, 101, result.stderr)
            self.assertIn(b"E0425", result.stdout + result.stderr)
            self.assertTrue(any(r["message"].get("code") and r["message"]["code"]["code"] == "E0425"
                                for r in self.records()[before:]), subcommand)

    def test_clippy_keeps_arguments_after_the_cargo_separator(self):
        manifest = self.package("warnings", "pub fn warning() { let unused = 1; }\n")
        result = self.invoke(["--", "cargo", "clippy", "--offline", "--manifest-path", str(manifest),
                              "--", "-D", "warnings"])
        self.assertEqual(result.returncode, 101, result.stderr)
        self.assertTrue(any(r["message"]["level"] == "error" for r in self.records()))

    def test_doctest_failures_and_expected_compile_failures_remain_cargo_owned(self):
        manifest = self.package("documentation", "")
        for fence, expected in [("", 101), ("compile_fail", 0)]:
            (manifest.parent / "src/lib.rs").write_text(
                f'/// ```{fence}\n/// absent_doc_value;\n/// ```\npub fn documented() {{}}\n')
            result = self.invoke(["--", "cargo", "test", "--offline", "--doc", "--manifest-path", str(manifest)])
            self.assertEqual(result.returncode, expected, result.stderr + result.stdout)
            self.assertEqual(self.records(), [])

    def test_no_run_does_not_execute_tests_and_disabled_collection_is_transparent(self):
        manifest = self.package("norun", '#[test] fn cannot_run() { panic!("must not execute"); }')
        result = self.invoke(["--", "cargo", "test", "--offline", "--no-run", "--manifest-path", str(manifest)])
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn(b"test cannot_run", result.stdout + result.stderr)
        self.env.pop(ENV_KEY)
        result = self.invoke(["--", "sh", "-c", "printf raw-output; exit 23"])
        self.assertEqual(result.returncode, 23)
        self.assertEqual(result.stdout, b"raw-output")

    def test_real_test_execution_preserves_selection_order_and_nested_isolation(self):
        manifest = self.package("passing", "")
        tests = manifest.parent / "tests"
        tests.mkdir()
        broken = self.package("nested", "pub fn broken() { nested_absent; }\n")
        source = r'''
use std::io::Write;
#[test] fn selected() {
    assert!(std::env::var_os("STORYHOOK_COMPILER_DIAGNOSTICS").is_none());
    std::io::stdout().write_all(b"error: intentional error\n").unwrap();
    std::io::stdout().write_all(b"{\"reason\":\"compiler-message\",\"message\":{\"level\":\"error\",\"message\":\"impostor\"}}\n").unwrap();
    let status = std::process::Command::new("python3")
        .args(["-B", &std::env::var("SH685_ADAPTER").unwrap(), "--", "cargo", "check", "--offline", "--manifest-path", &std::env::var("SH685_NESTED").unwrap()])
        .status().unwrap();
    assert!(!status.success());
}
#[test] fn excluded() { panic!("filter lost"); }
'''
        for name in ["first", "second"]:
            (tests / f"{name}.rs").write_text(source)
        self.env.update(SH685_ADAPTER=str(SCRIPT), SH685_NESTED=str(broken))
        capture = self.root / "combined.log"
        progress = self.root / "progress.jsonl"
        self.env["STORYHOOK_GATE_PROGRESS"] = str(progress)
        result = subprocess.run([sys.executable, "-B", str(ROOT / "scripts/activity-run.py"),
                                 "--capture", str(capture), "--test-progress", "rust", "fixture", "--",
                                 sys.executable, "-B", str(SCRIPT), "--", "cargo", "test", "--offline",
                                 "--manifest-path", str(manifest), "--", "--exact", "selected"],
                                cwd=self.root, env=self.env, capture_output=True, timeout=60)
        self.assertEqual(result.returncode, 0, result.stderr + result.stdout)
        parsed = subprocess.run([sys.executable, "-B", str(ROOT / "scripts/test_output.py")],
                                input=capture.read_bytes(), capture_output=True, check=True)
        self.assertEqual(parsed.stdout.decode().splitlines(),
                         ["first\tselected\tPASS", "second\tselected\tPASS"])
        self.assertIn(b"intentional error", capture.read_bytes())
        self.assertIn(b"nested_absent", capture.read_bytes())
        self.assertEqual(self.records(), [])
        self.assertEqual(sum(json.loads(x).get("kind") == "case" for x in progress.read_text().splitlines()), 2)


if __name__ == "__main__":
    unittest.main()
