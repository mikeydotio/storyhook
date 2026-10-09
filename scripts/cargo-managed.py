#!/usr/bin/env python3
"""Repository-local whole-Cargo entry. No global PATH or toolchain changes.

Usage: scripts/cargo-managed.py <cargo arguments...>
       scripts/cargo-managed.py --cargo-executable /exact/cargo -- <arguments...>

The optional executable form lets cross-build runners preserve their selected
toolchain. Bare Cargo remains supported but does not enroll a reclaimable owner.
"""

import os
from pathlib import Path
import shutil
import signal
import sys

sys.dont_write_bytecode = True


def command(argv):
    if argv[:1] == ["--cargo-executable"]:
        if len(argv) < 3 or argv[2] != "--" or not os.path.isabs(argv[1]):
            raise ValueError("expected --cargo-executable /absolute/cargo -- <arguments>")
        executable, arguments = argv[1], argv[3:]
    else:
        executable, arguments = shutil.which("cargo"), argv
    if executable is None:
        raise FileNotFoundError("cargo was not found on PATH")
    # Resolve once; the supervised command does not perform a second PATH lookup.
    executable = str(Path(executable).absolute())
    if Path(executable).resolve() == Path(__file__).resolve():
        raise ValueError("Cargo executable resolves to this wrapper")
    return [sys.executable, "-B", str(Path(__file__).with_name("host-admit.py")),
            "--entry", "cargo-managed", "--", executable, *arguments]


def main(argv):
    if sys.version_info < (3, 11):
        print("cargo-managed: Python >= 3.11 required; use scripts/python-runtime.sh", file=sys.stderr)
        return 125
    from build_products import run_managed
    from host_admission.policy import Refusal
    try:
        result = run_managed(command(argv))
        if result < 0:
            # Preserve signal termination for callers such as subprocess, not
            # only the shell's conventional 128+signal numeric exit encoding.
            if -result != signal.SIGKILL:
                signal.signal(-result, signal.SIG_DFL)
            os.kill(os.getpid(), -result)
        return result
    except FileNotFoundError as error:
        print(f"cargo-managed: {error}", file=sys.stderr)
        return 127
    except (OSError, Refusal, ValueError) as error:
        print(f"cargo-managed: custody unresolved; products preserved: {error}", file=sys.stderr)
        return 125


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
