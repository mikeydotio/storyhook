"""Resolve revivify ownership before contacting a tmux server (SH-825).

Library only: the daemon embeds this source as well as shipping it in the
plugin. Callers supply their bounded runner and filtered server environment;
discovery needs the original HOME/XDG selectors, never snapshot overrides.
"""

import hashlib
import json
import os
from pathlib import Path
import re
import stat
import subprocess


def logical_socket(socket, environ):
    """Resolve tmux's socket convention without starting or probing tmux."""
    selected = socket or environ.get("TMUX", "").split(",", 1)[0]
    if not selected:
        selected = os.path.join(environ.get("TMUX_TMPDIR") or "/tmp",
                                "tmux-" + str(os.getuid()), "default")
    if not os.path.isabs(selected):
        raise RuntimeError(f"tmux socket must be absolute: {selected!r}")
    return os.path.realpath(selected)


def activation_path(socket, environ):
    """Return RV-10's canonical discovery key under the standard state root."""
    base = environ.get("XDG_STATE_HOME") or str(Path(environ.get("HOME") or Path.home()) / ".local/state")
    if not os.path.isabs(base):
        raise RuntimeError(f"revivify state root must be absolute: {base!r}")
    digest = hashlib.sha256(os.fsencode(socket)).hexdigest()
    return Path(base) / "tmux-revivify/activation" / (digest + ".json")


def read_private_record(path, missing=False):
    """Read private owned JSON; dangling links and denied reads are not absence."""
    try:
        metadata = path.lstat()
        if (not stat.S_ISREG(metadata.st_mode) or metadata.st_uid != os.getuid()
                or stat.S_IMODE(metadata.st_mode) & 0o077):
            raise ValueError("record must be a private regular file owned by this user")
        value = json.loads(path.read_bytes())
        if not isinstance(value, dict):
            raise ValueError("record is not an object")
        return value
    except FileNotFoundError as error:
        if missing and not os.path.lexists(path):
            return None
        raise RuntimeError(f"cannot read revivify activation {path}: {error}") from error
    except (OSError, ValueError) as error:
        raise RuntimeError(f"cannot read revivify activation {path}: {error}") from error


def validate_activation(record, socket, executable=True):
    """Validate the versioned consumer contract before trusting any executable."""
    try:
        if type(record.get("version")) is not int or record["version"] != 1:
            raise ValueError("unsupported activation version")
        if record.get("socket") != socket or type(record.get("active")) is not bool:
            raise ValueError("activation socket or active flag is invalid")
        if not record["active"]:
            return record
        for key in ("executable", "state_dir", "endpoint"):
            if not isinstance(record.get(key), str) or not os.path.isabs(record[key]):
                raise ValueError(f"{key} must be an absolute path")
        history = record.get("history")
        if not isinstance(history, list):
            raise ValueError("missing generation history")
        for generation in [record.get("generation"), *history]:
            if not isinstance(generation, str) or not re.fullmatch(r"[0-9a-f]{32}", generation):
                raise ValueError("invalid generation ID")
        expected = str(Path(socket).parent / (".rv-" + record["generation"]) / "s")
        if record["endpoint"] != expected:
            raise ValueError("endpoint is not the generation's private socket")
        if record.get("phase") not in ("reserved", "restoring", "ready", "failed"):
            raise ValueError("invalid generation phase")
        for key in ("reservation_host", "reservation_boot"):
            if not isinstance(record.get(key), str) or not record[key]:
                raise ValueError(f"missing {key}")
        identity = record.get("identity")
        if identity is None:
            if record["phase"] != "reserved":
                raise ValueError("published generation lacks process identity")
        elif (not isinstance(identity, dict) or set(identity) != {"host", "boot", "pid", "start"}
              or type(identity["pid"]) is not int or identity["pid"] <= 0
              or any(not isinstance(identity[k], str) or not identity[k] for k in ("host", "boot", "start"))):
            raise ValueError("invalid generation process identity")
        if executable and not os.access(record["executable"], os.X_OK):
            raise ValueError(f"revivify executable unavailable: {record['executable']}")
        return record
    except (ValueError, KeyError, TypeError) as error:
        raise RuntimeError(f"invalid revivify activation for {socket}: {error}") from error


def discover_activation(socket, environ):
    """Resolve logical, current private, or proven predecessor socket ownership."""
    path = activation_path(socket, environ)
    record = read_private_record(path, missing=True)
    if record is not None:
        return validate_activation(record, socket)
    endpoint = Path(socket)
    if endpoint.name != "s" or not re.fullmatch(r"\.rv-[0-9a-f]{32}", endpoint.parent.name):
        return None
    # Do not let Path.glob suppress a discovery directory permission error.
    try:
        files = sorted(path.parent.iterdir())
    except FileNotFoundError:
        files = []
    except OSError as error:
        raise RuntimeError(f"cannot discover revivify activation for {socket}: {error}") from error
    matches = []
    for candidate in files:
        if candidate.suffix != ".json":
            continue
        value = read_private_record(candidate)
        logical = value.get("socket")
        if not isinstance(logical, str) or not os.path.isabs(logical):
            raise RuntimeError(f"invalid activation socket in {candidate}")
        if activation_path(logical_socket(logical, environ), environ) != candidate:
            raise RuntimeError(f"invalid activation key in {candidate}")
        validate_activation(value, logical)
        if value.get("endpoint") == socket:
            matches.append(value)
        elif value["active"] and endpoint.parent.name[4:] in value["history"]:
            old = read_private_record(Path(value["state_dir"]) / "owners" / (endpoint.parent.name[4:] + ".json"))
            validate_activation(old, logical, executable=False)
            if old["generation"] != endpoint.parent.name[4:] or old["endpoint"] != socket:
                raise RuntimeError(f"invalid revivify history for {socket}")
            matches.append(value)
    if len(matches) != 1:
        raise RuntimeError(f"private revivify endpoint {socket} has {len(matches)} proven activations")
    return matches[0]


def resolve_target(socket, environ, runner, server_environment, ensure=False):
    """Resolve one operation's immutable transport target or fail visibly."""
    socket = logical_socket(socket, environ)
    record = discover_activation(socket, environ)
    if record is None or not record["active"]:
        return dict(protected=False, socket=socket, endpoint=socket)
    logical = record["socket"]
    argv = [record["executable"], "server", "ensure" if ensure else "inspect",
            "--socket", logical, "--json"]
    if ensure:
        argv += ["--state-dir", record["state_dir"]]
    context = f"revivify {logical} generation {record['generation']}"
    try:
        answer = runner(argv, env=server_environment, capture_output=True, text=True, close_fds=True)
    except (OSError, subprocess.TimeoutExpired) as error:
        raise RuntimeError(f"{context}: {error}") from error
    if answer.returncode:
        raise RuntimeError(f"{context}: {argv[2]} exited {answer.returncode}: {answer.stderr.strip()}; {answer.stdout.strip()}")
    try:
        report = json.loads(answer.stdout)
        if not isinstance(report, dict):
            raise ValueError("ownership response is not an object")
        validate_activation(report, logical)
        if (report.get("restore_ready") is not True or not report["active"]
                or report.get("ownership_state") != "reachable" or report.get("phase") != "ready"):
            raise ValueError("protected server is not restore-ready")
        # A concurrent ensure may publish a successor, but a fabricated or stale
        # response cannot redirect an operation to a different server.
        published = read_private_record(activation_path(logical, environ))
        validate_activation(published, logical)
        if any(report.get(k) != published.get(k) for k in
               ("active", "generation", "endpoint", "identity", "state_dir", "executable", "phase", "history")):
            raise ValueError("ownership response differs from published activation")
        generation_state = str(Path(report["state_dir"]) / "generations" / report["generation"])
        if report.get("generation_state_dir") != generation_state:
            raise ValueError("invalid generation state directory")
        return dict(protected=True, socket=logical, endpoint=report["endpoint"],
                    generation=report["generation"], state_dir=generation_state,
                    executable=report["executable"], history=report["history"], identity=report["identity"])
    except (ValueError, KeyError, TypeError, RuntimeError) as error:
        raise RuntimeError(f"{context}: {error}") from error


def require_current_selector(target, selected):
    """A previously captured private selector must never follow a successor."""
    selected = Path(selected)
    if (target['protected'] and selected.name == 's'
            and re.fullmatch(r'\.rv-[0-9a-f]{32}', selected.parent.name)
            and str(selected) != target['endpoint']):
        raise RuntimeError(f'protected tmux binding on {selected} requires re-adoption before using {target["endpoint"]}')


def target_arguments(target, arguments):
    """Pin protected commands, preserving all unmanaged arguments verbatim."""
    if not target["protected"]:
        return list(arguments)
    prefix, command, _socket = split_tmux_arguments(arguments, {})
    return ["-N", "-S", target["endpoint"], *prefix, *command]


def split_tmux_arguments(arguments, environ):
    """Separate client flags from command arguments without interpreting commands."""
    prefix, socket, name = [], None, None
    index = 0
    while index < len(arguments) and arguments[index].startswith("-"):
        flag = arguments[index]
        if flag == "--":
            index += 1
            break
        if flag in ("-S", "-L", "-f", "-c", "-T"):
            if index + 1 >= len(arguments):
                raise RuntimeError(f"tmux client flag {flag} requires a value")
            value = arguments[index + 1]
            index += 2
        elif flag.startswith(("-S", "-L")) and len(flag) > 2:
            flag, value = flag[:2], flag[2:]
            index += 1
        else:
            prefix.append(flag)
            index += 1
            continue
        if flag == "-S":
            socket = value
        elif flag == "-L":
            name = value
        else:
            prefix += [flag, value]
    if socket is None and name is not None:
        socket = os.path.join(environ.get("TMUX_TMPDIR") or "/tmp", "tmux-" + str(os.getuid()), name)
    return prefix, list(arguments[index:]), socket


def restore_evidence(target, environ):
    """Corroborate UUID mappings with an immutable, authorized source snapshot.

    This is evidence only. Callers must match live options, process and worktree
    identity and hold their own mutation authority before changing any binding.
    """
    if not target['protected']:
        return None
    try:
        activation = activation_path(target['socket'], environ)
        current = read_private_record(activation)
        validate_activation(current, target['socket'])
        if (not current['active'] or current['phase'] != 'ready'
                or any(current.get(k) != target.get(k) for k in ('generation', 'endpoint', 'identity', 'history'))
                or str(Path(current['state_dir']) / 'generations' / current['generation']) != target['state_dir']):
            raise ValueError('activation changed behind the captured restore target')
        receipt = read_private_record(Path(target['state_dir']) / 'run/last-restore.json', missing=True)
        if receipt is None:
            # RV-10 live adoption deliberately performs no replay and writes
            # only ownership-restore.json. This is not an absent restore result.
            witness = read_private_record(Path(target['state_dir']) / 'run/ownership-restore.json')
            if current.get('adopted') is True and witness.get('ready') is True and witness.get('generation') == target['generation']:
                return None
            raise ValueError('missing restore receipt without explicit live-adoption evidence')
        mapping = receipt.get('pane_map')
        if (receipt.get('state') not in ('done', 'skipped') or receipt.get('failed') != []
                or not isinstance(mapping, dict)
                or any(not isinstance(k, str) or not k or not isinstance(v, str)
                       or not re.fullmatch(r'%[0-9]+', v) for k, v in mapping.items())
                or len(set(mapping.values())) != len(mapping)):
            raise ValueError('failed or ambiguous restore receipt')
        if not mapping:
            return None
        snapshot_id = receipt.get('snapshot_id')
        if not isinstance(snapshot_id, str) or not re.fullmatch(r'[0-9]{8}T[0-9]{6}\.[0-9]{6}Z-[a-z0-9-]+', snapshot_id):
            raise ValueError('invalid restore snapshot ID')
        base = Path(current['state_dir'])
        sources = []
        if current.get('legacy_snapshot') is not None:
            if current['legacy_snapshot'] != snapshot_id:
                raise ValueError('restore receipt differs from explicit legacy selection')
            # Only the provider's explicit selection grants a legacy stream.
            sources.append((None, None, read_private_record(base / 'snapshots' / (snapshot_id + '.json'))))
        else:
            for index, generation in enumerate(current['history']):
                path = base / 'generations' / generation / 'snapshots' / (snapshot_id + '.json')
                snapshot = read_private_record(path, missing=True)
                if snapshot is None:
                    continue
                owner = read_private_record(base / 'owners' / (generation + '.json'))
                validate_activation(owner, target['socket'], executable=False)
                if (owner.get('generation') != generation or owner.get('state_dir') != current['state_dir']
                        or owner.get('history') != current['history'][:index]
                        or snapshot.get('provenance') != {'socket': target['socket'], 'generation': generation}):
                    raise ValueError('snapshot provenance differs from retained owner history')
                sources.append((generation, owner, snapshot))
        if len(sources) != 1:
            raise ValueError(f'restore snapshot has {len(sources)} authorized source streams')
        generation, owner, snapshot = sources[0]
        panes = _restore_snapshot_panes(snapshot, receipt)
        if not set(mapping).issubset(panes):
            raise ValueError('restore map names a UUID absent from its snapshot')
        result = {}
        for pane_uuid, pane_id in mapping.items():
            saved = panes[pane_uuid]
            if not saved['sessions']:
                raise ValueError('mapped pane has no restored session')
            result[pane_uuid] = dict(saved, pane_id=pane_id)
        if read_private_record(activation) != current:
            raise ValueError('activation changed while reading restore evidence')
        return dict(snapshot_id=snapshot_id, source_generation=generation,
                    source_endpoint=owner['endpoint'] if owner else None,
                    source_history=owner['history'] if owner else [], panes=result)
    except (OSError, ValueError, KeyError, TypeError, RuntimeError) as error:
        raise RuntimeError(f'revivify restore evidence for {target["socket"]}: {error}') from error


def _restore_snapshot_panes(snapshot, receipt):
    """Index unique UUIDs and the restored sessions linked to each saved window."""
    if (type(snapshot.get('schema')) is not int or snapshot['schema'] != 1
            or not isinstance(snapshot.get('windows'), dict) or not isinstance(snapshot.get('sessions'), list)
            or not isinstance(receipt.get('session_ids'), dict) or not isinstance(receipt.get('restored'), list)):
        raise ValueError('invalid restore snapshot or session map')
    sessions = receipt['session_ids']
    restored = receipt['restored']
    if (any(not isinstance(name, str) or not name or not isinstance(value, str)
            or not re.fullmatch(r'\$[0-9]+', value) for name, value in sessions.items())
            or len(set(sessions.values())) != len(sessions)
            or any(not isinstance(name, str) or not name for name in restored)
            or len(set(restored)) != len(restored) or set(restored) != set(sessions)):
        raise ValueError('invalid restored session ID')
    links = {}
    names = set()
    for session in snapshot['sessions']:
        if (not isinstance(session, dict) or not isinstance(session.get('name'), str)
                or not session['name'] or session['name'] in names or not isinstance(session.get('links'), list)):
            raise ValueError('invalid snapshot session')
        names.add(session['name'])
        for link in session['links']:
            if not isinstance(link, dict) or link.get('window_key') not in snapshot['windows']:
                raise ValueError('invalid snapshot window link')
            if session['name'] in sessions and session['name'] in receipt['restored']:
                links.setdefault(link['window_key'], {})[session['name']] = sessions[session['name']]
    if not set(restored).issubset(names):
        raise ValueError('restored session is absent from its snapshot')
    panes = {}
    for key, window in snapshot['windows'].items():
        if (not isinstance(window, dict) or window.get('key') != key or not isinstance(window.get('name'), str)
                or not isinstance(window.get('options'), dict) or not isinstance(window.get('panes'), list)):
            raise ValueError('invalid snapshot window')
        for pane in window['panes']:
            if (not isinstance(pane, dict) or not isinstance(pane.get('uuid'), str) or not pane['uuid']
                    or not isinstance(pane.get('options'), dict) or not isinstance(pane.get('cwd'), str)
                    or not os.path.isabs(pane['cwd']) or pane['uuid'] in panes):
                raise ValueError('invalid or duplicate snapshot pane UUID')
            panes[pane['uuid']] = dict(pane=pane, window=window, sessions=links.get(key, {}))
    return panes
