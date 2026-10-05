#!/usr/bin/env python3
"""Native Rust observation worker; only its parent can validate causal inputs."""

import hashlib
import json
import os
from pathlib import Path
import re
import stat
import subprocess
import sys

from host_admission.policy import Refusal
from host_admission import diagnosis

CAPTURE_LIMIT = 16 * 1024 * 1024
ENTRY = "causal-rust"


class Pipeline:
    """A selected Cargo/native harness pipeline inside its parent's managed session."""

    def __init__(self, request):
        fields = {"version", "source", "output", "package", "target", "case", "tools", "wrapper", "lock_root", "deadline", "clock"}
        if set(request) != fields or request["version"] != 1 or request["clock"] != "CLOCK_MONOTONIC":
            raise Refusal("unsupported native Rust request")
        if not os.environ.get("STORYHOOK_HOST_GRANT") or not os.environ.get("STORYHOOK_HOST_REQUEST"):
            raise Refusal("native Rust worker requires its parent's managed grant")
        for key in ("package", "target", "case"):
            if not re.fullmatch(r"[A-Za-z_][A-Za-z0-9_-]*", request[key]):
                raise Refusal(f"unsupported literal {key}")
        self.request = request
        self.deadline = request["deadline"]
        self.source = Path(request["source"]).resolve(strict=True)
        self.output = Path(request["output"]).resolve(strict=True)
        if self.output == self.source or self.output.is_relative_to(self.source) or self.source.is_relative_to(self.output):
            raise Refusal("native source and output must be disjoint")
        for directory in (self.source, *self.source.parents):
            if any(os.path.lexists(directory / ".cargo" / name) for name in ("config", "config.toml")):
                raise Refusal("ambient Cargo configuration prevents isolated compilation")
        self.env = {"PATH": str(Path(sys.executable).parent)+os.pathsep+os.defpath,
                    "LANG": "C", "LC_ALL": "C", "RUST_BACKTRACE": "0", "CARGO_INCREMENTAL": "0",
                    "CARGO_BUILD_JOBS": "1", "RUSTC": request["tools"]["rustc"],
                    "STORYHOOK_LOCK_DIR": request["lock_root"]}
        for name in ("STORYHOOK_HOST_GRANT", "STORYHOOK_HOST_REQUEST", "STORYHOOK_HOST_LEASE_FD"):
            if name in os.environ:
                self.env[name] = os.environ[name]
        for key, leaf in (("HOME", "home"), ("CARGO_HOME", "cargo-home"), ("CARGO_TARGET_DIR", "target"), ("TMPDIR", "tmp")):
            directory = self.output / leaf
            directory.mkdir(mode=0o700)
            self.env[key] = str(directory)
        self.observed = {"version": 1}

    def remaining(self):
        """Every stage consumes the parent's single monotonic allowance."""
        remaining = self.deadline-diagnosis.monotonic()
        if remaining <= 0:
            raise Refusal("native Rust pipeline exhausted its active allowance")
        return remaining

    def fingerprint(self, path):
        """Bind an actual regular executable before and after its use."""
        self.remaining()
        path = Path(path)
        fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
        with os.fdopen(fd, "rb") as stream:
            info = os.fstat(stream.fileno())
            if not stat.S_ISREG(info.st_mode) or info.st_size > 512 * 1024 * 1024:
                raise Refusal("native tool or artifact is not a bounded regular file")
            digest = hashlib.sha256()
            while data := stream.read(1024 * 1024):
                self.remaining()
                digest.update(data)
            final = os.fstat(stream.fileno())
            fields = lambda s: (s.st_dev, s.st_ino, s.st_size, s.st_mode, s.st_mtime_ns, s.st_ctime_ns)
            if fields(info) != fields(final):
                raise Refusal("native executable changed during its fingerprint")
            return dict(path=str(path), identity=fields(final), sha256=digest.hexdigest())

    def stage(self, name, argv):
        """Capture real output to newly owned files, without a shell or a test retry."""
        with open(self.output / f"{name}.stdout", "xb") as stdout, open(self.output / f"{name}.stderr", "xb") as stderr:
            result = subprocess.run(argv, cwd=self.source, env=self.env, stdin=subprocess.DEVNULL,
                                    stdout=stdout, stderr=stderr, timeout=self.remaining())
        record = dict(argv=argv, exit=result.returncode)
        self.observed[name] = record
        for suffix in ("stdout", "stderr"):
            if (self.output / f"{name}.{suffix}").stat().st_size > CAPTURE_LIMIT:
                raise Refusal(f"native {name} {suffix} exceeded the capture bound")
        self.remaining()
        return result.returncode

    def successful(self, name, argv):
        """A build or discovery failure is unavailable, not a failed test observation."""
        if self.stage(name, argv) != 0:
            raise Refusal(f"native {name} failed; retained its stdout and stderr")

    def execute(self):
        """Run metadata, one target build, exact discovery, then exactly one test."""
        request = self.request
        tools = request["tools"]
        if set(tools) != {"cargo", "rustc"} or any(not Path(v).is_absolute() for v in tools.values()):
            raise Refusal("native Cargo and Rust compiler paths must be pinned")
        pinned = {name: self.fingerprint(path) for name, path in tools.items()}
        pinned["wrapper"] = self.fingerprint(request["wrapper"])
        for name, path in tools.items():
            self.successful(name+"-version", [path, "-vV"])
        # The wrapper preserves the machine-wide compiler bound as well as the
        # enclosing native resource lease. Source and user Cargo configs are absent.
        cargo = [tools["cargo"], "--config", "build.rustc-wrapper="+json.dumps(request["wrapper"])]
        self.successful("metadata", cargo+["metadata", "--offline", "--locked", "--format-version", "1", "--no-deps"])
        self.successful("build", cargo+["test", "--offline", "--locked", "--package", request["package"],
                                        "--test", request["target"], "--no-run", "--message-format=json"])
        records = [json.loads(line) for line in (self.output / "build.stdout").read_text().splitlines()]
        executables = [r["executable"] for r in records if r.get("reason") == "compiler-artifact"
                       and r.get("target", {}).get("name") == request["target"]
                       and r.get("target", {}).get("kind") == ["test"]
                       and r.get("profile", {}).get("test") is True and r.get("executable")]
        if len(executables) != 1:
            raise Refusal("native build has no unique selected executable")
        executable = Path(executables[0])
        if executable.resolve(strict=True) != executable or not executable.is_relative_to(self.output / "target"):
            raise Refusal("native artifact is not owned by the diagnostic build")
        self.observed["artifact_before"] = self.fingerprint(executable)
        self.observed["executable"] = str(executable)
        self.successful("listing", [str(executable), "--list", "--exact", request["case"], "--format", "pretty"])
        if (self.output / "listing.stdout").read_text() != f"{request['case']}: test\n\n1 test, 0 benchmarks\n" or (self.output / "listing.stderr").stat().st_size:
            raise Refusal("native harness did not list exactly the requested test")
        self.stage("run", [str(executable), "--exact", request["case"], "--format", "pretty", "--color", "never", "--test-threads", "1"])
        self.observed["artifact_after"] = self.fingerprint(executable)
        after = {name: self.fingerprint(path) for name, path in tools.items()}
        after["wrapper"] = self.fingerprint(request["wrapper"])
        if after != pinned or self.observed["artifact_before"] != self.observed["artifact_after"]:
            raise Refusal("native toolchain or executable changed during diagnosis")
        self.observed["tools"] = pinned


def main():
    """Collect a selected native pipeline; this command grants no return authority."""
    worker = len(sys.argv) == 3 and sys.argv[1] == "--worker"
    if not worker and len(sys.argv) != 2:
        raise Refusal("native Rust worker requires one owned request file")
    path = Path(sys.argv[-1]).resolve(strict=True)
    request = json.loads(path.read_text())
    if not worker:
        if set(request) != {"pipeline", "project", "binding", "journal", "request_id"}:
            raise Refusal("unsupported diagnostic admission request")
        code, evidence = diagnosis.run([sys.executable, "-B", str(Path(__file__).resolve()), "--worker", str(path)],
            project=request["project"], binding=request["binding"], journal=request["journal"],
            request_id=request["request_id"], deadline=request["pipeline"]["deadline"])
        with open(Path(request["pipeline"]["output"]) / "resource.json", "x") as stream:
            json.dump(dict(entry=ENTRY, worker_exit=code, **evidence), stream)
        return
    pipeline = Pipeline(request["pipeline"])
    try:
        pipeline.execute()
    except (Refusal, OSError, ValueError, subprocess.TimeoutExpired) as error:
        pipeline.observed["error"] = str(error)
        raise
    finally:
        with open(pipeline.output / "observation.json", "x") as stream:
            json.dump(pipeline.observed, stream)


if __name__ == "__main__":
    try:
        main()
    except (Refusal, OSError, ValueError, subprocess.TimeoutExpired) as error:
        print(f"attribution-rust: {error}", file=sys.stderr)
        sys.exit(125)
