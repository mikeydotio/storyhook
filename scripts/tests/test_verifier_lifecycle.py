#!/usr/bin/env python3
"""Real Git regressions for shared verifier ownership and recovery (SH-683)."""

import json
import os
import signal
import sys
import time
from pathlib import Path
import subprocess
import tempfile
import unittest

SCRIPTS = Path(__file__).resolve().parents[1]


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
                                capture_output=True, text=True, timeout=30)
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
        """A child result cannot precede a contradictory supervisor verdict."""
        ready = self.root / "survivor-pid"
        code = "import os,time; pid=os.fork(); "
        code += "\nif pid==0: time.sleep(30); os._exit(0)"
        code += "\nopen(" + repr(str(ready)) + ",'w').write(str(pid))"
        code += "\nprint('{\"result\":\"ready\"}',flush=True)"
        child, log = self.spawn(sys.executable, str(SCRIPTS / "verifier-owner.py"),
                                "run-json", str(self.common), str(self.wt), "--",
                                sys.executable, "-c", code)
        self.wait_path(ready)
        self.addCleanup(lambda: self.stop_pid(int(ready.read_text())))
        self.assertEqual(child.wait(timeout=15), 0)
        log.seek(0)
        output = log.read().decode()
        json_lines = [line for line in output.splitlines() if line.startswith("{")]
        self.assertEqual(len(json_lines), 1, output)
        verdict = json.loads(json_lines[0])
        self.assertEqual(verdict["result"], "infrastructure-failure")
        self.assertIn("live writers", verdict["detail"])

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

    def wait_path(self, path):
        """Synchronize on a real child milestone, bounded by fixture timeout."""
        deadline = time.monotonic() + 15
        while not path.exists():
            if time.monotonic() >= deadline:
                self.fail(f"child did not publish {path}")
            time.sleep(0.01)

    def spawn(self, *args):
        """Own a child and regular log files so survivors cannot hold pipes open."""
        log = tempfile.TemporaryFile()
        self.addCleanup(log.close)
        child = subprocess.Popen(args, cwd=self.repo, env=self.env, stdout=log, stderr=log)
        def cleanup():
            if child.poll() is None:
                child.kill()
            child.wait(timeout=10)
        self.addCleanup(cleanup)
        return child, log

    def test_concurrent_preflight_cannot_mutate_live_owned_checkout(self):
        """Kernel ownership excludes preflight even if no gate wrapper is alive."""
        self.assertEqual(self.ensure()["result"], "verifier-worktree-ready")
        ready = self.root / "ready"
        release = self.root / "release"
        code = "from pathlib import Path; import time; Path(\"" + str(ready) + "\").touch(); "
        code += "\nwhile not Path(\"" + str(release) + "\").exists(): time.sleep(.01)"
        child, _ = self.spawn("python3", str(SCRIPTS / "verifier-owner.py"), "run",
                              str(self.common), str(self.wt), "--", "python3", "-c", code)
        self.wait_path(ready)
        pointer = (self.wt / ".git").read_bytes()
        result = self.ensure()
        self.assertEqual(result["result"], "infrastructure-failure")
        self.assertIn("live verifier owner", result["detail"])
        self.assertEqual((self.wt / ".git").read_bytes(), pointer)
        release.touch()
        self.assertEqual(child.wait(timeout=15), 0)
        self.assertEqual(self.ensure()["result"], "verifier-worktree-ready")

    def test_killed_supervisor_cannot_reclaim_descriptor_closing_child(self):
        """A recorded surviving session protects files after every flock copy closes."""
        self.assertEqual(self.ensure()["result"], "verifier-worktree-ready")
        ready = self.root / "child-pid"
        code = "import os,time; os.closerange(3,256); open(" + repr(str(ready)) + ",'w').write(str(os.getpid())); time.sleep(30)"
        child, _ = self.spawn("python3", str(SCRIPTS / "verifier-owner.py"), "run",
                              str(self.common), str(self.wt), "--", "python3", "-c", code)
        self.wait_path(ready)
        pid = int(ready.read_text())
        self.addCleanup(lambda: self.stop_pid(pid))
        child.kill()
        child.wait(timeout=10)
        result = self.ensure()
        self.assertEqual(result["result"], "infrastructure-failure")
        self.assertIn("live owner session", result["detail"])
        self.assertIn(str(pid), result["detail"])
        self.stop_pid(pid)

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
        self.wait_path(ready)
        owner_path = next((self.common / "storyhook/verifier-lifecycle").glob("*.owner"))
        owner = json.loads(owner_path.read_text())
        os.killpg(owner["session"], signal.SIGKILL)
        child.wait(timeout=15)
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


if __name__ == "__main__":
    unittest.main()
