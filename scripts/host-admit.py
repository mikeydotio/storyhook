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


def fast(argv):
    """Exec without importing the authority when no admission decision is needed."""
    if len(argv) < 4 or argv[0] != "--entry" or "--" not in argv:
        return
    entry, separator = argv[1], argv.index("--")
    command, options = argv[separator + 1:], argv[2:separator]
    if entry not in LEAVES | POOLS or not command or options not in ([], ["--units", argv[3]]):
        return
    if options and not (argv[3].isdecimal() and int(argv[3]) > 0):
        return
    env = dict(os.environ, STORYHOOK_HOST_ENTRY=f"{entry}:{os.getpid()}")
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
    from host_admission.adapter import main

    sys.exit(main())
