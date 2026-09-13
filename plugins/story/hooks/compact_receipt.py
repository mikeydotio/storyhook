"""Forward native compaction completion without treating it as resumed execution."""

import json
import os
import sys

from codex_classifier import run_process
from plan_request import unique_object
from session_handoff import IDENTITY, MAX_TRANSCRIPT, STORY_ID, diagnostic, native_origin

OUTSTANDING = ('pending', 'attempting', 'awaiting-ack', 'needs-attention')


def handle(payload, env, process=run_process):
    """Correlate one root PostCompact event; the supervisor validates lifecycle ownership."""
    marker = env.get('STORYHOOK_FULL_AUTO') or env.get('STORYHOOK_AUTO') or ''
    if (not STORY_ID.fullmatch(marker) or not isinstance(payload, dict)
            or payload.get('hook_event_name') != 'PostCompact'
            or payload.get('agent_id') or payload.get('agent_type')):
        return {}
    try:
        if (not isinstance(payload.get('session_id'), str)
                or not IDENTITY.fullmatch(payload['session_id'])
                or any(not isinstance(payload.get(key), str)
                       or not os.path.isabs(payload[key])
                       for key in ('cwd', 'transcript_path'))
                or payload.get('trigger') not in ('manual', 'auto')):
            raise ValueError('invalid compaction event session, paths, or trigger')
        provider = 'codex' if 'turn_id' in payload else 'claude'
        if provider == 'codex':
            if (not isinstance(payload['turn_id'], str)
                    or not IDENTITY.fullmatch(payload['turn_id'])):
                raise ValueError('invalid Codex compaction turn identity')
        elif not isinstance(payload.get('compact_summary'), str):
            raise ValueError('missing Claude compaction summary')
        base = ['story', '--deadline', '2', 'continuation']
        status = json.loads(process(base + ['status', marker, '--json'], timeout=3,
                                    cwd=payload['cwd']), object_pairs_hook=unique_object)
        if (not isinstance(status, dict) or status.get('result') != 'ok'
                or status.get('story_id') != marker
                or not isinstance(status.get('requests'), list)):
            raise ValueError('invalid continuation status response: ' + str(status))
        matches = []
        for record in status['requests']:
            if not isinstance(record, dict) or not isinstance(record.get('capture'), dict):
                raise ValueError('invalid continuation status record')
            capture = record['capture']
            if (record.get('story_id') == marker and record.get('status') in OUTSTANDING
                    and record.get('phase') in ('native-continuation', 'resume')
                    and capture.get('provider') == provider
                    and capture.get('session_id') == payload['session_id']
                    and capture.get('transcript_path') == payload['transcript_path']):
                matches.append(record)
        if not matches:
            return {}
        if (len(matches) != 1 or not isinstance(matches[0].get('id'), str)
                or not IDENTITY.fullmatch(matches[0]['id'])):
            raise ValueError('ambiguous continuation request for compaction event')
        # PostCompact has a different turn ID from the handoff in Codex. The
        # daemon verifies that native lifecycle; matching old turn_context here
        # would discard every successful manual compact.
        receipt = {'event': 'post-compact', 'provider': provider,
                   'session_id': payload['session_id'], 'origin': payload | native_origin(env)}
        answer = json.loads(process(
            base + ['receipt', marker, matches[0]['id'], '--stdin', '--json'], timeout=3,
            cwd=payload['cwd'], text=json.dumps(receipt)), object_pairs_hook=unique_object)
        if not isinstance(answer, dict) or answer.get('result') != 'ok':
            raise ValueError('supervisor refused compaction receipt: ' + str(answer))
        return {}
    except (OSError, ValueError, KeyError, TypeError, RuntimeError, RecursionError) as exc:
        return diagnostic(exc)


def main():
    """Use stderr for observable diagnostics because Claude discards PostCompact output."""
    try:
        raw = sys.stdin.read(MAX_TRANSCRIPT + 1)
        if len(raw) > MAX_TRANSCRIPT:
            raise ValueError('compaction hook payload exceeds input bound')
        result = handle(json.loads(raw, object_pairs_hook=unique_object), os.environ)
    except (ValueError, RecursionError) as exc:
        result = diagnostic(exc)
    if result.get('systemMessage'):
        print(result['systemMessage'], file=sys.stderr)
    print('{}')


if __name__ == '__main__':
    main()
