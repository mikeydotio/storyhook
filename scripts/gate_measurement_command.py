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
    custody = ProductCustody(root, argv)
    directory = custody.root
    (directory / "input").write_text(input)
    (directory / "request.json").write_text(json.dumps({"argv": argv}))
    limit = Deadline(seconds)

    class Observe:
        def publish(self):
            try:
                limit.require("measurement helper")
            except Refusal as error:
                raise CustodyRefusal(str(error)) from error

    command = [sys.executable, "-B", str(Path(__file__).resolve()), "exec", str(directory)]
    process = None
    try:
        process = ManagedProcess(custody, custody.lease, command, publisher=Observe(),
                                 env=env, cwd=cwd, grant_environment=False)
        code = process.wait()
    except (CustodyRefusal, OSError) as error:
        raise Refusal(f"measurement helper failed; retained {directory}: {error}") from error
    finally:
        if process is not None:
            process.close()
    result = json.loads((directory / "result.json").read_text())
    if result["exit_code"] != code:
        raise Refusal(f"measurement helper supervision mismatch: {directory}")
    answer = subprocess.CompletedProcess(argv, code, (directory / "stdout").read_text(),
                                         (directory / "stderr").read_text())
    answer.wall_seconds = result["wall_seconds"]
    return answer


def execute(directory):
    """Capture real command time, excluding custody admission/cleanup overhead."""
    directory = Path(directory)
    argv = json.loads((directory / "request.json").read_text())["argv"]
    with (directory / "input").open("rb") as source, (directory / "stdout").open("xb") as out, \
            (directory / "stderr").open("xb") as err:
        started = time.monotonic()
        result = subprocess.run(argv, stdin=source, stdout=out, stderr=err)
        record = {"exit_code": result.returncode, "wall_seconds": time.monotonic() - started}
    with (directory / "result.json").open("x") as stream:
        json.dump(record, stream)
        stream.flush()
        os.fsync(stream.fileno())
    return result.returncode


if __name__ == "__main__":
    if len(sys.argv) != 3 or sys.argv[1] != "exec":
        raise SystemExit("invalid measurement helper invocation")
    raise SystemExit(execute(sys.argv[2]))
