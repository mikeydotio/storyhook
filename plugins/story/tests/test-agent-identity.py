"""Exercise production agent identity and notification on a private tmux server."""

import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import tempfile
import time
import unittest


PLUGIN = Path(__file__).resolve().parents[1]
HELPER = PLUGIN / "bin/story.sh"
IDENTITY = PLUGIN / "lib/agent_identity.py"
OPTION = "@storyhook-identity-v1"


class AgentIdentityTests(unittest.TestCase):
    """Native fixture providers record bytes and draw a composer; all routing is production code."""

    @classmethod
    def setUpClass(cls):
        """Compile the composer-drawing byte recorder as both provider executables."""
        cls.tools = tempfile.TemporaryDirectory(prefix="story-identity-tools-", dir="/tmp")
        cls.bin = Path(cls.tools.name)
        source = PLUGIN / "tests/fakes/composer-provider.c"
        subprocess.run(["cc", "-Wall", "-Wextra", "-Werror", str(source), "-o", str(cls.bin / "codex")], check=True)
        shutil.copy2(cls.bin / "codex", cls.bin / "claude")
        repo = PLUGIN.parents[1]
        artifact = Path(os.environ.get("CARGO_TARGET_DIR", repo / "target")) / "debug/story"
        lease = subprocess.run(["bash", "-c",
            'source "$1"; storyhook_lease_binary "$2" "$3"', "fixture",
            str(repo / "scripts/binary-lease.sh"), str(artifact), str(os.getpid())],
            capture_output=True, text=True, check=True)
        cls.story = Path(lease.stdout.strip())
        cls.addClassCleanup(shutil.rmtree, cls.story.parent)
        cls.tmux_bin = shutil.which("tmux")
        if not cls.tmux_bin:
            raise RuntimeError("native tmux is required")

    @classmethod
    def tearDownClass(cls):
        """Remove only this class's executable fixtures."""
        cls.tools.cleanup()

    def setUp(self):
        """Create an independent repository, worktree, and tmux server per case."""
        self.scratch = tempfile.TemporaryDirectory(prefix="story-identity-", dir="/tmp")
        self.addCleanup(self.scratch.cleanup)
        self.root = Path(self.scratch.name)
        self.repo = self.root / "repo"
        self.repo.mkdir()
        self.env = {key: value for key, value in os.environ.items()
                    if not key.startswith(("GIT_", "STORY_", "STORYHOOK_", "TMUX", "FAKE_TMUX"))}
        isolation = subprocess.run(["bash", "-c",
            'source "$1"; storyhook_isolate --home --parent-pid "$2" "$3"; '
            'exec python3 -c "import json,os; print(json.dumps(dict(os.environ)))"',
            "fixture", str(PLUGIN.parents[1] / "scripts/test-env.sh"), str(os.getpid()),
            str(self.root / "isolation")], env=self.env, capture_output=True, text=True, check=True)
        self.env = json.loads(isolation.stdout)
        self.env.update({"PATH": f"{self.bin}:{self.story.parent}:{self.env['PATH']}",
                         "STORY_PASTE_SETTLE_DELAY": "0", "TMUX_TMPDIR": str(self.root / "private-tmux")})
        self.git("init", "-q", "-b", "main")
        self.git("-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid",
                 "-c", "core.hooksPath=/dev/null", "commit", "--allow-empty", "-qm", "fixture")
        self.addCleanup(self.stop_daemon)
        self.run_command([str(self.story), "project", "new", "--prefix", "TST", "--name", "fixture"], check=True)
        project = json.loads(self.run_command([str(self.story), "project", "show", "--json"], check=True).stdout)
        self.project = project["project"]["slug"]
        self.git("add", ".storyhook.toml")
        self.git("-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid",
                 "-c", "core.hooksPath=/dev/null", "commit", "-qm", "project pointer")
        story = json.loads(self.run_command([str(self.story), "new", "Repair the fixture", "--json"], check=True).stdout)
        self.assertEqual(story["story"]["story"]["id"], "TST-1")
        self.worktree = self.repo / ".codex/worktrees/TST-1"
        self.git("worktree", "add", "-qb", "worktree-TST-1", str(self.worktree))
        self.socket = self.root / "tmux.sock"
        self.server = subprocess.Popen([self.tmux_bin, "-D", "-f", "/dev/null", "-S", str(self.socket)],
                                       env=self.env, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
        self.addCleanup(self.stop_server)
        self.wait_for(lambda: self.socket.exists(), "tmux socket")
        self.tmux("new-session", "-d", "-s", "fixture", "sleep 600")
        self.env["TMUX"] = self.tmux("display-message", "-p", "#{socket_path},#{pid},0").strip()
        self.pane = self.launch()

    def stop_daemon(self):
        """Reap this case's isolated store owner before removing its home."""
        self.run_command([str(self.story), "daemon", "stop", "--force"], check=True)

    def stop_server(self):
        """Terminate this private server and reap its process."""
        if self.server.poll() is None:
            self.server.terminate()
        self.server.communicate(timeout=10)

    def run_command(self, args, **kwargs):
        """Run a bounded subprocess under this fixture's environment."""
        return subprocess.run(args, env=self.env, cwd=self.repo, capture_output=True,
                              text=True, timeout=15, **kwargs)

    def git(self, *args):
        """Run Git only in the fixture repository."""
        return self.run_command(["git", *args], check=True).stdout

    def tmux(self, *args):
        """Address only this case's private socket."""
        return self.run_command([self.tmux_bin, "-S", str(self.socket), *args], check=True).stdout

    def wait_for(self, predicate, description):
        """Wait for an observable fixture event with a finite deadline."""
        deadline = time.monotonic() + 5
        while not predicate():
            if time.monotonic() > deadline:
                self.fail(f"timeout waiting for {description}")
            time.sleep(0.01)

    def launch(self, provider="codex", worktree=None, name="TST-1", split=False, screen=None):
        """Start a direct provider executable and wait for its input recorder.

        `screen` is drawn in place of the idle composer (a dialog or a ghost
        suggestion); the recorder draws it before it signals readiness.
        """
        output = self.root / f"input-{time.monotonic_ns()}"
        command = f"{shlex.quote(str(self.bin / provider))} {shlex.quote(str(output))}"
        if screen is not None:
            screen_file = self.root / f"screen-{time.monotonic_ns()}"
            screen_file.write_bytes(screen)
            command += f" {shlex.quote(str(screen_file))}"
        if split:
            args = ["split-window", "-d", "-t", self.pane]
        else:
            args = ["new-window", "-d", "-n", name]
        pane = self.tmux(*args, "-c", str(worktree or self.worktree), "-P", "-F", "#{pane_id}", command).strip()
        self.tmux("set-window-option", "-t", pane, "automatic-rename", "off")
        self.tmux("set-window-option", "-t", pane, "remain-on-exit", "on")
        self.wait_for(lambda: output.exists() and output.stat().st_size >= 6, "provider readiness")
        if not hasattr(self, "inputs"):
            self.inputs = {}
        self.inputs[pane] = output
        return pane

    def notify(self, message="REPAIR_FIXTURE\nsecond line", **env):
        """Drive the real Bash notification entry point."""
        before = self.env.copy()
        self.env.update(env)
        try:
            result = self.run_command(["bash", str(HELPER), "--project", self.project, "notify", "TST-1", message])
        finally:
            self.env = before
        self.assertTrue(result.stdout.strip(), result.stderr)
        return json.loads(result.stdout)

    def record(self, pane=None):
        """Read only the pane-local record, excluding inherited window options."""
        return json.loads(self.tmux("show-options", "-p", "-v", "-t", pane or self.pane, OPTION))

    def test_untagged_direct_codex_reaches_its_exact_live_worktree(self):
        """The reported missing-tag failure must recover without a respawn."""
        pid = self.tmux("display-message", "-p", "-t", self.pane, "#{pane_pid}")
        result = self.notify()
        self.assertTrue(result["ok"], result)
        self.wait_for(lambda: b"REPAIR_FIXTURE" in self.inputs[self.pane].read_bytes(), "remediation bytes")
        self.assertIn(b"\x1b[200~REPAIR_FIXTURE\rsecond line\x1b[201~", self.inputs[self.pane].read_bytes())
        self.assertTrue(self.inputs[self.pane].read_bytes().endswith(b"\t"))
        self.assertEqual(pid, self.tmux("display-message", "-p", "-t", self.pane, "#{pane_pid}"))
        self.assertEqual(self.record()["provider"], "codex")

    def test_c_locale_preserves_inventory_and_exact_pane_revalidation(self):
        """An ASCII locale cannot erase inventory delimiters or pane authority."""
        self.env["LC_ALL"] = "C"
        registered = self.register()
        self.assertTrue(registered["ok"], registered)
        result = self.notify("C_LOCALE_DELIVERY")
        self.assertTrue(result["ok"], result)
        self.assertEqual(result["pane"], self.pane)
        self.wait_for(lambda: b"C_LOCALE_DELIVERY" in self.inputs[self.pane].read_bytes(),
                      "C locale remediation bytes")
        self.assertTrue(self.inputs[self.pane].read_bytes().endswith(b"\t"))

    def test_interrupt_never_adopts_an_unregistered_direct_provider(self):
        """Delayed authority cannot bind itself to a later discovered session."""
        result = self.notify("--interrupt")
        self.assertFalse(result["ok"], result)
        self.assertEqual(result["reason"], "pane-provider-unknown")
        self.assertEqual(self.inputs[self.pane].read_bytes(), b"READY\n")
        self.assertEqual(self.tmux("show-options", "-p", "-qv", "-t", self.pane, OPTION), "")

    def test_resume_never_adopts_an_unregistered_direct_provider(self):
        """A session-bound Resume cannot register a replacement on discovery."""
        answer = self.run_command(["bash", str(HELPER), "--project", self.project,
            "notify", "TST-1", "RESUME_FIXTURE", "--expected-target", "previous-session"])
        result = json.loads(answer.stdout)
        self.assertFalse(result["ok"], result)
        self.assertEqual(result["reason"], "pane-provider-unknown")
        self.assertEqual(self.inputs[self.pane].read_bytes(), b"READY\n")
        self.assertEqual(self.tmux("show-options", "-p", "-qv", "-t", self.pane, OPTION), "")

    def test_registered_resume_never_adopts_an_unregistered_direct_provider(self):
        """A resume with no acknowledged interrupt reaches only an existing registration (SH-772)."""
        answer = self.run_command(["bash", str(HELPER), "--project", self.project,
            "notify", "TST-1", "RESUME_FIXTURE", "--registered-session"])
        result = json.loads(answer.stdout)
        self.assertFalse(result["ok"], result)
        self.assertEqual(result["reason"], "pane-provider-unknown")
        self.assertEqual(self.inputs[self.pane].read_bytes(), b"READY\n")
        self.assertEqual(self.tmux("show-options", "-p", "-qv", "-t", self.pane, OPTION), "")

    def test_adoption_refuses_a_bad_revocation_receipt_before_registration_or_input(self):
        """Only the durable endpoint is replaced; native identity and input are real."""
        endpoint = self.root / "story-endpoint"
        endpoint.write_text("#!/usr/bin/env python3\nimport os,sys\n"
            "if 'supersede-block-deliveries' in sys.argv:\n"
            " print('{\"protocol_version\":1,\"project\":\"foreign\",\"story_id\":\"TST-1\",\"superseded\":0}')\n"
            " sys.exit(0)\n"
            f"os.execv({str(self.story)!r}, [{str(self.story)!r}] + sys.argv[1:])\n")
        endpoint.chmod(0o755)
        result = self.notify(STORY_BIN=str(endpoint))
        self.assertFalse(result["ok"], result)
        self.assertEqual(result["reason"], "pane-query-failed")
        self.assertIn("revocation receipt", result["display"])
        self.assertEqual(self.inputs[self.pane].read_bytes(), b"READY\n")
        self.assertEqual(self.tmux("show-options", "-p", "-qv", "-t", self.pane, OPTION), "")

    def test_claude_uses_its_submit_key(self):
        """Provider identity determines Enter versus Tab."""
        self.tmux("kill-pane", "-t", self.pane)
        self.pane = self.launch(provider="claude")
        result = self.notify()
        self.assertTrue(result["ok"], result)
        self.wait_for(lambda: b"REPAIR_FIXTURE" in self.inputs[self.pane].read_bytes(), "Claude bytes")
        self.assertTrue(self.inputs[self.pane].read_bytes().endswith(b"\r"))

    def test_duplicate_live_candidates_receive_nothing(self):
        """Two agents in the same story worktree have no unique destination."""
        other = self.launch()
        result = self.notify()
        self.assertFalse(result["ok"], result)
        for pane in [self.pane, other]:
            self.assertEqual(self.inputs[pane].read_bytes(), b"READY\n")

    def test_active_unrelated_split_does_not_inherit_window_identity(self):
        """A live agent is selected by ownership, not the window's active pane."""
        self.assertTrue(self.notify()["ok"])
        sibling = self.tmux("split-window", "-d", "-t", self.pane, "-c", str(self.repo),
                            "-P", "-F", "#{pane_id}", "sleep 600").strip()
        self.tmux("select-pane", "-t", sibling)
        result = self.notify("ONLY_THE_AGENT")
        self.assertTrue(result["ok"], result)
        self.assertEqual(result["pane"], self.pane)
        self.assertNotIn("ONLY_THE_AGENT", self.tmux("capture-pane", "-p", "-t", sibling))

    def test_stale_process_record_is_refused_without_adoption(self):
        """A record from an earlier process incarnation is never repaired by guessing."""
        self.assertTrue(self.notify()["ok"])
        record = self.record()
        record["process"]["start"] = "stale-incarnation"
        self.tmux("set-option", "-p", "-t", self.pane, OPTION, json.dumps(record))
        before = self.inputs[self.pane].read_bytes()
        result = self.notify("MUST_NOT_ARRIVE")
        self.assertFalse(result["ok"], result)
        self.assertEqual(self.inputs[self.pane].read_bytes(), before)

    def register(self, pane=None, provider="codex"):
        """Call the production registration boundary used by managed dispatch."""
        pane = pane or self.pane
        pid = self.tmux("display-message", "-p", "-t", pane, "#{pane_pid}").strip()
        result = self.run_command(["python3", str(IDENTITY), "register", self.project, "TST-1",
                                   "TST-1", str(self.worktree), pane, pid, provider])
        return json.loads(result.stdout)

    def test_managed_registration_binds_the_exact_pane(self):
        """Successful startup includes a checked pane-local identity."""
        result = self.register()
        self.assertTrue(result["ok"], result)
        record = self.record()
        self.assertEqual(record["pane"], self.pane)
        self.assertEqual(record["worktree"], str(self.worktree.resolve()))
        self.assertTrue(record["process"]["start"])

    def assert_rollback_refuses(self, token):
        """Unproved launch ownership must leave a real terminal process running."""
        pid = self.tmux("display-message", "-p", "-t", self.pane, "#{pane_pid}").strip()
        result = self.run_command(["python3", str(PLUGIN / "lib/stop-dispatch-pane.py"),
                                   self.pane, pid, token])
        receipt = json.loads(result.stdout)
        self.assertFalse(receipt["ok"], receipt)
        self.assertIn("launch start", receipt["error"])
        os.kill(int(pid), 0)
        self.assertNotIn("T", self.run_command(["ps", "-o", "stat=", "-p", pid]).stdout)
        self.assertEqual(self.inputs[self.pane].read_bytes(), b"READY\n")
        self.assertTrue(self.worktree.is_dir())

    def test_rollback_refuses_reused_pid_with_stale_start(self):
        """A matching PID with another start time cannot authorize termination."""
        self.assert_rollback_refuses("earlier-launch")

    def test_rollback_refuses_missing_launch_start(self):
        """Cleanup cannot invent ownership when the launch probe failed."""
        self.assert_rollback_refuses("")

    def test_reused_pid_during_startup_cannot_register(self):
        """The PID must still have the incarnation captured before readiness."""
        pid = self.tmux("display-message", "-p", "-t", self.pane, "#{pane_pid}").strip()
        result = self.run_command(["python3", str(IDENTITY), "register", self.project, "TST-1",
                                   "TST-1", str(self.worktree), self.pane, pid, "codex", "earlier-start"])
        receipt = json.loads(result.stdout)
        self.assertFalse(receipt["ok"], receipt)
        self.assertEqual(receipt["reason"], "pane-changed")
        self.assertEqual(self.inputs[self.pane].read_bytes(), b"READY\n")

    def test_registration_failure_is_not_success(self):
        """A real terminal with a refused metadata write remains unregistered."""
        adapter = self.root / "adapters"
        adapter.mkdir()
        wrapper = adapter / "tmux"
        wrapper.write_text('#!/bin/sh\ncase "$*" in *"set-option -p"*) echo "fixture metadata write refused" >&2; exit 1 ;; esac\n'
                           f'exec {shlex.quote(self.tmux_bin)} "$@"\n')
        wrapper.chmod(0o755)
        self.env["PATH"] = f"{adapter}:{self.env['PATH']}"
        result = self.register()
        self.assertFalse(result["ok"], result)
        self.assertIn("fixture metadata write refused", result["display"])
        self.assertEqual(self.inputs[self.pane].read_bytes(), b"READY\n")

    def test_conflicting_records_never_fall_back_to_adoption(self):
        """All identity dimensions reject stale records before any input."""
        self.assertTrue(self.notify()["ok"])
        original = self.record()
        for field, value in [("project", "another"), ("story", "TST-2"),
                             ("provider", "claude"), ("worktree", str(self.repo)),
                             ("common", str(self.root)), ("pane", "%999999")]:
            with self.subTest(field=field):
                record = dict(original, **{field: value})
                self.tmux("set-option", "-p", "-t", self.pane, OPTION, json.dumps(record))
                before = self.inputs[self.pane].read_bytes()
                result = self.notify("MUST_NOT_ARRIVE")
                self.assertFalse(result["ok"], result)
                self.assertEqual(self.inputs[self.pane].read_bytes(), before)
        self.tmux("set-option", "-p", "-t", self.pane, OPTION, "malformed-json")
        self.assertFalse(self.notify()["ok"])

    def test_respawned_process_cannot_use_the_old_record(self):
        """The same pane ID after respawn does not identify the same agent."""
        self.assertTrue(self.notify()["ok"])
        output = self.root / "replacement-input"
        self.tmux("respawn-pane", "-k", "-c", str(self.worktree), "-t", self.pane,
                  f"{shlex.quote(str(self.bin / 'codex'))} {shlex.quote(str(output))}")
        self.wait_for(lambda: output.exists(), "replacement provider")
        self.assertFalse(self.notify()["ok"])
        self.assertEqual(output.read_bytes(), b"READY\n")

    def test_copied_registration_cannot_redirect_to_another_pane(self):
        """A valid record belongs only to the pane on which it was recorded."""
        self.assertTrue(self.register()["ok"])
        record = self.record()
        self.tmux("rename-window", "-t", self.pane, "different-window")
        other = self.launch()
        self.tmux("set-option", "-p", "-t", other, OPTION, json.dumps(record))
        self.assertFalse(self.notify()["ok"])
        for pane in [self.pane, other]:
            self.assertEqual(self.inputs[pane].read_bytes(), b"READY\n")

    def test_conflicting_legacy_tag_is_not_silently_replaced(self):
        """Recovery cannot discard an existing contradictory provider claim."""
        self.tmux("set-option", "-w", "-t", self.pane, "@storyhook-agent", "claude")
        self.assertFalse(self.notify()["ok"])
        self.assertEqual(self.inputs[self.pane].read_bytes(), b"READY\n")

    def test_wrong_worktree_and_unregistered_directory_are_refused(self):
        """A matching window and provider cannot authorize a wrong directory."""
        self.tmux("kill-pane", "-t", self.pane)
        self.pane = self.launch(worktree=self.repo)
        self.assertFalse(self.notify()["ok"])
        self.assertEqual(self.inputs[self.pane].read_bytes(), b"READY\n")
        self.tmux("kill-pane", "-t", self.pane)
        unregistered = self.root / ".codex/worktrees/TST-1"
        unregistered.mkdir(parents=True)
        self.pane = self.launch(worktree=unregistered)
        self.assertFalse(self.notify()["ok"])
        self.assertEqual(self.inputs[self.pane].read_bytes(), b"READY\n")

    def test_provider_named_shell_wrapper_is_not_a_direct_launch(self):
        """Startup text cannot establish which executable currently runs."""
        self.tmux("kill-pane", "-t", self.pane)
        fake_bin = self.root / "wrapper-bin"
        fake_bin.mkdir()
        script = fake_bin / "codex"
        script.write_text("#!/bin/sh\nexec sleep 600\n")
        script.chmod(0o755)
        self.env["PATH"] = f"{fake_bin}:{self.env['PATH']}"
        self.pane = self.tmux("new-window", "-d", "-n", "TST-1", "-c", str(self.worktree),
                              "-P", "-F", "#{pane_id}", str(script)).strip()
        self.tmux("set-window-option", "-t", self.pane, "automatic-rename", "off")
        self.assertFalse(self.notify()["ok"])
        self.assertNotIn("REPAIR_FIXTURE", self.tmux("capture-pane", "-p", "-t", self.pane))

    def test_legacy_dead_agent_remains_absent(self):
        """A positively identified legacy dead pane can still be resumed."""
        self.tmux("set-option", "-w", "-t", self.pane, "@storyhook-agent", "codex")
        pid = int(self.tmux("display-message", "-p", "-t", self.pane, "#{pane_pid}"))
        os.kill(pid, 9)
        self.wait_for(lambda: self.tmux("display-message", "-p", "-t", self.pane, "#{pane_dead}").strip() == "1", "dead pane")
        result = self.notify()
        self.assertFalse(result["ok"], result)
        self.assertEqual(result["reason"], "pane-dead")

    def test_registered_dead_agent_remains_absent(self):
        """A checked registration survives the loss of live process probes."""
        self.assertTrue(self.register()["ok"])
        self.test_legacy_dead_agent_remains_absent()

    def test_failed_probe_never_delivers_or_reports_absence(self):
        """An unavailable exact-pane query cannot permit replacement."""
        self.assertTrue(self.register()["ok"])
        adapter = self.root / "adapters"
        adapter.mkdir()
        wrapper = adapter / "tmux"
        wrapper.write_text('#!/bin/sh\ncase "$*" in *"display-message"*) echo "fixture probe failed" >&2; exit 1 ;; esac\n'
                           f'exec {shlex.quote(self.tmux_bin)} "$@"\n')
        wrapper.chmod(0o755)
        self.env["PATH"] = f"{adapter}:{self.env['PATH']}"
        result = self.notify()
        self.assertFalse(result["ok"], result)
        self.assertEqual(result["reason"], "pane-query-failed")
        self.assertIn("fixture probe failed", result["display"])
        self.assertEqual(self.inputs[self.pane].read_bytes(), b"READY\n")

    def test_lease_selects_its_socket_and_worktree(self):
        """The caller's ambient tmux server cannot redirect a leased callback."""
        lease = dict(version=1, project_slug=self.project, story_id="TST-1",
                     repository_path=str(self.repo.resolve()), worktree_path=str(self.worktree.resolve()),
                     branch="worktree-TST-1", tmux={"socket_path": str(self.socket.resolve())})
        result = self.notify(TMUX="/nonexistent-story-fixture-socket,0,0",
                             STORYHOOK_NOTIFY_LEASE_V1=json.dumps(lease))
        self.assertTrue(result["ok"], result)
        lease["worktree_path"] = str(self.repo)
        before = self.inputs[self.pane].read_bytes()
        self.assertFalse(self.notify(STORYHOOK_NOTIFY_LEASE_V1=json.dumps(lease))["ok"])
        self.assertEqual(self.inputs[self.pane].read_bytes(), before)

    def test_recorded_socket_routes_an_inactive_agent_from_an_unrelated_server(self):
        """Canonical location and exact process ownership compose across servers."""
        self.assertTrue(self.register()["ok"])
        lease = dict(version=1, project_slug=self.project, story_id="TST-1",
                     repository_path=str(self.repo.resolve()), worktree_path=str(self.worktree.resolve()),
                     branch="worktree-TST-1", tmux={"socket_path": str(self.socket.resolve())})
        gitdir = Path(self.git("-C", str(self.worktree), "rev-parse", "--absolute-git-dir").strip())
        (gitdir / "storyhook-cleanup-lease-v1.json").write_text(json.dumps(lease))
        sibling = self.tmux("split-window", "-d", "-t", self.pane, "-c", str(self.repo),
                            "-P", "-F", "#{pane_id}", "sleep 600").strip()
        self.tmux("select-pane", "-t", sibling)
        caller_socket = self.root / "caller.sock"
        self.run_command([self.tmux_bin, "-f", "/dev/null", "-S", str(caller_socket),
                          "new-session", "-d", "-s", "caller", "-n", "TST-1", "sleep 600"], check=True)
        self.addCleanup(lambda: self.run_command([self.tmux_bin, "-S", str(caller_socket), "kill-server"], check=True))
        result = self.notify("OWNER_ONLY", TMUX=f"{caller_socket},0,0")
        self.assertTrue(result["ok"], result)
        self.assertEqual(result["pane"], self.pane)
        self.wait_for(lambda: b"OWNER_ONLY" in self.inputs[self.pane].read_bytes(), "owner input")
        self.assertNotIn("OWNER_ONLY", self.tmux("capture-pane", "-p", "-t", sibling))

    def test_missing_recorded_server_is_absence_without_using_the_ambient_agent(self):
        """A proved missing recorded socket cannot redirect to another live server."""
        lease = dict(version=1, project_slug=self.project, story_id="TST-1",
                     repository_path=str(self.repo.resolve()), worktree_path=str(self.worktree.resolve()),
                     branch="worktree-TST-1", tmux={"socket_path": str(self.root / "missing.sock")})
        result = self.notify(STORYHOOK_NOTIFY_LEASE_V1=json.dumps(lease))
        self.assertFalse(result["ok"], result)
        self.assertEqual(result["reason"], "pane-unavailable")
        self.assertEqual(self.inputs[self.pane].read_bytes(), b"READY\n")

    def test_registration_changed_after_paste_prevents_submission(self):
        """The final submit cannot reuse identity that changed after delivery."""
        self.assertTrue(self.register()["ok"])
        record = self.record()
        record["process"]["start"] = "changed-after-paste"
        adapter = self.root / "adapters"
        adapter.mkdir()
        wrapper = adapter / "tmux"
        mutate = [self.tmux_bin, "-S", str(self.socket), "set-option", "-p", "-t",
                  self.pane, OPTION, json.dumps(record)]
        wrapper.write_text('#!/bin/sh\n' + shlex.quote(self.tmux_bin) + ' "$@" || exit $?\n'
                           + 'case " $* " in *" paste-buffer "*) '
                           + shlex.join(mutate) + ' ;; esac\n')
        wrapper.chmod(0o755)
        result = self.notify("PASTED_WITHOUT_SUBMIT", PATH=f"{adapter}:{self.env['PATH']}")
        self.assertFalse(result["ok"], result)
        self.assertEqual(result["reason"], "pane-changed")
        self.wait_for(lambda: b"PASTED_WITHOUT_SUBMIT" in self.inputs[self.pane].read_bytes(), "pasted input")
        self.assertFalse(self.inputs[self.pane].read_bytes().endswith(b"\t"))

    def test_failed_registration_readback_is_refused(self):
        """A successful write without trustworthy readback is not registration."""
        adapter = self.root / "adapters"
        adapter.mkdir()
        wrapper = adapter / "tmux"
        wrapper.write_text('#!/bin/sh\ncase "$*" in *"show-options -p -v"*) echo "{}"; exit 0 ;; esac\n'
                           f'exec {shlex.quote(self.tmux_bin)} "$@"\n')
        wrapper.chmod(0o755)
        self.env["PATH"] = f"{adapter}:{self.env['PATH']}"
        result = self.register()
        self.assertFalse(result["ok"], result)
        self.assertEqual(self.inputs[self.pane].read_bytes(), b"READY\n")


if __name__ == "__main__":
    unittest.main()
