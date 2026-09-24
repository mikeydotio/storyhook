"""The environment a storyhook-started tmux server may retain (SH-758).

A tmux server copies the environment of the client that starts it into its
global environment, and every later pane on that server inherits it: the
user's own terminals as well as the provider panes storyhook dispatches. The
starting client is whatever storyhook process happened to need a server first,
so its environment describes that process, not the machine.

The boundary is therefore an allowlist of machine baseline names, like the
one `src/env/spawn_env.rs` gives every storyhook child. Storyhook's own
selectors travel on each pane with `-e` instead, and the user's configuration
returns through the shell each pane starts. docs/spec/provider-pane-routing.md
records the ownership boundary and the council decision behind it.

This module is a library only. The daemon prepends it to the embedded
verification-view program, so it must never act on import or define a
``__main__`` block; ``tmux-env.py`` is its command-line entry point.
"""

import os

# spawn_env.rs COMMON_MAY_SEE, pinned equal by a Rust unit test there.
COMMON_MAY_SEE = ("PATH", "HOME", "XDG_DATA_HOME", "XDG_CONFIG_HOME", "XDG_STATE_HOME", "TMPDIR",
                  "USER", "SHELL", "LANG", "LC_ALL", "LC_CTYPE", "TERM")
# How a client finds its server and current pane. A server started by this
# client lives at the socket they select, so they also describe that server.
TMUX_ROUTING = ("TMUX_TMPDIR", "TMUX", "TMUX_PANE")
SERVER_MAY_SEE = frozenset(COMMON_MAY_SEE + TMUX_ROUTING)
# tmux itself records the directory a server started in; no client sends it.
TMUX_OWNED = frozenset(("PWD",))

# spawn_env.rs GITHUB_CREDENTIAL_MAY_SEE, pinned equal by the same Rust test.
GITHUB_CREDENTIALS = ("GH_CONFIG_DIR", "GH_TOKEN", "GITHUB_TOKEN",
                      "GH_ENTERPRISE_TOKEN", "GITHUB_ENTERPRISE_TOKEN")
GITHUB_ROUTING = ("STORYHOOK_GITHUB_AUTHORITY", "STORYHOOK_GITHUB_EXPECTED")
# Environment.child_vars() names, pinned equal by the same Rust unit test:
# what a `story` run in a pane needs to reach its caller's store and daemon.
PANE_SELECTORS = ("STORYHOOK_STORE_PATH", "XDG_STATE_HOME", "STORYHOOK_VERIFIER_MIRROR")

# Hook-scoped roots both provider hosts export to a plugin's hooks.
PLUGIN_ROOTS = ("PLUGIN_ROOT", "PLUGIN_DATA", "CLAUDE_PLUGIN_ROOT", "CLAUDE_PLUGIN_DATA")
# Session-scoped state an already-running server may have retained. This is
# the denylist half of the boundary and is used only to clean storyhook-owned
# sessions and to report pollution; it never decides what a server retains.
# Claude names come from the Claude Code 2.1.280 child-environment constructor
# and a live session's Bash environment; storyhook names are per-call values.
RETAINED_NAMES = frozenset(PLUGIN_ROOTS + GITHUB_CREDENTIALS + GITHUB_ROUTING + (
    "CLAUDE_PROJECT_DIR", "CLAUDE_ENV_FILE", "CLAUDECODE", "CLAUDE_PID", "CLAUDE_EFFORT", "AI_AGENT",
    "CLAUDE_CODE_SESSION_ID", "CLAUDE_CODE_CHILD_SESSION", "CLAUDE_CODE_SESSION_ATTENDED",
    "CLAUDE_CODE_ENTRYPOINT", "CLAUDE_CODE_EXECPATH", "CLAUDE_CODE_MESSAGING_SOCKET",
    "CLAUDE_CODE_MESSAGING_TOKEN",
    "STORY_BIN", "STORY_AGENT", "STORY_TARGET_SESSION", "STORY_CREATE_SESSION",
    "STORY_LANE_TOOL_CEILING_MS", "STORY_WORKSPACE_LOCK_FD", "STORYHOOK_ACTIVITY_LOG_DIR",
    "STORYHOOK_ACTIVITY_CONTEXT", "STORYHOOK_DISPATCH", "STORYHOOK_AUTO", "STORYHOOK_FULL_AUTO",
    "STORYHOOK_CODEX_BOOTSTRAP"))
# Codex exports its thread, sandbox and tool-pipe state under one prefix.
RETAINED_PREFIXES = ("CODEX_",)
# CODEX_HOME is the user's Codex configuration directory, not session state.
RETAINED_KEPT = frozenset(("CODEX_HOME",))

# tmux's own diagnostics for "nothing is listening at this socket".
NO_SERVER = ("no server running", "no sessions", "No such file or directory")


def _absolute(value):
    """Return a normalized absolute path, or None for an empty or relative value."""
    return os.path.normpath(value) if value and os.path.isabs(value) else None


def host_roots(environ):
    """Return the absolute host-owned directories whose PATH entries are dropped.

    Each is a directory a provider host manages for its own sessions: Codex's
    home (arg0 shims, plugin caches), its bundled runtimes, Claude's plugin
    cache, the storyhook plugin payload a host prepends, and every plugin root
    the caller was handed. App bundles are installations, not state, and stay.
    """
    home = _absolute(environ.get("HOME", ""))
    roots = [_absolute(environ.get(name, "")) for name in PLUGIN_ROOTS]
    if home:
        roots.append(_absolute(environ.get("CODEX_HOME", "")) or os.path.join(home, ".codex"))
        roots.append(os.path.join(home, ".cache", "codex-runtimes"))
        claude = _absolute(environ.get("CLAUDE_CONFIG_DIR", "")) or os.path.join(home, ".claude")
        roots.append(os.path.join(claude, "plugins"))
        data = _absolute(environ.get("XDG_DATA_HOME", "")) or os.path.join(home, ".local", "share")
        roots.append(os.path.join(data, "storyhook", "plugins"))
    return [root for root in roots if root]


def filter_path(environ):
    """Return PATH without entries under a host root, or None when PATH is unset.

    Matching is by whole path component after lexical normalization, so
    `~/.codexfoo` and macOS's `/var/run/.../codex.system` survive. Empty and
    relative entries keep their meaning, and order and duplicates are kept.
    """
    path = environ.get("PATH")
    if path is None:
        return None
    roots = host_roots(environ)

    def hosted(entry):
        normal = _absolute(entry)
        return normal is not None and any(normal == root or normal.startswith(root + os.sep)
                                          for root in roots)
    return ":".join(entry for entry in path.split(":") if not hosted(entry))


def client_environment(environ):
    """Return the complete environment for a tmux client that may start a server."""
    client = {name: environ[name] for name in SERVER_MAY_SEE if name in environ}
    path = filter_path(environ)
    if path is not None:
        client["PATH"] = path
    return client


def reports_no_server(stderr):
    """True only for tmux's documented diagnostics for an absent server."""
    return any(text in stderr for text in NO_SERVER)


def parse_environment(listing):
    """Return {name: value} for the set variables in `show-environment` output."""
    variables = {}
    for line in listing.splitlines():
        name, separator, value = line.partition("=")
        if separator and not name.startswith("-"):
            variables[name] = value
    return variables


def removed_names(listing):
    """Return the names `show-environment` marks as removed (`-NAME`)."""
    return {line[1:] for line in listing.splitlines() if line.startswith("-") and "=" not in line}


def retained_names(variables):
    """Return the sorted session-scoped names present in `variables`."""
    return sorted(name for name in variables if name not in RETAINED_KEPT
                  and (name in RETAINED_NAMES or name.startswith(RETAINED_PREFIXES)))


def scrub_owned_session(run, session, environ):
    """Remove session-scoped state from one storyhook-owned session's new panes.

    `run(*arguments)` executes one tmux command and returns its stdout, raising
    with tmux's diagnostic on failure. A retained server's global environment
    and every other session are the user's, so only this session changes: each
    retained name is truly unset there (`-r`, not an empty value), and its PATH
    is the effective PATH without host roots. Returns what changed.
    """
    target = "=" + session
    global_listing = run("show-environment", "-g")
    session_listing = run("show-environment", "-t", target)
    effective = dict(parse_environment(global_listing))
    for name in removed_names(session_listing):
        effective.pop(name, None)
    effective.update(parse_environment(session_listing))
    removed = retained_names(effective)
    for name in removed:
        run("set-environment", "-t", target, "-r", name)
    rewritten = False
    if "PATH" in effective:
        roots_from = dict(environ, **{name: value for name, value in effective.items()
                                      if name in PLUGIN_ROOTS or name == "PATH"})
        path = filter_path(roots_from)
        if path != effective["PATH"]:
            run("set-environment", "-t", target, "PATH", path)
            rewritten = True
    return {"session": session, "removed": removed, "path_rewritten": rewritten}
