#!/usr/bin/env python3
"""Exercise selective gate provenance with real Make, Git and receipt scripts."""
import argparse
import json
import os
from pathlib import Path
import subprocess
import tempfile


def executable(path, text):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text)
    path.chmod(0o755)


class Fixture:
    def __init__(self, source, override):
        self.temp = tempfile.TemporaryDirectory(prefix="story-selective-receipt-", dir="/tmp")
        self.root = Path(self.temp.name)
        self.log = self.root / ".git/executions"
        self.env = {
            "PATH": str(self.root / "bin") + ":" + os.environ["PATH"],
            "HOME": str(self.root / "home"),
            "GIT_CONFIG_GLOBAL": "/dev/null", "GIT_CONFIG_NOSYSTEM": "1",
            "GIT_TERMINAL_PROMPT": "0", "LC_ALL": "C", "TZ": "UTC",
            "STORYHOOK_TEST_LOG": str(self.log),
        }
        (self.root / "home").mkdir()
        for name in [
            "Makefile", "scripts/gate-receipt.sh", "scripts/tree-receipt.sh",
            "scripts/tracked-tree.sh", "scripts/leg.sh",
            "scripts/gate-leg-fingerprint.sh", "scripts/gate-legs.sh",
            "scripts/cargo_diagnostics.py", "scripts/activity-log.sh",
            "scripts/gate-progress.sh", "scripts/with-orphan-postlude.sh",
            "scripts/select-tests.sh", "scripts/run-changed.sh",
            "scripts/run-rust-battery.sh", "scripts/rust-test-targets.sh",
        ]:
            target = (override / name) if override and (override / name).is_file() else source / name
            destination = self.root / name
            destination.parent.mkdir(parents=True, exist_ok=True)
            destination.symlink_to(target)
        # All behavior under review remains production code. Expensive leaf
        # checks are controlled executors; none starts Cargo, a provider or daemon.
        executable(self.root / "scripts/run-tests.sh", """#!/bin/bash
set -eu
printf '%s\n' "$*" >> "$STORYHOOK_TEST_LOG"
exit "${FIXTURE_TEST_EXIT:-0}"
""")
        executable(self.root / "scripts/check-no-orphan-servers.sh", """#!/bin/bash
set -eu
if [ "${1:-}" = postlude ] && [ "${FIXTURE_DROP_TIER:-0}" = 1 ]; then
  rm -f .git/storyhook-changed-tier-args
fi
""")
        for name in ["scripts/release-status.sh", "scripts/browser-status.sh",
                     "plugins/story/tests/run-tests.sh", ".githooks/pre-push"]:
            executable(self.root / name, "#!/bin/bash\nexit 0\n")
        metadata = {"packages": [{"targets": [
            {"name": name, "kind": ["test"], "src_path": str(self.root / f"tests/{name}.rs")}
            for name in ["alpha", "beta", "scanner"]
        ]}]}
        executable(self.root / "bin/cargo", "#!/bin/bash\nif [ \"${1:-}\" = metadata ]; then\ncat <<'JSON'\n"
                   + json.dumps(metadata) + "\nJSON\nfi\n")
        (self.root / "src").mkdir()
        (self.root / "src/alpha.rs").write_text("fn alpha() {}\n")
        (self.root / "tests").mkdir()
        for name in ["alpha", "beta"]:
            (self.root / f"tests/{name}.rs").write_text("#[test] fn fixture() {}\n")
        (self.root / "tests/scanner.rs").write_text('const ROOT: &str = env!("CARGO_MANIFEST_DIR");\n')
        (self.root / "scripts/test-impact.tsv").write_text("scanner\ttests/scanner.rs\n")
        self.run(["git", "init", "-q", "-b", "main"])
        self.run(["git", "config", "user.name", "Selective Fixture"])
        self.run(["git", "config", "user.email", "selective@example.test"])
        self.run(["git", "add", "."])
        self.run(["git", "commit", "-qm", "fixture baseline"])
        self.base = self.run(["git", "rev-parse", "HEAD^{tree}"]).stdout.strip()
        # The production receipt writer records a fixture's successful baseline;
        # a fixture coverage map supplies only the instrumented endpoint data.
        self.run(["bash", "scripts/gate-receipt.sh", "preflight"])
        self.run(["bash", "scripts/gate-receipt.sh", "postlude", "gate"])
        maps = self.root / ".git/storyhook/coverage-maps"
        maps.mkdir(parents=True)
        (maps / self.base).write_text("alpha\tsrc/alpha.rs\n")
        (self.root / "src/alpha.rs").write_text("fn alpha() { let changed = 1; }\n")
        self.tree = self.run(["bash", "scripts/tracked-tree.sh"]).stdout.strip()

    def run(self, args, expected=0, extra=None):
        env = self.env | (extra or {})
        out = subprocess.run(args, cwd=self.root, env=env, text=True,
                             stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=30)
        if expected is not None:
            assert out.returncode == expected, (args, out.returncode, out.stdout, out.stderr)
        return out

    def tier(self):
        receipt = self.root / ".git/storyhook/gate-receipts" / self.tree
        if not receipt.exists():
            return None
        return next(line[5:] for line in receipt.read_text().splitlines() if line.startswith("tier "))

    def executions(self):
        return self.log.read_text().splitlines() if self.log.exists() else []

    def close(self):
        self.temp.cleanup()


def check(source, override, scenario):
    f = Fixture(source, override)
    try:
        if scenario == "all":
            (f.root / ".git/storyhook/coverage-maps" / f.base).unlink()
        before = f.run(["bash", "scripts/select-tests.sh"]).stdout
        assert ("\nALL\n" in before) == (scenario == "all"), before
        if scenario == "missing":
            out = f.run(["make", "--no-print-directory", "test-changed"],
                        expected=None, extra={"FIXTURE_DROP_TIER": "1"})
            assert out.returncode != 0, "missing selective provenance certified a successful gate"
            assert f.tier() is None, "missing selective provenance minted a receipt"
            print("PASS: missing provenance refuses certification")
            return
        f.run(["make", "--no-print-directory", "test-changed"])
        expected = "gate" if scenario == "all" else "changed"
        assert f.tier() == expected, f.tier()
        first = f.executions()
        out = f.run(["make", "--no-print-directory", "test-changed"])
        assert f.tier() == expected, f"cached {scenario} became {f.tier()}; expected {expected}"
        assert f.executions() == first, "unchanged selected checks unnecessarily executed again"
        assert "REUSED" in out.stderr, out.stderr
        if scenario == "all":
            f.run(["bash", "scripts/leg.sh", "--reuse", "rust-suite", "--",
                   "bash", "scripts/run-rust-battery.sh", "core"])
            assert f.executions() == first, "ALL did not preserve ordinary core-battery evidence"
        print(f"PASS: {scenario} remains {expected}; unchanged tests reuse evidence")
    finally:
        f.close()


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("source", type=Path)
    parser.add_argument("scenario", choices=["subset", "all", "missing"])
    parser.add_argument("--override", type=Path)
    args = parser.parse_args()
    check(args.source, args.override, args.scenario)
