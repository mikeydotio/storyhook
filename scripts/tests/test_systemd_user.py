#!/usr/bin/env python3
"""Exercise production daemon ownership in an explicitly disposable Linux account.

Run as that account with STORYHOOK_SYSTEMD_TEST_HOME equal to its HOME and
--binary naming a production (not fault-injection) build copied outside target/.
The caller provisions and tears down the account and its user manager. This
suite never changes the host account, lingering, or administrator policies.
"""
import argparse
import concurrent.futures
import json
import os
from pathlib import Path
import subprocess
import sys


def main():
    """Compare the inherited control with the managed daemon's kernel metadata."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    args = parser.parse_args()
    home = Path.home()
    assert sys.platform == "linux" and os.getuid() != 0
    assert os.environ.get("STORYHOOK_SYSTEMD_TEST_HOME") == str(home), "requires an explicitly disposable account"
    binary = args.binary.resolve()
    store = home / "test stores" / "literal %n $name.db"
    store.parent.mkdir(exist_ok=True)
    base = [str(binary), "--store-path", str(store)]
    env = dict(os.environ, PATH=f"{binary.parent}:/usr/bin:/bin")
    env.pop("STORYHOOK_PARENT_PID", None)
    env.pop("STORYHOOK_FULL_AUTO", None)
    env.pop("STORYHOOK_AUTO", None)
    env["XDG_CONFIG_HOME"] = str(home / "config")
    env["XDG_STATE_HOME"] = str(home / "state")

    def run(*tail, prefix=(), overrides=None, check=True):
        result = subprocess.run([*prefix, *base, *tail], env={**env, **(overrides or {})}, text=True, capture_output=True, timeout=90)
        if check:
            assert result.returncode == 0, (result.args, result.stdout, result.stderr)
        return result

    def info():
        files = list((home / "state/storyhook/daemons").glob("*/daemon.json"))
        assert len(files) == 1, files
        return json.loads(files[0].read_text())

    def kernel(pid):
        stat = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
        cgroup = Path(f"/proc/{pid}/cgroup").read_text()
        # ionice uses the kernel syscall, independent of StoryHook's status text.
        io = subprocess.check_output(["ionice", "-p", str(pid)], text=True).strip()
        return int(stat[16]), cgroup, io

    unit = None
    try:
        # Recover this fixture's registration after a prior interrupted run.
        run("daemon", "uninstall")
        # The actual unfixed mechanism: niced clients pass their class to a fork.
        control = run("daemon", "start", prefix=("nice", "-n", "10", "ionice", "-c2", "-n7"))
        assert "no systemd user service" in control.stderr, control.stderr
        inherited = info()
        nice, group, io = kernel(inherited["pid"])
        assert nice == 10 and "prio 7" in io, (nice, group, io)
        assert group == Path("/proc/self/cgroup").read_text()
        run("daemon", "stop")
        installed = run("daemon", "install")
        managed = info()
        unit = managed["owner"]["Systemd"]["unit"]
        run("daemon", "stop")
        first = run("daemon", "start", prefix=("nice", "-n", "10", "ionice", "-c2", "-n7"))
        assert "starting the daemon directly" not in first.stderr
        managed = info()
        nice, group, io = kernel(managed["pid"])
        assert nice == 0 and "best-effort" in io and "prio 4" in io, (nice, group, io)
        assert unit in group and group != Path("/proc/self/cgroup").read_text(), group
        assert managed["store_path"] == str(store)
        status = run("daemon", "status").stdout
        assert "systemd user service" in status and "nice 0" in status and "cgroup" in status, status
        doctor = run("doctor", "install").stdout
        assert "agent PATH" in doctor and env["PATH"] in doctor, doctor
        with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
            list(pool.map(lambda _: run("daemon", "start"), range(4)))
        assert info()["pid"] == managed["pid"]
        run("daemon", "restart")
        restarted = info()
        assert restarted["pid"] != managed["pid"] and restarted["port"] == managed["port"]
        run("daemon", "install")
        assert info()["owner"]["Systemd"]["unit"] == unit
        run("daemon", "uninstall")
        unit_path = home / "config/systemd/user" / unit
        assert not unit_path.exists()
        absent = run("daemon", "start", overrides={"DBUS_SESSION_BUS_ADDRESS": "unix:path=/nonexistent/sh787-bus", "XDG_RUNTIME_DIR": str(home / "no-runtime") })
        assert "no usable systemd user manager" in absent.stderr, absent.stderr
        assert info()["owner"]["Forked"]["reason"] == "NoUserManager"
        run("daemon", "stop")
        print(json.dumps({"result": "PASS", "unit": unit, "nice": nice, "io": io, "cgroup": group.strip(), "port_preserved": restarted["port"]}))
    finally:
        run("daemon", "stop", "--force", check=False)
        if unit:
            subprocess.run(["systemctl", "--user", "disable", "--now", unit], env=env, check=False, capture_output=True, timeout=30)


if __name__ == "__main__":
    main()
