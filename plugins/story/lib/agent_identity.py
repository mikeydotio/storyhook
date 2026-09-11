"""Bind StoryHook notifications to one verified pane and process incarnation.

Managed launches supply the provider after their readiness gate. Direct-launch
recovery requires independent executable evidence and a registered story worktree.
No operation in this module types into or terminates a pane.
"""

import json
import os
from pathlib import Path
import re
import shlex
import shutil
import subprocess
import sys

sys.dont_write_bytecode = True
from process_identity import process_identity


OPTION = "@storyhook-identity-v1"
FORMAT = "\t".join("#{" + field + "}" for field in (
    "window_name", "pane_id", "pane_pid", "pane_dead", "pane_current_path",
    "pane_current_command", "pane_start_command", "socket_path"))
FORMAT = FORMAT.replace("#{pane_current_path}", "#{?pane_dead,#{pane_start_path},#{pane_current_path}}")
PROVIDERS = ("claude", "codex")


class IdentityError(Exception):
    """A contextual refusal, never implicit permission to replace a live pane."""

    def __init__(self, reason, detail):
        """Keep the helper refusal token separate from its human diagnostic."""
        super().__init__(detail)
        self.reason = reason


def run(*args, reason="pane-query-failed"):
    """Run a bounded external probe and retain stderr on failure."""
    result = subprocess.run(args, capture_output=True, text=True, timeout=5, check=False)
    if result.returncode:
        raise IdentityError(reason, f"{args[0]} {args[1]}: {result.stderr.strip() or result.returncode}")
    return result.stdout.rstrip("\n")


def canonical(path):
    """Require a real directory rather than normalizing a guessed missing path."""
    result = Path(path).resolve(strict=True)
    if not result.is_dir():
        raise IdentityError("pane-provider-unknown", f"not a directory: {path}")
    return str(result)


def tmux(socket, *args):
    """Use a captured socket when available; otherwise retain caller tmux context."""
    return run("tmux", *(["-S", socket] if socket else []), *args)


def panes(socket=""):
    """Read one consistent terminal inventory and reject malformed rows."""
    return parse_panes(tmux(socket, "list-panes", "-a", "-F", FORMAT))


def pane_at(pane, socket=""):
    """Probe an exact pane without relying on the active window or pane."""
    return parse_panes(tmux(socket, "display-message", "-p", "-t", pane, FORMAT)).get(pane)


def parse_panes(output):
    """Decode terminal rows, including duplicate session links to one pane."""
    result = {}
    for row in output.splitlines():
        parts = row.split("\t")
        if len(parts) != 8:
            raise IdentityError("pane-query-failed", "tmux returned malformed pane identity")
        window, pane, pid, dead, cwd, command, launch, server = parts
        if not re.fullmatch(r"%[0-9]+", pane) or not pid.isdecimal() or dead not in ("0", "1") or not server.startswith("/"):
            raise IdentityError("pane-query-failed", f"tmux returned invalid identity for {pane}")
        value = dict(window=window, pane=pane, pid=int(pid), dead=dead == "1",
                     cwd=cwd, command=command, launch=launch, socket=server)
        # Linked windows appear in multiple sessions, but remain one pane.
        if pane in result and value != result[pane]:
            raise IdentityError("pane-query-failed", f"conflicting tmux rows for {pane}")
        result[pane] = value
    return result


def context(project, story, window, worktree=None):
    """Resolve the requested repository and its registered worktrees."""
    common = canonical(run("git", "rev-parse", "--path-format=absolute", "--git-common-dir"))
    paths = set()
    for field in run("git", "worktree", "list", "--porcelain", "-z").split("\0"):
        if field.startswith("worktree "):
            path = Path(field[9:])
            if path.is_dir():
                paths.add(canonical(path))
    expected = canonical(worktree) if worktree else None
    if expected and expected not in paths:
        raise IdentityError("pane-provider-unknown", f"worktree {expected} is not registered in {common}")
    return dict(project=project, story=story, window=window, common=common,
                worktrees=paths, expected=expected)


def eligible_worktree(ctx, path, registered=False):
    """Accept an exact lease/registration, or a conventional linked story worktree."""
    worktree = canonical(path)
    if worktree not in ctx["worktrees"]:
        return False
    if ctx["expected"]:
        return worktree == ctx["expected"]
    if registered:
        return True
    p = Path(worktree)
    return (p.name == ctx["window"] and p.parent.name == "worktrees"
            and p.parent.parent.name in (".claude", ".codex"))


def read_record(pane):
    """Read only local pane options; missing metadata differs from a failed query."""
    options = tmux(pane["socket"], "show-options", "-p", "-t", pane["pane"])
    # Read all local options first: '-qv' alone cannot distinguish an absent
    # option from an unavailable pane on all supported tmux versions.
    if not any(line.startswith(OPTION + " ") for line in options.splitlines()):
        return None
    raw = tmux(pane["socket"], "show-options", "-p", "-v", "-t", pane["pane"], OPTION)
    try:
        record = json.loads(raw)
        if (not isinstance(record, dict) or record.get("version") != 1
                or record.get("provider") not in PROVIDERS):
            raise ValueError("unsupported identity record")
        return record
    except (ValueError, TypeError) as error:
        raise IdentityError("pane-provider-unknown", f"invalid identity on {pane['pane']}: {error}") from error


def observe(ctx, pane, provider):
    """Capture every immutable ownership fact and current foreground evidence."""
    if pane["dead"]:
        raise IdentityError("pane-dead", f"pane {pane['pane']} has exited")
    try:
        process = process_identity(pane["pid"])
    except (OSError, ValueError, IndexError) as error:
        raise IdentityError("pane-query-failed", f"cannot identify pane {pane['pane']} process: {error}") from error
    return dict(version=1, project=ctx["project"], story=ctx["story"],
                common=ctx["common"], worktree=canonical(pane["cwd"]),
                pane=pane["pane"], socket=pane["socket"], provider=provider,
                process=process, command=pane["command"], launch=pane["launch"])


def direct_provider(pane):
    """Identify a direct provider executable without permissive name matching."""
    command = pane["launch"]
    if any(char in command for char in "\n;|&<>`$"):
        return None
    try:
        argv = shlex.split(command)
        # tmux quotes a single shell-command argument in pane_start_command.
        if len(argv) == 1 and " " in argv[0]:
            argv = shlex.split(argv[0])
        if not argv:
            return None
        launched = shutil.which(argv[0])
        if not launched:
            return None
        actual = process_identity(pane["pid"])["executable"]
        if actual != os.path.realpath(launched) or Path(actual).name != pane["command"]:
            return None
        candidates = [provider for provider in PROVIDERS
                      if shutil.which(provider) and
                      os.path.realpath(shutil.which(provider)) == actual]
        # Executable aliases still need an unambiguous provider launch word.
        if Path(argv[0]).name in candidates:
            return Path(argv[0]).name
        return candidates[0] if len(candidates) == 1 else None
    except ValueError:
        return None


def validate(record):
    """Revalidate the exact stored process incarnation before a later effect."""
    current = pane_at(record["pane"], record["socket"])
    if not current or current["dead"]:
        raise IdentityError("pane-changed", f"registered pane {record['pane']} exited or disappeared")
    ctx = context(record["project"], record["story"], current["window"], record["worktree"])
    if observe(ctx, current, record["provider"]) != record:
        raise IdentityError("pane-changed", f"pane {record['pane']} process, provider, or worktree identity changed")
    if read_record(current) != record:
        raise IdentityError("pane-changed", f"pane {record['pane']} registration changed")
    direct = direct_provider(current)
    if direct is not None and direct != record["provider"]:
        raise IdentityError("pane-changed", f"pane {record['pane']} provider conflicts with its live executable")
    legacy = tmux(record["socket"], "show-options", "-w", "-qv", "-t", record["pane"], "@storyhook-agent")
    if legacy and legacy != record["provider"]:
        raise IdentityError("pane-changed", f"pane {record['pane']} provider records disagree")
    return record


def write_record(ctx, pane, provider, expected=None):
    """Register a live observation and require exact readback before success."""
    record = observe(ctx, pane, provider)
    if expected is not None and record != expected:
        raise IdentityError("pane-changed", f"pane {pane['pane']} changed after provider identification")
    again = pane_at(pane["pane"], pane["socket"])
    if not again or observe(ctx, again, provider) != record:
        raise IdentityError("pane-changed", f"pane {pane['pane']} changed before registration")
    encoded = json.dumps(record, separators=(",", ":"), sort_keys=True)
    tmux(pane["socket"], "set-option", "-p", "-t", pane["pane"], OPTION, encoded)
    # Existing census and resume readers consume this compatibility tag. It
    # does not authorize live notification; only the pane-local record does.
    tmux(pane["socket"], "set-option", "-w", "-t", pane["pane"], "@storyhook-agent", provider)
    if tmux(pane["socket"], "show-options", "-w", "-v", "-t", pane["pane"], "@storyhook-agent") != provider:
        raise IdentityError("pane-provider-unknown", f"provider readback failed for {pane['pane']}")
    return validate(record)


def register(project, story, window, worktree, pane_id, pid, provider, launch_start=None):
    """Bind the exact launch owner supplied by dispatch after provider readiness."""
    if provider not in PROVIDERS:
        raise IdentityError("pane-provider-unknown", f"unsupported provider {provider}")
    ctx = context(project, story, window, worktree)
    pane = pane_at(pane_id)
    if not pane or pane["pid"] != int(pid) or not eligible_worktree(ctx, pane["cwd"]):
        raise IdentityError("pane-changed", f"dispatch pane {pane_id} no longer belongs to {story}")
    expected = observe(ctx, pane, provider)
    if launch_start is not None and expected["process"]["start"] != launch_start:
        raise IdentityError("pane-changed", f"dispatch pane {pane_id} process was replaced during startup")
    return write_record(ctx, pane, provider, expected=expected)


def resolve(project, story, window):
    """Resolve one owner; unknown live identity never becomes agent absence."""
    lease = json.loads(os.environ.get("STORYHOOK_NOTIFY_LEASE_V1", "null"))
    socket = ""
    worktree = None
    if lease is not None:
        if lease["version"] != 1 or lease["project_slug"] != project or lease["story_id"] != story:
            raise IdentityError("pane-provider-unknown", "notification lease names another project or story")
        socket = lease["tmux"]["socket_path"]
        worktree = lease["worktree_path"]
    inventory = [p for p in panes(socket).values() if p["window"] == window]
    if not inventory:
        raise IdentityError("pane-unavailable", f"no tmux window named {window} on the requested server")
    ctx = context(project, story, window, worktree)
    if lease and canonical(run("git", "-C", canonical(lease["repository_path"]), "rev-parse",
                               "--path-format=absolute", "--git-common-dir")) != ctx["common"]:
        raise IdentityError("pane-provider-unknown", "notification lease belongs to another repository")
    matches = []
    dead = []
    for pane in inventory:
        record = read_record(pane)
        if record is not None:
            if (record.get("pane"), record.get("socket")) != (pane["pane"], pane["socket"]):
                raise IdentityError("pane-provider-unknown", f"pane {pane['pane']} carries another pane's registration")
            if (record.get("project"), record.get("story"), record.get("common")) != (project, story, ctx["common"]):
                raise IdentityError("pane-provider-unknown", f"pane {pane['pane']} belongs to another project or story")
            if not eligible_worktree(ctx, pane["cwd"], registered=True):
                raise IdentityError("pane-provider-unknown", f"pane {pane['pane']} left its registered worktree")
            if pane["dead"]:
                if (record.get("process", {}).get("pid") != pane["pid"]
                        or record.get("pane") != pane["pane"]
                        or record.get("socket") != pane["socket"]
                        or record.get("worktree") != canonical(pane["cwd"])
                        or record.get("launch") != pane["launch"]):
                    raise IdentityError("pane-changed", "dead pane no longer matches its recorded owner")
                dead.append(pane)
            else:
                matches.append((pane, validate(record)))
        elif eligible_worktree(ctx, pane["cwd"]):
            if pane["dead"]:
                legacy = tmux(pane["socket"], "show-options", "-w", "-qv", "-t", pane["pane"], "@storyhook-agent")
                if legacy in PROVIDERS:
                    dead.append(pane)
            else:
                provider = direct_provider(pane)
                if provider:
                    legacy = tmux(pane["socket"], "show-options", "-w", "-qv", "-t", pane["pane"], "@storyhook-agent")
                    if legacy and legacy != provider:
                        raise IdentityError("pane-provider-unknown", f"pane {pane['pane']} provider tag conflicts with its executable")
                    matches.append((pane, observe(ctx, pane, provider)))
    if len(matches) != 1:
        if not matches and len(dead) == 1 and len(inventory) == 1:
            raise IdentityError("pane-dead", f"registered agent in {window} has exited (remain-on-exit)")
        raise IdentityError("pane-provider-unknown", f"window {window} has {len(matches)} verified live destinations; require exactly one agent in its registered story worktree")
    pane, record = matches[0]
    if read_record(pane) is None:
        record = write_record(ctx, pane, record["provider"], expected=record)
    return validate(record)


def main():
    """Expose JSON receipts to the Bash provider adapter."""
    try:
        verb, *args = sys.argv[1:]
        if verb == "capture":
            record = process_identity(int(args[0]))
        elif verb == "register":
            record = register(*args)
        elif verb == "resolve":
            record = resolve(*args)
        elif verb == "validate":
            record = validate(json.loads(args[0]))
        else:
            raise ValueError(f"unknown identity operation: {verb}")
        print(json.dumps({"ok": True, "identity": record}))
        return 0
    except IdentityError as error:
        print(json.dumps({"ok": False, "reason": error.reason, "display": str(error)}))
    except (OSError, ValueError, KeyError, TypeError, IndexError, subprocess.TimeoutExpired) as error:
        print(json.dumps({"ok": False, "reason": "pane-query-failed", "display": f"agent identity: {error}"}))
    return 1


if __name__ == "__main__":
    sys.exit(main())
