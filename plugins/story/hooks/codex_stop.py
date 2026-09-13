"""Codex autonomous plan continuation at the provider Stop boundary."""

import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import sys

from codex_classifier import classify, run_process
from plan_request import parse_plan_request
from session_handoff import handle_stop

MAX_MESSAGE = 64 * 1024
MAX_TRANSCRIPT_TAIL = 4 * 1024 * 1024
IDENTITY = re.compile(r'^[A-Za-z0-9][A-Za-z0-9_-]{0,127}$')
STORY_ID = re.compile(r'^[A-Za-z][A-Za-z0-9]*-[0-9]+$')


def diagnostic(reason):
    """Expose a failure without manufacturing permission or continuing a turn."""
    return {'systemMessage': 'StoryHook plan approval unavailable: ' + str(reason)
            + '. No approval was sent; the native plan-menu watcher remains available.'}


def object_value(value, name):
    """Validate external object shapes before accessing provider or CLI fields."""
    if not isinstance(value, dict):
        raise RuntimeError(f'{name} is not a JSON object')
    return value


def matching_cwd(value, cwd):
    """Require an explicit absolute provider cwd, never an inherited default."""
    return isinstance(value, str) and os.path.isabs(value) and Path(value).resolve() == cwd


def transcript_mode(payload):
    """Bind the Stop event to the current root session, cwd, and mode."""
    path = Path(payload['transcript_path'])
    with path.open('rb') as transcript:
        meta = json.loads(transcript.readline(MAX_TRANSCRIPT_TAIL))
        transcript.seek(0, os.SEEK_END)
        start = max(0, transcript.tell() - MAX_TRANSCRIPT_TAIL)
        transcript.seek(start)
        if start:
            transcript.readline()
        lines = transcript.read(MAX_TRANSCRIPT_TAIL).splitlines()
    meta = object_value(meta, 'transcript header')
    meta = object_value(meta.get('payload', {}), 'session metadata') if meta.get('type') == 'session_meta' else {}
    cwd = Path(payload['cwd']).resolve()
    if meta.get('id') != payload['session_id'] or not matching_cwd(meta.get('cwd'), cwd):
        raise RuntimeError('transcript session or cwd does not match Stop')
    if meta.get('source') not in ('cli', 'exec', 'vscode'):
        raise RuntimeError('transcript is not a root Codex session')
    for line in reversed(lines):
        event = object_value(json.loads(line), 'transcript event')
        if event.get('type') != 'turn_context':
            continue
        context = object_value(event.get('payload', {}), 'turn context')
        if context.get('turn_id') != payload['turn_id'] or not matching_cwd(context.get('cwd'), cwd):
            raise RuntimeError('Stop is stale or belongs to another working directory')
        mode = object_value(context.get('collaboration_mode', {}), 'collaboration mode').get('mode')
        if mode not in ('default', 'plan'):
            raise RuntimeError('unknown Codex collaboration mode')
        return mode
    raise RuntimeError('current turn context is absent from bounded transcript tail')


def story_json(cwd, *args):
    """Ask the authoritative CLI with an inner deadline and outer process bound."""
    return object_value(json.loads(run_process(['story', '--deadline', '2', *args, '--json'],
                                              timeout=3, cwd=cwd)), 'story response')


def eligible(cwd, story_id):
    """Require an active, unblocked story in the hook's own project."""
    response = story_json(cwd, 'session-eligibility', story_id)
    if response.get('result') != 'ok':
        raise RuntimeError('session eligibility lookup failed: ' + str(response.get('error', 'no result')))
    answer = response.get('session_eligibility')
    if (not isinstance(answer, dict)
            or type(answer.get('schema_version')) is not int or answer['schema_version'] != 1
            or answer.get('story_id') != story_id
            or type(answer.get('eligible')) is not bool
            or answer.get('reason') not in ('eligible', 'closed', 'inactive', 'awaiting', 'blocked')
            or answer['eligible'] != (answer['reason'] == 'eligible')):
        raise RuntimeError('invalid session eligibility response schema or story identity')
    return answer['eligible']


def handle(payload, env, classify=classify, eligible=eligible):
    """Return at most one plan continuation per root autonomous session."""
    marker = env.get('STORYHOOK_FULL_AUTO') or env.get('STORYHOOK_AUTO') or ''
    if not STORY_ID.fullmatch(marker) or not isinstance(payload, dict):
        return {}
    if (payload.get('hook_event_name') != 'Stop' or payload.get('agent_id')
            or payload.get('agent_type') or type(payload.get('stop_hook_active')) is not bool):
        return {}
    if any(not isinstance(payload.get(k), str) or not IDENTITY.fullmatch(payload[k])
           for k in ('session_id', 'turn_id')):
        return {}
    if any(not isinstance(payload.get(k), str) or not os.path.isabs(payload[k])
           for k in ('cwd', 'transcript_path')):
        return {}
    message = payload.get('last_assistant_message')
    if not isinstance(message, str) or not message.strip():
        return {}
    if len(message.encode()) > MAX_MESSAGE:
        return diagnostic('assistant message exceeds 64 KiB; it was not truncated')
    if re.fullmatch(r'\s*<proposed_plan>[\s\S]*</proposed_plan>\s*', message):
        return {}
    try:
        administrative = handle_stop(payload, env, 'codex', run_process)
        if administrative is not None:
            return administrative
        if payload['stop_hook_active']:
            return {}
        mode = transcript_mode(payload)
        plan = parse_plan_request(message, marker)
        # A sidecar to the provider-owned transcript survives worktree cleanup,
        # requires no repository files, and shares the transcript's lifetime.
        journal = payload['transcript_path'] + '.storyhook-plan-approval'
        fd = os.open(journal, os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600)
        with os.fdopen(fd, 'r+') as record:
            info = os.fstat(record.fileno())
            if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid():
                raise RuntimeError('approval journal is not an owned regular file')
            try:
                fcntl.flock(record, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError:
                return {}
            if record.read(1):
                return {}
            if not eligible(payload['cwd'], marker):
                return {}
            if plan is None:
                answer = classify(message)
                if (not isinstance(answer, dict) or set(answer) != {'decision', 'evidence'}
                        or answer['decision'] not in ('approve_plan', 'other', 'uncertain')
                        or not isinstance(answer['evidence'], str)):
                    raise RuntimeError('invalid Luna classification schema')
                if answer['decision'] == 'other':
                    return {}
                if answer['decision'] == 'uncertain':
                    return diagnostic('Luna could not identify an unambiguous implementation-plan approval')
                evidence = answer['evidence'].strip()
                if not evidence or evidence not in message:
                    raise RuntimeError('Luna approval evidence is absent from the assistant message')
            # Classification may have taken twenty seconds. Revalidate both the
            # provider turn and the story before issuing authority to continue.
            if transcript_mode(payload) != mode or not eligible(payload['cwd'], marker):
                return {}
            record.seek(0)
            json.dump({'session_id': payload['session_id'], 'turn_id': payload['turn_id'],
                       'story_id': marker, 'message_sha256': hashlib.sha256(message.encode()).hexdigest(),
                       'source': 'structured' if plan is not None else 'prose',
                       'plan_sha256': hashlib.sha256((plan if plan is not None else message).encode()).hexdigest(),
                       'mode': mode}, record)
            record.flush()
            os.fsync(record.fileno())
        if mode == 'plan':
            reason = ('StoryHook has identified your completed implementation plan. '
                      'Do not implement or write files while in Plan mode. Present that same plan '
                      'inside <proposed_plan> tags so the native plan-review menu can open; '
                      'StoryHook will approve it automatically. Keep posting the entire approved '
                      f'plan verbatim as a comment on {marker} as its first implementation step.')
        else:
            reason = (f'Autonomous StoryHook session {marker}: your implementation plan is approved '
                      'automatically. First post the entire approved plan verbatim as a comment on '
                      f'{marker}, then implement it under the original task instructions. '
                      'This approves only the implementation plan, not additional operational '
                      'permissions, scope changes, or choices left unresolved in the plan.')
        if plan is not None:
            reason += (' The approved content is the decoded plan string in your JSON request; '
                       'preserve its text verbatim, not the JSON envelope. Schema validation '
                       'records your readiness declaration, not semantic proof of completeness. '
                       'This grants no additional operational permissions: network, credentials, '
                       'deletion, deployment, scope changes, and unresolved choices remain '
                       'subject to the original instructions and independent permission controls.')
        return {'decision': 'block', 'reason': reason}
    except (OSError, ValueError, KeyError, TypeError, RuntimeError) as exc:
        return diagnostic(exc)


def main():
    """Drain the provider payload and emit only the Stop protocol object."""
    raw = sys.stdin.read(MAX_TRANSCRIPT_TAIL + 1)
    if len(raw) > MAX_TRANSCRIPT_TAIL:
        print(json.dumps(diagnostic('Stop payload exceeds input limit')))
        return
    try:
        payload = json.loads(raw)
    except ValueError:
        payload = None
    print(json.dumps(handle(payload, os.environ)))


if __name__ == '__main__':
    main()
