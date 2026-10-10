"""Bounded measurement helpers with retained native child custody and streams.

Uses the existing independent custody adapter, in a private measurement namespace.
This does not acquire a host grant or permission to reclaim any Cargo products.
"""

import json
import os
from pathlib import Path
import subprocess
import sys
import time

from host_admission.diagnostics import ChildTrace, Trace, boundary


def bounded(argv, *, root, seconds=30, cwd=None, env=None, input=""):
    from build_products import ProductCustody
    from host_admission.supervisor import ManagedProcess
    from host_admission.policy import Refusal as CustodyRefusal
    from gate_measurement_bounds import Deadline
    from verifier_state import Refusal

    root = Path(root)
    if root.is_symlink() or root.resolve() != root:
        raise Refusal("measurement command namespace must be physical")
    root.mkdir(mode=0o700, parents=True, exist_ok=True)
    try:
        trace = Trace()
    except OSError:
        trace = None  # Diagnostic setup cannot change custody or command results.
    process, directory = None, None
    try:
        custody = ProductCustody(root, argv, trace=trace)
        directory = custody.root
        (directory / "input").write_text(input)
        (directory / "request.json").write_text(json.dumps({"argv": argv}))
        limit = Deadline(seconds)

        class Observe:
            def publish(self):
                try:
                    limit.require("measurement helper")
                except Refusal as error:
                    if trace is not None:
                        trace.emit("deadline", "error")
                    raise CustodyRefusal(str(error)) from error

        command = [sys.executable, "-B", str(Path(__file__).resolve()), "exec", str(directory)]
        descriptors = () if trace is None else (trace.writer.fileno(),)
        if descriptors:
            command.append(str(descriptors[0]))
        try:
            process = boundary(trace, "admission", ManagedProcess,
                               custody, custody.lease, command, publisher=Observe(),
                               env=env, cwd=cwd, grant_environment=False,
                               trace=trace, pass_fds=descriptors)
            code = boundary(trace, "supervision", process.wait)
        except (CustodyRefusal, OSError) as error:
            raise Refusal(f"measurement helper failed; retained {directory}: {error}") from error
        finally:
            if process is not None:
                boundary(trace, "close", process.close)
    finally:
        if trace is not None:
            try:
                if directory is not None:
                    trace.save(directory / "boundaries.json")
            finally:
                trace.close()
    result = json.loads((directory / "result.json").read_text())
    if result["exit_code"] != code:
        raise Refusal(f"measurement helper supervision mismatch: {directory}")
    answer = subprocess.CompletedProcess(argv, code, (directory / "stdout").read_text(),
                                         (directory / "stderr").read_text())
    answer.wall_seconds = result["wall_seconds"]
    return answer


def execute(directory, trace_fd=None):
    """Capture real command time, excluding custody admission/cleanup overhead."""
    directory = Path(directory)
    try:
        trace = None if trace_fd is None else ChildTrace(trace_fd)
    except OSError:
        trace = None
    try:
        argv = json.loads(boundary(trace, "request", (directory / "request.json").read_text))["argv"]
        from host_admission.supervisor import inherited_descriptors
        with (directory / "input").open("rb") as source, (directory / "stdout").open("xb") as out, \
                (directory / "stderr").open("xb") as err:
            started = time.monotonic()
            descriptors = boundary(trace, "descriptor_capture", inherited_descriptors)
            if trace is not None:
                trace.emit("command_spawn", "begin")
            try:
                child = subprocess.Popen(argv, stdin=source, stdout=out, stderr=err,
                                         pass_fds=tuple(sorted(descriptors)))
            except BaseException:
                if trace is not None:
                    trace.emit("command_spawn", "error")
                raise
            if trace is not None:
                trace.emit("command_spawn", "end", child.pid)
                trace.emit("command_wait", "begin")
            try:
                code = child.wait()
            except BaseException:
                if trace is not None:
                    trace.emit("command_wait", "error")
                # Preserve subprocess.run's local exception cleanup. The outer
                # supervisor still owns descendants and the lifetime guard.
                boundary(trace, "command_cancel", child.kill)
                boundary(trace, "command_reap", child.wait)
                raise
            if trace is not None:
                trace.emit("command_wait", "end", code)
            record = {"exit_code": code, "wall_seconds": time.monotonic() - started}
        with (directory / "result.json").open("x") as stream:
            json.dump(record, stream)
            boundary(trace, "result_flush", stream.flush)
            boundary(trace, "result_fsync", os.fsync, stream.fileno())
        return code
    finally:
        if trace is not None:
            trace.close()


if __name__ == "__main__":
    if len(sys.argv) not in (3, 4) or sys.argv[1] != "exec":
        raise SystemExit("invalid measurement helper invocation")
    raise SystemExit(execute(sys.argv[2], int(sys.argv[3]) if len(sys.argv) == 4 else None))
