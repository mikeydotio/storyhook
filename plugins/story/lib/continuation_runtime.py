"""Bounded native observations for durable continuation; never interrupt live turns."""

import base64
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import subprocess
import sys
import tempfile

MAX_BYTES = 64 * 1024 * 1024
OPTION = '@storyhook-continuation'
MARKER = 'storyhook-cleanup-lease-v1.json'


def require(condition, detail):
    """Refuse uncertain identity rather than guessing an owner."""
    if not condition:
        raise RuntimeError(detail)


def command(argv, cwd=None, timeout=2, env=None):
    """Bound subprocess lifetime and output without an in-memory output pipe."""
    with tempfile.TemporaryFile(dir='/tmp') as out, tempfile.TemporaryFile(dir='/tmp') as err:
        result = subprocess.run(argv, cwd=cwd, env=env, stdout=out, stderr=err,
                                timeout=timeout, check=False)
        require(out.tell() <= MAX_BYTES and err.tell() <= MAX_BYTES,
                'continuation subprocess output exceeds 64 MiB: ' + argv[0])
        out.seek(0)
        err.seek(0)
        require(result.returncode == 0,
                'continuation command failed: ' + repr(argv) + ': '
                + err.read().decode('utf-8', errors='replace'))
        return out.read()


def git(cwd, *args):
    """Read repository evidence with optional index updates disabled."""
    return command(['git', '--no-optional-locks', '-C', str(cwd), *args])


def read_json(path):
    """Read a bounded ordinary JSON document."""
    with Path(path).open('rb') as stream:
        raw = stream.read(MAX_BYTES + 1)
    require(len(raw) <= MAX_BYTES, 'continuation evidence exceeds 64 MiB: ' + str(path))
    return json.loads(raw)


def transcript(capture):
    """Read the identity header and complete recent records without loading all history."""
    path = Path(capture['transcript_path'])
    with path.open('rb') as stream:
        header = stream.readline(min(MAX_BYTES, 1024 * 1024) + 1)
        require(header.endswith(b'\n'), 'native transcript has an incomplete identity header')
        size = stream.seek(0, os.SEEK_END)
        start = max(0, size - MAX_BYTES)
        stream.seek(start)
        if start:
            stream.readline(MAX_BYTES)
        raw = stream.read(MAX_BYTES + 1)
    require(len(raw) <= MAX_BYTES and raw.endswith(b'\n'),
            'native transcript has an oversized or incomplete last record')
    if start:
        raw = header + raw
    records = [json.loads(line) for line in raw.splitlines() if line.strip()]
    require(all(isinstance(row, dict) for row in records), 'invalid native transcript records')
    return records


def native_state(capture):
    """Read the provider's current task boundary from its owned transcript."""
    rows = transcript(capture)
    cwd = Path(capture['lease']['worktree_path']).resolve(strict=True)
    sid = capture['session_id']
    if capture['provider'] == 'codex':
        sessions = [row['payload'] for row in rows if row.get('type') == 'session_meta']
        require(len(sessions) == 1 and sessions[0].get('id') == sid
                and Path(sessions[0].get('cwd', '')).resolve() == cwd
                and sessions[0].get('source') == 'cli', 'foreign or missing root Codex session')
        active = None
        state = 'uncertain'
        for row in rows:
            payload = row.get('payload', {})
            if row.get('type') != 'event_msg':
                continue
            if payload.get('type') == 'task_started':
                active = payload.get('turn_id')
                require(isinstance(active, str) and active, 'missing native task identity')
                state = 'busy'
            elif payload.get('type') in ('task_complete', 'turn_aborted'):
                completed = payload.get('turn_id')
                require(isinstance(completed, str) and completed
                        and (active is None or completed == active),
                        'unmatched native completion boundary')
                state = 'idle'
            elif payload.get('type') == 'user_message':
                state = 'busy'
        return state
    require(capture['provider'] == 'claude', 'unsupported provider')
    for row in reversed(rows):
        if row.get('type') not in ('assistant', 'user'):
            continue
        require(row.get('sessionId') == sid and row.get('isSidechain') is False
                and Path(row.get('cwd', '')).resolve() == cwd,
                'foreign or missing root Claude session')
        return ('idle' if row['type'] == 'assistant'
                and row.get('message', {}).get('stop_reason') == 'end_turn' else 'busy')
    raise RuntimeError('Claude transcript has no native conversation boundary')


def cleanup_lease(cwd, story_id):
    """Validate only linked-worktree metadata, without reading main's working files."""
    root = Path(git(cwd, 'rev-parse', '--show-toplevel').decode().strip()).resolve(strict=True)
    private = Path(git(root, 'rev-parse', '--absolute-git-dir').decode().strip()).resolve(strict=True)
    common = Path(git(root, 'rev-parse', '--path-format=absolute', '--git-common-dir').decode().strip()).resolve(strict=True)
    require(private != common, 'continuation requires a retained linked worktree')
    lease = read_json(private / MARKER)
    listing = git(root, 'worktree', 'list', '--porcelain', '-z').split(b'\0')
    repository = next((item[9:] for item in listing if item.startswith(b'worktree ')), None)
    require(repository is not None, 'Git inventory has no repository')
    branch = git(root, 'symbolic-ref', '--short', 'HEAD').decode().strip()
    require(lease.get('version') == 1 and lease.get('story_id') == story_id
            and Path(lease['worktree_path']).resolve(strict=True) == root
            and Path(lease['repository_path']).resolve(strict=True) == Path(os.fsdecode(repository)).resolve(strict=True)
            and lease['branch'] == branch and branch != 'main'
            and Path(lease['tmux']['socket_path']).is_absolute(),
            'cleanup lease differs from retained Git resources')
    return lease


def fingerprint(cwd):
    """Hash tracked changes and untracked bytes without altering the worktree."""
    digest = hashlib.sha256()
    for args in [('rev-parse', 'HEAD'), ('status', '--porcelain=v1', '-z'),
                 ('diff', '--binary', '--no-ext-diff'),
                 ('diff', '--cached', '--binary', '--no-ext-diff')]:
        value = git(cwd, *args)
        digest.update(len(value).to_bytes(8, 'big'))
        digest.update(value)
    for name in sorted(git(cwd, 'ls-files', '--others', '--exclude-standard', '-z').split(b'\0')):
        if not name:
            continue
        path = Path(cwd) / os.fsdecode(name)
        info = path.lstat()
        digest.update(name + b'\0' + str(info.st_mode).encode() + b'\0')
        if stat.S_ISLNK(info.st_mode):
            value = os.fsencode(os.readlink(path))
        else:
            require(stat.S_ISREG(info.st_mode) and info.st_size <= MAX_BYTES,
                    'untracked evidence is not a bounded ordinary file: ' + str(path))
            with path.open('rb') as stream:
                value = stream.read(MAX_BYTES + 1)
            require(len(value) <= MAX_BYTES, 'untracked evidence grew beyond 64 MiB')
        digest.update(len(value).to_bytes(8, 'big'))
        digest.update(value)
    return digest.hexdigest()


def tmux(socket, *args):
    """Address exactly the captured server, never a default socket."""
    require(isinstance(socket, str) and os.path.isabs(socket), 'missing absolute tmux socket')
    return command(['tmux', '-S', socket, *args]).decode().strip()


def panes(socket):
    """Inventory exact pane IDs; failure never means absence."""
    raw = tmux(socket, 'list-panes', '-a', '-F',
               '#{pane_id}\t#{window_id}\t#{window_name}\t#{pane_pid}\t#{pane_dead}\t#{pane_current_command}')
    rows = [line.split('\t') for line in raw.splitlines()]
    require(all(len(row) == 6 for row in rows), 'invalid tmux pane inventory')
    return rows


def process_start(pid):
    """Read process incarnation; permission failures are not absence."""
    require(type(pid) is int and pid > 0, 'invalid provider PID')
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return None
    except PermissionError as exc:
        raise RuntimeError('provider PID observation denied') from exc
    started = command(['ps', '-p', str(pid), '-o', 'lstart=']).decode().strip()
    require(bool(started), 'provider start time unavailable')
    return started


def owner(capture):
    """Require the original dispatcher-owned pane and process incarnation."""
    rows = panes(capture['socket'])
    matches = [row for row in rows if row[0] == capture['pane']]
    named = [row for row in rows if row[2] == capture['lease']['story_id']]
    started = process_start(capture['pid'])
    if not matches:
        require(not named and started is None, 'missing pane has a surviving or replaced owner')
        return 'absent'
    require(len(matches) == 1 and len(named) == 1 and matches == named,
            'story pane identity is ambiguous or renamed')
    row = matches[0]
    require(row[1] == capture['window'] and row[3] == str(capture['pid']),
            'pane process or window ownership changed')
    if row[4] == '1':
        require(started is None, 'dead pane still has a live captured process')
        return 'absent'
    require(row[4] == '0' and started == capture['started'], 'provider process incarnation changed')
    require(capture['provider'] in row[5].lower(), 'provider command differs from dispatcher identity')
    metadata = json.loads(tmux(capture['socket'], 'show-options', '-w', '-v', '-t', row[0], OPTION))
    for key in ('provider', 'session_id', 'pid', 'started'):
        require(metadata.get(key) == capture[key], 'dispatcher identity differs: ' + key)
    return 'present'


def capture_request(value):
    """Bind a root native Stop to its dispatch lease, process, session, and dirty work."""
    origin = value['origin']
    sid = value['handoff']['story_id']
    lease = cleanup_lease(origin['cwd'], sid)
    socket = origin['tmux'].split(',')[0]
    require(socket == lease['tmux']['socket_path'], 'native hook socket differs from cleanup lease')
    pane = origin['tmux_pane']
    require(re.fullmatch(r'%[0-9]+', pane or ''), 'missing native hook pane')
    metadata = json.loads(tmux(socket, 'show-options', '-w', '-v', '-t', pane, OPTION))
    require(metadata.get('protocol_version') == 1, 'dispatch lacks continuation identity capability')
    require(metadata.get('provider') == value['provider']
            and metadata.get('session_id') == origin['session_id'], 'native session differs from dispatcher')
    sentinel = read_json(Path(lease['worktree_path']) / '.claude/dispatch-sentinel.json')
    require(sentinel.get('session_id') == origin['session_id'] and sentinel.get('story_id') == sid,
            'current SessionStart witness belongs to another session')
    result = metadata | {'lease': lease, 'socket': socket, 'pane': pane,
                         'turn_id': origin['turn_id'], 'mode': origin['collaboration_mode'],
                         'transcript_path': origin['transcript_path'], 'autonomy': True}
    require(result['mode'] in ('plan', 'default') and owner(result) == 'present',
            'native handoff requires a known collaboration mode and live owner')
    native_state(result)
    result['head'] = git(lease['worktree_path'], 'rev-parse', 'HEAD').decode().strip()
    result['fingerprint'] = fingerprint(lease['worktree_path'])
    result['dirty_status_base64'] = base64.b64encode(
        git(lease['worktree_path'], 'status', '--porcelain=v1', '-z')).decode('ascii')
    return {'ok': True, 'capture': result}


def observe(value):
    """Observe retained resources; this operation never injects input or restarts."""
    capture = value['capture']
    cwd = capture['lease']['worktree_path']
    require(cleanup_lease(cwd, value['story_id']) == capture['lease'], 'retained cleanup lease changed')
    presence = owner(capture)
    if presence == 'absent':
        return {'ok': True, 'phase': 'absent', 'detail': 'captured provider process is absent'}
    state = native_state(capture)
    fresh = capture | {'head': git(cwd, 'rev-parse', 'HEAD').decode().strip()}
    if 'receipt' in value:
        receipt = value['receipt']
        origin = receipt['origin']
        require(receipt['provider'] == capture['provider']
                and receipt['session_id'] == capture['session_id']
                and origin['transcript_path'] == capture['transcript_path']
                and Path(origin['cwd']).resolve() == Path(cwd).resolve()
                and origin['tmux_pane'] == capture['pane']
                and origin['tmux'].split(',')[0] == capture['socket'], 'foreign compaction receipt')
        rows = transcript(capture)
        if capture['provider'] == 'codex':
            turn = origin['turn_id']
            started = False
            compacted = False
            for row in rows:
                payload = row.get('payload', {})
                if row.get('type') == 'event_msg' and payload.get('type') == 'task_started':
                    started = payload.get('turn_id') == turn
                if row.get('type') == 'compacted' and started:
                    compacted = True
            require(compacted, 'receipt has no matching native compaction lifecycle')
        else:
            require(any(row.get('type') == 'system' and row.get('subtype') == 'compact_boundary'
                        and row.get('sessionId') == capture['session_id'] for row in rows),
                    'receipt has no native Claude compaction boundary')
        state = 'compacted'
    return {'ok': True, 'phase': state, 'capture': fresh,
            'detail': 'exact native session observed; native queue delivery is not inferred'}


def register(value):
    """Publish the dispatcher's session binding before it sends the story charter."""
    lease = cleanup_lease(value['cwd'], value['story_id'])
    require(value['socket'] == lease['tmux']['socket_path'], 'registration socket differs from lease')
    require(value['provider'] in ('codex', 'claude')
            and value['autonomy_mode'] in ('auto', 'full-auto'), 'unsupported registration settings')
    rows = [row for row in panes(value['socket']) if row[2] == value['story_id']]
    require(len(rows) == 1 and rows[0][0] == value['pane'] and rows[0][4] == '0',
            'registration requires one exact live story pane: ' + repr(rows))
    row = rows[0]
    require(value['provider'] in row[5].lower(), 'registration provider command differs')
    sentinel = read_json(Path(lease['worktree_path']) / '.claude/dispatch-sentinel.json')
    require(sentinel.get('story_id') == value['story_id']
            and isinstance(sentinel.get('session_id'), str) and sentinel['session_id']
            and isinstance(sentinel.get('transcript_path'), str)
            and Path(sentinel['transcript_path']).is_absolute(),
            'registration requires a native session and transcript witness')
    pid = int(row[3])
    started = process_start(pid)
    require(started is not None, 'registration provider exited')
    metadata = {key: value[key] for key in ('provider', 'model', 'effort', 'speed', 'autonomy_mode')}
    metadata.update(protocol_version=1, session_id=sentinel['session_id'],
                    transcript_path=sentinel['transcript_path'], pid=pid, started=started,
                    window=row[1], socket=value['socket'], pane=row[0])
    probe = metadata | {'lease': lease}
    # SessionStart can precede the first transcript record. Registration binds
    # identity only; accepting a handoff later requires actual native history.
    tmux(value['socket'], 'set-option', '-w', '-t', value['pane'], OPTION, json.dumps(metadata))
    require(owner(probe) == 'present', 'registration ownership changed while publishing')
    return {'ok': True, 'capture': metadata}


def resume_preflight(value):
    """Require retained resources and the exact dead pane before any replacement effect."""
    capture = value['capture']
    require(observe(value)['phase'] == 'absent', 'automatic resume requires proven absence')
    rows = [row for row in panes(capture['socket']) if row[0] == capture['pane']]
    require(len(rows) == 1 and rows[0][4] == '1',
            'missing pane requires explicit recovery; automatic window creation is not atomic')
    cwd = capture['lease']['worktree_path']
    require(git(cwd, 'rev-parse', 'HEAD').decode().strip() == capture['head']
            and fingerprint(cwd) == capture['fingerprint'],
            'retained Git evidence changed after handoff; inspect before retrying')
    return {'ok': True, 'capture': capture, 'phase': 'absent'}


def resume(value):
    """Recover only an absent provider through the helper's atomic no-k guarded path."""
    resume_preflight(value)
    capture = value['capture']
    lease = capture['lease']
    helper = Path(__file__).resolve().parents[1] / 'bin/story.sh'
    request_id = value.get('id', value.get('request_id'))
    require(isinstance(request_id, str) and re.fullmatch(r'[A-Za-z0-9_-]+', request_id),
            'missing durable request identity')
    with tempfile.NamedTemporaryFile(mode='w', prefix='story-continuation-', suffix='.json', dir='/tmp') as request:
        json.dump(value, request)
        request.flush()
        argv = ['bash', str(helper), '--project', lease['project_slug'], 'dispatch',
                value['story_id'], '--auto', '--resume', '--require-absent',
                '--continuation-file=' + request.name, '--agent=' + capture['provider']]
        if capture['autonomy_mode'] == 'full-auto':
            argv.append('--full-auto')
        for key in ('model', 'effort', 'speed'):
            require(isinstance(capture.get(key), str), 'missing captured setting: ' + key)
            # An empty effort is the provider's explicit default selection in
            # existing dispatch output; passing --effort= is invalid grammar.
            if capture[key]:
                argv.append('--' + key + '=' + capture[key])
        env = dict(os.environ)
        env['TMUX'] = capture['socket'] + ',0,0'
        env.pop('TMUX_PANE', None)
        env['STORY_PROMPT_EXTRA'] = (
            f'Receive durable continuation {request_id}. Read story continuation status '
            f'{value["story_id"]} --json and all current story comments; reconcile every '
            'pending correction and landed prerequisite. Repeat the complete obviation review. '
            'Preserve approved scope and existing dirty work. In Plan mode review and plan '
            'only; after ordinary implementation approval, acknowledge this request using '
            'story continuation ack with current reviewed sequence, HEAD, provider and this '
            'new root session identity before changing work. Unknown context alone is not a '
            'dependency or a reason to defer already assigned work. Record corrections that '
            'must fence submission on the story; acknowledgement cannot observe the native queue.')
        answer = json.loads(command(argv, cwd=lease['worktree_path'], timeout=120, env=env))
    require(answer.get('ok') is True and answer.get('window_reused') is True
            and answer.get('worktree_reused') is True and answer.get('branch_reused') is True,
            'guarded dispatch did not confirm retained resources')
    metadata = json.loads(tmux(capture['socket'], 'show-options', '-w', '-v', '-t', capture['pane'], OPTION))
    fresh = capture | metadata | {'mode': 'plan', 'turn_id': 'receiving-' + request_id}
    require(fresh['session_id'] != capture['session_id'] and owner(fresh) == 'present',
            'guarded dispatch did not establish a new receiving session')
    return {'ok': True, 'phase': 'submitted', 'capture': fresh,
            'detail': 'retained dead pane resumed in Plan mode; explicit receiving acknowledgement required'}


def main():
    """Expose the daemon's private bounded JSON transport."""
    try:
        raw = sys.stdin.read(MAX_BYTES + 1)
        require(len(raw) <= MAX_BYTES, 'continuation input exceeds 64 MiB')
        value = json.loads(raw)
        operation = sys.argv[1]
        if operation == 'capture':
            answer = capture_request(value)
        elif operation == 'observe':
            answer = observe(value)
        elif operation == 'register':
            answer = register(value)
        elif operation == 'resume-preflight':
            answer = resume_preflight(value)
        elif operation == 'resume':
            answer = resume(value)
        else:
            raise RuntimeError('unsupported continuation operation; live compaction and input injection are prohibited')
    except (OSError, ValueError, KeyError, TypeError, RuntimeError, subprocess.TimeoutExpired) as exc:
        answer = {'ok': False, 'detail': str(exc)}
    print(json.dumps(answer))


if __name__ == '__main__':
    main()
