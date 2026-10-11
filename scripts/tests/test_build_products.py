"""SH-835 focused ownership regressions; no live project, cache, or host policy."""

import fcntl
import importlib.util
import json
import os
import signal
import shutil
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch

SCRIPTS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPTS))
import build_products as products
from host_admission.policy import Refusal

spec = importlib.util.spec_from_file_location("cargo_managed", SCRIPTS / "cargo-managed.py")
cargo = importlib.util.module_from_spec(spec)
spec.loader.exec_module(cargo)


class ProductOwnershipTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="sh835-products-")
        self.root = Path(self.tmp.name)
        self.root.chmod(0o700)
        self.retain = False

    def tearDown(self):
        if self.retain:
            self.tmp._finalizer.detach()
            print(f"Unsettled native fixture retained: {self.root}", file=sys.stderr)
        else:
            self.tmp.cleanup()

    def custody(self):
        return products.ProductCustody(self.root, [sys.executable, "fixture"])

    def test_shared_wait_has_bounded_refusal_without_launching(self):
        with products.ProductLease(self.root, reclaim=True):
            with self.assertRaisesRegex(Refusal, "timed out waiting"):
                products.ProductLease(self.root, wait_seconds=0.02)
        self.assertEqual([p.name for p in self.root.iterdir()], ["products.lock"])

    def test_real_managed_rebuild_waits_for_short_detach_guard(self):
        repo = self.root / "repo"
        repo.mkdir()
        subprocess.run(["git", "init", "-q", str(repo)], check=True, timeout=10)
        root = products.namespace(repo)
        marker = repo / "products" / "rebuilt"
        script = ("import sys;sys.path.insert(0," + repr(str(SCRIPTS)) + ");"
                  "import build_products;print('waiting',flush=True);"
                  "raise SystemExit(build_products.run_managed([sys.executable,'-c',"
                  + repr("from pathlib import Path;p=Path('products');p.mkdir();(p/'rebuilt').write_text('new')")
                  + "],cwd=" + repr(str(repo)) + "))")
        with products.ProductLease(root, reclaim=True):
            child = subprocess.Popen([sys.executable, "-B", "-c", script],
                                     stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            try:
                import select
                self.assertTrue(select.select([child.stdout], [], [], 10)[0])
                self.assertEqual(child.stdout.readline(), b"waiting\n")
                self.assertIsNone(child.poll())
                self.assertFalse(marker.exists())
            except BaseException:
                child.kill(); child.communicate(timeout=10)
                raise
        try:
            stdout, stderr = child.communicate(timeout=15)
            self.assertEqual(child.returncode, 0, (stdout, stderr))
            self.assertEqual(marker.read_text(), "new")
        finally:
            if child.poll() is None:
                child.kill(); child.communicate(timeout=10)

    def test_cancel_waiting_managed_build_leaves_no_reserved_owner(self):
        repo = self.root / "repo"
        repo.mkdir()
        subprocess.run(["git", "init", "-q", str(repo)], check=True, timeout=10)
        root = products.namespace(repo)
        script = ("import sys;sys.path.insert(0," + repr(str(SCRIPTS)) + ");"
                  "import build_products;print('waiting',flush=True);"
                  "build_products.run_managed([sys.executable,'-c','raise SystemExit(91)'],cwd="
                  + repr(str(repo)) + ")")
        with products.ProductLease(root, reclaim=True):
            child = subprocess.Popen([sys.executable, "-B", "-c", script], stdout=subprocess.PIPE,
                                     stderr=subprocess.PIPE)
            try:
                import select
                self.assertTrue(select.select([child.stdout], [], [], 10)[0])
                self.assertEqual(child.stdout.readline(), b"waiting\n")
                child.terminate()
                child.communicate(timeout=10)
                self.assertEqual(child.returncode, -signal.SIGTERM)
                self.assertFalse(list(root.glob("build-*")))
            finally:
                if child.poll() is None:
                    child.kill(); child.communicate(timeout=10)

    def test_live_build_excludes_reclaim_even_without_host_policy(self):
        with products.ProductLease(self.root):
            with self.assertRaises(BlockingIOError):
                products.ProductLease(self.root, reclaim=True)

    def test_reclaim_excludes_new_build(self):
        with products.ProductLease(self.root, reclaim=True):
            with self.assertRaises(BlockingIOError):
                products.ProductLease(self.root)

    def test_concurrent_builds_share_products(self):
        with products.ProductLease(self.root), products.ProductLease(self.root):
            pass

    def test_descendant_inherits_product_exclusion_after_parent_closes(self):
        lease = products.ProductLease(self.root)
        child = subprocess.Popen([sys.executable, "-c", "import sys; sys.stdin.buffer.read()"],
                                 pass_fds=(lease.fd,), stdin=subprocess.PIPE)
        lease.close()
        try:
            with self.assertRaises(BlockingIOError):
                products.ProductLease(self.root, reclaim=True)
        finally:
            child.stdin.close()
            child.wait(timeout=10)
        with products.ProductLease(self.root, reclaim=True):
            pass

    def test_crashed_supervisor_record_blocks_reclaim_after_fds_close(self):
        with products.ProductLease(self.root):
            custody = self.custody()
        before = (custody.root / "record.json").read_bytes()
        with self.assertRaisesRegex(Refusal, "unsettled product owner"):
            products.ProductLease(self.root, reclaim=True)
        self.assertEqual(before, (custody.root / "record.json").read_bytes())

    def test_malformed_and_unknown_owners_preserve_products(self):
        unknown = self.root / "legacy-owner"
        unknown.write_text("not managed")
        with self.assertRaisesRegex(Refusal, "unknown product custody"):
            products.ProductLease(self.root, reclaim=True)
        unknown.unlink()
        custody = self.custody()
        (custody.root / "record.json").write_text("{")
        with self.assertRaisesRegex(Refusal, "unreadable product owner"):
            products.ProductLease(self.root, reclaim=True)

    def test_lock_symlink_and_hardlink_are_refused(self):
        outside = self.root / "outside"
        outside.touch(mode=0o600)
        lock = self.root / "products.lock"
        lock.symlink_to(outside)
        with self.assertRaises(Refusal):
            products.ProductLease(self.root)
        lock.unlink()
        os.link(outside, lock)
        with self.assertRaises(Refusal):
            products.ProductLease(self.root)

    def test_nested_entry_requires_same_held_inheritable_inode(self):
        with products.ProductLease(self.root) as lease:
            env = {products.PRODUCT_FD: str(lease.fd), products.PRODUCT_ROOT: str(self.root)}
            self.assertEqual(products.inherited_lease(self.root, env), lease.fd)
            os.set_inheritable(lease.fd, False)
            with self.assertRaisesRegex(Refusal, "inheritable"):
                products.inherited_lease(self.root, env)

    def test_unheld_descriptor_cannot_claim_nested_custody(self):
        with products.ProductLease(self.root):
            pass
        fd = os.open(self.root / "products.lock", os.O_RDWR)
        os.set_inheritable(fd, True)
        try:
            with self.assertRaisesRegex(Refusal, "not held"):
                products.inherited_lease(self.root, {
                    products.PRODUCT_FD: str(fd), products.PRODUCT_ROOT: str(self.root)})
        finally:
            os.close(fd)

    def test_live_lifetime_guard_defers_settlement_outside_session(self):
        custody = self.custody()
        guard = products.open_private(custody.root / "guard.lock", create=True)
        fcntl.flock(guard, fcntl.LOCK_EX | fcntl.LOCK_NB)
        custody.call("attach", **custody.lease,
                     execution={"id": "execution", "session": 999999, "guard": "guard.lock"})
        try:
            with patch.object(products.native, "session_members", return_value=[]):
                with self.assertRaisesRegex(Refusal, "guard is still held"):
                    custody.call("settle", **custody.lease, execution_id="execution")
            self.assertFalse(custody.call("finish", **custody.lease))
        finally:
            os.close(guard)

    def test_unknown_process_census_never_settles_owner(self):
        custody = self.custody()
        fd = products.open_private(custody.root / "guard.lock", create=True)
        os.close(fd)
        custody.call("attach", **custody.lease,
                     execution={"id": "execution", "session": 999999, "guard": "guard.lock"})
        with patch.object(products.native, "session_members", side_effect=PermissionError("fixture")):
            with self.assertRaises(PermissionError):
                custody.call("settle", **custody.lease, execution_id="execution")
        self.assertFalse(custody.call("finish", **custody.lease))

    def test_settled_owner_allows_reclaim_but_keeps_receipt(self):
        custody = self.custody()
        fd = products.open_private(custody.root / "guard.lock", create=True)
        os.close(fd)
        custody.call("attach", **custody.lease,
                     execution={"id": "execution", "session": 999999, "guard": "guard.lock"})
        with patch.object(products.native, "session_members", return_value=[]):
            custody.call("settle", **custody.lease, execution_id="execution")
        self.assertTrue(custody.call("finish", **custody.lease))
        with products.ProductLease(self.root, reclaim=True):
            self.assertEqual(products.read_record(custody.root / "record.json")["state"], "finished")

    def test_duplicate_owner_publication_cannot_overwrite(self):
        path = self.root / "record.json"
        products.write_record(path, {"version": 1, "value": "original"}, create=True)
        with self.assertRaises(FileExistsError):
            products.write_record(path, {"version": 1, "value": "replacement"}, create=True)
        self.assertEqual(products.read_record(path)["value"], "original")

    def test_cargo_arguments_and_exact_toolchain_preserved(self):
        argv = ["test", "--", "name with spaces", "--exact"]
        with patch.object(cargo.shutil, "which", return_value="/fixture/bin/cargo"):
            result = cargo.command(argv)
        self.assertEqual(result[-5:], ["/fixture/bin/cargo", *argv])
        explicit = cargo.command(["--cargo-executable", "/fixture/toolchain/cargo", "--", *argv])
        self.assertEqual(explicit[-5:], ["/fixture/toolchain/cargo", *argv])
        self.assertIn("cargo-managed", explicit)

    def test_cargo_missing_or_recursive_executable_refuses(self):
        with patch.object(cargo.shutil, "which", return_value=None):
            with self.assertRaises(FileNotFoundError):
                cargo.command(["build"])
        with self.assertRaisesRegex(ValueError, "this wrapper"):
            cargo.command(["--cargo-executable", str(SCRIPTS / "cargo-managed.py"), "--"])

    def test_custody_does_not_mint_or_replace_host_grant(self):
        # Exercise the constructor through its blocked launcher with a fake client;
        # retain actual pipe/exec/child handling, replacing only the global census
        # with an exact fixture-owned PID observation for portability.
        env = dict(os.environ, STORYHOOK_HOST_GRANT="outer-grant", STORYHOOK_HOST_REQUEST="outer-request")
        output = self.root / "env.json"
        custody = self.custody()
        script = "import json,os,pathlib; pathlib.Path(os.environ['FIXTURE_OUTPUT']).write_text(json.dumps({k:v for k,v in os.environ.items() if k.startswith('STORYHOOK_HOST_')}))"
        env["FIXTURE_OUTPUT"] = str(output)
        process = products.ManagedProcess(custody, custody.lease, [sys.executable, "-c", script],
                                           env=env, grant_environment=False)
        def members(_session):
            return [] if process._exited() else [process.child.pid]
        try:
            with patch.object(products.native, "session_members", side_effect=members):
                # finish reaps the child before checking session settlement.
                def settle_members(session):
                    return [] if process.finished else members(session)
                with patch.object(products.native, "session_members", side_effect=settle_members):
                    self.assertEqual(process.wait(), 0)
            values = json.loads(output.read_text())
            self.assertEqual(values["STORYHOOK_HOST_GRANT"], "outer-grant")
            self.assertEqual(values["STORYHOOK_HOST_REQUEST"], "outer-request")
        finally:
            process.close()

    def test_real_whole_command_settles_and_preserves_exit_status(self):
        repo = self.root / "repo"
        repo.mkdir()
        subprocess.run(["git", "init", "-q", str(repo)], check=True)
        self.retain = True
        result = products.run_managed([sys.executable, "-c", "raise SystemExit(7)"], cwd=repo)
        self.assertEqual(result, 7)
        root = products.namespace(repo)
        with products.ProductLease(root, reclaim=True):
            records = list(root.glob("build-*/record.json"))
            self.assertEqual(len(records), 1)
            self.assertEqual(products.read_record(records[0])["state"], "finished")
        self.retain = False

    def test_real_command_does_not_finish_while_descendant_owns_guard(self):
        repo = self.root / "repo"
        repo.mkdir()
        subprocess.run(["git", "init", "-q", str(repo)], check=True)
        pid_path = self.root / "child.pid"
        child_code = "import time; time.sleep(30)"
        parent_code = ("import subprocess,sys,pathlib; "
                       f"p=subprocess.Popen([sys.executable,'-c',{child_code!r}],close_fds=False); "
                       f"pathlib.Path({str(pid_path)!r}).write_text(str(p.pid))")
        self.retain = True
        self.assertEqual(products.run_managed([sys.executable, "-c", parent_code], cwd=repo), 0)
        child_pid = int(pid_path.read_text())
        try:
            observed = products.native.process(child_pid, products.native.boot_identity())
        except ProcessLookupError:
            observed = {"live": False}
        self.assertFalse(observed["live"])
        with products.ProductLease(products.namespace(repo), reclaim=True):
            pass
        self.retain = False

    def test_fork_after_census_is_drained_before_custody_settles(self):
        # Freeze exactly one PID snapshot before an in-session process forks.
        # Its parent exits before liveness observation; its child still owns
        # the inherited guard and must be found by a subsequent census.
        ready, release, descendant = [self.root / name for name in
                                       ("forker-ready", "release-fork", "descendant")]
        code = f"""
import os,time
from pathlib import Path
if os.fork():
    os._exit(0)
Path({str(ready)!r}).write_text(str(os.getpid()))
while not Path({str(release)!r}).exists():
    time.sleep(0.005)
if os.fork():
    os._exit(0)
Path({str(descendant)!r}).write_text(str(os.getpid()))
time.sleep(30)
"""
        custody = self.custody()
        process = products.ManagedProcess(custody, custody.lease,
                                           [sys.executable, "-c", code], grant_environment=False)
        original_pids = products.native.pids
        child_identity = None
        captured = False

        def until(predicate):
            deadline = time.monotonic() + 10
            while not predicate():
                if time.monotonic() >= deadline:
                    self.fail("controlled fork did not reach its handshake")
                time.sleep(0.005)

        def census():
            nonlocal captured, child_identity
            snapshot = original_pids()
            if captured:
                return snapshot
            captured = True
            release.touch()
            until(descendant.exists)
            child_pid = int(descendant.read_text())
            child_identity = products.native.identity(child_pid, process.boot)
            self.assertEqual(os.getsid(child_pid), process.child.pid)
            self.assertNotIn(child_pid, snapshot)
            forker = int(ready.read_text())
            def exited():
                try:
                    return not products.native.process(forker, process.boot)["live"]
                except ProcessLookupError:
                    return True
            until(exited)
            return snapshot

        self.retain = True
        try:
            until(lambda: ready.exists() and process._exited())
            with patch.object(products.native, "pids", side_effect=census):
                self.assertEqual(process.wait(), 0)
            self.assertTrue(captured)
            self.assertEqual(products.read_record(custody.root / "record.json")["state"], "finished")
            self.assertFalse(custody.row["executions"])
        finally:
            if child_identity is not None:
                pid = child_identity["pid"]
                try:
                    if products.native.identity(pid, process.boot) == child_identity:
                        os.kill(pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
            try:
                process.close()
            except Refusal:
                pass  # The red control intentionally leaves an unresolved record.
            # Preserve failed ownership evidence; passing custody may be removed.
            self.retain = custody.row["state"] != "finished"

    def test_empty_census_cannot_settle_a_persistent_guard_holder(self):
        custody = self.custody()
        process = products.ManagedProcess(custody, custody.lease,
                                           [sys.executable, "-c", "pass"], grant_environment=False)
        # An independently retained inherited description, outside the session.
        held = os.dup(process.guard)
        process.timing = dict(process.timing, cleanup_ms=20, sample_ms=5)
        try:
            with self.assertRaisesRegex(Refusal, "did not settle"):
                process.wait()
            self.assertTrue(process._exited())
            self.assertFalse(process.finished)
            row = products.read_record(custody.root / "record.json")
            self.assertEqual(row["state"], "running")
            self.assertEqual(len(row["executions"]), 1)
            self.assertFalse(custody.call("finish", **custody.lease))
        finally:
            os.close(held)
            process.close()

    def test_signal_exit_preserved_only_after_native_settlement(self):
        repo = self.root / "repo"
        repo.mkdir()
        subprocess.run(["git", "init", "-q", str(repo)], check=True)
        self.retain = True
        code = "import os,signal; os.kill(os.getpid(),signal.SIGTERM)"
        self.assertEqual(products.run_managed([sys.executable, "-c", code], cwd=repo), -signal.SIGTERM)
        with products.ProductLease(products.namespace(repo), reclaim=True):
            pass
        self.retain = False

    def test_custody_command_uses_the_owned_checkout(self):
        repo = self.root / "repo"
        repo.mkdir()
        subprocess.run(["git", "init", "-q", str(repo)], check=True)
        self.retain = True
        code = "import os,pathlib; pathlib.Path('cwd.txt').write_text(os.getcwd())"
        self.assertEqual(products.run_managed([sys.executable, "-c", code], cwd=repo), 0)
        self.assertEqual(Path((repo / "cwd.txt").read_text()), repo.resolve())
        self.retain = False

    def test_shell_entry_preserves_arguments_streams_and_exit_with_admission_disabled(self):
        repo = self.root / "repo"
        repo.mkdir()
        subprocess.run(["git", "init", "-q", str(repo)], check=True)
        scripts = repo / "scripts"
        scripts.mkdir()
        for name in ("managed-cargo.sh", "cargo-managed.py", "python-runtime.sh", "build_products.py"):
            shutil.copy2(SCRIPTS / name, scripts / name)
        shutil.copytree(SCRIPTS / "python-bin", scripts / "python-bin")
        shutil.copytree(SCRIPTS / "host_admission", scripts / "host_admission",
                        ignore=shutil.ignore_patterns("__pycache__"))
        # The sole seam is the policy location. It is absent inside this fixture;
        # the host's actual policy and broker are never queried or changed.
        adapter = (SCRIPTS / "host-admit.py").read_text()
        adapter = adapter.replace('POLICY = "/var/tmp/storyhook-host-admission-v1/policy.json"',
                                  f'POLICY = {str(self.root / "absent-policy.json")!r}')
        (scripts / "host-admit.py").write_text(adapter)
        code = ("import os,sys,json; print(json.dumps(sys.argv[1:])); "
                "print('fixture stderr',file=sys.stderr); "
                "assert 'STORYHOOK_HOST_GRANT' not in os.environ; raise SystemExit(7)")
        env = {k: v for k, v in os.environ.items()
               if not k.startswith(("STORYHOOK_HOST_", "STORYHOOK_PRODUCT_"))}
        env["STORYHOOK_PYTHON"] = sys.executable
        self.retain = True
        result = subprocess.run([str(scripts / "managed-cargo.sh"), "--cargo-executable",
                                 sys.executable, "--", "-c", code, "a b", "--literal", ""],
                                cwd=repo, env=env, text=True, capture_output=True, timeout=20)
        self.assertEqual(result.returncode, 7, result.stderr)
        self.assertEqual(json.loads(result.stdout), ["a b", "--literal", ""])
        self.assertEqual(result.stderr, "fixture stderr\n")
        with products.ProductLease(products.namespace(repo), reclaim=True):
            pass
        self.retain = False

    def test_inheritable_jobserver_descriptor_reaches_whole_command(self):
        repo = self.root / "repo"
        repo.mkdir()
        subprocess.run(["git", "init", "-q", str(repo)], check=True)
        read_fd, write_fd = os.pipe()
        os.set_inheritable(read_fd, True)
        os.write(write_fd, b"J")
        self.retain = True
        try:
            code = f"import os; assert os.read({read_fd},1) == b'J'"
            self.assertEqual(products.run_managed([sys.executable, "-c", code], cwd=repo), 0)
            self.retain = False
        finally:
            os.close(read_fd)
            os.close(write_fd)

    def test_nested_same_checkout_uses_one_durable_owner(self):
        repo = self.root / "repo"
        repo.mkdir()
        subprocess.run(["git", "init", "-q", str(repo)], check=True)
        inner = (f"import sys; sys.path.insert(0,{str(SCRIPTS)!r}); "
                 "from build_products import run_managed; "
                 "raise SystemExit(run_managed([sys.executable,'-c','raise SystemExit(4)']))")
        self.retain = True
        self.assertEqual(products.run_managed([sys.executable, "-c", inner], cwd=repo), 4)
        root = products.namespace(repo)
        with products.ProductLease(root, reclaim=True):
            self.assertEqual(len(list(root.glob("build-*/record.json"))), 1)
        self.retain = False

    def test_nested_other_checkout_retains_both_owners(self):
        first, second = self.root / "first", self.root / "second"
        for repo in (first, second):
            repo.mkdir()
            subprocess.run(["git", "init", "-q", str(repo)], check=True)
        inner = (f"import sys; sys.path.insert(0,{str(SCRIPTS)!r}); "
                 "from build_products import run_managed; "
                 f"raise SystemExit(run_managed([sys.executable,'-c','raise SystemExit(4)'],cwd={str(second)!r}))")
        self.retain = True
        self.assertEqual(products.run_managed([sys.executable, "-c", inner], cwd=first), 4)
        for repo in (first, second):
            root = products.namespace(repo)
            with products.ProductLease(root, reclaim=True):
                self.assertEqual(len(list(root.glob("build-*/record.json"))), 1)
        self.retain = False


if __name__ == "__main__":
    unittest.main()
