"""Name the live provider processes that work inside a story worktree (SH-850).

A story's tmux window is one witness that its agent is gone, and it can be
wrong. A tmux server whose socket path another server took over keeps its
panes running where the recorded socket no longer reaches them (SH-850's own
lane, 2026-09-29), and an agent that a person started by hand never had a
window. A resume that replaced such an agent would put two agents in one
worktree. This census asks the kernel instead: which provider processes have
their working directory inside the worktree.

A process is a provider the way pane readiness decides it (`pane_runs`,
lib/session.sh): its name matches the provider pattern, its executable is a
resolved launch binary, or it is version-named in the same directory as one
(Claude Code's native installer runs ~/.local/share/claude/versions/<version>,
so the kernel names that process "2.1.285"). A shell is none of these.

Usage: worktree_occupants.py <worktree> [--pattern <regex>] [--launch <word>]...
Prints {"occupants": [{"pid", "name", "executable", "cwd"}, ...]} and exits 0.
Exits 1 with a diagnostic on stderr when the process table cannot be read:
a census that could not run has not answered "nobody".
"""

import argparse
import ctypes
import json
import os
import re
import shutil
import sys
from pathlib import Path

VERSION_NAME = re.compile(r"^[0-9]+(\.[0-9]+)*$")
DEFAULT_PATTERN = r"^(claude|node|codex)$"
# struct proc_vnodepathinfo (sys/proc_info.h): the cwd's vnode_info (152
# bytes) is followed by its MAXPATHLEN path; the root directory's pair follows.
PROC_PIDVNODEPATHINFO = 9
VNODE_INFO_SIZE = 152
MAXPATHLEN = 1024


def _darwin_processes():
    """Yield (pid, name, executable, cwd) for every process this user may read."""
    lib = ctypes.CDLL("/usr/lib/libproc.dylib", use_errno=True)
    lib.proc_listallpids.argtypes = [ctypes.c_void_p, ctypes.c_int]
    lib.proc_pidinfo.argtypes = [ctypes.c_int, ctypes.c_int, ctypes.c_uint64,
                                 ctypes.c_void_p, ctypes.c_int]
    lib.proc_pidpath.argtypes = [ctypes.c_int, ctypes.c_void_p, ctypes.c_uint32]
    lib.proc_name.argtypes = [ctypes.c_int, ctypes.c_void_p, ctypes.c_uint32]
    count = lib.proc_listallpids(None, 0)
    if count <= 0:
        raise OSError(ctypes.get_errno(), "cannot count processes")
    # Room for processes that start between the two calls.
    pids = (ctypes.c_int * (count + 256))()
    count = lib.proc_listallpids(pids, ctypes.sizeof(pids))
    if count <= 0:
        raise OSError(ctypes.get_errno(), "cannot list processes")
    size = 2 * (VNODE_INFO_SIZE + MAXPATHLEN)
    info = ctypes.create_string_buffer(size)
    path = ctypes.create_string_buffer(4096)
    name = ctypes.create_string_buffer(256)
    for pid in pids[:count]:
        if pid <= 0:
            continue
        # Another user's process, or one that exited meanwhile, answers
        # nothing; an agent always runs as this user.
        if lib.proc_pidinfo(pid, PROC_PIDVNODEPATHINFO, 0, info, size) != size:
            continue
        cwd = info.raw[VNODE_INFO_SIZE:VNODE_INFO_SIZE + MAXPATHLEN].split(b"\0", 1)[0]
        executable = path.value if lib.proc_pidpath(pid, path, len(path)) > 0 else b""
        label = name.value if lib.proc_name(pid, name, len(name)) > 0 else b""
        yield pid, os.fsdecode(label), os.fsdecode(executable), os.fsdecode(cwd)


def _linux_processes():
    """Yield (pid, name, executable, cwd) for every process this user may read."""
    for entry in Path("/proc").iterdir():
        if not entry.name.isdigit():
            continue
        try:
            cwd = os.readlink(entry / "cwd")
            label = (entry / "comm").read_text().strip()
        except (FileNotFoundError, PermissionError, ProcessLookupError):
            continue
        try:
            executable = os.readlink(entry / "exe")
        except (FileNotFoundError, PermissionError, ProcessLookupError):
            executable = ""
        yield int(entry.name), label, executable, cwd


def processes():
    """Every readable process, from the platform's own process table."""
    if sys.platform == "darwin":
        return _darwin_processes()
    if sys.platform.startswith("linux"):
        return _linux_processes()
    raise OSError(f"worktree census is unsupported on {sys.platform}")


def launch_binaries(words):
    """The resolved executables the launch words name on this PATH."""
    resolved = []
    for word in words:
        found = shutil.which(word)
        if found:
            resolved.append(os.path.realpath(found))
    return resolved


def is_provider(name, executable, pattern, launches):
    """Whether a process is a provider, by pane_runs' three rules."""
    if name and pattern.search(name):
        return True
    if not executable:
        return False
    real = os.path.realpath(executable)
    for launch in launches:
        if real == launch:
            return True
        # Update skew: the process still runs the version it launched from.
        if (os.path.dirname(real) == os.path.dirname(launch)
                and VERSION_NAME.match(os.path.basename(launch))
                and VERSION_NAME.match(os.path.basename(real))):
            return True
    return False


def inside(cwd, worktree):
    """Whether cwd is the worktree or a directory below it."""
    return cwd == worktree or cwd.startswith(worktree.rstrip(os.sep) + os.sep)


def occupants(worktree, pattern, launch_words):
    """The provider processes whose working directory is inside worktree."""
    root = os.path.realpath(worktree)
    matcher = re.compile(pattern)
    launches = launch_binaries(launch_words)
    found = []
    for pid, name, executable, cwd in processes():
        if pid == os.getpid() or not cwd:
            continue
        if inside(os.path.realpath(cwd), root) and is_provider(name, executable, matcher, launches):
            found.append({"pid": pid, "name": name, "executable": executable, "cwd": cwd})
    return found


def main(argv):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("worktree")
    parser.add_argument("--pattern", default=DEFAULT_PATTERN)
    parser.add_argument("--launch", action="append", default=[])
    args = parser.parse_args(argv)
    try:
        found = occupants(args.worktree, args.pattern, args.launch)
    except (OSError, re.error) as error:
        print(f"worktree census failed: {error}", file=sys.stderr)
        return 1
    print(json.dumps({"occupants": found}))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
