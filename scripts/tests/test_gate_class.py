#!/usr/bin/env python3
"""SH-785: every verifier gate runs at the verifier's scheduling class.

A gate runs for minutes beside agent sessions, hooks and the daemon. The
verifier starts it clamped to utility QoS on macOS, and at nice +10 with
best-effort I/O level 7 on Linux, so that interactive work wins contention.
The class applies to the gate session only; the supervisor that reaps it keeps
its own class.

Why the mechanism case starts its supervisor under `taskpolicy -c background`:
once this ships, the installed verifier runs storyhook's own gate at utility,
and no descendant can shed that clamp. A case that only read "the gate is at
utility" would then pass even with the verifier's clamp deleted. A later `-c`
clamp replaces an earlier one, so under a background supervisor only the
verifier's own clamp can lift the gate to utility -- in every ambient class,
including inside an enclosing verifier gate.

Mutation-checked before commit against scripts/verifier-owner.py, each
mutant restored from a copy afterwards:
- the class prefix dropped from the gate launch -> the mechanism and the
  production cases red (the gate read its caller's class);
- the supervisor re-executed under the class before the fork -> the
  mechanism case red (the supervisor read utility, not background);
- an empty launch report read as launched -> test_verifier_verdict.py's
  class-tool case red (the existing launch-failure case stays green: a
  missing command still reports a failure);
- the launcher catching only OSError -> test_verifier_verdict.py's
  launcher-fault case red (a ValueError after LAUNCHING read as a red gate).
"""

import importlib.util
import json
import os
from pathlib import Path
import stat
import sys
import tempfile
import unittest

sys.dont_write_bytecode = True
from test_verifier_lifecycle import SCRIPTS, VerifierLifecycle

sys.path.insert(0, str(SCRIPTS))
from verifier_state import Refusal

# qos_class_t values from <sys/qos.h>; a public ABI, not scheduler detail.
QOS_CLASS_UTILITY = 0x11
QOS_CLASS_BACKGROUND = 0x09
# PRIO_DARWIN_PROCESS from <sys/resource.h>: reads 1 under darwin-BG
# (`taskpolicy -b`, launchd ProcessType=Background), which no QoS clamp lifts.
PRIO_DARWIN_PROCESS = 4

# Reports the class of the process that runs it, and of its parent, as JSON.
# For a gate the parent is the gate supervisor, the only process between the
# verifier and the gate that must keep its own class.
REPORTER = r'''
import ctypes, json, os, subprocess, sys

def pri(pid):
    listed = subprocess.run(["ps", "-o", "pri=", "-p", str(pid)],
                            capture_output=True, text=True, check=True)
    return int(listed.stdout)

def ionice(pid):
    listed = subprocess.run(["ionice", "-p", str(pid)],
                            capture_output=True, text=True, check=True)
    return listed.stdout.strip()

me, parent = os.getpid(), os.getppid()
report = {"pid": me, "parent": parent, "pri": pri(me), "parent_pri": pri(parent),
          "nice": os.getpriority(os.PRIO_PROCESS, 0)}
if sys.platform == "darwin":
    libc = ctypes.CDLL(None)
    libc.qos_class_self.restype = ctypes.c_uint
    report["qos"] = libc.qos_class_self()
else:
    report["ionice"] = ionice(me)
    report["parent_ionice"] = ionice(parent)
with open(sys.argv[1], "w") as out:
    json.dump(report, out)
'''


def load_owner():
    """Import the production supervisor module under a legal module name."""
    spec = importlib.util.spec_from_file_location("verifier_owner", SCRIPTS / "verifier-owner.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def executable(path, body="#!/bin/sh\nexit 0\n"):
    """Write one executable file, the way a class tool is found on PATH."""
    path.write_text(body)
    path.chmod(path.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)
    return path


class GateClassTable(unittest.TestCase):
    """The class argv and its refusals, independent of the host platform."""

    def setUp(self):
        self.owner = load_owner()
        self.tmp = tempfile.TemporaryDirectory(prefix="sh785-", dir="/tmp")
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name).resolve()

    def test_macos_class_is_utility_and_never_background(self):
        """Background QoS and darwin-BG confine a gate to the efficiency cores."""
        self.assertEqual(self.owner.GATE_CLASS["darwin"],
                         [("/usr/sbin/taskpolicy", ["-c", "utility"])])
        words = [word for tool, arguments in self.owner.GATE_CLASS["darwin"]
                 for word in [tool, *arguments]]
        self.assertNotIn("-b", words)
        self.assertNotIn("background", words)
        self.assertNotIn("maintenance", words)

    @unittest.skipUnless(sys.platform == "darwin", "taskpolicy(8) ships only with macOS")
    def test_macos_class_resolves_to_the_system_taskpolicy(self):
        """No PATH lookup: the system tool is named absolutely."""
        self.assertEqual(self.owner.gate_class("darwin", path=str(self.root)),
                         ["/usr/sbin/taskpolicy", "-c", "utility"])

    def test_linux_class_resolves_each_tool_to_an_absolute_path(self):
        """nice lowers the CPU share and ionice the disk share; both are resolved."""
        tools = self.root / "bin"
        tools.mkdir()
        nice = executable(tools / "nice")
        ionice = executable(tools / "ionice")
        self.assertEqual(self.owner.gate_class("linux", path=str(tools)),
                         [str(nice), "-n", "10", str(ionice), "-c2", "-n7"])

    def test_missing_linux_tool_is_refused_by_name(self):
        """A host without util-linux gets a refusal, never an unclassed gate."""
        tools = self.root / "bin"
        tools.mkdir()
        executable(tools / "nice")
        with self.assertRaises(Refusal) as refused:
            self.owner.gate_class("linux", path=str(tools))
        self.assertIn("ionice", str(refused.exception))

    def test_relative_path_entry_cannot_supply_a_class_tool(self):
        """The gate's working directory is the candidate tree, which it must not pick from."""
        executable(self.root / "nice")
        executable(self.root / "ionice")
        here = os.getcwd()
        os.chdir(self.root)
        self.addCleanup(os.chdir, here)
        with self.assertRaises(Refusal) as refused:
            self.owner.gate_class("linux", path=".")
        self.assertIn("nice", str(refused.exception))
        self.assertIn("absolute", str(refused.exception))

    def test_non_executable_absolute_tool_is_refused(self):
        """An absolute tool is checked, not trusted."""
        inert = self.root / "taskpolicy"
        inert.write_text("#!/bin/sh\n")
        classes = {"testos": [(str(inert), ["-c", "utility"])]}
        with self.assertRaises(Refusal) as refused:
            self.owner.gate_class("testos", path=str(self.root), classes=classes)
        self.assertIn(str(inert), str(refused.exception))

    def test_unknown_platform_is_refused_by_name(self):
        """Only a class chosen for the platform may run a gate."""
        with self.assertRaises(Refusal) as refused:
            self.owner.gate_class("plan9", path=str(self.root))
        self.assertIn("plan9", str(refused.exception))


class GateSchedulingClass(unittest.TestCase):
    """Drive the real supervisor and the real verify-pr.sh chain with real Git."""

    def setUp(self):
        """Reuse the real-Git fixture without inheriting its unrelated test cases."""
        self.fx = VerifierLifecycle()
        self.fx.setUp()
        self.addCleanup(self.fx.doCleanups)
        self.reporter = self.fx.root / "report-class.py"
        self.reporter.write_text(REPORTER)
        self.assertEqual(self.fx.ensure()["result"], "verifier-worktree-ready")

    def report(self, name):
        """Read one reporter's JSON from the fixture root, never the worktree."""
        return json.loads((self.fx.root / name).read_text())

    def lowered(self):
        """The prefix that starts a process below the verifier's class."""
        if sys.platform == "darwin":
            return ["/usr/sbin/taskpolicy", "-c", "background"]
        return ["ionice", "-c3"]

    def refuse_darwin_bg_ambient(self):
        """darwin-BG is not lifted by any QoS clamp, so no gate could reach utility."""
        if sys.platform == "darwin" and os.getpriority(PRIO_DARWIN_PROCESS, 0) == 1:
            self.fail("this suite runs under darwin-BG (taskpolicy -b or launchd "
                      "ProcessType=Background); a gate started from here cannot reach utility. "
                      "The daemon must not run at ProcessType=Background (SH-784).")

    def test_gate_is_lifted_to_the_class_while_its_supervisor_keeps_its_own(self):
        """Only the verifier's own clamp can lift the gate above a background supervisor."""
        self.refuse_darwin_bg_ambient()
        control = self.fx.command(*self.lowered(), sys.executable, str(self.reporter),
                                  str(self.fx.root / "control.json"))
        self.assertEqual(control.returncode, 0, control.stdout + control.stderr)
        control = self.report("control.json")
        result = self.fx.owner(*self.lowered(), sys.executable, str(SCRIPTS / "verifier-owner.py"),
                               "gate", str(self.fx.common), str(self.fx.wt), "--",
                               sys.executable, str(self.reporter), str(self.fx.root / "gate.json"))
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        gate = self.report("gate.json")
        if sys.platform == "darwin":
            # The reader is not a constant: a process started lowered reads background.
            self.assertEqual(control["qos"], QOS_CLASS_BACKGROUND, control)
            self.assertEqual(gate["qos"], QOS_CLASS_UTILITY, gate)
            self.assertGreater(gate["pri"], gate["parent_pri"], gate)
            # The supervisor kept the lowered class it was started with.
            self.assertEqual(gate["parent_pri"], control["pri"], (gate, control))
        else:
            self.assertEqual(control["ionice"], "idle", control)
            self.assertEqual(gate["ionice"], "best-effort: prio 7", gate)
            self.assertEqual(gate["parent_ionice"], "idle", gate)
            self.assertEqual(gate["nice"], min(control["nice"] + 10, 19), (gate, control))

    def test_gate_status_and_signal_death_pass_through_the_class_chain(self):
        """The class tools replace themselves; the supervisor still sees the gate's own end."""
        for script, expected in (("exit 7", 7), ("kill -TERM $$", 128 + 15)):
            with self.subTest(script=script):
                result = self.fx.owner(sys.executable, str(SCRIPTS / "verifier-owner.py"),
                                       "gate", str(self.fx.common), str(self.fx.wt), "--",
                                       "sh", "-c", script)
                self.assertEqual(result.returncode, expected, result.stdout + result.stderr)

    def test_production_gate_reports_the_class(self):
        """The literal acceptance: a gate verify-pr.sh starts reads the verifier's class."""
        self.refuse_darwin_bg_ambient()
        tree = self.fx.git("merge-tree", "--write-tree", self.fx.base, self.fx.head)
        result = self.fx.command("bash", str(SCRIPTS / "verify-pr.sh"), "--run-gate", "1", tree,
                                 self.fx.base, self.fx.head, str(self.fx.wt), "--",
                                 sys.executable, str(self.reporter), str(self.fx.root / "gate.json"))
        verdict = json.loads(result.stdout)
        self.assertEqual(verdict["result"], "gate-passed", verdict)
        gate = self.report("gate.json")
        if sys.platform == "darwin":
            self.assertEqual(gate["qos"], QOS_CLASS_UTILITY, gate)
        else:
            self.assertEqual(gate["ionice"], "best-effort: prio 7", gate)


if __name__ == "__main__":
    unittest.main(defaultTest=["GateClassTable", "GateSchedulingClass"])
