"""Bind handoff generations to provider-owned assistant records, never prose hashes."""

import json
from pathlib import Path
import re

IDENTITY = re.compile(r'^[A-Za-z0-9][A-Za-z0-9_-]{0,127}$')


def require(condition, detail):
    """Refuse incomplete or conflicting native evidence without inventing identity."""
    if not condition:
        raise RuntimeError('handoff message identity: ' + detail)


def same_path(value, expected):
    """Compare explicit absolute provider paths after canonicalization."""
    return isinstance(value, str) and Path(value).is_absolute() and Path(value).resolve() == expected


def unique_object(pairs):
    """A transcript's ambiguous JSON cannot authenticate an accepted envelope."""
    value = {}
    for key, item in pairs:
        require(key not in value, 'duplicate envelope key: ' + key)
        value[key] = item
    return value


def handoff_message_id(capture, origin, handoff, rows):
    """Validate the latest root assistant, turn, mode and envelope at capture time."""
    cwd = Path(capture['lease']['worktree_path']).resolve()
    sid, turn = capture['session_id'], capture['turn_id']
    if capture['provider'] == 'codex':
        sessions = [row.get('payload', {}) for row in rows if row.get('type') == 'session_meta']
        require(len(sessions) == 1 and sessions[0].get('id') == sid
                and sessions[0].get('source') in ('cli', 'exec', 'vscode')
                and same_path(sessions[0].get('cwd'), cwd), 'foreign root Codex session')
        context = next((row.get('payload', {}) for row in reversed(rows)
                        if row.get('type') == 'turn_context'), {})
        require(context.get('turn_id') == turn and same_path(context.get('cwd'), cwd)
                and context.get('collaboration_mode', {}).get('mode') == capture['mode'],
                'stale turn, cwd or mode')
        active = next((row.get('payload', {}).get('turn_id') for row in reversed(rows)
                       if row.get('type') == 'event_msg'
                       and row.get('payload', {}).get('type') == 'task_started'), None)
        require(active == turn, 'stale native task')
        # Only messages after the most recent task start can own this Stop.
        current = []
        for row in rows:
            if row.get('type') == 'event_msg' and row.get('payload', {}).get('type') == 'task_started':
                current = []
            elif row.get('type') == 'response_item':
                current.append(row.get('payload', {}))
        message = next((item for item in reversed(current) if item.get('type') == 'message'), {})
        require(message.get('role') == 'assistant', 'latest conversational message is not assistant')
        message_id = message.get('id')
        content = message.get('content')
        text_type = 'output_text'
    else:
        require(capture['provider'] == 'claude', 'unknown provider')
        row = next((row for row in reversed(rows) if row.get('type') in ('assistant', 'user')), {})
        require(row.get('type') == 'assistant' and row.get('sessionId') == sid
                and row.get('isSidechain') is False and same_path(row.get('cwd'), cwd)
                and row.get('uuid') == turn, 'foreign or stale Claude root message')
        message_id = row.get('uuid')
        content = row.get('message', {}).get('content')
        text_type = 'text'
    require(isinstance(message_id, str) and IDENTITY.fullmatch(message_id), 'missing native message ID')
    require(isinstance(content, list), 'missing assistant content')
    texts = [item.get('text') for item in content if isinstance(item, dict) and item.get('type') == text_type]
    require(texts and all(isinstance(text, str) for text in texts), 'missing assistant text')
    text = '\n'.join(texts)
    require(text == origin.get('last_assistant_message'), 'Stop differs from latest assistant content')
    native = json.loads(text, object_pairs_hook=unique_object)
    # Python considers True == 1 == 1.0; protocol envelope types are exact.
    require(json.dumps(native, sort_keys=True) == json.dumps(handoff, sort_keys=True),
            'envelope differs from native message')
    return message_id
