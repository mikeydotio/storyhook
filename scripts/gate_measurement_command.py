"""Bounded measurement helpers with retained native child custody and streams.

Uses the existing independent custody adapter, in a private measurement namespace.
This does not acquire a host grant or permission to reclaim any Cargo products.
"""

import json
import os
from pathlib import Path
import subprocess
import signal
import sys
import time

from host_admission.diagnostics import ChildTrace, Trace, boundary


def bounded(argv, *, root, seconds=30, cwd=None, env=None, input="",
            optional_overall_end=None):
    from build_products import ProductCustody
    from host_admission.supervisor import ManagedProcess
    from host_admission.policy import Refusal as CustodyRefusal
    from gate_measurement_bounds import Deadline
    from verifier_state import Refusal

    if optional_overall_end is not None:
        from gate_measurement_optional import require_optional
        require_optional(argv, seconds, optional_overall_end)
    root = Path(root)
    if root.is_symlink() or root.resolve() != root:
        raise Refusal("measurement command namespace must be physical")
    root.mkdir(mode=0o700, parents=True, exist_ok=True)
    try:
        trace = Trace()
    except OSError:
        trace = None  # Diagnostic setup cannot change custody or command results.
    process, directory, local_timeout = None, None, None
    try:
        custody = ProductCustody(root, argv, trace=trace)
        directory = custody.root
        (directory / "input").write_text(input)
        (directory / "request.json").write_text(json.dumps({"argv": argv}))
        limit = Deadline(seconds, end=optional_overall_end)

        class Observe:
            def publish(self):
                nonlocal local_timeout
                if optional_overall_end is not None and time.monotonic() >= optional_overall_end:
                    raise CustodyRefusal("overall observation deadline expired")
                try:
                    limit.require("measurement helper")
                except Refusal as error:
                    if trace is not None:
                        trace.emit("deadline", "error")
                    if optional_overall_end is not None:
                        from gate_measurement_optional import LocalDeadline
                        local_timeout = LocalDeadline("descriptive helper local deadline expired")
                        raise local_timeout from error
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
            if (local_timeout is not None and process is not None
                    and process.failure_cause is local_timeout and process.finished
                    and not process.cancelled and not process.drain_reason
                    and process.observation_failure is None
                    and (process.child.returncode == 0 or
                         (process.child.returncode in (-signal.SIGTERM, -signal.SIGKILL)
                          and -process.child.returncode in process.leader_signals))
                    and isinstance(error, CustodyRefusal)
                    and time.monotonic() < optional_overall_end):
                from gate_measurement_optional import ExposureTimeout, quiescence
                result_path = directory / "result.json"
                if result_path.exists():
                    try:
                        known = json.loads(result_path.read_text())
                    except (ValueError, UnicodeError) as failure:
                        raise Refusal("timed-out helper has malformed result evidence") from failure
                    if not isinstance(known, dict) or type(known.get("exit_code")) is not int or known["exit_code"] != 0:
                        raise Refusal("timed-out helper already reported a command failure")
                proof = quiescence(directory)
                if time.monotonic() >= optional_overall_end:
                    raise Refusal("overall deadline expired during helper cleanup") from error
                raise ExposureTimeout(proof) from error
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
    if optional_overall_end is not None and any(
            (directory / name).stat().st_size > 4 * 1024 * 1024 for name in ("stdout", "stderr")):
        raise Refusal("descriptive helper output exceeds its retained-data bound")
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
