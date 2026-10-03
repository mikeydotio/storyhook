"""Rebind a restored dispatch under workspace exclusion without starting an agent.

Readiness and proof happen before publication. Every publication is repeatable:
local fields may contain only the immutable snapshot value or its derived new
value. Native callers retain their own lane/story/continuation revision guards.
"""
import copy
import json
import os
from pathlib import Path
import shutil
import sys
import tempfile
import probe_budget

import agent_identity
import continuation_runtime as continuation
import tmux_client
from process_observation import observe_process
from restored_dispatch import source_dispatch, restored_launch, live_provider, require
from tmux_target import restore_evidence
from workspace_ownership import require_workspace

IDENTITY = agent_identity.OPTION
CONTINUATION = continuation.OPTION


def tmux(socket, *args):
    """Use the already proven private endpoint for every local operation."""
    return agent_identity.tmux(socket, *args)


def read_option(socket, pane, key, window=False):
    """Read a local JSON option; absent metadata never confers authority."""
    scope = '-w' if window else '-p'
    options = tmux(socket, 'show-options', scope, '-t', pane)
    if not any(line.startswith(key + ' ') for line in options.splitlines()):
        return None
    return json.loads(tmux(socket, 'show-options', scope, '-v', '-t', pane, key))


def parents():
    """Take a fresh kernel ancestry census within the shared probe budget."""
    rows = agent_identity.run('ps', '-axo', 'pid=,ppid=').splitlines()
    result = {}
    for row in rows:
        parts = row.split()
        require(len(parts) == 2 and all(part.isdecimal() for part in parts), 'invalid process census')
        pid, parent = map(int, parts)
        require(pid not in result, 'duplicate process in census')
        result[pid] = parent
    return result


def validate_provider(record):
    """Revalidate the exact resumed child as well as the pane's kernel identity."""
    restored = record['restored']
    source = dict(provider=record['provider'], provider_process=restored['provider_process'],
                  session_id=restored['session_id'], lease=dict(worktree_path=record['worktree']))
    row = live_provider(source, record['process'], parents(), shutil.which(record['provider']))
    require(row['process'] == restored['provider_process'], 'restored provider was replaced')


def propose(value):
    """Read current readiness, immutable lineage, Git and native session proof."""
    lease = value['lease']
    client = tmux_client.client(lease['tmux']['socket_path'], ensure=True)
    target = client.target
    if not target['protected']:
        return None
    evidence = restore_evidence(target, os.environ)
    socket = target['endpoint']
    cwd = lease['worktree_path']
    common = continuation.git(cwd, 'rev-parse', '--path-format=absolute', '--git-common-dir').decode().strip()
    private = continuation.git(cwd, 'rev-parse', '--absolute-git-dir').decode().strip()
    marker = continuation.cleanup_lease(cwd, lease['story_id'])
    window = value.get('window') or lease['story_id']
    if not value.get('window') and evidence:
        named = []
        for saved in evidence['panes'].values():
            raw = saved['pane']['options'].get(IDENTITY)
            if raw:
                identity = json.loads(raw)
                if identity.get('story') == lease['story_id'] and identity.get('project') == lease['project_slug']:
                    named.append(saved['window']['name'])
        require(len(named) <= 1, 'multiple restored dispatch windows claim the story')
        if named:
            window = named[0]
    if lease['tmux']['socket_path'] == socket:
        current = [p for p in agent_identity.panes(socket).values() if p['window'] == window]
        if len(current) == 1:
            registered = agent_identity.read_record(current[0])
            if registered is not None and 'restored' not in registered and registered.get('socket') == socket:
                require(marker == lease, 'current dispatch cleanup lease changed')
                agent_identity.validate(registered)
                return None
    if evidence is None:
        return live_adoption(value, target, common, private, marker, window)
    # A partially published native lane may already name the successor. Select
    # the source through its preserved identity, never through numeric pane IDs.
    old = copy.deepcopy(lease)
    candidates = [saved for saved in evidence['panes'].values() if saved['window']['name'] == window]
    if len(candidates) == 1:
        original = json.loads(candidates[0]['pane']['options'].get(IDENTITY, 'null'))
        if isinstance(original, dict):
            old['tmux']['socket_path'] = original.get('socket')
    source = source_dispatch(target, evidence, old, common, window)
    if source is None:
        # An old numeric binding is not proof of absence on a new generation.
        require(lease['tmux']['socket_path'] == socket, 'no restored source for the retained dispatch')
        return None
    new = source['lease']
    require(lease in (old, new) and marker in (old, new), 'retained cleanup lease changed')
    inventory = agent_identity.panes(socket)
    matching = [p for p in inventory.values() if p['window'] == window]
    require(len(matching) == 1 and matching[0]['pane'] == source['saved']['pane_id'],
            'restored story has ambiguous or missing panes')
    pane = matching[0]
    uuid_rows = tmux(socket, 'list-panes', '-a', '-F', '#{pane_id}\t#{@revivify-uuid}').splitlines()
    holders = set()
    for row in uuid_rows:
        parts = row.split('\t')
        require(len(parts) == 2, 'invalid live UUID inventory')
        if parts[1] == source['uuid']:
            holders.add(parts[0])
    require(holders == {pane['pane']}, 'restored UUID has duplicate or missing live owners')
    require(tmux(socket, 'show-options', '-p', '-v', '-t', pane['pane'], '@revivify-uuid') == source['uuid'],
            'live pane UUID differs from the restore receipt')
    restored_launch(target, source['uuid'], pane['launch'])
    ctx = dict(project=lease['project_slug'], story=lease['story_id'], common=common)
    identity = agent_identity.observe(ctx, pane, source['provider'])
    require(identity['worktree'] == cwd, 'restored pane left the registered worktree')
    provider = live_provider(source, identity['process'], parents(), shutil.which(source['provider']))
    identity['restored'] = dict(generation=target['generation'], snapshot_id=evidence['snapshot_id'],
                                uuid=source['uuid'], session_id=source['session_id'],
                                provider_process=provider['process'])
    metadata = source['saved']['window']['options'].get(CONTINUATION)
    metadata = json.loads(metadata) if metadata else None
    return finish_proposal(target, old, new, common, private, pane, source['identity'], identity,
                           metadata, source['session_id'])


def live_adoption(value, target, common, private, marker, window):
    """Move a live public alias only when the original process remains unchanged."""
    lease = value['lease']
    if lease['tmux']['socket_path'] == target['endpoint']:
        return None
    require(lease['tmux']['socket_path'] == target['socket'], 'missing restore proof for predecessor generation')
    inventory = [p for p in agent_identity.panes(target['endpoint']).values() if p['window'] == window]
    require(len(inventory) == 1, 'live adoption has no unique story pane')
    pane = inventory[0]
    old = read_option(target['endpoint'], pane['pane'], IDENTITY)
    require(isinstance(old, dict) and old.get('socket') in (target['socket'], target['endpoint']), 'missing public pane identity')
    old = dict(old, socket=target['socket'])
    ctx = dict(project=lease['project_slug'], story=lease['story_id'], common=common)
    identity = agent_identity.observe(ctx, pane, old['provider'])
    require(dict(identity, socket=old['socket']) == old and old['worktree'] == lease['worktree_path'],
            'live adoption changed the original process or dispatch')
    new = copy.deepcopy(lease)
    new['tmux']['socket_path'] = target['endpoint']
    require(marker in (lease, new), 'live cleanup lease changed')
    metadata = read_option(target['endpoint'], pane['pane'], CONTINUATION, window=True)
    # Live alias adoption changes no kernel or conversation identity.
    if metadata is not None:
        require(metadata.get('socket') in (target['socket'], target['endpoint']), 'live continuation socket changed')
        metadata = dict(metadata, socket=target['socket'])
    sentinel = continuation.read_json(Path(lease['worktree_path']) / '.claude/dispatch-sentinel.json')
    sid = metadata.get('session_id') if metadata else sentinel.get('session_id')
    return finish_proposal(target, lease, new, common, private, pane, old, identity, metadata, sid)


def finish_proposal(target, old, new, common, private, pane, before, identity, metadata, sid):
    """Corroborate SessionStart and transcript identity and derive physical fields."""
    socket = target['endpoint']
    provider = identity['provider']
    require(tmux(socket, 'show-options', '-w', '-v', '-t', pane['pane'], '@storyhook-agent') == provider,
            'live provider option changed')
    sentinel = continuation.read_json(Path(new['worktree_path']) / '.claude/dispatch-sentinel.json')
    require(sentinel.get('story_id') == new['story_id'] and sentinel.get('session_id') == sid,
            'SessionStart does not corroborate the retained conversation')
    transcript = sentinel.get('transcript_path')
    capture = dict(provider=provider, session_id=sid, transcript_path=transcript, lease=new)
    continuation.native_state(capture)
    window_id = tmux(socket, 'display-message', '-p', '-t', pane['pane'], '#{window_id}')
    rebound = None
    if metadata is not None:
        require(isinstance(metadata, dict), 'invalid continuation metadata')
        require(metadata.get('protocol_version') == 1 and metadata.get('provider') == provider
                and metadata.get('session_id') == sid and metadata.get('transcript_path') == transcript
                and metadata.get('socket') == old['tmux']['socket_path']
                and metadata.get('pane') == before['pane'] and metadata.get('pid') == before['process']['pid'],
                'snapshot continuation metadata conflicts with dispatch')
        rebound = dict(metadata, socket=socket, pane=pane['pane'], pid=identity['process']['pid'],
                       started=continuation.process_start(identity['process']['pid']), window=window_id)
        if 'restored' in identity:
            rebound['restored'] = identity['restored']
    return dict(lease_before=old, lease=new, marker=str(Path(private) / continuation.MARKER),
                common=common, story=new['story_id'], pane=pane['pane'], window=window_id,
                identity_before=before, identity=identity, metadata_before=metadata, metadata=rebound)


def publish(value, expected):
    """Reprove under exclusion, then publish only old-or-derived fields."""
    require_workspace(expected['common'], expected['story'])
    fresh = propose(value)
    require(fresh == expected, 'restoration proof changed before publication')
    socket, pane = fresh['identity']['socket'], fresh['pane']
    marker = Path(fresh['marker'])
    current_lease = continuation.read_json(marker)
    current_identity = read_option(socket, pane, IDENTITY)
    current_metadata = read_option(socket, pane, CONTINUATION, window=True)
    for current, old, new in ((current_lease, fresh['lease_before'], fresh['lease']),
                              (current_identity, fresh['identity_before'], fresh['identity']),
                              (current_metadata, fresh['metadata_before'], fresh['metadata'])):
        require(current in (old, new), 'local restoration binding changed')
    for key, scope, current, new in ((IDENTITY, '-p', current_identity, fresh['identity']),
                                    (CONTINUATION, '-w', current_metadata, fresh['metadata'])):
        if current != new:
            tmux(socket, 'set-option', scope, '-t', pane, key, json.dumps(new, sort_keys=True, separators=(',', ':')))
            require(read_option(socket, pane, key, window=scope == '-w') == new, 'binding readback changed')
    if current_lease != fresh['lease']:
        with tempfile.NamedTemporaryFile(mode='w', dir=marker.parent, prefix=marker.name + '.', delete=False) as stream:
            temporary = Path(stream.name)
            try:
                json.dump(fresh['lease'], stream, sort_keys=True)
                stream.flush()
                os.fsync(stream.fileno())
                os.replace(temporary, marker)
            finally:
                temporary.unlink(missing_ok=True)
    return fresh


def main():
    """Expose bounded proposal and guarded publication to native reconciliation."""
    try:
        value = json.loads(sys.stdin.read(continuation.MAX_BYTES + 1))
        with probe_budget.operation(float(os.environ.get('STORY_RESTORATION_BUDGET', '30'))), tmux_client.operation():
            if sys.argv[1] == 'propose':
                result = propose(value)
            elif sys.argv[1] in ('publish', 'rearm'):
                result = publish(value, value['proposal'])
                if sys.argv[1] == 'rearm':
                    from approval_tmux import schedule
                    schedule(result['identity'], result['metadata'])
            else:
                raise RuntimeError('unsupported restoration operation')
        print(json.dumps(dict(ok=True, proposal=result)))
    except (OSError, ValueError, KeyError, TypeError, RuntimeError, agent_identity.IdentityError) as error:
        print(json.dumps(dict(ok=False, detail=str(error))))
        return 1
    return 0


if __name__ == '__main__':
    sys.exit(main())
