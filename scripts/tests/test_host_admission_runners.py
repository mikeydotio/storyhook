"""Simultaneous runners across two projects stay within one host budget (SH-869, acceptance 2)."""

import os
from pathlib import Path
import subprocess
import sys
import threading
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parent))
from host_admit_fixture import PROCESS_ALLOWANCE_S, AuthorityCase

# A stand-in workload: it starts a descendant in its own process group and a
# grandchild launched with Python's default close_fds, runs briefly, and, for
# a pool, runs one nested leaf through the adapter inside its grant.
WORKLOAD = """
import os, subprocess, sys, time
nested = sys.argv[1:]
grandchild = "import subprocess,sys,time;subprocess.Popen([sys.executable,'-c','import time;time.sleep(0.2)']);time.sleep(0.2)"
subprocess.Popen([sys.executable, "-c", grandchild], process_group=0)
time.sleep(0.3)
if nested:
    sys.exit(subprocess.run(nested).returncode)
"""

# (entry, requested units, pool runs a nested leaf)
RUNNERS = (("rustc", 1, False), ("rust-pool", 4, True), ("plugin-pool", 4, True), ("browser-pool", 3, True))


class TwoProjects(AuthorityCase):
    def project(self, name):
        path = self.dir / name
        path.mkdir()
        subprocess.run(["git", "init", "-q"], cwd=path, check=True)
        return path

    def test_compile_rust_plugin_and_browser_work_across_two_projects_share_one_cap(self):
        normal = dict(cpu=self.value["capacity"]["cpu"] - self.value["headroom"]["cpu"]
                      - self.value["reserve"]["cpu"],
                      memory=self.value["capacity"]["memory"] - self.value["headroom"]["memory"]
                      - self.value["reserve"]["memory"])
        peaks, stop, errors = [], threading.Event(), []

        def sample():
            while not stop.is_set():
                try:
                    peaks.append(self.client.call("status")["allocated"])
                except Exception as error:  # surfaced below; the sampler must not die silently
                    errors.append(error)
                stop.wait(self.value["sample_ms"] / 1000)

        sampler = threading.Thread(target=sample)
        sampler.start()
        env = {k: v for k, v in os.environ.items() if not k.startswith("STORYHOOK_HOST_")}
        processes = []
        try:
            for project in (self.project("alpha"), self.project("beta")):
                for entry, units, pool in RUNNERS:
                    nested = self.cli("--entry", "cargo-test-binary", "--", sys.executable, "-c", "pass") if pool else []
                    processes.append((project.name, entry, subprocess.Popen(
                        self.cli("--entry", entry, "--units", str(units), "--",
                                 sys.executable, "-c", WORKLOAD, *nested),
                        cwd=project, env=env, stderr=subprocess.PIPE, text=True)))
            for project, entry, process in processes:
                _, stderr = process.communicate(timeout=PROCESS_ALLOWANCE_S * 4)
                self.assertEqual(process.returncode, 0, f"{project} {entry}: {stderr}")
        finally:
            stop.set()
            sampler.join()
            for _, _, process in processes:
                if process.poll() is None:
                    process.kill()
                    process.wait()
        self.assertEqual(errors, [], "the sampler could not read the authority")
        self.assertTrue(peaks, "the authority was never sampled")
        for observed in peaks:
            self.assertLessEqual(observed["cpu"], normal["cpu"], observed)
            self.assertLessEqual(observed["memory"], normal["memory"], observed)
        self.assertGreater(max(p["cpu"] for p in peaks), 0, "the sampler saw no grant at all")

        events, cursor = [], 0
        while True:
            page = self.client.call("events", after=cursor)
            if not page:
                break
            events.extend(page)
            cursor = page[-1]["sequence"]
        grants = [e for e in events if e["event"] == "grant"]
        self.assertEqual(len(grants), len(processes), "nested leaves took no lease of their own")
        self.assertEqual({e["project"] for e in grants}, {str((self.dir / name).resolve() / ".git")
                                                         for name in ("alpha", "beta")})
        self.assertEqual({e["work"] for e in grants}, {"build", "test"})
        self.assertTrue(any(e["wait_ms"] > 0 for e in grants), "contention never made a runner wait")
        self.assertEqual(self.client.call("status")["allocated"], dict(cpu=0, memory=0))
        for lease in self.leases():
            self.assertEqual(lease["state"], "released", lease)

    def test_a_cancelled_runner_holds_capacity_until_its_tree_settles(self):
        stubborn = ("import signal,subprocess,sys,time;"
                    "subprocess.Popen([sys.executable,'-c','import signal,time;"
                    "signal.signal(signal.SIGTERM,signal.SIG_IGN);time.sleep(60)'],process_group=0);"
                    "time.sleep(60)")
        project = self.project("gamma")
        env = {k: v for k, v in os.environ.items() if not k.startswith("STORYHOOK_HOST_")}
        process = subprocess.Popen(self.cli("--entry", "plugin-pool", "--units", "2", "--",
                                            sys.executable, "-c", stubborn), cwd=project, env=env,
                                   stderr=subprocess.PIPE, text=True)
        self.eventually(lambda: any(l["state"] == "running" for l in self.leases()), "never ran")
        held = self.client.call("status")["allocated"]["cpu"]
        self.assertEqual(held, 500 + 2 * 1000)
        process.terminate()
        _, stderr = process.communicate(timeout=PROCESS_ALLOWANCE_S * 2)
        self.assertNotEqual(process.returncode, 0, stderr)
        self.assertEqual(self.client.call("status")["allocated"]["cpu"], 0,
                         "released only after the TERM-ignoring descendant was killed and reaped")


if __name__ == "__main__":
    unittest.main()
