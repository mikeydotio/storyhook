#!/usr/bin/env python3
"""Real Git regressions for shared verifier ownership and recovery (SH-683).

Mutation-checked for SH-695 (SH-295: a pin that cannot fail is not a pin),
each run by hand against scripts/verifier-owner.py before commit:
- the immediate refusal on exit-with-survivors restored -> 3 of 3 red
  (orphan reaped, TERM-resistant orphan, json result waits) and the
  merge_gate orphan regression red;
- admission ignoring the recorded leader exit -> the quiet-session
  admission case red; admission ignoring the census -> the live-session
  refusal case red;
- the outer never signalling a dead supervisor's gate session -> the outer
  reap case red (no TERM reached the member).
- the leader reaped at observation (waitpid instead of waitid/WNOWAIT) ->
  the pinned-leader case red (zombie span 0.085 s against a 1 s floor).
- the cleanup budget validated after the record write -> the invalid
  budget case red (gate_started left true).

SH-698 observation regressions were mutation-checked in memory, leaving the
production scripts unchanged: existence-before-content, a 15-second startup
ceiling, ignored child death, and a restarted settlement deadline each turned
its corresponding HarnessObservation case red.
"""

import io
import json
import os
import shlex
import signal
import sys
import time
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest import mock

sys.dont_write_bytecode = True
import load_grace

SCRIPTS = Path(__file__).resolve().parents[1]

# The fixture supplies this budget, rather than inheriting the enclosing gate's
# policy. Cancellation cases override it explicitly to exercise shorter ladders.
CLEANUP_BUDGET_MS = "30000"
# Scheduling, polling, log flushing and reaping beyond the supervised allowance.
MARGIN = 5
# Startup has no production deadline. Three cleanup budgets allow interpreter,
# fork/setsid and publication delays under concurrent builds; dead children fail
# immediately, so this generosity costs time only for a live stalled fixture.
MILESTONE_DEADLINE = 3 * int(CLEANUP_BUDGET_MS) / 1000 + MARGIN
POLL_INTERVAL = 0.01
# SH-347's recorded tolerance for any one graced wait. The whole Python suite
# is one Rust test inside the gate, whose silence watchdog is about 29 minutes,
# so a real hang must fail as a case well before that (SH-767).
PATIENCE_CEILING = 15 * 60
# Startup patience, the largest allowance, reaches the ceiling at this grace.
# Every allowance scales by the same grace, so their order never changes.
MAX_GRACE = PATIENCE_CEILING / MILESTONE_DEADLINE
# An injected census this many times the reaping eighth: the kill's own census
# then spans the whole window, as a loaded ps did in the PR 857 gate (SH-767).
CENSUS_OVERRUN = 1.25


class VerifierLifecycle(unittest.TestCase):
    """Each case owns its repository, Git configuration, locks and evidence."""

    # Set by setUp; an observation-only harness has no repository.
    common = None
    # Sampled by setUp; an observation-only harness runs at the idle values.
    grace = 1.0

    def setUp(self):
        """Create two published commits in an isolated repository."""
        self.tmp = tempfile.TemporaryDirectory(prefix="sh683-", dir="/tmp")
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name).resolve()
        self.repo = self.root / "repository with spaces"
        self.repo.mkdir()
        interpreters = self.root / "interpreters"
        interpreters.mkdir()
        (interpreters / "python3").symlink_to(sys.executable)
        self.env = {"PATH": str(interpreters) + ":" + os.environ["PATH"], "HOME": str(self.root),
                    "GIT_CONFIG_GLOBAL": "/dev/null", "GIT_CONFIG_NOSYSTEM": "1",
                    "STORYHOOK_STORE_PATH": str(self.root / "store.db"),
                    "STORYHOOK_LOCK_DIR": str(self.root / "locks"),
                    "STORYHOOK_VERIFIER_MIRROR": "0", "TMPDIR": "/tmp"}
        self.sample_grace()
        self.set_cleanup_budget(CLEANUP_BUDGET_MS)
        self.git("init", "-q", "-b", "main")
        self.git("config", "user.name", "Lifecycle Fixture")
        self.git("config", "user.email", "lifecycle@example.test")
        (self.repo / "f").write_text("base\n")
        self.git("add", "f")
        self.git("commit", "-qm", "base")
        self.base = self.git("rev-parse", "HEAD")
        self.git("checkout", "-qb", "feature")
        (self.repo / "g").write_text("candidate\n")
        self.git("add", "g")
        self.git("commit", "-qm", "candidate")
        self.head = self.git("rev-parse", "HEAD")
        self.git("checkout", "-q", "main")
        self.common = self.repo / ".git"
        self.wt = self.common / "storyhook/verification-worktree"
        self.admin = self.common / "worktrees/verification-worktree"

    def command(self, *args, cwd=None, check=True):
        """Run real commands with bounded waits and fixture-only environment."""
        result = subprocess.run(args, cwd=cwd or self.repo, env=self.env,
                                capture_output=True, text=True, timeout=self.milestone_deadline)
        if check:
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        return result

    def git(self, *args, cwd=None):
        """Run ordinary Git without private object overrides."""
        return self.command("git", *args, cwd=cwd).stdout.strip()

    def ensure(self):
        """Drive the production verifier preflight seam."""
        result = self.command("bash", str(SCRIPTS / "verify-pr.sh"),
                              "--ensure-verifier-worktree", self.base)
        return json.loads(result.stdout)

    def speculate(self, command):
        """Run production speculative checkout and cleanup with real Git."""
        tree = self.git("merge-tree", "--write-tree", self.base, self.head)
        return self.command("bash", str(SCRIPTS / "merge-watch.sh"),
                            "--speculative-run", tree, self.base, self.head,
                            str(self.wt), "--", "bash", "-c", command, check=False)

    def test_normalizes_owned_suffix_without_replacing_metadata(self):
        """A freed canonical basename is repaired without rebuilding its owner."""
        other = self.root / "verification-worktree"
        self.git("worktree", "add", "-q", "--detach", str(other), self.base)
        self.wt.parent.mkdir(parents=True, exist_ok=True)
        self.git("worktree", "add", "-q", "--detach", str(self.wt), self.base)
        old = Path((self.wt / ".git").read_text().strip()[8:])
        self.assertNotEqual(old, self.admin)
        saved = {p.relative_to(old): p.read_bytes() for p in old.rglob("*") if p.is_file()}
        self.git("worktree", "remove", str(other))
        self.assertEqual(self.ensure()["result"], "verifier-worktree-ready")
        self.assertEqual(self.git("rev-parse", "--absolute-git-dir", cwd=self.wt), str(self.admin))
        for name, body in saved.items():
            self.assertEqual((self.admin / name).read_bytes(), body, name)

    def test_recovers_private_pointer_and_preserves_actual_gate_edits(self):
        """Restoration must retain the private index and checkout as one unit."""
        self.assertEqual(self.ensure()["result"], "verifier-worktree-ready")
        result = self.speculate("printf 'gate edit\\n' > f; git add f; printf evidence > untracked")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("preserv", result.stderr)
        repaired = self.ensure()
        self.assertEqual(repaired["result"], "verifier-worktree-ready", repaired)
        self.assertEqual(self.git("status", "--porcelain", cwd=self.wt), "")
        retained = list((self.common / "storyhook").glob("verification-recovery-*/worktree/f"))
        self.assertEqual(len(retained), 1)
        self.assertEqual(retained[0].read_text(), "gate edit\n")
        self.assertEqual((retained[0].parent / "untracked").read_text(), "evidence")
        self.assertEqual(self.ensure()["result"], "verifier-worktree-ready")
        self.assertEqual(len(list((self.common / "storyhook").glob("verification-recovery-*"))), 1)

    def owner(self, *args, check=False):
        """Drive the production supervisor without changing its behavior."""
        return self.command("python3", str(SCRIPTS / "verifier-owner.py"),
                            "run", str(self.common), str(self.wt), "--", *args, check=check)

    def test_valid_canonical_collision_preserves_both_mappings(self):
        """Another legitimate same-basename checkout is never renamed or removed."""
        other = self.root / "verification-worktree"
        self.git("worktree", "add", "-q", "--detach", str(other), self.base)
        original = (other / ".git").read_bytes()
        result = self.ensure()
        self.assertEqual(result["result"], "infrastructure-failure")
        self.assertIn("canonical name collision", result["detail"])
        self.assertIn(str(other), result["detail"])
        self.assertEqual((other / ".git").read_bytes(), original)
        self.assertFalse(self.wt.exists())

    def test_malformed_owner_does_not_count_as_a_different_boot(self):
        """An incomplete owner record cannot authorize recovery by omission."""
        self.assertEqual(self.ensure()["result"], "verifier-worktree-ready")
        record_path = next((self.common / "storyhook/verifier-lifecycle").glob("*.owner"))
        record = json.loads(record_path.read_text())
        for field in ("boot", "session", "supervisor"):
            with self.subTest(field=field):
                malformed = dict(record)
                del malformed[field]
                record_path.write_text(json.dumps(malformed))
                result = self.ensure()
                self.assertEqual(result["result"], "infrastructure-failure", result)
                self.assertEqual(json.loads(record_path.read_text()), malformed)

    def test_json_result_waits_for_supervision_to_finish(self):
        """A child result is published only once its survivors are settled.

        Before SH-695 the surviving fork made the supervisor refuse and the
        child's own verdict was discarded; now the survivor of an exited
        leader is reaped on the cancellation ladder and the verdict that
        reaches stdout is the child's, after the reap, never before it.
        """
        ready = self.root / "survivor-pid"
        code = "import os,time; pid=os.fork(); "
        code += "\nif pid==0: time.sleep(30); os._exit(0)"
        code += "\nopen(" + repr(str(ready)) + ",'w').write(str(pid)+'\\n')"
        code += "\nprint('{\"result\":\"ready\"}',flush=True)"
        child, log = self.spawn(sys.executable, str(SCRIPTS / "verifier-owner.py"),
                                "run-json", str(self.common), str(self.wt), "--",
                                sys.executable, "-c", code)
        pid = self.wait_pid(ready, child, log)
        self.addCleanup(lambda: self.stop_pid(pid))
        self.assertEqual(self.wait_settled(child, log), 0)
        self.assertGone(pid)
        log.seek(0)
        output = log.read().decode()
        json_lines = [line for line in output.splitlines() if line.startswith("{")]
        self.assertEqual(len(json_lines), 1, output)
        verdict = json.loads(json_lines[0])
        self.assertEqual(verdict["result"], "ready", output)
        self.assertIn("leaving survivors", output)

    def test_unfinished_lease_allocation_is_recovered_on_restart(self):
        """A crash during private preparation cannot leak an unrecorded lease."""
        self.assertEqual(self.ensure()["result"], "verifier-worktree-ready")
        result = self.owner("python3", str(SCRIPTS / "verifier-worktree.py"),
                            "allocate", str(self.common), str(self.wt), self.base)
        self.assertEqual(result.returncode, 0, result.stderr)
        lease = Path(result.stdout.strip())
        self.assertTrue(lease.is_dir())
        (lease / "partial-index").write_text("incomplete")
        self.assertEqual(self.ensure()["result"], "verifier-worktree-ready")
        self.assertFalse(lease.exists())
        self.assertEqual(self.ensure()["result"], "verifier-worktree-ready")

    def test_partial_linked_lease_deletion_resumes(self):
        """A restored checkout permits removal of an already-partly-deleted lease."""
        self.assertEqual(self.ensure()["result"], "verifier-worktree-ready")
        allocated = self.owner("python3", str(SCRIPTS / "verifier-worktree.py"),
                               "allocate", str(self.common), str(self.wt), self.base)
        self.assertEqual(allocated.returncode, 0, allocated.stderr)
        lease = Path(allocated.stdout.strip())
        (lease / "remaining-object").write_text("remaining cleanup")
        state_path = next((self.common / "storyhook/verifier-lifecycle").glob("*.state"))
        state = json.loads(state_path.read_text())
        state.update(lease_phase="ready", restore="linked")
        state_path.write_text(json.dumps(state))
        self.assertEqual(self.ensure()["result"], "verifier-worktree-ready")
        self.assertFalse(lease.exists())
        self.assertEqual(self.ensure()["result"], "verifier-worktree-ready")

    def test_interrupted_admin_rename_resumes(self):
        """A journaled rename without its forward-pointer update converges."""
        self.assertEqual(self.ensure()["result"], "verifier-worktree-ready")
        suffix = self.admin.with_name("verification-worktree1")
        self.admin.rename(suffix)
        (self.wt / ".git").write_text(f"gitdir: {suffix}\n")
        state_path = next((self.common / "storyhook/verifier-lifecycle").glob("*.state"))
        state = json.loads(state_path.read_text())
        state["rename"] = {"source": str(suffix), "target": str(self.admin)}
        state_path.write_text(json.dumps(state))
        suffix.rename(self.admin)
        index = (self.admin / "index").read_bytes()
        self.assertEqual(self.ensure()["result"], "verifier-worktree-ready")
        self.assertEqual((self.admin / "index").read_bytes(), index)
        self.assertEqual(self.git("rev-parse", "--absolute-git-dir", cwd=self.wt), str(self.admin))
        self.assertEqual(self.ensure()["result"], "verifier-worktree-ready")

    def test_verified_boot_change_retires_previous_gate_identity(self):
        """Only a complete prior identity can use a different kernel boot."""
        self.assertEqual(self.ensure()["result"], "verifier-worktree-ready")
        owner_path = next((self.common / "storyhook/verifier-lifecycle").glob("*.owner"))
        owner = json.loads(owner_path.read_text())
        owner.update(boot="00000000-0000-0000-0000-000000000000", gate_started=True)
        owner_path.write_text(json.dumps(owner))
        self.assertEqual(self.ensure()["result"], "verifier-worktree-ready")
        self.assertNotEqual(json.loads(owner_path.read_text())["boot"], owner["boot"])

    def test_ownerless_private_pointer_is_preserved_with_diagnostics(self):
        """Legacy ambiguity is named rather than forced through Git removal."""
        self.assertEqual(self.ensure()["result"], "verifier-worktree-ready")
        private = self.common / "storyhook/merge-watch-objects.legacy/.git"
        private.mkdir(parents=True)
        original = (self.wt / ".git").read_bytes()
        (private / "gitdir").write_text(str(self.wt / ".git") + "\n")
        (private / "commondir").write_text(str(self.common) + "\n")
        (private / "HEAD").write_text(self.base + "\n")
        (private.parent / "original-gitlink").write_bytes(original)
        (self.wt / ".git").write_text(f"gitdir: {private}\n")
        pointer = (self.wt / ".git").read_bytes()
        result = self.ensure()
        self.assertEqual(result["result"], "infrastructure-failure")
        self.assertIn("ambiguous legacy", result["detail"])
        self.assertIn(str(self.admin), result["detail"])
        self.assertEqual((self.wt / ".git").read_bytes(), pointer)
        self.assertTrue(private.exists())

    def wait_path(self, path, child, log):
        """Wait for an existence-only milestone or diagnose its writer's death."""
        self._wait_publication(path, child, log, pid_record=False)

    def wait_pid(self, path, child, log):
        """Read one newline-complete PID; open or partial writes are pending."""
        return self._wait_publication(path, child, log, pid_record=True)

    def _wait_publication(self, path, child, log, *, pid_record):
        started = time.monotonic()
        patience = self.patience(self.milestone_deadline, started)
        content = None

        def published():
            nonlocal content
            if not pid_record:
                return path.exists()
            try:
                content = path.read_text()
            except FileNotFoundError:
                content = None
                return None
            if not content.endswith("\n"):
                return None
            digits = content[:-1]
            if not digits.isascii() or not digits.isdecimal() or int(digits) <= 0:
                self.fail(f"malformed PID record {path}: {content!r}; child pid={child.pid}")
            return int(digits)

        while True:
            value = published()
            if value:
                return value
            status = child.poll()
            if status is not None:
                # The writer can publish its final bytes between our read and
                # poll. Exit is conclusive only after a final publication read.
                value = published()
                if value:
                    return value
            now = time.monotonic()
            elapsed = now - started
            if status is not None or patience.expired(now):
                reason = f"exited {status}" if status is not None else "deadline expired"
                # pread leaves the child's shared log offset untouched.
                evidence = os.pread(log.fileno(), os.fstat(log.fileno()).st_size, 0).decode(errors="replace")
                self.fail(f"child pid={child.pid} {reason} before publishing {path}; "
                          f"elapsed={elapsed:.3f}s allowance={patience.allowance}s "
                          f"load={os.getloadavg()} content={content!r}\n{evidence}")
            time.sleep(POLL_INTERVAL)

    def sample_grace(self):
        """Grace this case once by measured contention (SH-767).

        Production reads its cleanup budget once, at launch, so the budget a
        case hands over is a snapshot; only the harness's own waits resample.
        """
        ratio = load_grace.contention()
        self.grace = load_grace.multiplier(ratio, MAX_GRACE)
        if self.grace > 1:
            print(f"{self.id()}: {load_grace.describe(ratio, self.grace)}", file=sys.stderr)

    def set_cleanup_budget(self, milliseconds, graced=True):
        """Supply a valid fixture budget, graced by this case's contention.

        Leading zeros survive grace, so they still reach verify-pr.sh's
        decimal normalization; graced=False hands the spelling over unchanged.
        """
        def assert_valid(value):
            # The same policy verify-pr.sh and verifier-owner.py enforce.
            self.assertTrue(value.isascii() and value.isdecimal() and len(value) <= 8, value)
            self.assertTrue(4000 <= int(value) <= 99999999, value)
        assert_valid(milliseconds)
        spelled = load_grace.graced_spelling(milliseconds, self.grace) if graced else milliseconds
        assert_valid(spelled)
        self.env["STORYHOOK_VERIFIER_CLEANUP_GRACE_MS"] = spelled
        self.budget = int(spelled) / 1000

    @property
    def settle_timeout(self):
        """Allow the supplied cancellation ladder plus a margin graced like it."""
        return self.budget + MARGIN * self.grace

    @property
    def milestone_deadline(self):
        """Startup and synchronous-command patience, graced like every allowance."""
        return MILESTONE_DEADLINE * self.grace

    def patience(self, allowance, started):
        """Start a harness wait that extends while contention rises (SH-347)."""
        return load_grace.Patience(allowance, self.grace, MAX_GRACE, started)

    def evidence(self, log):
        """Collect the child's own log and every gate attempt log for a failure.

        The gate supervisor's refusals reach only the attempt log, so a wrapper
        log alone cannot tell which layer ended a cancellation (SH-767).
        """
        parts = []
        if log is not None:
            # pread leaves the child's shared log offset untouched.
            parts.append(os.pread(log.fileno(), os.fstat(log.fileno()).st_size, 0).decode(errors="replace"))
        logs = self.common / "storyhook/verification-logs" if self.common else None
        if logs is not None and logs.is_dir():
            for path in sorted(logs.glob("pr-1-*-attempt.*")):
                if not path.name.endswith(".jsonl"):
                    parts.append(f"--- {path}\n{path.read_text(errors='replace')}")
        return "\n".join(parts)

    def settle_failure(self, reason, log, started, allowance):
        """Fail a settlement wait with its timing, load and explaining logs."""
        elapsed = time.monotonic() - started
        self.fail(f"{reason}: elapsed={elapsed:.3f}s allowance={allowance}s "
                  f"load={os.getloadavg()}\n{self.evidence(log)}")

    def session_pids(self, sid):
        """List live members of one recorded session, independently of the owner."""
        result = subprocess.run(["ps", "-axo", "pid=,stat="], capture_output=True, text=True, check=True)
        members = []
        for line in result.stdout.splitlines():
            pid_text, state = line.split(maxsplit=1)
            try:
                if not state.startswith("Z") and os.getsid(int(pid_text)) == sid:
                    members.append(int(pid_text))
            except ProcessLookupError:
                continue
        return members

    def wait_cancelled(self, child, pid, log=None, sessions=lambda: ()):
        """Observe the wrapper, gate and recorded sessions within one shared allowance.

        A wrapper that ended its lifecycle supervisor early leaves that session
        running; the record is final only once every recorded session is quiet.
        """
        started = time.monotonic()
        patience = self.patience(self.settle_timeout, started)
        while True:
            try:
                child.wait(timeout=patience.remaining(time.monotonic()))
                break
            except subprocess.TimeoutExpired:
                if patience.expired(time.monotonic()):
                    self.settle_failure(f"verifier pid={child.pid} did not finish cancellation",
                                        log, started, patience.allowance)
        while True:
            try:
                os.kill(pid, 0)
            except ProcessLookupError:
                break
            if patience.expired(time.monotonic()):
                self.settle_failure(f"gate session pid={pid} survived cancellation",
                                    log, started, patience.allowance)
            time.sleep(POLL_INTERVAL)
        while True:
            members = [member for sid in sessions() for member in self.session_pids(sid)]
            if not members:
                return
            if patience.expired(time.monotonic()):
                self.settle_failure(f"recorded sessions still have members {members}",
                                    log, started, patience.allowance)
            time.sleep(POLL_INTERVAL)

    def wait_settled(self, child, log):
        """Wait one settlement allowance for a supervised child, failing with evidence."""
        started = time.monotonic()
        patience = self.patience(self.settle_timeout, started)
        while True:
            try:
                return child.wait(timeout=patience.remaining(time.monotonic()))
            except subprocess.TimeoutExpired:
                if patience.expired(time.monotonic()):
                    self.settle_failure(f"child pid={child.pid} did not settle",
                                        log, started, patience.allowance)

    def spawn(self, *args):
        """Own a child and regular log files so survivors cannot hold pipes open."""
        log = tempfile.TemporaryFile()
        self.addCleanup(log.close)
        child = subprocess.Popen(args, cwd=self.repo, env=self.env, stdout=log, stderr=log)
        cleanup_timeout = self.settle_timeout
        def cleanup():
            if child.poll() is None:
                child.kill()
            child.wait(timeout=cleanup_timeout)
        self.addCleanup(cleanup)
        return child, log

    def test_concurrent_preflight_cannot_mutate_live_owned_checkout(self):
        """Kernel ownership excludes preflight even if no gate wrapper is alive."""
        self.assertEqual(self.ensure()["result"], "verifier-worktree-ready")
        ready = self.root / "ready"
        release = self.root / "release"
        code = "from pathlib import Path; import time; Path(\"" + str(ready) + "\").touch(); "
        code += "\nwhile not Path(\"" + str(release) + "\").exists(): time.sleep(.01)"
        child, log = self.spawn("python3", str(SCRIPTS / "verifier-owner.py"), "run",
                              str(self.common), str(self.wt), "--", "python3", "-c", code)
        self.wait_path(ready, child, log)
        pointer = (self.wt / ".git").read_bytes()
        result = self.ensure()
        self.assertEqual(result["result"], "infrastructure-failure")
        self.assertIn("live verifier owner", result["detail"])
        self.assertEqual((self.wt / ".git").read_bytes(), pointer)
        release.touch()
        self.assertEqual(self.wait_settled(child, log), 0)
        self.assertEqual(self.ensure()["result"], "verifier-worktree-ready")

    def test_killed_supervisor_cannot_reclaim_descriptor_closing_child(self):
        """A recorded surviving session protects files after every flock copy closes."""
        self.assertEqual(self.ensure()["result"], "verifier-worktree-ready")
        ready = self.root / "child-pid"
        code = "import os,time; os.closerange(3,256); open(" + repr(str(ready)) + ",'w').write(str(os.getpid())+'\\n'); time.sleep(30)"
        child, log = self.spawn("python3", str(SCRIPTS / "verifier-owner.py"), "run",
                              str(self.common), str(self.wt), "--", "python3", "-c", code)
        pid = self.wait_pid(ready, child, log)
        self.addCleanup(lambda: self.stop_pid(pid))
        child.kill()
        self.wait_settled(child, log)
        result = self.ensure()
        self.assertEqual(result["result"], "infrastructure-failure")
        self.assertIn("live owner session", result["detail"])
        self.assertIn(str(pid), result["detail"])
        self.stop_pid(pid)

    def test_cancellation_preserves_lifecycle_cleanup_workers(self):
        """The lifecycle leader must finish its workers before ownership settles."""
        leader = self.root / "lifecycle.py"
        leader.write_text('''import signal, subprocess, sys, time
from pathlib import Path
ready, result = map(Path, sys.argv[1:])
worker = subprocess.Popen([sys.executable, "-c", "input()"], stdin=subprocess.PIPE, text=True)
def finish(signum, frame):
    # Hold cleanup open while the cancellation fan-out finishes. Its workers
    # belong to the lifecycle leader, not the arbitrary gate process tree.
    time.sleep(.25)
    worker.communicate("cleanup complete\\n")
    result.write_text(str(worker.returncode))
    sys.exit(128 + signum)
for signum in (signal.SIGHUP, signal.SIGINT, signal.SIGTERM):
    signal.signal(signum, finish)
ready.write_text(str(worker.pid)+'\\n')
while True:
    signal.pause()
''')
        for signum in (signal.SIGHUP, signal.SIGINT, signal.SIGTERM):
            with self.subTest(signal=signum):
                ready = self.root / f"ready-{signum}"
                result = self.root / f"result-{signum}"
                owner, log = self.spawn(sys.executable, str(SCRIPTS / "verifier-owner.py"),
                                        "run", str(self.common), str(self.wt), "--",
                                        sys.executable, str(leader), str(ready), str(result))
                pid = self.wait_pid(ready, owner, log)
                self.addCleanup(lambda pid=pid: self.stop_pid(pid))
                owner.send_signal(signum)
                status = self.wait_settled(owner, log)
                log.seek(0)
                self.assertEqual(status, 128 + signum, log.read().decode())
                self.assertEqual(result.read_text(), "0", "cleanup worker was cancelled directly")

    def test_cancellation_reaches_gate_workers_in_separate_process_groups(self):
        """Arbitrary gate subgroups settle before their durable completion record."""
        worker = self.root / "gate-worker.py"
        worker.write_text('''import os, signal, sys
from pathlib import Path
os.setpgrp()
ready, result = map(Path, sys.argv[1:])
def finish(signum, frame):
    result.write_text(str(signum))
    sys.exit(0)
for signum in (signal.SIGHUP, signal.SIGINT, signal.SIGTERM):
    signal.signal(signum, finish)
ready.write_text(str(os.getpid())+'\\n')
while True:
    signal.pause()
''')
        gate = self.root / "gate.py"
        gate.write_text(f'''import signal, subprocess, sys
SETTLE_TIMEOUT = {self.settle_timeout!r}
def finish(signum, frame):
    worker.wait(timeout=SETTLE_TIMEOUT)
    sys.exit(128 + signum)
for signum in (signal.SIGHUP, signal.SIGINT, signal.SIGTERM):
    signal.signal(signum, finish)
worker = subprocess.Popen([sys.executable, *sys.argv[1:]])
while True:
    signal.pause()
''')
        for signum in (signal.SIGHUP, signal.SIGINT, signal.SIGTERM):
            with self.subTest(signal=signum):
                ready = self.root / f"gate-ready-{signum}"
                result = self.root / f"gate-result-{signum}"
                supervisor = [sys.executable, str(SCRIPTS / "verifier-owner.py")]
                paths = [str(self.common), str(self.wt), "--"]
                owner, log = self.spawn(*supervisor, "run", *paths,
                                        *supervisor, "gate", *paths, sys.executable,
                                        str(gate), str(worker), str(ready), str(result))
                pid = self.wait_pid(ready, owner, log)
                self.addCleanup(lambda pid=pid: self.stop_pid(pid))
                owner.send_signal(signum)
                status = self.wait_settled(owner, log)
                log.seek(0)
                self.assertEqual(status, 128 + signum, log.read().decode())
                self.assertEqual(result.read_text(), str(int(signum)))
                record_path = next((self.common / "storyhook/verifier-lifecycle").glob("*.owner"))
                record = json.loads(record_path.read_text())
                self.assertFalse(record["gate_started"])
                self.assertIsNone(record["gate_session"])
                self.assertTrue(record["completed"])

    def stop_pid(self, pid):
        """Stop only a fixture PID recorded by this test's child."""
        try:
            os.kill(pid, signal.SIGKILL)
        except ProcessLookupError:
            pass

    def test_sigkill_after_private_pointer_swap_recovers_clean_checkout(self):
        """Kill the actual speculative shell after mv, before any gate launches."""
        self.assertEqual(self.ensure()["result"], "verifier-worktree-ready")
        ready = self.root / "swapped"
        wrapper = self.root / "bin"
        wrapper.mkdir()
        real_mv = self.command("sh", "-c", "command -v mv").stdout.strip()
        (wrapper / "mv").write_text(
            '#!/bin/bash\n"' + real_mv + '" "$@" || exit $?\n'
            'if [ "${!#}" = "' + str(self.wt / ".git") + '" ]; then\n'
            '  kill -STOP "$PPID"\n  touch "' + str(ready) + '"\nfi\n')
        (wrapper / "mv").chmod(0o755)
        self.env["PATH"] = str(wrapper) + ":" + self.env["PATH"]
        tree = self.git("merge-tree", "--write-tree", self.base, self.head)
        child, log = self.spawn("bash", str(SCRIPTS / "merge-watch.sh"),
                                "--speculative-run", tree, self.base, self.head,
                                str(self.wt), "--", "true")
        self.wait_path(ready, child, log)
        owner_path = next((self.common / "storyhook/verifier-lifecycle").glob("*.owner"))
        owner = json.loads(owner_path.read_text())
        os.killpg(owner["session"], signal.SIGKILL)
        self.wait_settled(child, log)
        self.assertNotEqual(child.returncode, 0)
        self.env["PATH"] = self.env["PATH"].split(":", 1)[1]
        result = self.ensure()
        self.assertEqual(result["result"], "verifier-worktree-ready", result)
        self.assertEqual(self.git("status", "--porcelain", cwd=self.wt), "")
        self.assertEqual(self.git("rev-parse", "HEAD", cwd=self.wt), self.base)
        self.assertEqual(list((self.common / "storyhook").glob("merge-watch-objects.*")), [])
        self.assertEqual(self.ensure()["result"], "verifier-worktree-ready")

    def test_stale_owned_registration_is_retained_before_normalization(self):
        """A duplicate stale mapping is reconciled only for this reserved checkout."""
        self.assertEqual(self.ensure()["result"], "verifier-worktree-ready")
        stale_index = (self.admin / "index").read_bytes()
        old_wt = self.root / "old-worktree"
        self.git("worktree", "move", str(self.wt), str(old_wt))
        self.git("worktree", "add", "-q", "--detach", str(self.wt), self.base)
        # Reproduce stale owned administration left with the old reserved
        # backlink after a replacement acquired Git's suffixed registration.
        (self.admin / "gitdir").write_text(str(self.wt / ".git") + "\n")
        self.assertNotEqual(self.git("rev-parse", "--absolute-git-dir", cwd=self.wt), str(self.admin))
        result = self.ensure()
        self.assertEqual(result["result"], "verifier-worktree-ready", result)
        self.assertEqual(self.git("rev-parse", "--absolute-git-dir", cwd=self.wt), str(self.admin))
        retained = list((self.common / "storyhook").glob("verification-recovery-*/stale-admin/index"))
        self.assertEqual(len(retained), 1)
        self.assertEqual(retained[0].read_bytes(), stale_index)

    def test_interrupted_retention_moves_resume_without_losing_edits(self):
        """Kill the real recovery process at filesystem rename boundaries."""
        for boundary in ("before-worktree", "after-worktree", "after-admin"):
            with self.subTest(boundary=boundary):
                self.assertEqual(self.ensure()["result"], "verifier-worktree-ready")
                (self.wt / "f").write_text(boundary)
                self.git("add", "f", cwd=self.wt)
                index = (self.admin / "index").read_bytes()
                code = "import sys,os,signal,importlib.util; sys.dont_write_bytecode=True; "
                code += "sys.path.insert(0," + repr(str(SCRIPTS)) + "); "
                code += "from verifier_state import paths; "
                code += "spec=importlib.util.spec_from_file_location('workspace'," + repr(str(SCRIPTS / "verifier-worktree.py")) + "); "
                code += "module=importlib.util.module_from_spec(spec); spec.loader.exec_module(module); "
                code += "real=module.Path.rename; boundary=" + repr(boundary) + "; wt=" + repr(str(self.wt)) + "; admin=" + repr(str(self.admin)) + "\n"
                code += "def crash(source,destination,*args,**kwargs):\n"
                code += " if boundary=='before-worktree' and str(source)==wt: os.kill(os.getpid(),signal.SIGKILL)\n"
                code += " result=real(source,destination,*args,**kwargs)\n"
                code += " if (boundary=='after-worktree' and str(source)==wt) or (boundary=='after-admin' and str(source)==admin): os.kill(os.getpid(),signal.SIGKILL)\n"
                code += " return result\n"
                code += "module.Path.rename=crash\nws=module.Workspace(*paths(" + repr(str(self.common)) + ",wt)); ws.archive(module.Path(admin))"
                result = self.owner("python3", "-c", code)
                self.assertEqual(result.returncode, 137, result.stderr)
                self.assertEqual(self.ensure()["result"], "verifier-worktree-ready")
                retained = [p for p in (self.common / "storyhook").glob("verification-recovery-*/worktree/f") if p.read_text() == boundary]
                self.assertEqual(len(retained), 1)
                self.assertEqual((retained[0].parents[1] / "admin/index").read_bytes(), index)
                self.assertEqual(self.ensure()["result"], "verifier-worktree-ready")

    def test_untracked_collision_during_restoration_is_preserved(self):
        """An untracked file obstructing base checkout is retained, not erased."""
        self.git("checkout", "-q", "feature")
        self.git("rm", "f")
        self.git("commit", "-qm", "remove base path")
        self.head = self.git("rev-parse", "HEAD")
        self.git("checkout", "-q", "main")
        self.assertEqual(self.ensure()["result"], "verifier-worktree-ready")
        result = self.speculate("printf untracked-evidence > f")
        self.assertNotEqual(result.returncode, 0)
        result = self.ensure()
        self.assertEqual(result["result"], "verifier-worktree-ready", result)
        retained = list((self.common / "storyhook").glob("verification-recovery-*/worktree/f"))
        self.assertEqual(len(retained), 1)
        self.assertEqual(retained[0].read_text(), "untracked-evidence")

    def test_incomplete_arbitrary_gate_remains_ambiguous_on_same_boot(self):
        """A missing owner PID never fabricates a completed gate session."""
        self.assertEqual(self.ensure()["result"], "verifier-worktree-ready")
        owner_path = next((self.common / "storyhook/verifier-lifecycle").glob("*.owner"))
        owner = json.loads(owner_path.read_text())
        owner["gate_started"] = True
        owner["gate_session"] = None
        owner_path.write_text(json.dumps(owner))
        pointer = (self.wt / ".git").read_bytes()
        result = self.ensure()
        self.assertEqual(result["result"], "infrastructure-failure")
        self.assertIn("ambiguous ownership", result["detail"])
        self.assertEqual((self.wt / ".git").read_bytes(), pointer)
        self.assertEqual(json.loads(owner_path.read_text()), owner)
        # A recorded leader exit is evidence only in the pairing the gate
        # supervisor writes: with a started gate and its integer session.
        for fields in ({"gate_started": True, "gate_session": None, "gate_leader_exit": 3},
                       {"gate_started": True, "gate_session": 1, "gate_leader_exit": "3"},
                       {"gate_started": True, "gate_session": 1, "gate_leader_exit": -1},
                       {"gate_started": False, "gate_session": None, "gate_leader_exit": 3}):
            with self.subTest(fields=fields):
                malformed = dict(owner, **fields)
                owner_path.write_text(json.dumps(malformed))
                result = self.ensure()
                self.assertEqual(result["result"], "infrastructure-failure", result)
                self.assertIn("incomplete owner identity", result["detail"])
                self.assertEqual(json.loads(owner_path.read_text()), malformed)

    def test_symlink_and_backlink_corruption_never_authorize_rebuild(self):
        """Invalid mappings must fail before altering the checkout or target."""
        self.assertEqual(self.ensure()["result"], "verifier-worktree-ready")
        original = (self.wt / ".git").read_bytes()
        evidence = self.root / "external-gitlink"
        evidence.write_bytes(original)
        (self.wt / ".git").unlink()
        (self.wt / ".git").symlink_to(evidence)
        result = self.ensure()
        self.assertEqual(result["result"], "infrastructure-failure")
        self.assertEqual(evidence.read_bytes(), original)
        (self.wt / ".git").unlink()
        (self.wt / ".git").write_bytes(original)
        (self.admin / "gitdir").write_text(str(evidence))
        result = self.ensure()
        self.assertEqual(result["result"], "infrastructure-failure")
        self.assertEqual((self.wt / ".git").read_bytes(), original)

    def test_startup_archives_staged_and_unstaged_damage_before_cleaning(self):
        """Startup preserves tracked damage and accompanying extras as one unit."""
        for staged in (False, True):
            with self.subTest(staged=staged):
                self.assertEqual(self.ensure()["result"], "verifier-worktree-ready")
                evidence = "staged" if staged else "unstaged"
                (self.wt / "f").write_text(evidence)
                (self.wt / "extra").write_text(evidence)
                if staged:
                    self.git("add", "f", cwd=self.wt)
                index = (self.admin / "index").read_bytes()
                result = self.ensure()
                self.assertEqual(result["result"], "verifier-worktree-ready", result)
                self.assertEqual((self.wt / "f").read_text(), "base\n")
                self.assertFalse((self.wt / "extra").exists())
                retained = [p for p in self.wt.parent.glob("verification-recovery-*/worktree/f")
                            if p.read_text() == evidence]
                self.assertEqual(len(retained), 1)
                self.assertEqual((retained[0].parent / "extra").read_text(), evidence)
                self.assertEqual((retained[0].parents[1] / "admin/index").read_bytes(), index)
                self.assertEqual(self.ensure()["result"], "verifier-worktree-ready")
                self.assertEqual(len(list(self.wt.parent.glob("verification-recovery-*"))),
                                 2 if staged else 1)

    def test_startup_cleans_ignored_nested_and_symlink_leftovers(self):
        """Extras cannot leak between stories, including ignored nested repos."""
        self.assertEqual(self.ensure()["result"], "verifier-worktree-ready")
        (self.common / "info/exclude").write_text("ignored/\n")
        (self.wt / "ignored").mkdir()
        (self.wt / "ignored/cache").write_text("stale")
        (self.wt / "nested").mkdir()
        self.git("init", "-q", str(self.wt / "nested"))
        (self.wt / "nested/data").write_text("stale")
        (self.wt / "empty").mkdir()
        (self.wt / "line\nbreak").write_text("stale")
        external = self.root / "external"
        external.mkdir()
        (external / "keep").write_text("foreign")
        (self.wt / "external-link").symlink_to(external, target_is_directory=True)
        (self.wt / "dangling").symlink_to(self.root / "missing")
        result = self.ensure()
        self.assertEqual(result["result"], "verifier-worktree-ready", result)
        self.assertEqual({p.name for p in self.wt.iterdir()}, {".git", "f"})
        self.assertEqual((external / "keep").read_text(), "foreign")
        self.assertEqual(list(self.wt.parent.glob("verification-recovery-*")), [])
        self.assertEqual(self.ensure()["result"], "verifier-worktree-ready")

    def test_startup_clears_collisions_before_pinned_base_checkout(self):
        """The next candidate sees its own tree despite old ignored path collisions."""
        self.assertEqual(self.ensure()["result"], "verifier-worktree-ready")
        (self.common / "info/exclude").write_text("g\n")
        (self.wt / "g").mkdir()
        (self.wt / "g/stale").write_text("stale")
        self.base = self.head
        result = self.ensure()
        self.assertEqual(result["result"], "verifier-worktree-ready", result)
        self.assertEqual(self.git("rev-parse", "HEAD", cwd=self.wt), self.base)
        self.assertEqual((self.wt / "g").read_text(), "candidate\n")
        self.assertEqual(self.git("status", "--porcelain", "--ignored", cwd=self.wt), "")
        result = self.speculate("test \"$(cat g)\" = candidate && test ! -e stale")
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_startup_recovers_corrupt_index_and_stale_git_operation(self):
        """Owned damaged administration is retained, not left to block checkout."""
        for name in ("index", "index.lock", "MERGE_HEAD", "rebase-merge"):
            with self.subTest(name=name):
                self.assertEqual(self.ensure()["result"], "verifier-worktree-ready")
                path = self.admin / name
                if name == "rebase-merge":
                    path.mkdir()
                    (path / "head-name").write_text("stale operation")
                else:
                    path.write_text("stale operation")
                result = self.ensure()
                self.assertEqual(result["result"], "verifier-worktree-ready", result)
                self.assertEqual(self.git("status", "--porcelain", cwd=self.wt), "")
                retained = list(self.wt.parent.glob("verification-recovery-*/admin/" + name))
                self.assertTrue(retained)
                self.assertTrue(any((p / "head-name" if p.is_dir() else p).read_bytes()
                                    == b"stale operation" for p in retained))
                if name != "index":
                    self.assertFalse(path.exists())

    def test_startup_cleanup_failure_refuses_admission_and_retries(self):
        """A real deletion permission error cannot be reported ready."""
        self.assertEqual(self.ensure()["result"], "verifier-worktree-ready")
        protected = self.wt / "protected"
        protected.mkdir()
        (protected / "leftover").write_text("stale")
        protected.chmod(0o500)
        try:
            result = self.ensure()
            self.assertEqual(result["result"], "infrastructure-failure", result)
            self.assertIn("clean", result["detail"])
            self.assertIn(str(self.wt), result["detail"])
            self.assertEqual((protected / "leftover").read_text(), "stale")
        finally:
            protected.chmod(0o700)
        self.assertEqual(self.ensure()["result"], "verifier-worktree-ready")
        self.assertFalse(protected.exists())

    def test_startup_interrupted_cleanup_rechecks_damage_before_retry(self):
        """Partial deletion followed by tracked damage must archive remaining evidence."""
        self.assertEqual(self.ensure()["result"], "verifier-worktree-ready")
        for name in ("first", "remaining"):
            (self.wt / name).write_text(name)
        wrapper = self.root / "git-wrapper"
        wrapper.mkdir()
        real_git = self.command("sh", "-c", "command -v git").stdout.strip()
        (wrapper / "git").write_text(
            '#!/bin/bash\n'
            'if [ "${3:-}" = clean ] && [ "${4:-}" = -ffdx ]; then\n'
            '  "' + real_git + '" -C "$2" clean -ffdx -- first || exit $?\n'
            '  kill -KILL "$PPID"\n  exit 0\nfi\n'
            'exec "' + real_git + '" "$@"\n')
        (wrapper / "git").chmod(0o755)
        original_path = self.env["PATH"]
        self.env["PATH"] = str(wrapper) + ":" + original_path
        try:
            result = self.ensure()
            self.assertEqual(result["result"], "infrastructure-failure", result)
        finally:
            self.env["PATH"] = original_path
        self.assertFalse((self.wt / "first").exists())
        self.assertTrue((self.wt / "remaining").exists())
        (self.wt / "f").write_text("post-interruption edit")
        result = self.ensure()
        self.assertEqual(result["result"], "verifier-worktree-ready", result)
        retained = list(self.wt.parent.glob("verification-recovery-*/worktree/f"))
        self.assertEqual(len(retained), 1)
        self.assertEqual(retained[0].read_text(), "post-interruption edit")
        self.assertEqual((retained[0].parent / "remaining").read_text(), "remaining")
        self.assertFalse((retained[0].parent / "first").exists())
        self.assertEqual(self.ensure()["result"], "verifier-worktree-ready")

    def test_startup_post_checkout_edits_never_admit_a_dirty_gate(self):
        """Final checks catch hooks changing tracked or ignored inputs after cleaning."""
        self.assertEqual(self.ensure()["result"], "verifier-worktree-ready")
        hook = self.common / "hooks/post-checkout"
        (self.common / "info/exclude").write_text("ignored\n")
        for path in ("f", "ignored"):
            with self.subTest(path=path):
                hook.write_text(f"#!/bin/sh\nprintf hook-evidence > {path}\n")
                hook.chmod(0o755)
                result = self.ensure()
                self.assertEqual(result["result"], "infrastructure-failure", result)
                self.assertIn("startup cleanup", result["detail"])
                self.assertEqual((self.wt / path).read_text(), "hook-evidence")
                hook.unlink()
                self.assertEqual(self.ensure()["result"], "verifier-worktree-ready")

    def test_startup_replacement_damage_is_bounded_and_retained(self):
        """A hook damaging every fresh checkout cannot trigger endless rebuilding."""
        hook = self.common / "hooks/post-checkout"
        hook.write_text("#!/bin/sh\nprintf hook-evidence > f\n")
        hook.chmod(0o755)
        result = self.ensure()
        self.assertEqual(result["result"], "infrastructure-failure", result)
        self.assertIn("replacement verifier is still damaged", result["detail"])
        self.assertEqual((self.wt / "f").read_text(), "hook-evidence")
        self.assertEqual(len(list(self.wt.parent.glob("verification-recovery-*"))), 1)
        hook.unlink()
        self.assertEqual(self.ensure()["result"], "verifier-worktree-ready")


    def cancel_gate(self, resistant=False, damage=False, descendant=False, delay=0):
        """Interrupt the production outer group and check owned restoration."""
        self.set_cleanup_budget("16000" if delay else "08000")
        result = self.ensure()
        self.assertEqual(result["result"], "verifier-worktree-ready", result)
        ready = self.root / "gate-ready"
        terminated = self.root / "gate-terminated"
        gate = self.root / "gate.py"
        gate.write_text(
            "import os, signal, time, subprocess\nfrom pathlib import Path\n"
            + ("signal.signal(signal.SIGTERM, signal.SIG_IGN)\n" if resistant else
               "def term(sig, frame):\n    time.sleep(" + str(delay) + ")\n    Path(" + repr(str(terminated)) + ").touch()\n    raise SystemExit(143)\n"
               "signal.signal(signal.SIGTERM, term)\n")
            + ("subprocess.Popen(['python3', '-c', " + repr(
                "import os,signal,time; from pathlib import Path; os.setpgrp(); "
                "signal.signal(signal.SIGTERM, signal.SIG_IGN); Path(" + repr(str(ready) + ".child")
                + ").write_text(str(os.getpid())+'\\n'); time.sleep(60)") + "])\n"
                "while not Path(" + repr(str(ready) + ".child") + ").exists(): time.sleep(.01)\n" if descendant else "")
            + ("Path('f').write_text('gate evidence')\n" if damage else "")
            + "Path(" + repr(str(ready)) + ").write_text(str(os.getpid())+'\\n')\n"
            + "while True: time.sleep(.01)\n")
        tree = self.git("merge-tree", "--write-tree", self.base, self.head)
        log = tempfile.TemporaryFile()
        self.addCleanup(log.close)
        child = subprocess.Popen(
            ["bash", str(SCRIPTS / "verify-pr.sh"), "--run-gate", "1", tree,
             self.base, self.head, str(self.wt), "--", "python3", str(gate)],
            cwd=self.repo, env=self.env, stdout=log, stderr=log, start_new_session=True)
        owner_path = next((self.common / "storyhook/verifier-lifecycle").glob("*.owner"))

        cleanup_timeout = self.settle_timeout
        def cleanup():
            owner = json.loads(owner_path.read_text())
            for sid in {child.pid, owner.get("session"), owner.get("gate_session")} - {None}:
                # Only these fixture-created sessions are eligible for cleanup.
                result = subprocess.run(["ps", "-axo", "pid="], capture_output=True, text=True, check=True)
                for raw in result.stdout.split():
                    pid = int(raw)
                    try:
                        if os.getsid(pid) == sid:
                            os.kill(pid, signal.SIGKILL)
                    except ProcessLookupError:
                        pass
            child.wait(timeout=cleanup_timeout)
        self.addCleanup(cleanup)
        pid = self.wait_pid(ready, child, log)
        if descendant:
            descendant_pid = self.wait_pid(Path(str(ready) + ".child"), child, log)
        os.killpg(child.pid, signal.SIGTERM)

        def recorded_sessions():
            owner = json.loads(owner_path.read_text())
            return {owner.get("session"), owner.get("gate_session")} - {None}
        self.wait_cancelled(child, pid, log, recorded_sessions)
        evidence = self.evidence(log)
        if not resistant:
            self.assertTrue(terminated.exists(), "TERM never reached the supervised gate\n" + evidence)
        owner = json.loads(owner_path.read_text())
        self.assertFalse(owner["gate_started"], f"{owner}\n{evidence}")
        self.assertIsNone(owner["gate_session"], f"{owner}\n{evidence}")
        if descendant:
            # A killed descendant may still await its reaper as a zombie.
            self.assertGone(descendant_pid)
        if damage:
            # Tracked evidence must survive even when restoration cannot succeed.
            retained = list(self.wt.parent.glob("verification-recovery-*/worktree/f"))
            self.assertEqual(len(retained), 1, evidence)
            self.assertEqual(retained[0].read_text(), "gate evidence")
            self.assertTrue((retained[0].parent.parent / "lease").is_dir())
        else:
            self.assertEqual(self.git("rev-parse", "HEAD", cwd=self.wt), self.base, evidence)
            self.assertEqual(self.git("status", "--porcelain", cwd=self.wt), "", evidence)
            self.assertEqual(self.ensure()["result"], "verifier-worktree-ready", evidence)

    def test_outer_cancellation_reaches_cooperative_gate_and_restores(self):
        """The same process-group signal Rust sends reaches nested sessions."""
        self.cancel_gate()

    def test_outer_cancellation_kills_resistant_gate_before_releasing_owner(self):
        """A TERM-resistant gate cannot keep writing after cancellation returns."""
        self.cancel_gate(resistant=True)

    def test_cancellation_preserves_tracked_gate_damage(self):
        """An interrupted gate's tracked writes remain recoverable evidence."""
        self.cancel_gate(damage=True)

    def test_cancellation_settles_resistant_subgroup_after_gate_leader_exits(self):
        """Session ownership includes children outside the leader's process group."""
        self.cancel_gate(descendant=True)

    def test_outer_lock_allows_the_gate_its_nested_cleanup_budget(self):
        """A cooperative gate can finish beyond the lock wrapper's old two seconds."""
        self.cancel_gate(delay=2.5)

    def test_invalid_termination_grace_cannot_launch_a_command(self):
        """Malformed cleanup policy must be refused before lock admission."""
        marker = self.root / "must-not-start"
        for value in ("0", "00", "-1", "", "wat", "1.5", "999999999999999999999"):
            result = self.command("bash", str(SCRIPTS / "machine-lock.sh"),
                                  "--termination-grace", value, "gate", "--",
                                  "touch", str(marker), check=False)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("termination-grace", result.stderr)
            self.assertFalse(marker.exists())

    def test_invalid_cleanup_budget_cannot_launch_a_session(self):
        """The internal owner also refuses bad policy before arbitrary execution."""
        marker = self.root / "must-not-start"
        for value in ("0", "-1", "", "wat", "1.5", "999999999999999999999"):
            self.env["STORYHOOK_VERIFIER_CLEANUP_GRACE_MS"] = value
            result = self.command("python3", str(SCRIPTS / "verifier-owner.py"),
                                  "run-json", str(self.common), str(self.wt),
                                  "--", "touch", str(marker))
            self.assertEqual(json.loads(result.stdout)["result"], "infrastructure-failure")
            self.assertIn("cleanup budget", result.stdout)
            self.assertFalse(marker.exists())

    def test_cancellation_at_handshake_release_is_not_lost(self):
        """Deliver a real TERM at the former release-before-handler boundary."""
        self.assertEqual(self.ensure()["result"], "verifier-worktree-ready")
        ready = self.root / "handshake-ready"
        terminated = self.root / "handshake-terminated"
        gate = self.root / "handshake-gate.py"
        gate.write_text(
            "import os,signal,time\nfrom pathlib import Path\n"
            "def term(sig, frame):\n    Path(" + repr(str(terminated)) + ").touch()\n    raise SystemExit(143)\n"
            "signal.signal(signal.SIGTERM,term)\n"
            "Path(" + repr(str(ready)) + ").write_text(str(os.getpid())+'\\n')\n"
            "while True: time.sleep(.01)\n")
        bootstrap = self.root / "handshake-bootstrap.py"
        bootstrap.write_text(
            "import importlib.util,os,signal,sys,time\nfrom pathlib import Path\n"
            "sys.dont_write_bytecode=True\nsys.path.insert(0," + repr(str(SCRIPTS)) + ")\n"
            "spec=importlib.util.spec_from_file_location('owner'," + repr(str(SCRIPTS / "verifier-owner.py")) + ")\n"
            "owner=importlib.util.module_from_spec(spec)\nspec.loader.exec_module(owner)\n"
            "write=os.write\n"
            "def release(fd, data):\n"
            "    result=write(fd,data)\n"
            "    if data==b'1':\n"
            f"        deadline=time.monotonic()+{self.milestone_deadline!r}\n"
            "        while not Path(" + repr(str(ready)) + ").exists():\n"
            "            if time.monotonic()>deadline: raise RuntimeError('gate never ready')\n"
            "            time.sleep(.01)\n"
            "        os.kill(os.getpid(),signal.SIGTERM)\n"
            "    return result\n"
            "owner.os.write=release\n"
            "sys.argv=" + repr(["verifier-owner.py", "run", str(self.common), str(self.wt), "--", "python3", str(gate)]) + "\n"
            "sys.exit(owner.main())\n")
        child, log = self.spawn("python3", str(bootstrap))
        pid = self.wait_pid(ready, child, log)
        self.addCleanup(lambda: self.stop_pid(pid))
        self.wait_settled(child, log)
        log.seek(0)
        self.assertTrue(terminated.exists(), log.read().decode())
        owner_path = next((self.common / "storyhook/verifier-lifecycle").glob("*.owner"))
        owner = json.loads(owner_path.read_text())
        self.assertTrue(owner["completed"], owner)
        self.assertIsNone(owner["session"], owner)

    # -- SH-695: an exited leader's survivors are reaped, never a halt --------

    def process_state(self, pid):
        """Report a pid's scheduler state, empty once the kernel has forgotten it."""
        result = subprocess.run(["ps", "-o", "stat=", "-p", str(pid)],
                                capture_output=True, text=True)
        return result.stdout.strip()

    def assertGone(self, pid):
        """A reaped survivor is either gone or a zombie awaiting launchd."""
        state = self.process_state(pid)
        self.assertTrue(state == "" or state.startswith("Z"), f"pid {pid} is alive: {state!r}")

    def owner_record(self):
        """Locate the one owner record this fixture's lifecycle wrote."""
        owner_path = next((self.common / "storyhook/verifier-lifecycle").glob("*.owner"))
        return owner_path, json.loads(owner_path.read_text())

    def gate_verdict(self, script, budget_ms=None):
        """Run one production gate through verify-pr.sh and return its verdict."""
        if budget_ms is not None:
            self.set_cleanup_budget(budget_ms)
        self.assertEqual(self.ensure()["result"], "verifier-worktree-ready")
        tree = self.git("merge-tree", "--write-tree", self.base, self.head)
        result = self.command("bash", str(SCRIPTS / "verify-pr.sh"), "--run-gate", "1", tree,
                              self.base, self.head, str(self.wt), "--", "bash", "-c", script)
        return json.loads(result.stdout)

    def attempt_log(self):
        """The per-attempt gate log verify-pr.sh wrote for pull request 1."""
        logs = self.common / "storyhook/verification-logs"
        return next(p for p in logs.glob("pr-1-*-attempt.*") if not p.name.endswith(".jsonl"))

    def test_exited_gate_with_orphan_returns_the_gate_status_and_reaps_it(self):
        """A red gate that leaves a test orphan is red, and the orphan dies with it."""
        orphan = self.root / "orphan-pid"
        verdict = self.gate_verdict(f"sleep 300 & echo $! > {shlex.quote(str(orphan))}; exit 3")
        pid = int(orphan.read_text())
        self.addCleanup(lambda: self.stop_pid(pid))
        self.assertEqual(verdict["result"], "tests-failed", verdict)
        self.assertNotIn("live writers", verdict["detail"])
        self.assertGone(pid)
        _, owner = self.owner_record()
        self.assertFalse(owner["gate_started"], owner)
        self.assertIsNone(owner["gate_session"], owner)
        self.assertIsNone(owner["gate_leader_exit"], owner)
        log = self.attempt_log().read_text()
        self.assertIn("verifier-owner: gate_session", log)
        self.assertIn(f"leaving survivors [{pid}]", log)
        self.assertEqual(self.ensure()["result"], "verifier-worktree-ready")

    def test_exited_gate_survivor_ignoring_term_is_killed_within_the_budget(self):
        """A TERM-resistant orphan is killed at the gate's grace, not refused."""
        orphan = self.root / "orphan-pid"
        verdict = self.gate_verdict(
            f"trap '' TERM; sleep 300 & echo $! > {shlex.quote(str(orphan))}; exit 5",
            budget_ms="8000")
        pid = int(orphan.read_text())
        self.addCleanup(lambda: self.stop_pid(pid))
        self.assertEqual(verdict["result"], "tests-failed", verdict)
        self.assertGone(pid)
        _, owner = self.owner_record()
        self.assertFalse(owner["gate_started"], owner)
        self.assertIsNone(owner["gate_leader_exit"], owner)

    # -- SH-767: the reaping eighth runs from the delivered SIGKILL -----------

    def slow_census(self, delay):
        """Delay only the supervisors' session census, the way gate load does.

        The real ps still answers and every other ps shape passes straight
        through; only the census's timing changes (SH-767).
        """
        real_ps = self.command("sh", "-c", "command -v ps").stdout.strip()
        wrapper = self.root / "slow-census"
        wrapper.mkdir()
        (wrapper / "ps").write_text(
            '#!/bin/bash\n'
            'if [ "$#" -eq 2 ] && [ "$1" = -axo ] && [ "$2" = pid=,stat= ]; then\n'
            f'  sleep {delay:.3f}\nfi\n'
            'exec ' + shlex.quote(real_ps) + ' "$@"\n')
        (wrapper / "ps").chmod(0o755)
        self.env["PATH"] = str(wrapper) + ":" + self.env["PATH"]

    @property
    def census_outlasting_the_reaping_eighth(self):
        """One census longer than the whole post-KILL reaping window."""
        return self.budget / 8 * CENSUS_OVERRUN

    def test_slow_census_cannot_refuse_a_delivered_gate_kill(self):
        """A kill is judged by a census begun after its reaping eighth closed.

        Before SH-767 the gate supervisor refused in the same pass that sent
        SIGKILL, from the scheduled deadline, naming members its pre-kill census
        saw; the orphan was already dead and the record kept a started gate.
        """
        self.set_cleanup_budget("4000")
        self.slow_census(self.census_outlasting_the_reaping_eighth)
        orphan = self.root / "orphan-pid"
        verdict = self.gate_verdict(
            f"trap '' TERM; sleep 300 & echo $! > {shlex.quote(str(orphan))}; exit 5")
        pid = int(orphan.read_text())
        self.addCleanup(lambda: self.stop_pid(pid))
        log = self.attempt_log().read_text()
        self.assertEqual(verdict["result"], "tests-failed", verdict)
        self.assertNotIn("cleanup_failure", verdict, log)
        self.assertNotIn("could not reap", log)
        self.assertGone(pid)
        _, owner = self.owner_record()
        self.assertFalse(owner["gate_started"], owner)
        self.assertIsNone(owner["gate_leader_exit"], owner)
        self.assertEqual(self.ensure()["result"], "verifier-worktree-ready")

    def test_slow_census_cannot_refuse_a_delivered_lifecycle_kill(self):
        """The lifecycle supervisor shares the same reaping eighth (SH-767)."""
        self.set_cleanup_budget("4000")
        self.assertEqual(self.ensure()["result"], "verifier-worktree-ready")
        self.slow_census(self.census_outlasting_the_reaping_eighth)
        orphan = self.root / "orphan-pid"
        result = self.owner("bash", "-c", f"trap '' TERM; sleep 300 & echo $! > {shlex.quote(str(orphan))}")
        pid = int(orphan.read_text())
        self.addCleanup(lambda: self.stop_pid(pid))
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("leaving survivors", result.stderr)
        self.assertNotIn("could not reap", result.stderr)
        self.assertGone(pid)
        _, owner = self.owner_record()
        self.assertTrue(owner["completed"], owner)
        self.assertIsNone(owner["session"], owner)

    def test_supplied_budget_reaches_supervision_in_decimal(self):
        """The spelling a case supplies reaches every supervisor normalized.

        "08000" is not octal, so a lost 10# stops the ungraced gate; graced to
        "032000" it is valid octal, so a lost 10# would deliver 13312 (SH-767).
        A passing gate never escalates, so neither run depends on load.
        """
        seen = self.root / "budget-seen"
        script = f'printf %s "$STORYHOOK_VERIFIER_CLEANUP_GRACE_MS" > {shlex.quote(str(seen))}'
        for grace, graced, spelled, delivered in ((1.0, False, "08000", "8000"),
                                                  (4.0, True, "032000", "32000")):
            with self.subTest(grace=grace):
                self.grace = grace
                self.set_cleanup_budget("08000", graced=graced)
                self.assertEqual(self.env["STORYHOOK_VERIFIER_CLEANUP_GRACE_MS"], spelled)
                verdict = self.gate_verdict(script)
                self.assertEqual(verdict["result"], "gate-passed", verdict)
                self.assertEqual(seen.read_text(), delivered)

    def recorded_leader_exit(self, alive):
        """Rewrite the record as a gate supervisor that died mid-reap leaves it."""
        self.assertEqual(self.ensure()["result"], "verifier-worktree-ready")
        sleeper = subprocess.Popen(["sleep", "60"], start_new_session=True)
        self.addCleanup(sleeper.wait, timeout=self.settle_timeout)
        self.addCleanup(lambda: self.stop_pid(sleeper.pid))
        if not alive:
            sleeper.kill()
            sleeper.wait(timeout=self.settle_timeout)
        owner_path, owner = self.owner_record()
        owner.update(gate_started=True, gate_session=sleeper.pid, gate_leader_exit=3)
        owner_path.write_text(json.dumps(owner))
        return owner_path, owner, self.ensure()

    def test_recorded_leader_exit_is_admitted_once_its_session_is_quiet(self):
        """A recorded leader exit plus an empty census is a finished gate session."""
        owner_path, _, result = self.recorded_leader_exit(alive=False)
        self.assertEqual(result["result"], "verifier-worktree-ready", result)
        owner = json.loads(owner_path.read_text())
        self.assertFalse(owner["gate_started"], owner)
        self.assertIsNone(owner.get("gate_leader_exit"), owner)

    def test_recorded_leader_exit_with_live_session_is_still_refused(self):
        """The census, not the recorded exit, decides; live writers still refuse."""
        owner_path, owner, result = self.recorded_leader_exit(alive=True)
        self.assertEqual(result["result"], "infrastructure-failure", result)
        self.assertIn("has writers", result["detail"])
        self.assertEqual(json.loads(owner_path.read_text()), owner)

    def test_outer_reaps_the_gate_session_when_the_inner_supervisor_is_gone(self):
        """A dead gate supervisor's session is the outer's to settle, not to refuse."""
        self.set_cleanup_budget("8000")
        self.assertEqual(self.ensure()["result"], "verifier-worktree-ready")
        terminated = self.root / "gate-terminated"
        member = self.root / "gate-member.sh"
        member.write_text("trap 'touch " + shlex.quote(str(terminated)) + "; exit 0' TERM\n"
                          "while :; do sleep 0.05; done\n")
        member_pid = self.root / "gate-member-pid"
        leader = self.root / "leader.py"
        leader.write_text(
            "import os, subprocess, sys\nfrom pathlib import Path\n"
            "sys.dont_write_bytecode = True\nsys.path.insert(0, " + repr(str(SCRIPTS)) + ")\n"
            "from verifier_state import read, save\n"
            "member = subprocess.Popen(['bash', " + repr(str(member)) + "], start_new_session=True,\n"
            "                          stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)\n"
            "Path(" + repr(str(member_pid)) + ").write_text(str(member.pid)+'\\n')\n"
            "gone = subprocess.Popen(['true'])\n"
            "gone.wait()\n"
            "path = " + repr(str(self.common / "storyhook/verifier-lifecycle")) + "\n"
            "record = next(Path(path).glob('*.owner'))\n"
            "owner = read(record)\n"
            "owner.update(gate_started=True, gate_session=member.pid, gate_supervisor=gone.pid)\n"
            "save(record, owner)\n")
        result = self.owner("python3", str(leader))
        pid = int(member_pid.read_text())
        self.addCleanup(lambda: self.stop_pid(pid))
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertGone(pid)
        self.assertTrue(terminated.exists(), "the outer never sent TERM into the gate session")
        self.assertIn("leaving survivors", result.stderr)
        _, owner = self.owner_record()
        self.assertTrue(owner["completed"], owner)
        self.assertTrue(owner["gate_started"], owner)
        # No supervisor recorded that leader's exit: the gate stays interrupted.
        result = self.ensure()
        self.assertEqual(result["result"], "infrastructure-failure", result)
        self.assertIn("ambiguous ownership", result["detail"])

    def test_exited_leader_stays_pinned_until_its_session_is_quiet(self):
        """The leader remains a zombie, so its session id cannot be reused mid-reap."""
        if not hasattr(os, "waitid"):
            self.skipTest("this Python cannot observe an exit without reaping it")
        self.set_cleanup_budget("8000")
        self.assertEqual(self.ensure()["result"], "verifier-worktree-ready")
        orphan = self.root / "orphan-pid"
        leader = self.root / "leader-pid"
        tree = self.git("merge-tree", "--write-tree", self.base, self.head)
        # The verdict is read apart from the gate's own noise, which is kept
        # as failure evidence; the orphan's stdio goes to the attempt log, so
        # regular files are enough here.
        verdict_file = tempfile.TemporaryFile()
        self.addCleanup(verdict_file.close)
        diagnostics = tempfile.TemporaryFile()
        self.addCleanup(diagnostics.close)
        child = subprocess.Popen(
            ["bash", str(SCRIPTS / "verify-pr.sh"), "--run-gate", "1", tree, self.base,
             self.head, str(self.wt), "--", "bash", "-c",
             "trap '' TERM; echo $$ > " + shlex.quote(str(leader)) + "; sleep 300 & echo $! > "
             + shlex.quote(str(orphan)) + "; exit 5"],
            cwd=self.repo, env=self.env, stdout=verdict_file, stderr=diagnostics)
        self.addCleanup(child.wait, timeout=self.milestone_deadline)
        self.addCleanup(lambda: child.poll() is None and child.kill())
        orphan_pid = self.wait_pid(orphan, child, diagnostics)
        self.addCleanup(lambda: self.stop_pid(orphan_pid))
        leader_pid = self.wait_pid(leader, child, diagnostics)
        # A reaping poll also shows a zombie for one tick; the pin is that
        # the zombie outlives the TERM-resistant orphan's whole grace, which
        # is a quarter of the budget for the gate session (SH-686).
        zombie_seen = []
        started = time.monotonic()
        patience = self.patience(self.settle_timeout, started)
        while child.poll() is None:
            if patience.expired(time.monotonic()):
                self.settle_failure("supervisor did not settle the exited leader",
                                    diagnostics, started, patience.allowance)
            if self.process_state(leader_pid).startswith("Z"):
                zombie_seen.append(time.monotonic())
            time.sleep(.01)
        self.assertEqual(child.returncode, 0, self.evidence(diagnostics))
        self.assertTrue(zombie_seen, "the exited leader was never observed as a zombie")
        self.assertGreaterEqual(zombie_seen[-1] - zombie_seen[0], self.budget / 4 / 2,
                                "the exited leader was reaped before its session settled")
        self.assertGone(orphan_pid)
        verdict_file.seek(0)
        verdict = json.loads(verdict_file.read().decode())
        self.assertEqual(verdict["result"], "tests-failed", verdict)

    def test_invalid_cleanup_budget_refuses_before_marking_the_gate_started(self):
        """A refused budget must leave no started gate behind in the record."""
        self.assertEqual(self.ensure()["result"], "verifier-worktree-ready")
        status = self.root / "gate-status"
        snapshot = self.root / "owner-snapshot"
        owner_path, _ = self.owner_record()
        leader = self.root / "leader.sh"
        leader.write_text(
            "STORYHOOK_VERIFIER_CLEANUP_GRACE_MS=wat python3 " + shlex.quote(str(SCRIPTS / "verifier-owner.py"))
            + " gate " + shlex.quote(str(self.common)) + " " + shlex.quote(str(self.wt)) + " -- true\n"
            "echo $? > " + shlex.quote(str(status)) + "\n"
            "cp " + shlex.quote(str(owner_path)) + " " + shlex.quote(str(snapshot)) + "\n")
        result = self.owner("bash", str(leader))
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(status.read_text().strip(), "1")
        self.assertIn("cleanup budget", result.stderr)
        owner = json.loads(snapshot.read_text())
        self.assertFalse(owner["gate_started"], owner)
        self.assertIsNone(owner.get("gate_supervisor"), owner)
        self.assertEqual(self.ensure()["result"], "verifier-worktree-ready")



class HarnessObservation(unittest.TestCase):
    """Pin fixture observation independently of Git and production supervision."""

    def setUp(self):
        """Own only temporary files and the processes each observation starts."""
        tmp = tempfile.TemporaryDirectory(prefix="sh698-", dir="/tmp")
        self.addCleanup(tmp.cleanup)
        self.root = Path(tmp.name)
        self.harness = VerifierLifecycle()
        self.harness.repo = self.root
        self.harness.env = dict(os.environ)
        self.harness.set_cleanup_budget(CLEANUP_BUDGET_MS)
        self.addCleanup(self.harness.doCleanups)
        # Observation is pinned at idle; ContentionGrace owns contention.
        idle = mock.patch.object(load_grace, "contention", return_value=0.5)
        idle.start()
        self.addCleanup(idle.stop)

    def test_pid_reader_waits_for_empty_and_partial_records(self):
        """The writer completes only after the reader observes pending content."""
        for prefix in ("", "12"):
            with self.subTest(prefix=prefix):
                ready = self.root / ("partial" if prefix else "empty")
                ready.write_text(prefix)
                receive, release = os.pipe()
                try:
                    code = ("import os; from pathlib import Path; "
                            "os.read(0, 1); Path(" + repr(str(ready))
                            + ").write_text('12345\\n')")
                    log = tempfile.TemporaryFile()
                    self.addCleanup(log.close)
                    child = subprocess.Popen([sys.executable, "-c", code], stdin=receive,
                                             stdout=log, stderr=log)
                    self.addCleanup(child.wait, timeout=MILESTONE_DEADLINE)
                    self.addCleanup(lambda child=child: child.poll() is None and child.kill())
                    sleep = time.sleep
                    released = []

                    def observed_pending(period):
                        if not released:
                            released.append(True)
                            os.write(release, b"1")
                        sleep(period)

                    with mock.patch.object(time, "sleep", side_effect=observed_pending):
                        self.assertEqual(self.harness.wait_pid(ready, child, log), 12345)
                    self.assertTrue(released, "reader accepted an incomplete record")
                finally:
                    os.close(receive)
                    os.close(release)

    def test_dead_child_reports_status_and_evidence_without_poll_sleep(self):
        """A reaped child cannot publish a missing milestone later."""
        child, log = self.harness.spawn(sys.executable, "-c",
                                        "print('fixture evidence'); raise SystemExit(3)")
        child.wait(timeout=MILESTONE_DEADLINE)
        with mock.patch.object(time, "sleep", side_effect=AssertionError("waited on dead child")):
            with self.assertRaisesRegex(AssertionError, "exited 3") as failure:
                self.harness.wait_path(self.root / "missing", child, log)
        self.assertIn("fixture evidence", str(failure.exception))
        self.assertIn(str(child.pid), str(failure.exception))

    def test_completed_record_is_readable_after_writer_exits(self):
        """Exit and final publication may both occur between reader polls."""
        ready = self.root / "pid"
        child, log = self.harness.spawn(sys.executable, "-c",
            "from pathlib import Path; Path(" + repr(str(ready)) + ").write_text('12345\\n')")
        child.wait(timeout=MILESTONE_DEADLINE)
        self.assertEqual(self.harness.wait_pid(ready, child, log), 12345)

    def test_exit_observation_rechecks_the_final_publication(self):
        """Publishing between a read and poll must not look like early death."""
        ready = self.root / "pid"
        child = mock.Mock(pid=12345)

        def exit_after_publish():
            ready.write_text("12345\n")
            return 0

        child.poll.side_effect = exit_after_publish
        with tempfile.TemporaryFile() as log:
            self.assertEqual(self.harness.wait_pid(ready, child, log), 12345)

    def test_incomplete_pid_at_exit_is_diagnostic(self):
        """An incomplete final record fails with content and child evidence."""
        for text in ("", "123"):
            with self.subTest(text=text), tempfile.TemporaryFile() as log:
                ready = self.root / "pid"
                ready.write_text(text)
                child = mock.Mock(pid=12345)
                child.poll.return_value = 3
                with self.assertRaisesRegex(AssertionError, "exited 3") as failure:
                    self.harness.wait_pid(ready, child, log)
                self.assertIn(repr(text), str(failure.exception))

    def test_malformed_completed_pid_records_fail_before_signalling(self):
        """Only a positive ASCII decimal PID is an owned cleanup target."""
        for text in ("0\n", "-1\n", "wat\n", "12\n34\n", "١٢\n", "12 \n"):
            with self.subTest(text=text), tempfile.TemporaryFile() as log:
                ready = self.root / "pid"
                ready.write_text(text)
                child = mock.Mock(pid=12345)
                child.poll.return_value = None
                with self.assertRaisesRegex(AssertionError, "malformed PID"):
                    self.harness.wait_pid(ready, child, log)

    def test_pending_pid_uses_one_deadline_and_reports_load(self):
        """File creation does not reset the allowance for completing its record."""
        ready = self.root / "pid"
        child = mock.Mock(pid=12345)
        child.poll.return_value = None
        with tempfile.TemporaryFile() as log:
            log.write(b"fixture evidence")
            log.flush()
            with mock.patch.object(time, "monotonic", side_effect=[0, 0, MILESTONE_DEADLINE]), \
                 mock.patch.object(time, "sleep", side_effect=lambda _: ready.write_text("12")), \
                 mock.patch.object(os, "getloadavg", return_value=(48, 37, 48)):
                with self.assertRaisesRegex(AssertionError, "deadline") as failure:
                    self.harness.wait_pid(ready, child, log)
            for evidence in (str(ready), "12345", "48", "fixture evidence", repr("12"),
                             str(MILESTONE_DEADLINE)):
                self.assertIn(evidence, str(failure.exception))

    def test_live_writer_can_publish_after_a_cleanup_allowance_has_elapsed(self):
        """Startup patience exceeds cleanup; a loaded but live child can finish."""
        ready = self.root / "pid"
        child = mock.Mock(pid=12345)
        child.poll.return_value = None
        with tempfile.TemporaryFile() as log:
            with mock.patch.object(time, "monotonic", side_effect=[0, self.harness.settle_timeout]), \
                 mock.patch.object(time, "sleep", side_effect=lambda _: ready.write_text("12345\n")):
                self.assertEqual(self.harness.wait_pid(ready, child, log), 12345)

    def test_existence_only_marker_does_not_require_a_pid_record(self):
        """An empty touch marker remains sufficient for non-PID milestones."""
        ready = self.root / "marker"
        ready.touch()
        child = mock.Mock(pid=12345)
        with tempfile.TemporaryFile() as log:
            self.harness.wait_path(ready, child, log)
        child.poll.assert_not_called()

    def test_valid_budget_overrides_preserve_spelling_and_derive_settlement(self):
        """The same input governs supervision and its observation allowance."""
        for milliseconds in ("30000", "8000", "16000", "08000"):
            with self.subTest(milliseconds=milliseconds):
                self.harness.set_cleanup_budget(milliseconds)
                self.assertEqual(self.harness.env["STORYHOOK_VERIFIER_CLEANUP_GRACE_MS"], milliseconds)
                self.assertEqual(self.harness.settle_timeout, int(milliseconds) / 1000 + MARGIN)

    def test_wrapper_exit_does_not_restart_gate_disappearance_deadline(self):
        """A slow wrapper consumes the same allowance as its surviving gate."""
        self.harness.set_cleanup_budget("8000")
        child = mock.Mock(pid=12345)
        now = [0]

        def wrapper_exits(**kwargs):
            self.assertEqual(kwargs["timeout"], self.harness.settle_timeout)
            now[0] = self.harness.settle_timeout - POLL_INTERVAL

        child.wait.side_effect = wrapper_exits
        with mock.patch.object(time, "monotonic", side_effect=lambda: now[0]), \
             mock.patch.object(time, "sleep", side_effect=lambda _: now.__setitem__(0, now[0] + POLL_INTERVAL)), \
             mock.patch.object(os, "kill"):
            with self.assertRaisesRegex(AssertionError, "gate session pid=23456 survived"):
                self.harness.wait_cancelled(child, 23456)
        self.assertEqual(now[0], self.harness.settle_timeout)

    def test_settle_failure_carries_wrapper_and_attempt_logs(self):
        """A settle timeout names every log that could explain it (SH-767)."""
        self.harness.common = self.root / "common"
        attempts = self.harness.common / "storyhook/verification-logs"
        attempts.mkdir(parents=True)
        (attempts / "pr-1-tree-attempt.abc").write_text("attempt evidence")
        (attempts / "pr-1-tree-attempt.abc.compiler.jsonl").write_text("compiler noise")
        child = mock.Mock(pid=12345)
        now = [0]

        def times_out(timeout):
            now[0] += timeout
            raise subprocess.TimeoutExpired("fixture", timeout)

        child.wait.side_effect = times_out
        with tempfile.TemporaryFile() as log:
            log.write(b"wrapper evidence")
            log.flush()
            with mock.patch.object(time, "monotonic", side_effect=lambda: now[0]), \
                 self.assertRaisesRegex(AssertionError, "did not settle") as failure:
                self.harness.wait_settled(child, log)
        for evidence in ("12345", "wrapper evidence", "attempt evidence",
                         "allowance=" + str(self.harness.settle_timeout), "load="):
            self.assertIn(evidence, str(failure.exception))
        self.assertNotIn("compiler noise", str(failure.exception))

    def test_cancellation_waits_for_recorded_sessions_on_the_same_deadline(self):
        """A record is read only after its sessions are quiet, within one allowance."""
        self.harness.set_cleanup_budget("8000")
        for quiet_after in (2, None):
            with self.subTest(quiet_after=quiet_after):
                child = mock.Mock(pid=12345)
                now = [0]
                censuses = []

                def census(sid):
                    censuses.append(sid)
                    return [] if quiet_after and len(censuses) >= quiet_after else [34567]

                with mock.patch.object(time, "monotonic", side_effect=lambda: now[0]), \
                     mock.patch.object(time, "sleep", side_effect=lambda period: now.__setitem__(0, now[0] + period)), \
                     mock.patch.object(os, "kill", side_effect=ProcessLookupError), \
                     mock.patch.object(self.harness, "session_pids", side_effect=census):
                    if quiet_after:
                        self.harness.wait_cancelled(child, 23456, sessions=lambda: {45678})
                        self.assertEqual(censuses, [45678] * quiet_after)
                    else:
                        with self.assertRaisesRegex(AssertionError, r"recorded sessions still have members \[34567\]"):
                            self.harness.wait_cancelled(child, 23456, sessions=lambda: {45678})
                        self.assertGreaterEqual(now[0], self.harness.settle_timeout)

    def test_spawn_cleanup_keeps_the_budget_given_to_that_child(self):
        """Changing the next fixture input cannot tighten an existing cleanup."""
        self.harness.set_cleanup_budget("30000")
        original_timeout = self.harness.settle_timeout
        with mock.patch.object(subprocess, "Popen") as spawn:
            child = spawn.return_value
            child.poll.return_value = None
            self.harness.spawn("fixture-command")
        self.harness.set_cleanup_budget("8000")
        self.harness.doCleanups()
        child.kill.assert_called_once()
        child.wait.assert_called_once_with(timeout=original_timeout)


class ContentionGrace(unittest.TestCase):
    """Pin the SH-347 grace policy the harness applies under contention (SH-767)."""

    def harness(self, grace):
        """An observation-only harness at a chosen grace."""
        harness = VerifierLifecycle()
        harness.env = {}
        harness.grace = grace
        return harness

    def test_contention_is_one_minute_load_per_core(self):
        """The reading is runnable threads per core; an unobtainable one is None."""
        with mock.patch.object(os, "getloadavg", return_value=(48, 37, 20)), \
             mock.patch.object(load_grace, "cores", return_value=10):
            self.assertEqual(load_grace.contention(), 4.8)
        with mock.patch.object(os, "getloadavg", side_effect=OSError("unobtainable")):
            self.assertIsNone(load_grace.contention())

    def test_cores_prefer_the_process_count_and_never_reach_zero(self):
        """An affinity-limited count wins; older interpreters fall back."""
        for process_count, machine_count, expected in ((4, 10, 4), (None, 10, 10), (None, None, 1)):
            with self.subTest(process_count=process_count, machine_count=machine_count), \
                 mock.patch.object(os, "process_cpu_count", return_value=process_count, create=True), \
                 mock.patch.object(os, "cpu_count", return_value=machine_count):
                self.assertEqual(load_grace.cores(), expected)
        with mock.patch.object(os, "process_cpu_count", None, create=True), \
             mock.patch.object(os, "cpu_count", return_value=6):
            self.assertEqual(load_grace.cores(), 6)

    def test_multiplier_is_exactly_one_without_contention(self):
        """At or below one thread per core every allowance is its idle value."""
        for ratio in (None, 0, 0.5, 1):
            with self.subTest(ratio=ratio):
                self.assertEqual(load_grace.multiplier(ratio, MAX_GRACE), 1.0)

    def test_multiplier_follows_contention_up_to_the_ceiling(self):
        """Above contention the grace is the ratio, capped so no wait passes 15 minutes."""
        self.assertEqual(load_grace.multiplier(4.8, MAX_GRACE), 4.8)
        self.assertEqual(load_grace.multiplier(1000, MAX_GRACE), MAX_GRACE)
        self.assertAlmostEqual(MILESTONE_DEADLINE * MAX_GRACE, PATIENCE_CEILING)

    def test_graced_spelling_keeps_leading_zeros_and_whole_milliseconds(self):
        """Grace scales the value only; its spelling still exercises normalization."""
        for milliseconds, grace, expected in (("30000", 1.0, "30000"), ("08000", 1.0, "08000"),
                                              ("08000", 4.0, "032000"), ("16000", 1.5, "24000"),
                                              ("8000", 4.8, "38400"), ("4000", 1.0001, "4001"),
                                              ("0004000", 2.0, "0008000")):
            with self.subTest(milliseconds=milliseconds, grace=grace):
                self.assertEqual(load_grace.graced_spelling(milliseconds, grace), expected)
        for milliseconds in ("30000", "16000", "8000", "08000", "4000"):
            with self.subTest(ceiling=milliseconds):
                self.assertLessEqual(len(load_grace.graced_spelling(milliseconds, MAX_GRACE)), 8)

    def test_description_names_the_reading_and_the_grace(self):
        """A grace nobody can see is a verdict that depends on unreported state."""
        with mock.patch.object(load_grace, "cores", return_value=10):
            described = load_grace.describe(4.8, 4.8)
            for text in ("contention=4.80", "cores=10", "multiplier=4.80"):
                self.assertIn(text, described)
            self.assertIn("contention=unavailable", load_grace.describe(None, 1.0))

    def test_patience_reads_no_load_before_its_allowance(self):
        """An unexpired wait costs no reading and reports what is left."""
        patience = load_grace.Patience(10, 1.0, MAX_GRACE, 100)
        with mock.patch.object(load_grace, "contention", side_effect=AssertionError("resampled early")):
            self.assertFalse(patience.expired(109.99))
            self.assertEqual(patience.remaining(104), 6)
            self.assertEqual(patience.remaining(120), 0)

    def test_patience_expires_when_contention_has_not_risen(self):
        """A reading at or below the granted grace neither extends nor shrinks."""
        for granted, ratio in ((1.0, 0.5), (2.0, 1.5), (2.0, 2.0)):
            with self.subTest(granted=granted, ratio=ratio), \
                 mock.patch.object(load_grace, "contention", return_value=ratio):
                patience = load_grace.Patience(10 * granted, granted, MAX_GRACE, 0)
                self.assertTrue(patience.expired(10 * granted))
                self.assertEqual(patience.allowance, 10 * granted)

    def test_patience_extends_in_proportion_and_says_so(self):
        """Rising contention resets the deadline before it fires, never silently."""
        patience = load_grace.Patience(10, 1.0, MAX_GRACE, 0)
        with mock.patch.object(load_grace, "contention", return_value=3.0), \
             mock.patch.object(sys, "stderr", new_callable=io.StringIO) as stderr:
            self.assertFalse(patience.expired(10))
        self.assertEqual(patience.allowance, 30)
        self.assertIn("load-grace", stderr.getvalue())
        self.assertIn("30.000", stderr.getvalue())
        with mock.patch.object(load_grace, "contention", return_value=2.0):
            self.assertFalse(patience.expired(29))
            self.assertTrue(patience.expired(30))
        self.assertEqual(patience.allowance, 30)

    def test_patience_extension_stops_at_the_ceiling(self):
        """No reading can extend one wait past the recorded 15-minute tolerance."""
        patience = load_grace.Patience(MILESTONE_DEADLINE, 1.0, MAX_GRACE, 0)
        with mock.patch.object(load_grace, "contention", return_value=1000), \
             mock.patch.object(sys, "stderr", new_callable=io.StringIO):
            self.assertFalse(patience.expired(MILESTONE_DEADLINE))
            self.assertAlmostEqual(patience.allowance, PATIENCE_CEILING)
            self.assertTrue(patience.expired(PATIENCE_CEILING))

    def test_contended_case_scales_every_allowance_together(self):
        """Budget, settlement and startup patience keep their order under grace."""
        harness = self.harness(4.8)
        harness.set_cleanup_budget("8000")
        self.assertEqual(harness.env["STORYHOOK_VERIFIER_CLEANUP_GRACE_MS"], "38400")
        self.assertEqual(harness.budget, 38.4)
        self.assertAlmostEqual(harness.settle_timeout, 38.4 + MARGIN * 4.8)
        self.assertAlmostEqual(harness.milestone_deadline, MILESTONE_DEADLINE * 4.8)
        self.assertGreater(harness.milestone_deadline, harness.settle_timeout)
        self.assertGreater(harness.settle_timeout, harness.budget)
        harness.set_cleanup_budget("08000", graced=False)
        self.assertEqual(harness.env["STORYHOOK_VERIFIER_CLEANUP_GRACE_MS"], "08000")
        self.assertEqual(harness.budget, 8.0)
        self.assertAlmostEqual(harness.settle_timeout, 8.0 + MARGIN * 4.8)

    def test_budgets_outside_production_policy_are_refused(self):
        """The fixture refuses what verify-pr.sh and verifier-owner.py refuse."""
        harness = self.harness(1.0)
        for milliseconds in ("", "0", "3999", "wat", "1.5", "123456789", "\u0661\u0662\u0660\u0660\u0660"):
            with self.subTest(milliseconds=milliseconds), self.assertRaises(AssertionError):
                harness.set_cleanup_budget(milliseconds)
        with self.assertRaises(AssertionError):
            self.harness(MAX_GRACE).set_cleanup_budget("99999999")

    def test_sampling_is_silent_at_idle_and_named_under_contention(self):
        """A case graced by load says so once, with the reading that caused it."""
        for ratio, grace, named in ((0.5, 1.0, False), (None, 1.0, False), (4.8, 4.8, True)):
            harness = self.harness(1.0)
            with self.subTest(ratio=ratio), \
                 mock.patch.object(load_grace, "contention", return_value=ratio), \
                 mock.patch.object(load_grace, "cores", return_value=10), \
                 mock.patch.object(sys, "stderr", new_callable=io.StringIO) as stderr:
                harness.sample_grace()
                self.assertEqual(harness.grace, grace)
                self.assertEqual("multiplier=4.80" in stderr.getvalue(), named)


if __name__ == "__main__":
    unittest.main()
