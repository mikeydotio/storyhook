"""Prove what a storyhook-started tmux server retains, on private real servers (SH-758)."""

import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import sys
import tempfile
import time
import unittest


PLUGIN = Path(os.environ.get("STORY_TEST_PLUGIN_ROOT", Path(__file__).resolve().parents[1]))
REPO = PLUGIN.parents[1]
sys.path.insert(0, str(PLUGIN / "lib"))
sys.dont_write_bytecode = True
import tmux_server_env as policy  # noqa: E402  (the path above is the import root)

LAUNCHER = PLUGIN / "lib/tmux-launch.py"
ENV_CLI = PLUGIN / "lib/tmux-env.py"
# The daemon runs the view as this exact composition (src/daemon/activity/window.rs).
VIEW_PROGRAM = (PLUGIN / "lib/tmux_server_env.py").read_text() + "\n" + (REPO / "scripts/verification-view.py").read_text()
# Bounds one private tmux server operation, including its startup and a loaded
# macOS PTY allocation. It is a liveness ceiling, never a performance claim.
DEADLINE = 15
DUMP = ("import json, os, pathlib, sys; target = pathlib.Path(sys.argv[1]); "
        "temporary = target.with_suffix('.tmp'); temporary.write_text(json.dumps(dict(os.environ))); "
        "temporary.replace(target)")
CRYPTEX = "/var/run/com.apple.security.cryptexd/codex.system/bootstrap/usr/bin"
APP_BUNDLE = "/Applications/ChatGPT.app/Contents/Resources"


class PolicyTests(unittest.TestCase):
    """The pure policy, for inputs a live server cannot conveniently produce."""

    HOME = "/Users/fixture"

    def filtered(self, entries, **environ):
        environ.setdefault("HOME", self.HOME)
        return policy.filter_path(dict(environ, PATH=":".join(entries)))

    def test_host_roots_are_removed_by_path_component_in_order(self):
        home = self.HOME
        entries = ["/opt/homebrew/bin", home + "/.codex/tmp/arg0/codex-arg0kVkT4O",
                   home + "/.cache/codex-runtimes/codex-primary-runtime/dependencies/bin/override",
                   home + "/.local/bin", home + "/.claude/plugins/cache/agentics/greenlight/3.9.5/bin",
                   home + "/.local/share/storyhook/plugins/3.0.3/plugins/story/bin",
                   CRYPTEX, "/usr/bin", APP_BUNDLE, home + "/.codexfoo/bin", home + "/.claude/bin",
                   home + "/.local/share/storyhook/bin", "/opt/homebrew/bin"]
        self.assertEqual(self.filtered(entries), ":".join(
            ["/opt/homebrew/bin", home + "/.local/bin", CRYPTEX, "/usr/bin", APP_BUNDLE,
             home + "/.codexfoo/bin", home + "/.claude/bin", home + "/.local/share/storyhook/bin",
             "/opt/homebrew/bin"]))

    def test_a_root_itself_and_trailing_or_unnormalized_spellings_are_removed(self):
        home = self.HOME
        entries = [home + "/.codex", home + "/.codex/tmp/arg0/x/", home + "//.codex/tmp",
                   home + "/.codex/../bin", "/usr/bin"]
        self.assertEqual(self.filtered(entries), ":".join([home + "/.codex/../bin", "/usr/bin"]))

    def test_empty_and_relative_entries_keep_their_meaning(self):
        self.assertEqual(self.filtered(["", "bin", ".codex/tmp", "/usr/bin", ""]), ":bin:.codex/tmp:/usr/bin:")

    def test_overrides_move_the_roots_they_name(self):
        entries = ["/srv/codex/tmp/arg0/a", self.HOME + "/.codex/tmp/arg0/b", "/srv/claude/plugins/p/bin",
                   self.HOME + "/.claude/plugins/p/bin", "/srv/data/storyhook/plugins/3/bin"]
        self.assertEqual(
            self.filtered(entries, CODEX_HOME="/srv/codex", CLAUDE_CONFIG_DIR="/srv/claude",
                          XDG_DATA_HOME="/srv/data"),
            ":".join([self.HOME + "/.codex/tmp/arg0/b", self.HOME + "/.claude/plugins/p/bin"]))

    def test_relative_overrides_fall_back_to_the_default_roots(self):
        entries = [self.HOME + "/.codex/tmp/a", "relative/codex/tmp/a", "/usr/bin"]
        self.assertEqual(self.filtered(entries, CODEX_HOME="relative/codex"), "relative/codex/tmp/a:/usr/bin")

    def test_plugin_root_values_are_roots_even_outside_known_homes(self):
        entries = ["/opt/plugins/story/bin", "/opt/plugin-data/bin", "/opt/plugins-other/bin"]
        self.assertEqual(self.filtered(entries, PLUGIN_ROOT="/opt/plugins/story", CLAUDE_PLUGIN_DATA="/opt/plugin-data",
                                       CLAUDE_PLUGIN_ROOT="relative"),
                         "/opt/plugins-other/bin")

    def test_without_home_only_explicit_roots_apply(self):
        self.assertEqual(policy.filter_path({"PATH": "/Users/x/.codex/tmp:/opt/p/bin:/usr/bin",
                                             "PLUGIN_ROOT": "/opt/p"}),
                         "/Users/x/.codex/tmp:/usr/bin")

    def test_unset_path_stays_unset(self):
        self.assertIsNone(policy.filter_path({"HOME": self.HOME}))
        self.assertNotIn("PATH", policy.client_environment({"HOME": self.HOME}))

    def test_client_environment_is_the_allowlist_and_nothing_else(self):
        environ = {name: "value-" + name for name in policy.SERVER_MAY_SEE}
        environ.update(PATH="/usr/bin", SH758_UNLISTED_MARKER="never-enumerated", GH_TOKEN="secret",
                       STORYHOOK_STORE_PATH="/store", STORY_BIN="/bin/story", CLAUDE_CODE_OAUTH_TOKEN="user")
        client = policy.client_environment(environ)
        self.assertEqual(set(client), set(policy.SERVER_MAY_SEE))
        self.assertEqual(client["PATH"], "/usr/bin")
        for name in policy.SERVER_MAY_SEE - {"PATH"}:
            self.assertEqual(client[name], environ[name])

    def test_routing_is_allowed_and_no_storyhook_or_credential_name_is(self):
        self.assertTrue({"TMUX_TMPDIR", "TMUX", "TMUX_PANE"} <= policy.SERVER_MAY_SEE)
        for name in policy.SERVER_MAY_SEE:
            self.assertFalse(name.startswith(("STORY_", "STORYHOOK_", "CLAUDE", "CODEX_", "GH_", "GITHUB_")), name)

    def test_selectors_are_not_retained_by_a_server(self):
        self.assertEqual(set(policy.PANE_SELECTORS) & policy.SERVER_MAY_SEE, {"XDG_STATE_HOME"})

    def test_parse_environment_reads_set_values_and_skips_removals(self):
        listing = "A=b\n-REMOVED\nD=x=y\n\nEMPTY=\n"
        self.assertEqual(policy.parse_environment(listing), {"A": "b", "D": "x=y", "EMPTY": ""})

    def test_retained_names_are_the_session_scoped_families_only(self):
        variables = {"PLUGIN_ROOT": "/x", "CODEX_SHELL": "1", "CODEX_HOME": "/c", "CODEXISH": "1",
                     "CLAUDECODE": "1", "CLAUDE_CODE_OAUTH_TOKEN": "user", "STORYHOOK_ACTIVITY_LOG_DIR": "/j",
                     "STORY_TARGET_SESSION": "s", "STORYHOOK_STORE_PATH": "/s", "PATH": "/usr/bin",
                     "SH758_UNLISTED_MARKER": "1"}
        self.assertEqual(policy.retained_names(variables),
                         ["CLAUDECODE", "CODEX_SHELL", "PLUGIN_ROOT", "STORYHOOK_ACTIVITY_LOG_DIR",
                          "STORY_TARGET_SESSION"])


class PrivateServerTests(unittest.TestCase):
    """Each fixture owns one private default server through TMUX_TMPDIR."""

    def setUp(self):
        self.root = Path(tempfile.mkdtemp(prefix="sh758-", dir="/tmp"))
        self.addCleanup(shutil.rmtree, self.root, True)
        self.home = self.root / "home"
        self.home.mkdir()
        tmpdir = self.root / "tmux"
        tmpdir.mkdir()
        home = str(self.home)
        self.store = self.root / "store with spaces.db"
        self.plugin = home + "/.codex/plugins/cache/storyhook/story/3.0.3"
        tmux = shutil.which("tmux")
        self.assertIsNotNone(tmux, "tmux is required")
        system = [entry for entry in os.environ.get("PATH", "").split(":") if entry]
        self.host_entries = [home + "/.codex/tmp/arg0/codex-arg0kVkT4O",
                             home + "/.cache/codex-runtimes/codex-primary-runtime/dependencies/bin/override",
                             home + "/.claude/plugins/cache/agentics/greenlight/3.9.5/bin",
                             home + "/.local/share/storyhook/plugins/3.0.3/plugins/story/bin",
                             self.plugin + "/bin"]
        kept = [CRYPTEX, APP_BUNDLE, home + "/.codexfoo/bin"]
        self.expected_path = ":".join([str(Path(tmux).parent), kept[0]] + system + kept[1:])
        path = ":".join([str(Path(tmux).parent), self.host_entries[0], kept[0], self.host_entries[1]]
                        + system + self.host_entries[2:] + kept[1:])
        self.clean = {name: os.environ[name] for name in ("USER", "SHELL", "LANG", "TERM") if name in os.environ}
        self.clean.update(HOME=home, TMUX_TMPDIR=str(tmpdir), TMPDIR=str(self.root), PATH=self.expected_path)
        self.poisoned = dict(self.clean, PATH=path, **{
            "PLUGIN_ROOT": self.plugin, "PLUGIN_DATA": home + "/.codex/plugins/data/story-storyhook",
            "CLAUDE_PLUGIN_ROOT": self.plugin, "CLAUDE_PLUGIN_DATA": home + "/.codex/plugins/data/story-storyhook",
            "CODEX_HOME": home + "/.codex", "CODEX_SHELL": "1", "CODEX_APP_TOOLS_PIPE_PATH": "/tmp/codex-tools",
            "CODEX_INTERNAL_ORIGINATOR_OVERRIDE": "Codex Desktop", "CLAUDECODE": "1",
            "CLAUDE_CODE_SESSION_ID": "parent", "CLAUDE_CODE_CHILD_SESSION": "1",
            "CLAUDE_CODE_MESSAGING_TOKEN": "parent-capability", "CLAUDE_PID": "1",
            "STORY_TARGET_SESSION": "owned", "STORY_CREATE_SESSION": "1", "STORY_LANE_TOOL_CEILING_MS": "1",
            "STORY_AGENT": "codex", "STORYHOOK_ACTIVITY_LOG_DIR": str(self.root / "journal"),
            "STORYHOOK_ACTIVITY_CONTEXT": "project=fixture reader", "GH_TOKEN": "secret",
            "STORY_WORKSPACE_LOCK_FD": "9", "STORYHOOK_STORE_PATH": str(self.store),
            "STORYHOOK_VERIFIER_MIRROR": "1", "SH758_UNLISTED_MARKER": "never-enumerated"})
        self.addCleanup(self.kill_server)

    def tmux(self, *args, env=None, check=True):
        """Bound every private control operation."""
        result = subprocess.run(["tmux", *args], env=env or self.clean, capture_output=True, text=True,
                                timeout=DEADLINE)
        if check:
            self.assertEqual(result.returncode, 0, (args, result.stderr))
        return result

    def kill_server(self):
        """Stop only this fixture's server; its socket lives under the fixture root."""
        self.tmux("kill-server", check=False)
        end = time.monotonic() + DEADLINE
        while True:
            probe = self.tmux("list-sessions", check=False)
            if probe.returncode and policy.reports_no_server(probe.stderr):
                return
            self.assertLess(time.monotonic(), end, "fixture tmux server survived kill-server")
            time.sleep(.02)

    def global_environment(self):
        return policy.parse_environment(self.tmux("show-environment", "-g").stdout)

    def assert_server_retains_only_the_allowlist(self):
        retained = self.global_environment()
        for name in self.poisoned:
            if name not in policy.SERVER_MAY_SEE:
                with self.subTest(name=name):
                    self.assertNotIn(name, retained)
        allowed = policy.SERVER_MAY_SEE | policy.TMUX_OWNED
        self.assertLessEqual(set(retained), allowed, sorted(set(retained) - allowed))
        self.assertEqual(retained["PATH"], self.expected_path)
        self.assertEqual(retained["HOME"], str(self.home))

    def pane_environment(self, target):
        """Open a pane in `target` and return the environment its process received."""
        receipt = self.root / f"pane-{time.monotonic_ns()}.json"
        command = shlex.join(["python3", "-c", DUMP, str(receipt)]) + "; exec sleep 300"
        self.tmux("new-window", "-d", "-t", target, command)
        return self.read_receipt(receipt)

    def read_receipt(self, receipt):
        end = time.monotonic() + DEADLINE
        while not receipt.exists():
            self.assertLess(time.monotonic(), end, "pane never published its environment")
            time.sleep(.02)
        return json.loads(receipt.read_text())

    def run_launcher(self, *args, env):
        return subprocess.run(["python3", str(LAUNCHER), "-f", "/dev/null", *args], env=env,
                              capture_output=True, text=True, timeout=DEADLINE)

    def test_dispatch_launcher_starts_a_server_that_retains_only_the_allowlist(self):
        receipt = self.root / "first.json"
        command = shlex.join(["python3", "-c", DUMP, str(receipt)]) + "; exec sleep 300"
        started = self.run_launcher("new-session", "-d", "-s", "owned", "-c", str(self.root), command,
                                    env=self.poisoned)
        self.assertEqual(started.returncode, 0, started.stderr)
        self.assert_server_retains_only_the_allowlist()
        first = self.read_receipt(receipt)
        # Storyhook's own selectors ride the pane, never the server.
        self.assertEqual(first["STORYHOOK_STORE_PATH"], str(self.store))
        self.assertEqual(first["STORYHOOK_VERIFIER_MIRROR"], "1")
        self.assertEqual(first["STORY_BIN"], "")
        self.assertEqual(first["GH_TOKEN"], "")
        # `new-session -e` scopes selectors to the storyhook-owned session, so
        # its later lanes reach the same store without the server retaining it.
        later = self.pane_environment("=owned:")
        self.assertEqual(later["STORYHOOK_STORE_PATH"], str(self.store))
        for name in ("PLUGIN_ROOT", "CLAUDECODE", "CODEX_SHELL", "STORYHOOK_ACTIVITY_LOG_DIR",
                     "SH758_UNLISTED_MARKER"):
            self.assertNotIn(name, later)

    def test_dispatch_launcher_leaves_an_existing_servers_global_environment_alone(self):
        self.tmux("-f", "/dev/null", "new-session", "-d", "-s", "user", "exec sleep 300")
        self.tmux("set-environment", "-g", "SH758_USER_GLOBAL", "kept")
        before = self.tmux("show-environment", "-g").stdout
        started = self.run_launcher("new-session", "-d", "-s", "owned", "exec sleep 300", env=self.poisoned)
        self.assertEqual(started.returncode, 0, started.stderr)
        self.assertEqual(self.tmux("show-environment", "-g").stdout, before)

    def test_dispatch_launcher_surfaces_an_unexplained_server_probe_failure(self):
        # tmux refuses a socket directory other users can reach. That is not
        # "no server", so the launcher must not guess which environment to use.
        unsafe = self.root / "unsafe"
        (unsafe / f"tmux-{os.getuid()}").mkdir(parents=True)
        (unsafe / f"tmux-{os.getuid()}").chmod(0o707)
        broken = dict(self.poisoned, TMUX_TMPDIR=str(unsafe))
        started = self.run_launcher("new-session", "-d", "-s", "owned", "exec sleep 300", env=broken)
        self.assertNotEqual(started.returncode, 0)
        self.assertIn("unsafe permissions", started.stderr)
        self.assertIn("server probe", started.stderr)

    def test_verification_view_starts_a_server_that_retains_only_the_allowlist(self):
        reader = self.root / "reader"
        reader.write_text("#!/bin/sh\nexec sleep 300\n")
        reader.chmod(0o700)
        viewed = subprocess.run(["python3", "-c", VIEW_PROGRAM, "fixture", str(self.root / "logs"), str(reader)],
                                env=self.poisoned, capture_output=True, text=True, timeout=DEADLINE)
        self.assertEqual(viewed.returncode, 0, viewed.stderr)
        self.assert_server_retains_only_the_allowlist()
        name = self.tmux("display-message", "-p", "-t", "=fixture:=verification", "#{window_name}").stdout.strip()
        self.assertEqual(name, "verification")

    def polluted_server(self):
        """A server a pre-SH-758 storyhook (or a user) started from a host process."""
        self.tmux("-f", "/dev/null", "new-session", "-d", "-s", "owned", "exec sleep 300", env=self.poisoned)
        self.tmux("new-session", "-d", "-s", "user", "exec sleep 300")
        self.tmux("set-environment", "-t", "=user", "SH758_USER_SESSION", "kept")

    def scrub(self, session, env=None):
        return subprocess.run(["python3", str(ENV_CLI), "scrub-session", session], env=env or self.clean,
                              capture_output=True, text=True, timeout=DEADLINE)

    def test_scrubbing_an_owned_session_truly_unsets_retained_state_for_its_new_panes(self):
        self.polluted_server()
        global_before = self.tmux("show-environment", "-g").stdout
        user_before = self.tmux("show-environment", "-t", "=user").stdout
        scrubbed = self.scrub("owned")
        self.assertEqual(scrubbed.returncode, 0, scrubbed.stderr)
        report = json.loads(scrubbed.stdout)
        self.assertIn("PLUGIN_ROOT", report["removed"])
        self.assertTrue(report["path_rewritten"])
        pane = self.pane_environment("=owned:")
        for name in policy.retained_names(self.poisoned):
            with self.subTest(name=name):
                self.assertNotIn(name, pane)
        self.assertEqual(pane["PATH"], self.expected_path)
        self.assertEqual(pane["CODEX_HOME"], str(self.home / ".codex"))
        # Retained cleanup removes known session state only; it is not the allowlist.
        self.assertEqual(pane["SH758_UNLISTED_MARKER"], "never-enumerated")
        self.assertEqual(self.tmux("show-environment", "-g").stdout, global_before)
        self.assertEqual(self.tmux("show-environment", "-t", "=user").stdout, user_before)
        self.assertEqual(self.pane_environment("=user:")["PLUGIN_ROOT"], self.plugin)
        again = json.loads(self.scrub("owned").stdout)
        self.assertEqual(again, {"removed": [], "path_rewritten": False, "session": "owned"})

    def test_scrubbing_a_missing_session_fails_loudly(self):
        self.polluted_server()
        scrubbed = self.scrub("absent")
        self.assertNotEqual(scrubbed.returncode, 0)
        self.assertIn("absent", scrubbed.stderr)

    def test_retained_reports_session_state_on_the_current_server(self):
        self.polluted_server()
        reported = subprocess.run(["python3", str(ENV_CLI), "retained"], env=self.clean, capture_output=True,
                                  text=True, timeout=DEADLINE)
        self.assertEqual(reported.returncode, 0, reported.stderr)
        self.assertEqual(json.loads(reported.stdout)["names"], policy.retained_names(self.poisoned))

    def test_retained_is_empty_on_a_server_storyhook_started(self):
        started = self.run_launcher("new-session", "-d", "-s", "owned", "exec sleep 300", env=self.poisoned)
        self.assertEqual(started.returncode, 0, started.stderr)
        reported = subprocess.run(["python3", str(ENV_CLI), "retained"], env=self.clean, capture_output=True,
                                  text=True, timeout=DEADLINE)
        self.assertEqual(json.loads(reported.stdout), {"names": []})


if __name__ == "__main__":
    unittest.main()
