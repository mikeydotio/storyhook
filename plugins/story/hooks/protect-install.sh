#!/usr/bin/env bash
# Refuse edits to storyhook's own INSTALLED copies (SH-530).
#
# An agent that edits `plugins/story/bin/story.sh` in a checkout is doing the
# right thing: the change is reviewed, gated and shipped by the next release.
# An agent that edits the INSTALLED copy of the same file is doing something
# that is wrong twice over, and quietly:
#
#   1. it drifts the installation away from the release it claims to be, which
#      is the whole subject of SH-530; and
#   2. the next `story plugin install` overwrites it, so the change is LOST as
#      well as unversioned and untested.
#
# (2) is why this hook redirects instead of merely refusing. The work is
# usually good work aimed at the wrong file, and a refusal that does not say
# where the file actually lives just invites a second attempt.
#
# WHAT THIS IS NOT. It is not a security boundary and it is not the last line
# of defence. A Claude Code `PreToolUse` hook FAILS OPEN at its timeout -- the
# harness SIGTERMs it and lets the tool call proceed, silently (SH-306) -- so
# this is the cheap layer that catches the common case at the moment of the
# act. `story doctor install` is the authoritative check, because it reads the
# installation afterwards regardless of which agent, or which provider, did it.
#
# SPEED IS THEREFORE CORRECTNESS. The inert path pays no interpreter start, the
# discipline `full-auto.sh` already states: this reads stdin once and, if the
# raw payload does not mention a managed prefix as a plain substring, prints
# `{}` and exits in shell. Only a substring hit pays for python3. The substring
# test is a superset of true hits, so it errs safe. Nothing here runs `story`
# (the binary may be mid-replacement) or talks to the daemon.
#
# Both Claude Code and Codex run this on structured editors and shell commands.
# SH-550 is the direct Codex measurement: two read-only shell commands reached
# this hook and exposed that a path hit alone had been mistaken for an edit.

set -uo pipefail

emit_inert() { printf '{}'; exit 0; }

payload=""
if [ ! -t 0 ]; then
  payload="$(cat)"
fi
[ -n "$payload" ] || emit_inert

# Resolved the way `crate::env` resolves the data home, and the way
# `plugin::managed_paths_file` writes it. No manifest means no plugin install
# has happened here, so there is nothing installed to protect.
if [ -n "${STORYHOOK_DATA_DIR:-}" ]; then
  manifest="${STORYHOOK_DATA_DIR}/managed-paths"
elif [ -n "${XDG_DATA_HOME:-}" ]; then
  manifest="${XDG_DATA_HOME}/storyhook/managed-paths"
else
  manifest="${HOME}/.local/share/storyhook/managed-paths"
fi
[ -f "$manifest" ] || emit_inert

# The deliberate, discoverable escape hatch. A file rather than an environment
# variable (SH-411): a variable can sit exported in a shell for a week and
# authorize every later edit invisibly, where creating a file is a deliberate
# act that leaves a dated trace on disk.
override="$(dirname "$manifest")/ALLOW_INSTALLED_EDITS"
[ -e "$override" ] && emit_inert

# The fast prefilter: one substring test per managed prefix, no subprocess.
hit=""
while IFS= read -r prefix; do
  case "$prefix" in ''|'#'*) continue ;; esac
  case "$payload" in *"$prefix"*) hit="$prefix"; break ;; esac
done < "$manifest"
[ -n "$hit" ] || emit_inert

# Only now is it worth parsing. The prefilter said a managed path appears
# SOMEWHERE in the payload; this decides whether it is actually the target.
#
# The hook's own plugin root is the identity of the helper it may admit
# (SH-632): a host runs one copy of a plugin's hooks per session, so the
# `<plugin-root>` the skill resolved and this file's own `../` are one
# directory. Derived here, after the prefilter, so the inert path pays for no
# subshell; assigned unconditionally, so nothing exported around this run can
# name a root on the hook's behalf (SH-411). `session-start.sh` derives the
# same fact for the dispatch sentinel.
hook_plugin_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
STORYHOOK_MANIFEST="$manifest" STORYHOOK_HOOK_PLUGIN_ROOT="$hook_plugin_root" python3 -c '
import json, os, re, shlex, stat, sys

payload = json.load(sys.stdin)
tool = payload.get("tool_name", "")
supplied = payload.get("tool_input", {}) or {}

manifest = os.environ["STORYHOOK_MANIFEST"]
override_dir = os.path.dirname(manifest)

prefixes = []
with open(manifest, encoding="utf-8") as handle:
    for line in handle:
        line = line.strip()
        if line and not line.startswith("#"):
            prefixes.append(line)

# Dispatch changes stories, worktrees and panes, never the installed helper.
# SH-588: domain mutations are not evidence of an installed-artifact edit.
# The helper still owns provider catalogs, readiness and claims; the host
# still owns authorization. An inert hook response approves no operation.
def dispatch_preserves_artifacts(args):
    """Recognize dispatch arguments with no installed-file operand or code string."""
    if args[:1] != ["dispatch"]:
        return False
    target = None
    options = set()
    for arg in args[1:]:
        if arg == "--next" or re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_-]*", arg):
            if target is not None:
                return False
            target = arg
            continue
        name, _, value = arg.partition("=")
        if name in options:
            return False
        if arg in {"--auto", "--full-auto", "--force", "--resume"}:
            pass
        elif name == "--agent" and value in {"claude", "codex"}:
            pass
        elif name == "--speed" and value in {"standard", "fast"}:
            pass
        elif name in {"--model", "--effort"} and re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.-]*", value):
            pass
        else:
            return False
        options.add(name)
    return (
        target is not None
        and not {"--force", "--resume"} <= options
        and (target != "--next" or not options & {"--force", "--resume", "--full-auto"})
        and ("--full-auto" not in options or "--auto" in options)
    )


# The identity test of the helper door (SH-632). `entry` as spelled must be a proper
# path beneath a managed prefix — the prefilter only proved a substring — and
# must resolve to the `bin/story.sh` beside this very hook. Returns the spelled
# path, or None. Cheap: two realpath calls, no file opened yet.
def installed_helper_of_this_plugin(entry):
    if not os.path.isabs(entry):
        return None
    if not any(entry.startswith(prefix.rstrip("/") + "/") for prefix in prefixes):
        return None
    root = os.environ.get("STORYHOOK_HOOK_PLUGIN_ROOT", "")
    if not os.path.isabs(root):
        return None
    expected = os.path.join(os.path.realpath(root), "bin", "story.sh")
    if os.path.realpath(entry) != expected:
        return None
    return entry


# No component of `path` below its anchor may be a symlink: a redirected
# managed directory is a different installed identity, whatever it resolves
# to. The anchor is HOME when the path is under it — HOME itself can have
# canonical macOS ancestry (/tmp -> /private/tmp) — else the parent of the nearest
# managed prefix, which is where a data-dir projection outside HOME sits.
def unredirected_below(path, home):
    anchors = [home] + [os.path.dirname(prefix.rstrip("/")) for prefix in prefixes]
    for anchor in anchors:
        if path.startswith(anchor.rstrip("/") + "/"):
            relative = path[len(anchor.rstrip("/")) + 1:]
            return os.path.realpath(path) == os.path.join(os.path.realpath(anchor), relative)
    return False


# The spelled helper is a regular file reached without following a link at any
# depth. Bounded and non-blocking for the same reason the launcher read is:
# a FIFO must not spend the hook timeout waiting for a writer, and nothing is
# ever executed to classify.
def regular_file_at_its_own_path(path, home):
    if not unredirected_below(path, home):
        return False
    try:
        descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
        with os.fdopen(descriptor, "rb") as handle:
            return stat.S_ISREG(os.fstat(handle.fileno()).st_mode)
    except OSError:
        return False


# SH-585/SH-588/SH-632: this is an argv contract for TWO installed entry
# points, not a general permission to run scripts. Keep it paired with the
# real installer in tests/plugin_install.rs; a marker alone cannot prove
# executable contents.
#
# The Codex launcher is admitted by its BYTES: three known lines, compared.
# The helper of the plugin itself is admitted by its IDENTITY: it is the `bin/story.sh`
# of the plugin this hook was loaded from, which is the path every non-Codex
# host is told to run (`references/helper-command.md`). Its bytes are trusted
# exactly as the bytes of this file are — same installer, same directory — and a
# byte compare of a 4,000-line helper would be the hand-copied-twice shape
# (SH-136) with nothing to compare against.
def launcher_preserves_artifacts(command):
    """Admit supported operations only through an installer-owned entry point."""
    # shlex is a lexer, not a shell parser. Reject even quoted occurrences of
    # unsupported syntax instead of guessing whether expansion will occur.
    if any(c in command for c in "\n\r;$`|&()<>{}*?[]~#"):
        return False
    try:
        words = shlex.split(command)
    except ValueError:
        return False

    home = os.environ.get("HOME", "")
    if not os.path.isabs(home):
        return False
    if words[:1] in (["bash"], ["/bin/bash"], ["/usr/bin/bash"]):
        words = words[1:]
    if not words:
        return False
    entry = words[0]
    launcher = os.path.join(home, ".codex/storyhook/story.sh")
    helper = installed_helper_of_this_plugin(entry)
    if entry == launcher:
        if os.path.dirname(launcher) not in prefixes:
            return False
    elif helper is None:
        return False
    args = words[1:]
    # An admitted verb has no managed-file operand. Do not let a project selector
    # or malformed argument confer permission on another installed path.
    if any(prefix in arg for prefix in prefixes for arg in args):
        return False
    if args[:1] == ["--project"]:
        if len(args) < 2 or not args[1] or args[1].startswith("-"):
            return False
        args = args[2:]
    elif args and args[0].startswith("--project="):
        if not args[0].removeprefix("--project="):
            return False
        args = args[1:]

    valid = args in (
        ["context"], ["context", "--full"], ["list"],
        ["capabilities"], ["capabilities", "--agent=claude"],
        ["capabilities", "--agent=codex"], ["ensure-cli"],
    ) or (
        len(args) == 2 and args[0] == "view"
        and re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_-]*", args[1]) is not None
    ) or dispatch_preserves_artifacts(args)
    if not valid:
        return False

    if entry != launcher:
        return regular_file_at_its_own_path(helper, home)

    # HOME itself can have canonical macOS ancestry (/tmp -> /private/tmp).
    # No component below it may redirect to a different installed identity.
    if not unredirected_below(launcher, home):
        return False
    expected = (
        b"# storyhook-managed: codex-launcher-v1\n"
        b"# Recreated by \x60story plugin install codex\x60; do not edit.\n"
        b"exec story plugin run codex -- \"$@\"\n"
    )
    try:
        # Bound the read and refuse special files: a FIFO must not spend the
        # hook timeout waiting for a writer. Never execute anything to classify.
        descriptor = os.open(launcher, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
        with os.fdopen(descriptor, "rb") as handle:
            return stat.S_ISREG(os.fstat(handle.fileno()).st_mode) and handle.read(len(expected) + 1) == expected
    except OSError:
        return False


# A shell command is allowed only when every simple command that names a
# managed path can be PROVEN to be a reader. This is deliberately not a shell
# security boundary: it recognizes the agent inspection vocabulary, and
# denies malformed, indirect or unknown shapes rather than guessing.
def shell_only_reads_managed_paths(command):
    if "$(" in command or "`" in command:
        return False

    try:
        lexer = shlex.shlex(command, posix=True, punctuation_chars="();<>|&")
        lexer.whitespace_split = True
        lexer.commenters = "#"
        tokens = list(lexer)
    except ValueError:
        return False

    separators = {";", "&&", "||", "|", "&", "(", ")"}
    segments = []
    segment = []
    for token in tokens:
        if token in separators:
            if segment:
                segments.append(segment)
                segment = []
        else:
            segment.append(token)
    if segment:
        segments.append(segment)

    readers = {"cat", "grep", "head", "rg", "sed", "tail"}
    found_managed_path = False
    for words in segments:
        if not any(prefix in word for prefix in prefixes for word in words):
            continue
        found_managed_path = True

        # Output and unsupported compound redirections are not inspections.
        if any(">" in word or word in {"<<", "<<<", "<>"} for word in words):
            return False

        executable = os.path.basename(words[0])
        if executable not in readers:
            return False

        arguments = words[1:]
        if executable == "sed" and any(
            argument == "--in-place"
            or argument.startswith("--in-place=")
            or (
                argument.startswith("-")
                and not argument.startswith("--")
                and "i" in argument[1:]
            )
            for argument in arguments
        ):
            return False
        if executable == "rg" and any(
            argument == "--pre" or argument.startswith("--pre=")
            for argument in arguments
        ):
            return False

    return found_managed_path


if tool == "Bash":
    command = str(supplied.get("command", ""))
    if launcher_preserves_artifacts(command) or shell_only_reads_managed_paths(command):
        sys.stdout.write("{}")
        raise SystemExit(0)
    haystacks = [command]
else:
    haystacks = [
        str(supplied.get(key, ""))
        for key in ("file_path", "notebook_path", "path")
    ]

target = next(
    (
        prefix
        for prefix in prefixes
        for haystack in haystacks
        if prefix and prefix in haystack
    ),
    None,
)

if target is None:
    sys.stdout.write("{}")
    raise SystemExit(0)

decision = (
    "storyhook: refusing to edit an installed release artifact.\n"
    if tool != "Bash"
    else "storyhook: cannot establish that this operation preserves installed release artifacts.\n"
)
reason = (
    decision +
    f"  {target} is written by the storyhook plugin installer, so an edit there\n"
    f"  is overwritten by the next install -- lost, unversioned and untested --\n"
    f"  and it drifts this machine away from the release it reports.\n"
    f"\n"
    f"  Make the change in the storyhook CHECKOUT instead (plugins/story/...),\n"
    f"  where it is reviewed, gated and shipped by the next release.\n"
    f"  Run: story doctor install -- it shows what is pending. Nothing you have\n"
    f"  written is lost by this refusal.\n"
    f"\n"
    f"  If you genuinely mean to edit the installed copy, create\n"
    f"  {override_dir}/ALLOW_INSTALLED_EDITS."
)

sys.stdout.write(
    json.dumps(
        {
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "deny",
                "permissionDecisionReason": reason,
            }
        }
    )
)
' <<<"$payload"
exit 0
