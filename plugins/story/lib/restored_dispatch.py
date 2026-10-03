"""Join revivify snapshot lineage to a retained StoryHook dispatch."""

import copy
import json
import os
from pathlib import Path

from process_observation import descendants, observe_process

IDENTITY = '@storyhook-identity-v1'


def require(condition, detail):
    """Missing or conflicting proof is a refusal, never startup authority."""
    if not condition:
        raise RuntimeError('restored dispatch: ' + detail)


def source_dispatch(target, evidence, lease, common, window):
    """Select one independently proven source dispatch; do not mutate any binding.

    A source window naming this story but lacking identity is damaged evidence,
    not a missing agent. Only a genuinely absent source returns None.
    """
    candidates = []
    for uuid, saved in evidence['panes'].items():
        raw = saved['pane']['options'].get(IDENTITY)
        try:
            identity = json.loads(raw) if raw else None
        except (TypeError, ValueError) as error:
            if saved['window']['name'] == window:
                raise RuntimeError('restored dispatch: invalid source identity') from error
            continue
        if saved['window']['name'] == window or isinstance(identity, dict) and identity.get('story') == lease['story_id']:
            candidates.append((uuid, saved, identity))
    if not candidates:
        return None
    require(len(candidates) == 1, 'multiple snapshot panes claim this story')
    uuid, saved, identity = candidates[0]
    require(isinstance(identity, dict), 'story snapshot lacks registered identity')
    require(type(identity.get('version')) is int and identity['version'] == 1,
            'unsupported source identity version')
    require(saved['window']['name'] == window, 'source story window was renamed')
    for key, expected in dict(project=lease['project_slug'], story=lease['story_id'],
                              common=common, worktree=lease['worktree_path']).items():
        require(identity.get(key) == expected, 'source identity differs: ' + key)
    provider = identity.get('provider')
    require(provider in ('claude', 'codex') and saved['window']['options'].get('@storyhook-agent') == provider,
            'source provider options disagree')
    generation = evidence['source_generation']
    history = [*evidence['source_history'], generation] if generation else []
    sockets = {target['socket']}
    sockets.update(str(Path(target['socket']).parent / ('.rv-' + item) / 's') for item in history)
    require(identity.get('socket') in sockets and lease['tmux']['socket_path'] == identity['socket'],
            'source pane and cleanup lease do not share an authorized socket')
    provenance = lease['tmux'].get('revivify')
    if provenance is not None:
        require(isinstance(provenance, dict) and provenance.get('logical_socket') == target['socket']
                and provenance.get('origin_generation') in history,
                'cleanup origin is outside the restored lineage')
    agent = saved['pane'].get('agent')
    require(isinstance(agent, dict) and agent.get('kind') == provider,
            'snapshot does not identify this provider')
    session = agent.get('session_id')
    require(isinstance(session, str) and bool(session), 'snapshot lacks an exact conversation')
    require(agent.get('resume_cwd') == lease['worktree_path'] and saved['pane']['cwd'] == lease['worktree_path'],
            'snapshot provider worktree differs')
    process = identity.get('restored', {}).get('provider_process', identity.get('process'))
    require(isinstance(process, dict) and type(process.get('pid')) is int
            and process['pid'] > 1 and agent.get('old_pid') == process['pid']
            and isinstance(process.get('start'), str) and bool(process['start'])
            and isinstance(process.get('executable'), str) and Path(process['executable']).is_absolute(),
            'snapshot provider differs from the registered process')
    rebound = copy.deepcopy(lease)
    rebound['tmux']['socket_path'] = target['endpoint']
    if provenance is None and generation is not None:
        rebound['tmux']['revivify'] = dict(logical_socket=target['socket'], origin_generation=generation)
    return dict(uuid=uuid, saved=saved, identity=identity, provider=provider,
                provider_process=process, session_id=session, lease=rebound)


def _provider_args(row, source, launch):
    """Recognize executable evidence, including a provider's Node entry point."""
    executable = os.path.realpath(row['process']['executable'])
    prior = os.path.realpath(source['provider_process']['executable'])
    launch = os.path.realpath(launch) if launch else None
    argv = row['argv']
    if not argv:
        return None
    if Path(executable).name in ('node', 'nodejs'):
        if executable != prior or len(argv) < 2 or not launch or os.path.realpath(argv[1]) != launch:
            return None
        return argv[2:]
    if executable in (prior, launch):
        return argv[1:]
    return None


def _same_resume(provider, args, session):
    """Accept RV-10's exact resume forms, excluding a fork or competing session."""
    if provider == 'codex':
        return args[:2] == ['resume', session] and not any(
            arg in ('--fork', '--last', '--all') or arg.startswith('--fork=') for arg in args[2:])
    if any(arg in ('--fork-session', '--session-id', '--continue', '-c')
           or arg.startswith(('--session-id=', '--fork-session=')) for arg in args):
        return False
    sessions = []
    for index, arg in enumerate(args):
        if arg in ('--resume', '-r'):
            sessions.append(args[index + 1] if index + 1 < len(args) else None)
        elif arg.startswith('--resume='):
            sessions.append(arg.partition('=')[2])
    return sessions == [session]


def live_provider(source, root, parents, launch, observer=observe_process):
    """Prove one resumed provider below a captured pane process, then recheck it.

    Callers obtain a fresh process table for each proof. All processes on the
    provider's path back to the pane must retain their observed incarnations.
    """
    tree = descendants(parents, root['pid'])
    require(bool(tree), 'pane process is absent from the current process table')
    observed, candidates = {}, []
    for pid in sorted(tree):
        try:
            row = observer(pid)
        except ProcessLookupError:
            continue  # A departed child grants no evidence.
        require(row['process']['pid'] == pid and row['parent'] == parents[pid],
                'process ancestry changed during inventory')
        observed[pid] = row
        args = _provider_args(row, source, launch)
        if args is not None:
            candidates.append((row, args))
    require(root['pid'] in observed and observed[root['pid']]['process'] == root,
            'pane process incarnation changed')
    require(len(candidates) == 1, 'expected exactly one provider process in the restored pane')
    candidate, args = candidates[0]
    require(candidate['cwd'] == source['lease']['worktree_path'], 'live provider worktree differs')
    require(_same_resume(source['provider'], args, source['session_id']),
            'live provider does not resume the exact snapshot conversation')
    pid = candidate['process']['pid']
    while True:
        require(pid in observed and observer(pid) == observed[pid],
                'provider or ancestor changed during restoration proof')
        if pid == root['pid']:
            break
        pid = observed[pid]['parent']
    return candidate
