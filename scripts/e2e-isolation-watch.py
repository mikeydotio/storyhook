#!/usr/bin/env python3
"""Run and report the local nightly file-isolation proof; never certify a gate."""
import fcntl
import hashlib
import json
import os
from pathlib import Path
import plistlib
import shutil
import signal
import subprocess
import sys
import tempfile
import time

LABEL = "io.mikey.storyhook.e2e-isolation"
STALE_SECONDS = 2 * 24 * 60 * 60


def git(root, *args):
    """Read Git with the failing command and stderr retained on error."""
    return subprocess.check_output(["git", "-C", str(root), *args],
                                   text=True, stderr=subprocess.PIPE).strip()


def save(path, value):
    """Publish one complete JSON record atomically."""
    with tempfile.NamedTemporaryFile(mode="w", dir=path.parent, delete=False) as stream:
        temporary = Path(stream.name)
        json.dump(value, stream, indent=2)
        stream.write("\n")
        stream.flush()
        os.fsync(stream.fileno())
    try:
        os.replace(temporary, path)
    finally:
        temporary.unlink(missing_ok=True)


def process_started(pid):
    """Read start identity so PID reuse cannot disguise an interrupted attempt."""
    result = subprocess.run(["ps", "-p", str(pid), "-o", "lstart="],
                            capture_output=True, text=True)
    return result.stdout.strip() if result.returncode == 0 else ""


def branch(controller):
    """Read the integration role from the controller's tracked branch policy."""
    value = subprocess.check_output([
        "/bin/bash", "-c", 'source "$1"; printf "%s" "$STORYHOOK_INTEGRATION_BRANCH"',
        "isolation-watch", str(controller / "scripts/branch-policy.sh")], text=True)
    subprocess.run(["git", "check-ref-format", "refs/heads/" + value], check=True,
                   stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    return value


def run_proof(checkout, artifacts, log):
    """Forward cancellation to the proof's process group and await its cleanup."""
    environment = os.environ.copy()
    for name in ("GH_CONFIG_DIR", "GH_TOKEN", "GITHUB_TOKEN", "GH_ENTERPRISE_TOKEN",
                 "GITHUB_ENTERPRISE_TOKEN", "STORY_BIN", "STORYHOOK_GITHUB_AUTHORITY",
                 "STORYHOOK_GITHUB_EXPECTED", "STORYHOOK_GATE_PROGRESS", "STORYHOOK_GATE_PROGRESS_PATH"):
        environment.pop(name, None)
    # The scheduled proof must refuse an accidental test.only through the
    # config's existing forbidOnly policy, even though it runs on this Mac.
    environment.update(STORYHOOK_E2E_RESULTS_DIR=str(artifacts), STORYHOOK_E2E_JOBS="8", CI="1")
    interrupted = []
    handlers = {}
    child = None

    def deliver(signum):
        """Signal only the proof's owned group, tolerating its concurrent exit."""
        try:
            os.killpg(child.pid, signum)
        except ProcessLookupError:
            pass  # The child exited between the signal and forwarding.

    def forward(signum, _frame):
        """Remember cancellation even when it arrives during process creation."""
        if not interrupted:
            interrupted.append(signum)
            if child is not None:
                deliver(signum)

    for signum in (signal.SIGTERM, signal.SIGINT, signal.SIGHUP):
        handlers[signum] = signal.signal(signum, forward)
    try:
        with subprocess.Popen(["/bin/bash", "scripts/run-e2e.sh", "--isolate-files"],
                              cwd=checkout, env=environment, stdout=log, stderr=log,
                              start_new_session=True) as child:
            if interrupted:
                deliver(interrupted[0])
            code = child.wait()
            if interrupted:
                # The owner has finished its graceful teardown. A child
                # forked during that teardown must not outlive the proof.
                deliver(signal.SIGKILL)
    finally:
        for signum, handler in handlers.items():
            signal.signal(signum, handler)
    return 128 + interrupted[0] if interrupted else code


def watch(base):
    """Serialize one attempt, fetch integration, run the proof, and retain evidence."""
    base.mkdir(parents=True, exist_ok=True)
    with (base / "watch.lock").open("a") as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            print("e2e-isolation-watch: another pass is running; leaving its evidence intact")
            return 0
        return observe(base)


def observe(base):
    """Execute one lock-owned observation; failures replace older green evidence."""
    controller = base / "controller"
    checkout = base / "checkout"
    runs = base / "runs"
    runs.mkdir(exist_ok=True)
    run = Path(tempfile.mkdtemp(prefix=time.strftime("%Y%m%dT%H%M%S-", time.gmtime()), dir=runs))
    record = dict(schema=1, state="running", started=int(time.time()), finished=None,
                  pid=os.getpid(), process_started=None, commit=None,
                  tree=None, branch=None, exit_code=None, summary=None, log=str(run / "run.log"),
                  artifacts=str(run / "artifacts"))
    with (run / "run.log").open("w") as log:
        save(base / "latest.json", record)
        try:
            record["process_started"] = process_started(os.getpid())
            if not record["process_started"]:
                raise ValueError("cannot establish observer process identity")
            save(base / "latest.json", record)
            settings = json.loads((base / "settings.json").read_text())
            required = settings["required_commit"]
            if settings["schema"] != 1 or git(controller, "rev-parse", "HEAD") != required:
                raise ValueError("controller must be pinned to settings.required_commit")
            if git(controller, "status", "--porcelain", "--untracked-files=no"):
                raise ValueError("controller has tracked edits")
            record["branch"] = branch(controller)
            remote = git(controller, "config", "--get-all", "remote.origin.url")
            if not remote or len(remote.splitlines()) != 1:
                raise ValueError("controller requires exactly one origin")
            if not checkout.exists():
                subprocess.run(["git", "init", "-q", str(checkout)], check=True, stdout=log, stderr=log)
            subprocess.run(["git", "-C", str(checkout), "config", "--replace-all", "remote.origin.url", remote],
                           check=True, stdout=log, stderr=log)
            ref = "refs/remotes/origin/" + record["branch"]
            subprocess.run([os.environ.get("STORY_BIN", "story"), "github", "observe",
                            "--checkout", str(checkout), "--authority", str(controller), "--",
                            "fetch", "--quiet", "--no-tags", "origin",
                            "+refs/heads/" + record["branch"] + ":" + ref], check=True, stdout=log, stderr=log)
            record["commit"] = git(checkout, "rev-parse", ref)
            record["tree"] = git(checkout, "rev-parse", ref + "^{tree}")
            # The required commit may not be reachable remotely before merge.
            guard = subprocess.run(["git", "-C", str(checkout), "merge-base", "--is-ancestor",
                                    required, ref], stdout=log, stderr=log)
            if guard.returncode != 0:
                record["state"] = "awaiting integration"
                raise ValueError(f"required implementation {required} is not an ancestor of {ref}")
            if git(checkout, "status", "--porcelain", "--untracked-files=no"):
                raise ValueError("observer checkout has tracked edits; refusing to overwrite them")
            subprocess.run(["git", "-C", str(checkout), "checkout", "-q", "--detach", record["commit"]],
                           check=True, stdout=log, stderr=log)
            package = checkout / "e2e/package-lock.json"
            modules = checkout / "e2e/node_modules"
            digest = hashlib.sha256(package.read_bytes()).hexdigest()
            if (not (modules / "@playwright/test/cli.js").is_file()
                    or not (modules / ".storyhook-lock-sha256").is_file()
                    or (modules / ".storyhook-lock-sha256").read_text().strip() != digest):
                raise ValueError(f"toolchain absent or changed: run make e2e-install in {checkout}, "
                                 "then record package-lock.json SHA256 in e2e/node_modules/.storyhook-lock-sha256")
            save(base / "latest.json", record)
            record["exit_code"] = run_proof(checkout, run / "artifacts", log)
            summary = json.loads((run / "artifacts/isolation.json").read_text())
            if (summary["schema"] != 1 or type(summary["selected_tests"]) is not int
                    or summary["selected_tests"] <= 0 or type(summary["selected_files"]) is not int
                    or summary["selected_files"] <= 0 or not isinstance(summary["failures"], list)
                    or summary["exit_code"] not in (0, 1)):
                raise ValueError("invalid isolation report")
            record["summary"] = summary
            record["state"] = "successful" if (record["exit_code"] == 0
                and summary["exit_code"] == 0 and not summary["failures"]) else "failed"
        except (OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError) as error:
            if record["state"] != "awaiting integration":
                record["state"] = "failed"
            record["error"] = f"{error}; {getattr(error, 'stderr', '') or ''}"
            log.write(record["error"] + "\n")
        record["finished"] = int(time.time())
        save(run / "record.json", record)
        save(base / "latest.json", record)
    print(f'e2e-isolation-watch: {record["state"]}; commit={record["commit"]}; log={record["log"]}')
    return 0 if record["state"] == "successful" else 1


def status(base, now=None):
    """Read freshness without fetching, starting tests, or modifying evidence."""
    now = time.time() if now is None else now
    path = base / "latest.json"
    if not path.exists():
        return "missing", "e2e-isolation-status: missing; no scheduled observation"
    try:
        record = json.loads(path.read_text())
        state = record["state"]
        if (record["schema"] != 1 or type(record["started"]) is not int
                or record["started"] > now or not Path(record["log"]).is_file()):
            raise ValueError("invalid observation or missing log")
        if state == "running":
            if (type(record["pid"]) is not int or record["pid"] <= 0
                    or not record["process_started"]
                    or record["process_started"] != process_started(record["pid"])):
                state = "failed"
        elif state in {"successful", "failed", "awaiting integration"}:
            if type(record["finished"]) is not int or not record["started"] <= record["finished"] <= now:
                raise ValueError("invalid finish time")
            if state == "successful":
                summary = json.loads((Path(record["artifacts"]) / "isolation.json").read_text())
                if (record["exit_code"] != 0 or summary != record["summary"]
                        or summary["exit_code"] != 0 or summary["failures"]
                        or summary["selected_files"] <= 0 or summary["selected_tests"] <= 0):
                    raise ValueError("incomplete successful evidence")
                tree = git(base / "checkout", "rev-parse", "refs/remotes/origin/" + record["branch"] + "^{tree}")
                if now - record["finished"] > STALE_SECONDS or tree != record["tree"]:
                    state = "stale"
        else:
            raise ValueError("unknown observation state")
        age = int(now) - (record["finished"] or record["started"])
        return state, f'e2e-isolation-status: {state}; age={age}s commit={record["commit"]} log={record["log"]}'
    except (OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError) as error:
        return "failed", f"e2e-isolation-status: failed; {error}"


def launchd(base):
    """Render a daily job pinned to a durable standalone controller, without installing it."""
    controller = base / "controller"
    if not (controller / ".git").is_dir():
        raise ValueError("LaunchAgent requires a durable standalone controller checkout")
    source = Path(__file__).resolve().parent.parent
    entries = []
    for spelling in os.environ["PATH"].split(os.pathsep):
        path = Path(spelling)
        if (not path.is_absolute() or "worktrees" in path.parts or ".worktrees" in path.parts
                or path.resolve().is_relative_to(source)
                or path.resolve().is_relative_to(Path("/private/tmp"))
                or path.resolve().is_relative_to(Path(tempfile.gettempdir()).resolve())):
            continue
        if spelling not in entries:
            entries.append(spelling)
    durable_path = os.pathsep.join(entries)
    for tool in ("story", "git", "cargo", "node", "npm"):
        if shutil.which(tool, path=durable_path) is None:
            raise ValueError(f"durable LaunchAgent PATH cannot find {tool}")
    return plistlib.dumps({
        "Label": LABEL,
        "ProgramArguments": ["/bin/bash", str(controller / "scripts/e2e-isolation-watch.sh"), "watch"],
        "WorkingDirectory": str(controller),
        "EnvironmentVariables": {"PATH": durable_path, "STORYHOOK_ISOLATION_HOME": str(base)},
        "StartCalendarInterval": {"Hour": 4, "Minute": 17},
        "StandardOutPath": str(base / "launchd.log"),
        "StandardErrorPath": str(base / "launchd.log"),
    }, sort_keys=False)


def main():
    """Dispatch explicit watch, read-only status, and plist rendering commands."""
    base = Path(os.environ.get("STORYHOOK_ISOLATION_HOME",
                               str(Path.home() / ".local/share/storyhook/e2e-isolation"))).resolve()
    if sys.argv[1:] == ["watch"]:
        return watch(base)
    if sys.argv[1:] == ["status"]:
        state, message = status(base)
        print(message)
        return 0 if state == "successful" else 1
    if sys.argv[1:] == ["plist"]:
        sys.stdout.buffer.write(launchd(base))
        return 0
    raise ValueError("usage: e2e-isolation-watch.py watch|status|plist")


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError) as failure:
        print(f"e2e-isolation-watch: {failure}", file=sys.stderr)
        sys.exit(2)
