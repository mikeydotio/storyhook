#!/usr/bin/env python3
"""Run a managed runner entry under host admission (SH-869).

    host-admit.py --entry <id> [--units N] -- <command...>
    host-admit.py --drain-seconds

Cargo runs every test binary through this file (`.cargo/config.toml`), so
the common cases stay cheap and import nothing: with no host policy the
command is exec'd unchanged, and inside a granted root a non-pool entry is
exec'd in place. Everything else goes through `host_admission.adapter`.
Design of record: docs/spec/verification-throughput-and-recovery.md,
"SH-869 runner adoption".
"""

import os
import sys

sys.dont_write_bytecode = True

# host_admission.namespace.ROOT, repeated so the disabled path imports
# nothing; scripts/tests/test_host_admission_adapter.py pins the two equal.
POLICY = "/var/tmp/storyhook-host-admission-v1/policy.json"

# Entries that are never pools run in place inside a grant; pools take a unit
# count. The adapter's inventory is the source; the same test pins both sets.
LEAVES = frozenset({"rustc", "cargo-test-binary", "plugin-script", "verifier-gate",
                    "release", "release-observer"})
POOLS = frozenset({"rust-pool", "plugin-pool", "browser-pool", "verifier-python-workers"})


def run(command, env):
    """Replace this process, answering like a shell when the command cannot start."""
    try:
        os.execvpe(command[0], command, env)
    except OSError as error:
        print(f"host-admit: cannot run {command[0]}: {error}", file=sys.stderr)
        sys.exit(127 if isinstance(error, FileNotFoundError) else 126)


def application_run(entry, command):
    """Cargo's runner also wraps `cargo run` and examples; only test binaries are runners.

    Test binaries live in `target/<profile>/deps/`; rustdoc names a doctest
    binary `rust_out`. Anything else Cargo hands its runner is an application.
    """
    if entry != "cargo-test-binary":
        return False
    path = command[0]
    return os.path.basename(os.path.dirname(path)) != "deps" and os.path.basename(path) != "rust_out"


def fast(argv):
    """Exec without importing the authority when no admission decision is needed."""
    if argv == ["--drain-seconds"]:
        try:
            os.lstat(POLICY)
        except FileNotFoundError:
            print(0)  # disabled: nothing drains, an enclosing lock keeps its grace
            sys.exit(0)
        return
    if len(argv) < 4 or argv[0] != "--entry" or "--" not in argv:
        return
    entry, separator = argv[1], argv.index("--")
    command, options = argv[separator + 1:], argv[2:separator]
    if entry not in LEAVES | POOLS or not command or options not in ([], ["--units", argv[3]]):
        return
    if options and not (argv[3].isdecimal() and int(argv[3]) > 0):
        return
    env = dict(os.environ, STORYHOOK_HOST_ENTRY=f"{entry}:{os.getpid()}")
    if application_run(entry, command):
        run(command, dict(os.environ))
    try:
        os.lstat(POLICY)
    except FileNotFoundError:
        if options and entry not in LEAVES:
            env["STORYHOOK_HOST_UNITS"] = argv[3]
        run(command, env)
    if entry in LEAVES and os.environ.get("STORYHOOK_HOST_GRANT") and os.environ.get("STORYHOOK_HOST_REQUEST"):
        run(command, env)


if __name__ == "__main__":
    fast(sys.argv[1:])
    if sys.version_info < (3, 11):
        # Cargo can reach this file through an older `python3` on PATH; only
        # the fast paths above are written for it (scripts/python-runtime.sh).
        print(f"host-admit: host admission requires Python >= 3.11, not {sys.version.split()[0]};"
              " set STORYHOOK_PYTHON", file=sys.stderr)
        sys.exit(125)
    from host_admission.adapter import main

    sys.exit(main())
