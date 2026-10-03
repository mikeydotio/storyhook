"""Persistent activation selects transport before any tmux command (SH-825)."""

import copy
import importlib.util
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "lib"))
import tmux_target  # noqa: E402
import probe_budget
import agent_identity
import continuation_runtime
import tmux_client


def helper(name):
    """Load a production command module without invoking its CLI."""
    spec = importlib.util.spec_from_file_location(name.replace('-', '_'), Path(__file__).resolve().parents[1] / 'lib' / (name + '.py'))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class TargetTests(unittest.TestCase):
    """Only absent/inactive protection permits ordinary tmux startup."""

    def setUp(self):
        """Keep discovery, executable and generation records in a fake home."""
        self.temp = tempfile.TemporaryDirectory(prefix="sh825-target-", dir="/tmp")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.env = {"HOME": str(self.root), "TMUX_TMPDIR": str(self.root),
                    "REVIVIFY_STATE_DIR": "/not-the-discovery-root", "GH_TOKEN": "private"}
        self.socket = str(self.root / "tmux.sock")
        self.generation = "a" * 32
        self.endpoint = str(self.root / (".rv-" + self.generation) / "s")
        self.executable = self.root / "provider with spaces"
        self.executable.write_text("#!/bin/sh\nexit 0\n")
        self.executable.chmod(0o700)
        self.record = dict(version=1, active=True, socket=self.socket,
                           executable=str(self.executable), state_dir=str(self.root / "snapshots"),
                           generation=self.generation, endpoint=self.endpoint, phase="ready",
                           history=[], reservation_host="host", reservation_boot="boot",
                           identity=dict(host="host", boot="boot", pid=123, start="exact-start"))
        self.calls = []
        self.response = dict(self.record, restore_ready=True, ownership_state="reachable",
                             generation_state_dir=str(self.root / "snapshots/generations" / self.generation))

    def path(self, socket=None):
        """The provider's canonical discovery key, independent of snapshot storage."""
        base = Path(self.env.get("XDG_STATE_HOME", self.root / ".local/state"))
        digest = hashlib.sha256(os.fsencode(os.path.realpath(socket or self.socket))).hexdigest()
        return base / "tmux-revivify/activation" / (digest + ".json")

    def publish(self, record=None):
        """Publish the same private record shape RV-10 owns."""
        path = self.path()
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(self.record if record is None else record))
        path.chmod(0o600)
        return path

    def runner(self, argv, **kwargs):
        """Observe the external boundary without emulating tmux or restore."""
        self.calls.append((argv, kwargs))
        return subprocess.CompletedProcess(argv, 0, json.dumps(self.response), "")

    def resolve(self, socket=None, ensure=False):
        """Use a filtered server environment, retaining full discovery context."""
        return tmux_target.resolve_target(socket or self.socket, self.env, self.runner,
                                          {"HOME": str(self.root)}, ensure=ensure)

    def test_absent_and_explicitly_inactive_do_not_invoke_provider(self):
        for record in (None, dict(version=1, active=False, socket=self.socket)):
            if record is not None:
                self.publish(record)
            target = self.resolve()
            self.assertFalse(target["protected"])
            self.assertEqual(target["socket"], self.socket)
        self.assertEqual(self.calls, [])

    def test_inspect_and_ensure_use_recorded_executable_state_and_filtered_environment(self):
        self.publish()
        for ensure in (False, True):
            target = self.resolve(ensure=ensure)
            self.assertTrue(target["protected"])
            self.assertEqual(target["endpoint"], self.endpoint)
            self.assertEqual(target["generation"], self.generation)
            argv, kwargs = self.calls[-1]
            self.assertEqual(argv[:3], [str(self.executable), "server", "ensure" if ensure else "inspect"])
            self.assertIn(self.socket, argv)
            self.assertEqual(kwargs["env"], {"HOME": str(self.root)})
            if ensure:
                self.assertEqual(argv[argv.index("--state-dir") + 1], self.record["state_dir"])

    def test_xdg_discovery_and_socket_alias(self):
        self.env["XDG_STATE_HOME"] = str(self.root / "xdg")
        alias = self.root / "alias"
        alias.symlink_to(self.root, target_is_directory=True)
        self.publish()
        self.assertEqual(self.resolve(str(alias / "tmux.sock"))["endpoint"], self.endpoint)

    def test_current_endpoint_resolves_to_logical_socket(self):
        self.publish()
        self.assertEqual(self.resolve(self.endpoint)["socket"], self.socket)

    def test_predecessor_endpoint_requires_validated_owner_history(self):
        previous = "b" * 32
        old = dict(self.record, generation=previous, endpoint=str(self.root / (".rv-" + previous) / "s"))
        self.record["history"] = [previous]
        self.response["history"] = [previous]
        self.publish()
        owners = self.root / "snapshots/owners"
        owners.mkdir(parents=True)
        owner = owners / (previous + ".json")
        owner.write_text(json.dumps(old))
        owner.chmod(0o600)
        self.assertEqual(self.resolve(old["endpoint"])["endpoint"], self.endpoint)
        old["socket"] = "/foreign/socket"
        owner.write_text(json.dumps(old))
        with self.assertRaisesRegex(RuntimeError, "activation|history|socket"):
            self.resolve(old["endpoint"])

    def test_unknown_private_endpoint_never_becomes_unmanaged(self):
        with self.assertRaisesRegex(RuntimeError, "activation|private|generation"):
            self.resolve(self.endpoint)
        self.assertEqual(self.calls, [])

    def test_invalid_record_matrix_never_calls_provider(self):
        cases = [("version", 2), ("version", True), ("active", "yes"),
                 ("socket", "/another/socket"), ("executable", "relative"),
                 ("generation", "../escape"), ("state_dir", "relative"),
                 ("endpoint", "/public/socket"), ("phase", "unknown"),
                 ("identity", None), ("history", ["../escape"])]
        for key, value in cases:
            with self.subTest(key=key, value=value):
                self.publish(dict(self.record, **{key: value}))
                with self.assertRaises(RuntimeError):
                    self.resolve(ensure=True)
        self.assertEqual(self.calls, [])

    def test_unreadable_symlink_insecure_and_missing_executable_fail(self):
        path = self.publish()
        path.chmod(0o644)
        with self.assertRaises(RuntimeError):
            self.resolve()
        path.unlink()
        path.symlink_to(self.root / "missing")
        with self.assertRaises(RuntimeError):
            self.resolve()
        path.unlink()
        self.publish()
        self.executable.unlink()
        with self.assertRaises(RuntimeError):
            self.resolve()
        self.assertEqual(self.calls, [])

    def test_permission_denied_is_not_absence(self):
        self.publish()
        with patch("pathlib.Path.read_bytes", side_effect=PermissionError("denied")):
            with self.assertRaisesRegex(RuntimeError, "denied"):
                self.resolve()

    def test_readiness_failure_matrix_does_not_fall_back(self):
        self.publish()
        for field, value in (("restore_ready", False), ("restore_ready", 1),
                             ("ownership_state", "unknown"), ("phase", "failed"),
                             ("socket", "/wrong"), ("endpoint", self.socket),
                             ("active", False), ("generation", "c" * 32)):
            with self.subTest(field=field):
                before = copy.deepcopy(self.response)
                self.response[field] = value
                with self.assertRaises(RuntimeError):
                    self.resolve(ensure=True)
                self.response = before

    def test_provider_exit_malformed_json_and_timeout_are_errors(self):
        self.publish()
        results = [subprocess.CompletedProcess([], 1, '{"error":"unknown","error_type":"OwnershipError"}', "denied"),
                   subprocess.CompletedProcess([], 0, "not json", ""),
                   subprocess.CompletedProcess([], 0, "[]", "")]
        for result in results:
            with patch.object(self, "runner", return_value=result):
                with self.assertRaises(RuntimeError):
                    self.resolve()
        with patch.object(self, "runner", side_effect=subprocess.TimeoutExpired("revivify", 1)):
            with self.assertRaisesRegex(RuntimeError, "timed out|timeout"):
                self.resolve()

    def test_ensure_accepts_provider_published_successor_not_an_arbitrary_endpoint(self):
        self.publish()
        next_generation = "c" * 32
        successor = dict(self.record, generation=next_generation,
                         endpoint=str(self.root / (".rv-" + next_generation) / "s"), history=[self.generation])
        def ensure(argv, **kwargs):
            self.publish(successor)
            return subprocess.CompletedProcess(argv, 0, json.dumps(dict(successor,
                restore_ready=True, ownership_state="reachable",
                generation_state_dir=str(self.root / "snapshots/generations" / next_generation))), "")
        with patch.object(self, "runner", side_effect=ensure):
            self.assertEqual(self.resolve(ensure=True)["generation"], next_generation)

    def test_default_named_and_explicit_socket_precedence(self):
        env = dict(self.env, TMUX="/ambient/socket,1,0")
        for args, expected in (([], None), (["-L", "named"], str(self.root / ("tmux-" + str(os.getuid())) / "named")),
                               (["-S", self.socket, "-L", "ignored"], self.socket),
                               (["-u", "-S" + self.socket], self.socket)):
            with self.subTest(args=args):
                _, command, socket = tmux_target.split_tmux_arguments([*args, "display-message", "-p"], env)
                self.assertEqual(socket, expected)
                self.assertEqual(command, ["display-message", "-p"])
        self.assertEqual(tmux_target.logical_socket(None, env), "/ambient/socket")
        self.assertEqual(tmux_target.logical_socket(None, self.env),
                         str(self.root / ("tmux-" + str(os.getuid())) / "default"))

    def test_protected_argv_cannot_reselect_public_socket(self):
        self.publish()
        target = self.resolve()
        args = ["-u", "-S", self.socket, "-L", "other", "-f", "/config", "new-session", "-d", "-s", "project"]
        self.assertEqual(tmux_target.target_arguments(target, args),
                         ["-N", "-S", self.endpoint, "-u", "-f", "/config", "new-session", "-d", "-s", "project"])
        self.assertEqual(tmux_target.target_arguments(dict(protected=False), args), args)

    def launch_fixture(self, fail=False):
        """Run the production launcher against observable executable boundaries."""
        self.publish()
        self.executable.write_text("#!" + sys.executable + "\nimport json,os,sys\n"
            "from pathlib import Path\n"
            "Path(os.environ['HOME'], 'provider-call.json').write_text(json.dumps([sys.argv[1:],dict(os.environ)]))\n"
            + ("print('restore failed',file=sys.stderr); sys.exit(1)\n" if fail else
               "print(" + repr(json.dumps(self.response)) + ")\n"))
        tmux = self.root / "tmux"
        tmux.write_text("#!" + sys.executable + "\nimport json,os,sys\nfrom pathlib import Path\n"
            "with Path(os.environ['HOME'], 'tmux-calls.jsonl').open('a') as f: f.write(json.dumps(sys.argv[1:])+'\\n')\n")
        tmux.chmod(0o700)
        env = dict(self.env, PATH=str(self.root) + os.pathsep + os.environ["PATH"], TMUX=self.socket + ",1,0")
        # The outer patience covers interpreter startup; production owns all inner bounds.
        sys.path.insert(0, str(Path(__file__).resolve().parents[3] / "scripts/tests"))
        import load_grace
        return subprocess.run([sys.executable, "-B", str(Path(__file__).resolve().parents[1] / "lib/tmux-launch.py"),
                               "new-session", "-d", "-s", "fixture"], env=env, text=True,
                              capture_output=True, timeout=load_grace.patience(45, load_grace.contention()))

    def test_production_launcher_ensures_before_any_tmux_and_pins_all_commands(self):
        result = self.launch_fixture()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue((self.root / "provider-call.json").exists(), "launcher bypassed protected ownership")
        argv, env = json.loads((self.root / "provider-call.json").read_text())
        self.assertEqual(argv[:2], ["server", "ensure"])
        self.assertNotIn("GH_TOKEN", env)
        calls = [json.loads(line) for line in (self.root / "tmux-calls.jsonl").read_text().splitlines()]
        self.assertTrue(calls)
        self.assertTrue(all(call[:3] == ["-N", "-S", self.endpoint] for call in calls), calls)

    def test_production_launcher_does_not_probe_or_allocate_after_restore_failure(self):
        result = self.launch_fixture(fail=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("restore failed", result.stderr)
        self.assertFalse((self.root / "tmux-calls.jsonl").exists())

    def lifecycle(self, action, body=""):
        """Observe existing helper flows at their bounded subprocess boundary."""
        self.publish()
        def run(argv, **kwargs):
            self.calls.append(list(argv))
            output = json.dumps(self.response) if str(argv[0]) == str(self.executable) else body
            if kwargs.get('stdout') is not None:
                kwargs['stdout'].write(output.encode())
            return subprocess.CompletedProcess(argv, 0, output, "")
        with patch.dict(os.environ, dict(self.env, TMUX=self.endpoint + ',1,0'), clear=True), patch.object(probe_budget, 'run', side_effect=run):
            return action()

    def test_identity_discovers_before_missing_socket_and_normalizes_proven_alias(self):
        row = f"SH-1\t%7\t123\t0\t{self.root}\tcodex\tcodex\t{self.socket}\n"
        panes = self.lifecycle(lambda: agent_identity.panes(self.socket), row)
        self.assertIn('%7', panes)
        self.assertEqual(panes['%7']['socket'], self.endpoint)
        self.assertTrue(any(call[:4] == ['tmux', '-N', '-S', self.endpoint] for call in self.calls), self.calls)

    def test_identity_numeric_binding_cannot_follow_logical_socket(self):
        with self.assertRaisesRegex((RuntimeError, agent_identity.IdentityError), 'binding|re-adoption'):
            self.lifecycle(lambda: agent_identity.pane_at('%7', self.socket))
        self.assertFalse(any(call[0] == 'tmux' for call in self.calls))

    def test_continuation_queries_pin_the_bound_endpoint(self):
        self.lifecycle(lambda: continuation_runtime.tmux(self.endpoint, 'list-panes'))
        self.assertTrue(any(call[:4] == ['tmux', '-N', '-S', self.endpoint] for call in self.calls), self.calls)

    def test_startup_cleanup_pins_before_reading_numeric_identity(self):
        cleanup = helper('stop-dispatch-pane')
        self.lifecycle(lambda: cleanup.run('tmux', '-S', self.endpoint, 'display-message', '-p', '-t', '%7', '#{pane_pid}'), '123')
        self.assertTrue(any(call[:4] == ['tmux', '-N', '-S', self.endpoint] for call in self.calls), self.calls)

    def test_dropped_cleanup_cannot_turn_unbound_protected_socket_into_absence(self):
        cleanup = helper('dropped-cleanup-pane')
        with self.assertRaisesRegex((RuntimeError, cleanup.proc.CleanupError), 'binding|re-adoption'):
            self.lifecycle(lambda: cleanup.panes(dict(socket=self.socket, name='SH-1')))

    def test_environment_inspection_uses_the_protected_endpoint(self):
        environment = helper('tmux-env')
        self.lifecycle(lambda: environment.run('show-environment', '-g'))
        self.assertTrue(any(call[:4] == ['tmux', '-N', '-S', self.endpoint] for call in self.calls), self.calls)

    def test_helper_operation_keeps_one_generation_and_refreshes_next_operation(self):
        def action():
            with tmux_client.operation():
                original = tmux_client.client(self.socket)
                # A later malformed publication must not redirect this operation.
                self.path().write_text('{}')
                with tmux_client.operation():
                    self.assertIs(tmux_client.client(self.endpoint), original)
                    self.assertEqual(original.arguments(['kill-pane', '-t', '%7']),
                                     ['tmux', '-N', '-S', self.endpoint, 'kill-pane', '-t', '%7'])
            with tmux_client.operation():
                with self.assertRaisesRegex(RuntimeError, 'activation'):
                    tmux_client.client(self.socket)
        self.lifecycle(action)
        self.assertEqual(sum(call[0] == str(self.executable) for call in self.calls), 1)

    def test_identity_rejects_a_foreign_reported_socket(self):
        row = f"SH-1\t%7\t123\t0\t{self.root}\tcodex\tcodex\t/foreign/socket\n"
        with self.assertRaisesRegex(RuntimeError, 'foreign socket'):
            self.lifecycle(lambda: agent_identity.panes(self.socket), row)

    def test_protected_refusal_is_not_identity_absence(self):
        self.publish()
        def run(argv, **kwargs):
            if str(argv[0]) == str(self.executable):
                return subprocess.CompletedProcess(argv, 0, json.dumps(self.response), '')
            return subprocess.CompletedProcess(argv, 1, '', f'no server running on {self.endpoint}\n')
        with patch.dict(os.environ, self.env, clear=True), patch.object(probe_budget, 'run', side_effect=run):
            with self.assertRaises(agent_identity.IdentityError):
                agent_identity.panes(self.endpoint)


if __name__ == "__main__":
    unittest.main()
