"""Strict administrative Stop handoffs, independent of implementation approval."""

import json
import os
from pathlib import Path
import re
import sys

from codex_classifier import run_process
from plan_request import unique_object

REQUEST_TYPE = 'storyhook.session-handoff'
MAX_MESSAGE = 64 * 1024
MAX_TRANSCRIPT = 4 * 1024 * 1024
IDENTITY = re.compile(r'^[A-Za-z0-9][A-Za-z0-9_-]{0,127}$')
STORY_ID = re.compile(r'^[A-Za-z][A-Za-z0-9]*-[0-9]+$')


def diagnostic(reason):
    """Expose administrative failure without granting permission or retrying effects."""
    return {'systemMessage': 'StoryHook session handoff unavailable: ' + str(reason)
            + '. No implementation approval was issued; inspect continuation status.'}


def parse_request(message, story_id):
    """Decode the complete handoff envelope; invalid candidates never become prose."""
    normalized = re.sub(r'\\u([0-9a-fA-F]{4})', lambda match: chr(int(match[1], 16)), message)
    if (REQUEST_TYPE not in normalized
            and not all(re.search('"' + key + r'"\s*:', normalized)
                        for key in ('story_id', 'kind', 'evidence'))):
        return None
    try:
        value = json.loads(message, object_pairs_hook=unique_object)
        if (not isinstance(value, dict)
                or set(value) != {'type', 'version', 'story_id', 'kind', 'evidence'}
                or value['type'] != REQUEST_TYPE
                or type(value['version']) is not int or value['version'] != 1
                or value['story_id'] != story_id
                or value['kind'] not in ('context', 'obviation-review')
                or not isinstance(value['evidence'], dict)):
            raise ValueError('invalid schema, version, kind, or story identity')
        evidence = value['evidence']
        fields = ('context', 'outstanding_work') if value['kind'] == 'context' else ('context',)
        for field in fields:
            if not isinstance(evidence.get(field), str) or not evidence[field].strip():
                raise ValueError('missing nonempty evidence.' + field)
        if value['kind'] == 'obviation-review':
            candidates = evidence.get('candidates')
            original = evidence.get('original_state')
            if (not isinstance(candidates, list) or not candidates
                    or any(not isinstance(candidate, str) or not STORY_ID.fullmatch(candidate)
                           or candidate == story_id for candidate in candidates)
                    or len(set(candidates)) != len(candidates)
                    or not isinstance(original, str)
                    or not re.fullmatch(r'[a-z][a-z0-9-]*', original)):
                raise ValueError('invalid obviation candidates or original_state')
        json.dumps(value, ensure_ascii=False).encode('utf-8')
        return value
    except (ValueError, RecursionError) as exc:
        raise ValueError('invalid session handoff request: ' + str(exc)) from exc


def transcript_tail(path):
    """Read a bounded complete-record tail from a provider transcript."""
    with Path(path).open('rb') as stream:
        stream.seek(0, os.SEEK_END)
        start = max(0, stream.tell() - MAX_TRANSCRIPT)
        stream.seek(start)
        if start:
            stream.readline()
        lines = stream.read(MAX_TRANSCRIPT).splitlines()
    return [json.loads(line, object_pairs_hook=unique_object) for line in lines if line.strip()]


def claude_origin(payload):
    """Bind Claude Stop to the latest root conversational record and mode."""
    if payload.get('permission_mode') not in (
            'default', 'plan', 'acceptEdits', 'auto', 'dontAsk', 'bypassPermissions'):
        raise ValueError('unknown Claude permission mode')
    for event in reversed(transcript_tail(payload['transcript_path'])):
        if not isinstance(event, dict):
            raise ValueError('invalid Claude transcript event')
        if event.get('type') not in ('assistant', 'user'):
            continue
        if (event.get('type') != 'assistant'
                or event.get('sessionId') != payload['session_id']
                or event.get('isSidechain') is not False
                or not isinstance(event.get('cwd'), str)
                or not os.path.isabs(event['cwd'])
                or Path(event['cwd']).resolve() != Path(payload['cwd']).resolve()
                or not isinstance(event.get('uuid'), str)
                or not IDENTITY.fullmatch(event['uuid'])):
            raise ValueError('Claude Stop does not match the latest root assistant record')
        message = event.get('message')
        content = message.get('content') if isinstance(message, dict) else None
        if not isinstance(content, list):
            raise ValueError('invalid Claude assistant content')
        texts = [block.get('text') for block in content
                 if isinstance(block, dict) and block.get('type') == 'text']
        if (not texts or any(not isinstance(text, str) for text in texts)
                or '\n'.join(texts) != payload['last_assistant_message']):
            raise ValueError('Claude Stop message differs from the latest transcript record')
        return payload | {'turn_id': event['uuid'], 'collaboration_mode':
                          'plan' if payload['permission_mode'] == 'plan' else 'default'}
    raise ValueError('Claude transcript has no root assistant record')


def handle_stop(payload, env, provider, process=run_process):
    """Forward one validated administrative intent, or return None for unrelated input."""
    marker = env.get('STORYHOOK_FULL_AUTO') or env.get('STORYHOOK_AUTO') or ''
    if (not STORY_ID.fullmatch(marker) or not isinstance(payload, dict)
            or payload.get('hook_event_name') != 'Stop'
            or payload.get('agent_id') or payload.get('agent_type')
            or type(payload.get('stop_hook_active')) is not bool
            or provider not in ('codex', 'claude')
            or (provider == 'claude' and 'turn_id' in payload)):
        return None
    message = payload.get('last_assistant_message')
    if not isinstance(message, str) or not message.strip():
        return None
    try:
        if len(message.encode('utf-8')) > MAX_MESSAGE:
            raise ValueError('assistant message exceeds 64 KiB')
        request = parse_request(message, marker)
        if request is None:
            return None
        # Native feedback can discover a review hold in its continued turn;
        # only context delivery recurses, whereas administrative holds never do.
        if payload['stop_hook_active'] and request['kind'] == 'context':
            return None
        if (not isinstance(payload.get('session_id'), str)
                or not IDENTITY.fullmatch(payload['session_id'])
                or any(not isinstance(payload.get(key), str)
                       or not os.path.isabs(payload[key])
                       for key in ('cwd', 'transcript_path'))):
            raise ValueError('missing absolute paths or provider session identity')
        if provider == 'codex':
            from codex_stop import transcript_mode
            origin = payload | {'collaboration_mode': transcript_mode(payload)}
        else:
            origin = claude_origin(payload)
        origin = origin | native_origin(env)
        answer = json.loads(process(
            ['story', '--deadline', '2', 'continuation', 'request', marker, '--stdin', '--json'],
            timeout=3, cwd=payload['cwd'], text=json.dumps({
                'handoff': request, 'origin': origin, 'provider': provider})),
            object_pairs_hook=unique_object)
        if not isinstance(answer, dict) or answer.get('result') != 'ok':
            raise ValueError('supervisor refused handoff: ' + str(answer))
        if request['kind'] == 'obviation-review':
            return {}
        if type(answer.get('native_feedback')) is not bool:
            raise ValueError('supervisor omitted an atomic native feedback receipt')
        if not answer['native_feedback']:
            return {}
        record = answer.get('continuation')
        if (not isinstance(record, dict) or record.get('story_id') != marker
                or not isinstance(record.get('id'), str) or not IDENTITY.fullmatch(record['id'])
                or record.get('status') != 'awaiting-ack'
                or record.get('phase') != 'native-continuation'):
            raise ValueError('supervisor has not admitted native continuation: ' + str(record))
        return {'decision': 'block', 'reason': (
            f'StoryHook recorded context handoff {record["id"]} for {marker}. '
            'Continue the assigned work using native context management. '
            'Preserve the current collaboration mode: in Plan mode continue planning only; '
            'in Default mode continue only work already authorized. This is a context handoff, '
            'not implementation-plan approval or additional operational permission. '
            f'First read story continuation status {marker} --json, the current story comments, '
            f'and Git status, history, and diff. Run story help obviation-review and '
            f'story load-context --story {marker}; review every candidate and retain real holds. '
            'Reconcile pending corrections and prerequisite evidence. '
            'Do not acknowledge while in Plan mode: finish planning and use ordinary plan approval. '
            'Only after ordinary plan approval switches to Default mode, repeat this review and '
            'acknowledge before implementation. If already in Default mode, acknowledge now before '
            'continuing implementation. Read story help continuation and '
            f'acknowledge request {record["id"]} using story continuation ack {marker} '
            'with the fresh reviewed story sequence, HEAD, provider, and receiving session identity. '
            'Preserve approved scope, existing commits, dirty work, and queued corrections. '
            'Previously adopted work is now assigned work; unknown context capacity does not '
            'justify repeatedly deferring it. Do not type /compact or request a replacement '
            'session; the provider manages its native context and queue.')}
    except (OSError, ValueError, KeyError, TypeError, RuntimeError, RecursionError) as exc:
        return diagnostic(exc)


def native_origin(env):
    """Attach only dispatcher identity fields from the hook's native environment."""
    return {'tmux': env.get('TMUX'), 'tmux_pane': env.get('TMUX_PANE'),
            'autonomy': 'full-auto' if env.get('STORYHOOK_FULL_AUTO') else 'auto'}


def main():
    """Run the Claude-only Stop adapter without modifying provider decisions."""
    try:
        raw = sys.stdin.read(MAX_TRANSCRIPT + 1)
        if len(raw) > MAX_TRANSCRIPT:
            raise ValueError('hook payload exceeds input bound')
        payload = json.loads(raw, object_pairs_hook=unique_object)
        print(json.dumps(handle_stop(payload, os.environ, 'claude') or {}))
    except (ValueError, RecursionError) as exc:
        print(json.dumps(diagnostic(exc)))


if __name__ == '__main__':
    main()
