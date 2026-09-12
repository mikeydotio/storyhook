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


class VerifierLifecycle(unittest.TestCase):
    """Each case owns its repository, Git configuration, locks and evidence."""

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
                                capture_output=True, text=True, timeout=MILESTONE_DEADLINE)
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
        self.assertEqual(child.wait(timeout=self.settle_timeout), 0)
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
            elapsed = time.monotonic() - started
            if status is not None or elapsed >= MILESTONE_DEADLINE:
                reason = f"exited {status}" if status is not None else "deadline expired"
                # pread leaves the child's shared log offset untouched.
                evidence = os.pread(log.fileno(), os.fstat(log.fileno()).st_size, 0).decode(errors="replace")
                self.fail(f"child pid={child.pid} {reason} before publishing {path}; "
                          f"elapsed={elapsed:.3f}s allowance={MILESTONE_DEADLINE}s "
                          f"load={os.getloadavg()} content={content!r}\n{evidence}")
            time.sleep(POLL_INTERVAL)

    def set_cleanup_budget(self, milliseconds):
        """Supply a valid fixture budget while preserving its input spelling."""
        self.assertTrue(milliseconds.isascii() and milliseconds.isdecimal())
        self.assertTrue(4000 <= int(milliseconds) <= 99999999)
        self.env["STORYHOOK_VERIFIER_CLEANUP_GRACE_MS"] = milliseconds
        self.budget = int(milliseconds) / 1000

    @property
    def settle_timeout(self):
        """Allow the supplied cancellation ladder plus scheduling/reap margin."""
        return self.budget + MARGIN

    def wait_cancelled(self, child, pid):
        """Observe the cancelled wrapper and gate within one shared allowance."""
        deadline = time.monotonic() + self.settle_timeout
        try:
            child.wait(timeout=max(0, deadline - time.monotonic()))
        except subprocess.TimeoutExpired:
            self.fail(f"verifier pid={child.pid} did not finish cancellation within {self.settle_timeout}s")
        while True:
            try:
                os.kill(pid, 0)
            except ProcessLookupError:
                return
            if time.monotonic() >= deadline:
                self.fail(f"gate session pid={pid} survived cancellation within {self.settle_timeout}s")
            time.sleep(POLL_INTERVAL)

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
        self.assertEqual(child.wait(timeout=self.settle_timeout), 0)
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
        child.wait(timeout=self.settle_timeout)
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
                status = owner.wait(timeout=self.settle_timeout)
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
                status = owner.wait(timeout=self.settle_timeout)
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
        child.wait(timeout=self.settle_timeout)
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
        self.wait_cancelled(child, pid)
        if not resistant:
            self.assertTrue(terminated.exists(), "TERM never reached the supervised gate")
        owner = json.loads(owner_path.read_text())
        self.assertFalse(owner["gate_started"], owner)
        self.assertIsNone(owner["gate_session"], owner)
        if descendant:
            with self.assertRaises(ProcessLookupError):
                os.kill(descendant_pid, 0)
        if damage:
            # Tracked evidence must survive even when restoration cannot succeed.
            retained = list(self.wt.parent.glob("verification-recovery-*/worktree/f"))
            self.assertEqual(len(retained), 1)
            self.assertEqual(retained[0].read_text(), "gate evidence")
            self.assertTrue((retained[0].parent.parent / "lease").is_dir())
        else:
            self.assertEqual(self.git("rev-parse", "HEAD", cwd=self.wt), self.base)
            self.assertEqual(self.git("status", "--porcelain", cwd=self.wt), "")
            self.assertEqual(self.ensure()["result"], "verifier-worktree-ready")

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
            f"        deadline=time.monotonic()+{MILESTONE_DEADLINE!r}\n"
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
        child.wait(timeout=self.settle_timeout)
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
        # The verdict is read apart from the gate's own noise; the orphan's
        # stdio goes to the attempt log, so a regular file is enough here.
        verdict_file = tempfile.TemporaryFile()
        self.addCleanup(verdict_file.close)
        child = subprocess.Popen(
            ["bash", str(SCRIPTS / "verify-pr.sh"), "--run-gate", "1", tree, self.base,
             self.head, str(self.wt), "--", "bash", "-c",
             "trap '' TERM; echo $$ > " + shlex.quote(str(leader)) + "; sleep 300 & echo $! > "
             + shlex.quote(str(orphan)) + "; exit 5"],
            cwd=self.repo, env=self.env, stdout=verdict_file, stderr=subprocess.DEVNULL)
        self.addCleanup(child.wait, timeout=MILESTONE_DEADLINE)
        self.addCleanup(lambda: child.poll() is None and child.kill())
        orphan_pid = self.wait_pid(orphan, child, verdict_file)
        self.addCleanup(lambda: self.stop_pid(orphan_pid))
        leader_pid = self.wait_pid(leader, child, verdict_file)
        # A reaping poll also shows a zombie for one tick; the pin is that
        # the zombie outlives the TERM-resistant orphan's whole grace, which
        # is a quarter of the budget for the gate session (SH-686).
        zombie_seen = []
        deadline = time.monotonic() + self.settle_timeout
        while child.poll() is None:
            self.assertLess(time.monotonic(), deadline, "supervisor did not settle the exited leader")
            if self.process_state(leader_pid).startswith("Z"):
                zombie_seen.append(time.monotonic())
            time.sleep(.01)
        self.assertEqual(child.returncode, 0)
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


if __name__ == "__main__":
    unittest.main()
