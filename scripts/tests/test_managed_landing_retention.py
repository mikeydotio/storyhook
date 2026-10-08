"""SH-871 local orchestration regressions; never a live GitHub policy test.

Only the binary transport boundary is doubled. Production landing scripts,
machine lock, isolated merge computation and receipt writer use scratch Git.
Fixture receipts describe synthetic test inputs, never this repository's gate.
Every command owns a private session until its leader and descendants settle.
Timeout preserves the scratch root and aborts the suite. The Rust owner closes
a lifetime pipe before waiting for this Python owner to perform bounded cleanup.
These fixed helpers do not escape sessions; this is not escaped-process custody.
"""

import json
import os
from pathlib import Path
import shlex
import shutil
import select
import signal
import subprocess
import sys
import tempfile
import time
import unittest

from load_grace import contention, patience

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts"))
from host_admission import native

WATCH_OWNER = False
CANCELLED = False
CUSTODY_UNCERTAIN = False
OWNER = "11111111-2222-4333-8444-555555555555"
ATTEMPT = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee"
OTHER = "99999999-2222-4333-8444-555555555555"
IDENTITY = "github.com/fixture/managed"


class FixtureAborted(KeyboardInterrupt):
    """Unittest must not start another case after lost or uncertain ownership."""


def owner_cancelled():
    if CANCELLED or CUSTODY_UNCERTAIN:
        return True
    return (WATCH_OWNER and select.select([sys.stdin], [], [], 0)[0]
            and os.read(sys.stdin.fileno(), 1) == b"")


def session_members(session, boot):
    """Fresh native identities; missing members grant no signal authority."""
    found = []
    for pid in native.session_members(session):
        try:
            value = native.process(pid, boot)
        except ProcessLookupError:
            continue
        if value["session"] == session and value["live"]:
            found.append(value)
    return found


def bounded(args, cwd, env, root, deadline, *, own_session=True, cancel_when_exited=None):
    """File capture plus a waitable session leader, including set-m descendants."""
    global CUSTODY_UNCERTAIN
    remaining = min(patience(30, contention()), deadline - time.monotonic())
    if remaining <= 0 or owner_cancelled():
        (root / "preserve").touch()
        raise FixtureAborted("fixture deadline elapsed or owner cancelled before spawn")
    if own_session and not all(hasattr(os, name) for name in
                               ("waitid", "P_PID", "WEXITED", "WNOWAIT", "WNOHANG")):
        (root / "preserve").touch()
        raise FixtureAborted("Python lacks pinned-child waitid support; no child started")
    deadline = time.monotonic() + remaining
    boot = native.boot_identity() if own_session else None
    with tempfile.TemporaryFile(dir=root) as out, tempfile.TemporaryFile(dir=root) as err:
        child = subprocess.Popen(args, cwd=cwd, env=env, stdin=subprocess.DEVNULL,
                                 stdout=out, stderr=err, start_new_session=own_session)
        failure = None
        try:
            if not own_session:
                # Transport Git belongs to the top-level fixture command's
                # session. Do not create an untracked nested session here.
                child.wait(timeout=remaining)
            else:
                cleanup_deadline = None
                while True:
                    # WNOWAIT pins the session ID even when macOS can no longer
                    # inspect the exact zombie through proc_pidinfo/getsid.
                    exited = os.waitid(os.P_PID, child.pid, os.WEXITED | os.WNOHANG | os.WNOWAIT)
                    exited = exited is not None and exited.si_pid == child.pid
                    detector_cancel = exited and cancel_when_exited is not None and cancel_when_exited(child.pid)
                    if failure is None and (owner_cancelled() or detector_cancel or time.monotonic() >= deadline):
                        failure = "fixture command deadline or owner cancellation"
                        (root / "preserve").touch()
                        cleanup_deadline = time.monotonic() + patience(5, contention())
                    members = session_members(child.pid, boot)
                    if exited and not members:
                        # A second census AFTER positive direct-root exit proof
                        # closes an enumerate/fork/exit window before reaping.
                        if not session_members(child.pid, boot):
                            child.wait(timeout=max(0.01, (cleanup_deadline or deadline) - time.monotonic()))
                            break
                    if failure is not None:
                        for value in members:
                            try:
                                now = native.process(value["pid"], boot)
                                if (now["start"] == value["start"] and now["boot"] == value["boot"]
                                        and now["session"] == child.pid and now["live"]):
                                    os.kill(now["pid"], signal.SIGKILL)
                            except ProcessLookupError:
                                continue
                        if time.monotonic() >= cleanup_deadline:
                            raise FixtureAborted("owned session did not settle; scratch root retained")
                    # Observation cadence, not a workload completion allowance.
                    time.sleep(0.01)
        except BaseException as error:
            CUSTODY_UNCERTAIN = True
            (root / "preserve").touch()
            if not own_session:
                child.kill()  # Unreaped direct Git only; outer owner still holds the session.
                child.wait(timeout=patience(2, contention()))
            raise FixtureAborted(f"fixture custody is uncertain: {error}") from error
        if failure is not None:
            raise FixtureAborted(failure + "; owned session settled, root retained")
        out.seek(0)
        err.seek(0)
        return subprocess.CompletedProcess(args, child.returncode,
                                           out.read().decode(), err.read().decode())


def transport(root, argv):
    """Allowlisted local transport for the exact private fixture, never a URL."""
    config = json.loads((root / "transport.json").read_text())
    repo, bare = Path(config["repo"]), Path(config["bare"])
    assert repo.parent == root and bare.parent == root
    assert argv[:1] == ["github"] and len(argv) >= 6
    kind = argv[1]
    assert argv[2:6] == ["--checkout", str(repo), "--authority", str(repo)]
    rest = argv[6:]
    if rest[:1] == ["--expected"]:
        assert rest[1] == IDENTITY
        rest = rest[2:]
    if kind == "resolve":
        assert not rest
        print(json.dumps({"identity": {"host": "github.com", "owner": "fixture", "repo": "managed"}}))
        return
    assert rest[:1] == ["--"]
    args = rest[1:]
    with (root / "calls.jsonl").open("a") as calls:
        calls.write(json.dumps([kind, *args]) + "\n")
    branch, head = config["branch"], config["head"]
    if kind == "git":
        allowed = [
            ["ls-remote", "--symref", "origin", "HEAD"],
            ["ls-remote", "--heads", "origin", "refs/heads/" + branch],
            ["fetch", "-q", "origin", "+refs/heads/main:refs/remotes/origin/main",
             "+refs/pull/1/head:refs/remotes/origin/pr/1"],
            ["fetch", "-q", "origin", "+refs/heads/main:refs/remotes/origin/main"],
            ["fetch", "-q", "origin", "+refs/heads/main:refs/storyhook/landing/" + head],
            ["push", "-q", "origin", ":refs/heads/" + branch],
        ]
        assert args in allowed, args
        # Pin the fixture path on each transport command, even if local origin
        # configuration were accidentally changed by the harness.
        args = [str(bare) if arg == "origin" else arg for arg in args]
        os.chdir(repo)
        os.execvp("git", ["git", *args])
    if kind == "exec":
        assert args[:3] == ["pr", "view", "1"] and args[3:4] == ["--json"] and len(args) == 5
        merged = config.get("merged")
        print(json.dumps({"number": 1, "state": "MERGED" if merged else "OPEN",
                          "isDraft": False, "isCrossRepository": False,
                          "baseRefName": "main", "headRefName": branch, "headRefOid": head,
                          "mergedAt": "2026-01-01T00:00:00Z" if merged else None,
                          "mergeCommit": {"oid": merged} if merged else None}))
        return
    assert kind == "merge" and args == ["1", head], (kind, args)
    assert not config.get("merged"), "the production caller retried the merge"
    if config.get("merge_reply") == "refused":
        print('{"result":"refused","status":403}')
        return
    if config.get("merge_reply") == "unknown":
        raise SystemExit("synthetic lost response; no outcome proof")
    deadline = time.monotonic() + patience(20, contention())
    prefix = ["git", "--git-dir", str(bare), "-c", "user.name=Fixture",
              "-c", "user.email=fixture@example.invalid"]
    result = bounded([*prefix, "commit-tree", config["tree"], "-p", config["base"],
                      "-p", head, "-m", "Synthetic local transport merge"],
                     repo, os.environ.copy(), root, deadline, own_session=False)
    assert result.returncode == 0, result.stderr
    merged = result.stdout.strip()
    result = bounded([*prefix, "update-ref", "refs/heads/main", merged, config["base"]],
                     repo, os.environ.copy(), root, deadline, own_session=False)
    assert result.returncode == 0, result.stderr
    config["merged"] = merged
    (root / "transport.json").write_text(json.dumps(config))
    print('{"result":"accepted"}')


class ManagedLandingRetention(unittest.TestCase):
    def setUp(self):
        self.root = Path(tempfile.mkdtemp(prefix="sh871-retention-", dir="/tmp")).resolve()
        self.addCleanup(self.cleanup)
        self.deadline = time.monotonic() + patience(120, contention())
        self.repo = self.root / "repo"
        self.repo.mkdir()
        self.bare = self.root / "origin.git"
        self.marker = self.root / ("landing-" + ATTEMPT + ".attempted")
        self.branch = "storyhook/integration/" + OWNER
        self.wrapper = self.root / "fixture-story"
        self.wrapper.write_text("#!/bin/sh\nexec " + shlex.join([
            sys.executable, "-B", str(Path(__file__).resolve()), "--transport", str(self.root)
        ]) + ' "$@"\n')
        self.wrapper.chmod(0o700)
        self.git("init", "-q", "--template=", "-b", "main")
        self.git("config", "user.name", "Fixture")
        self.git("config", "user.email", "fixture@example.invalid")
        for field, value in [("name", "Fixture"), ("email", "fixture@example.invalid"),
                             ("role", "both"), ("reason", "Isolated local Git fixture")]:
            self.git("config", "storyhookIdentity.fixture." + field, value)
        (self.repo / "base").write_text("base\n")
        self.git("add", "base")
        self.git("commit", "-qm", "base")
        self.base = self.git("rev-parse", "HEAD").stdout.strip()
        (self.repo / "new").write_text("head\n")
        self.git("add", "new")
        self.git("commit", "-qm", "head")
        self.head = self.git("rev-parse", "HEAD").stdout.strip()
        self.tree = self.git("rev-parse", "HEAD^{tree}").stdout.strip()
        self.git("init", "-q", "--bare", "--template=", "-b", "main", str(self.bare))
        self.git("remote", "add", "origin", str(self.bare))
        self.git("push", "-q", "origin", self.base + ":refs/heads/main",
                 self.head + ":refs/heads/" + self.branch, self.head + ":refs/pull/1/head")
        (self.repo / ".githooks").symlink_to(ROOT / ".githooks", target_is_directory=True)
        (self.repo / "scripts").symlink_to(ROOT / "scripts", target_is_directory=True)
        (self.root / "transport.json").write_text(json.dumps({
            "repo": str(self.repo), "bare": str(self.bare), "branch": self.branch,
            "base": self.base, "head": self.head, "tree": self.tree,
        }))

    def cleanup(self):
        if (self.root / "preserve").exists():
            print("preserved uncertain fixture root: " + str(self.root), file=sys.stderr)
        else:
            shutil.rmtree(self.root)

    def command(self, *args, extra=None):
        # Start without credential/Git routing inheritance, then apply the
        # canonical environment contract before adding this fixture's transport.
        env = {"PATH": os.environ["PATH"], "LC_ALL": "C"}
        values = {"GIT_CONFIG_GLOBAL": "/dev/null", "GIT_CONFIG_SYSTEM": "/dev/null",
                  "GIT_CONFIG_NOSYSTEM": "1", "GIT_TERMINAL_PROMPT": "0",
                  "STORYHOOK_LOCK_DIR": str(self.root / "locks"),
                  "STORY_BIN": str(self.wrapper), "STORYHOOK_PYTHON": sys.executable}
        values.update(extra or {})
        return bounded([
            "bash", "-c", 'source "$1/scripts/test-env.sh"; storyhook_isolate --home "$2"; shift 2; exec "$@"',
            "sh871-retention", str(ROOT), str(self.root / "environment"), "env",
            *[key + "=" + value for key, value in values.items()], *map(str, args)
        ], self.repo, env, self.root, self.deadline)

    def git(self, *args):
        result = self.command("git", *args)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        return result

    def certify_fixture_tree(self):
        # Synthetic receipt only inside this disposable repository. It exercises
        # the real writer/reader boundary; no test suite is claimed to have run.
        for phase in ["preflight", "postlude"]:
            result = self.command("bash", ROOT / "scripts/gate-receipt.sh", phase)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def land(self, managed=True, mode="attempt", owner=OWNER, tree=None, marker=None, trailing=()):
        args = ["--managed-integration", owner, ATTEMPT] if managed else []
        return self.command("bash", ROOT / "scripts/landing-intent.sh", *args, mode, "1",
                            self.head, tree or self.tree, marker or self.marker, *trailing)

    def calls(self):
        path = self.root / "calls.jsonl"
        return [json.loads(line) for line in path.read_text().splitlines()] if path.exists() else []

    def mutations(self):
        return [call for call in self.calls() if call[0] == "merge" or call[:2] == ["git", "push"]]

    def branch_tip(self):
        return self.git("ls-remote", "--heads", "origin", "refs/heads/" + self.branch).stdout.strip()

    def test_managed_landing_retains_exact_owned_branch_after_certified_merge(self):
        self.certify_fixture_tree()
        result = self.land()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout)["result"], "merged", result.stdout)
        self.assertEqual(self.mutations(), [["merge", "1", self.head]])
        self.assertEqual(self.branch_tip().split()[0], self.head)
        self.assertEqual(self.marker.read_text().strip(), self.head + " " + self.tree)

    def test_ordinary_landing_still_deletes_remote_source_branch(self):
        self.certify_fixture_tree()
        result = self.land(managed=False)
        self.assertEqual(json.loads(result.stdout)["result"], "merged", result.stdout + result.stderr)
        self.assertEqual(self.mutations(), [["merge", "1", self.head],
                         ["git", "push", "-q", "origin", ":refs/heads/" + self.branch]])
        self.assertEqual(self.branch_tip(), "")

    def test_owner_branch_mismatch_never_merges_or_deletes(self):
        self.certify_fixture_tree()
        result = self.land(owner=OTHER)
        self.assertEqual(json.loads(result.stdout)["result"], "uncertain", result.stdout)
        self.assertEqual(self.mutations(), [])
        self.assertFalse(self.marker.exists())
        self.assertTrue(self.branch_tip())

    def test_managed_landing_requires_gate_receipt_and_exact_tree(self):
        for certified, tree in [(False, self.tree), (True, "0" * 40)]:
            with self.subTest(certified=certified):
                if certified:
                    self.certify_fixture_tree()
                result = self.land(tree=tree)
                self.assertEqual(json.loads(result.stdout)["result"], "not-attempted", result.stdout)
                self.assertEqual(self.mutations(), [])
                self.assertFalse(self.marker.exists())
                self.assertTrue(self.branch_tip())

    def test_managed_recovery_never_retries_merge_or_deletes_branch(self):
        result = self.land(mode="recover")
        self.assertEqual(json.loads(result.stdout)["result"], "uncertain", result.stdout)
        self.assertEqual(self.mutations(), [])
        self.certify_fixture_tree()
        result = self.land()
        self.assertEqual(json.loads(result.stdout)["result"], "merged", result.stdout)
        before = self.mutations()
        result = self.land(mode="recover")
        self.assertEqual(json.loads(result.stdout)["result"], "merged", result.stdout)
        self.assertEqual(self.mutations(), before)
        self.assertTrue(self.branch_tip())

    def test_managed_refusal_retains_exact_causal_receipt_without_cleanup(self):
        self.certify_fixture_tree()
        config = json.loads((self.root / "transport.json").read_text())
        config["merge_reply"] = "refused"
        (self.root / "transport.json").write_text(json.dumps(config))
        for mode in ["attempt", "recover"]:
            result = self.land(mode=mode)
            self.assertEqual(json.loads(result.stdout)["result"], "refused", result.stdout)
        self.assertEqual(self.mutations(), [["merge", "1", self.head]])
        self.assertEqual(json.loads(Path(str(self.marker) + ".refused").read_text()),
                         {"version": 1, "head": self.head, "tree": self.tree, "number": "1", "status": 403})
        self.assertTrue(self.branch_tip())

    def test_managed_unknown_outcome_never_fabricates_refusal_or_retries(self):
        self.certify_fixture_tree()
        config = json.loads((self.root / "transport.json").read_text())
        config["merge_reply"] = "unknown"
        (self.root / "transport.json").write_text(json.dumps(config))
        for mode in ["attempt", "recover"]:
            result = self.land(mode=mode)
            self.assertEqual(json.loads(result.stdout)["result"], "uncertain", result.stdout)
        self.assertEqual(self.mutations(), [["merge", "1", self.head]])
        self.assertTrue(self.marker.exists())
        self.assertFalse(Path(str(self.marker) + ".refused").exists())
        self.assertTrue(self.branch_tip())

    def test_managed_protocol_rejects_malformed_identity_marker_skipped_and_flags(self):
        for changes in [{"owner": "invalid"}, {"marker": self.root / "other.attempted"},
                        {"trailing": ("skipped-attempt",)}, {"mode": "--unknown"}]:
            with self.subTest(changes=changes):
                result = self.land(**changes)
                self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertEqual(self.mutations(), [])
                self.assertFalse(self.marker.exists())
        for flag in ["--managed-unknown", "--locked-managed", "--merge-managed"]:
            result = self.command("bash", ROOT / "scripts/land-pr.sh", flag)
            self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertEqual(self.mutations(), [])

    def test_cancellation_settles_descendant_in_another_group_before_return(self):
        marker = self.root / "owned-descendant.json"
        descendant = (
            "import json,os,pathlib,time;"
            f"p=pathlib.Path({str(marker)!r});"
            "p.with_suffix('.tmp').write_text(json.dumps({'pid':os.getpid(),'session':os.getsid(0)}));"
            "p.with_suffix('.tmp').replace(p);"
            "time.sleep(600)"
        )
        leader = "import subprocess,sys;subprocess.Popen([sys.executable,'-c'," + repr(descendant) + "],process_group=0)"
        witness = {}
        def cancel_after_exact_exit(pid):
            # The helper supplies this direct-child identity only after its own
            # positive WNOWAIT observation. Independently retain the same proof.
            observed = os.waitid(os.P_PID, pid, os.WEXITED | os.WNOHANG | os.WNOWAIT)
            if marker.exists() and observed is not None and observed.si_pid == pid:
                witness.update(leader=pid, status=observed.si_status)
                return True
            return False
        with self.assertRaisesRegex(FixtureAborted, "owned session settled"):
            bounded([sys.executable, "-c", leader], self.repo, {"PATH": os.environ["PATH"]},
                    self.root, self.deadline, cancel_when_exited=cancel_after_exact_exit)
        self.assertTrue(marker.exists(), "the detector must observe actual descendant readiness")
        descendant = json.loads(marker.read_text())
        pid = descendant["pid"]
        self.assertEqual(witness.get("status"), 0, "cancellation must follow exact successful leader exit")
        self.assertEqual(witness.get("leader"), descendant["session"])
        self.assertNotEqual(witness["leader"], pid)
        try:
            value = native.process(pid, native.boot_identity())
        except ProcessLookupError:
            value = None
        self.assertTrue(value is None or not value["live"], "owned descendant survived cancellation")
        self.assertTrue((self.root / "preserve").exists())
        # This deliberately induced cancellation has positive settlement proof.
        # Only this detector may retire its own expected preservation marker.
        (self.root / "preserve").unlink()


if __name__ == "__main__":
    if sys.argv[1:2] == ["--transport"]:
        transport(Path(sys.argv[2]), sys.argv[3:])
    else:
        if "--watch-owner" in sys.argv:
            sys.argv.remove("--watch-owner")
            WATCH_OWNER = True
        def cancellation(_signal, _frame):
            global CANCELLED
            CANCELLED = True
        for sig in (signal.SIGTERM, signal.SIGINT, signal.SIGHUP):
            signal.signal(sig, cancellation)
        try:
            unittest.main()
        except FixtureAborted as error:
            print(error, file=sys.stderr)
            sys.exit(1)
