"""Strict, capability-free decoding of an explicit implementation-plan request."""

import json
import re

REQUEST_TYPE = 'storyhook.implementation-plan'


def unique_object(pairs):
    """Reject duplicate keys rather than letting a last value change authority."""
    value = {}
    for key, item in pairs:
        if key in value:
            raise ValueError('duplicate structured plan key: ' + key)
        value[key] = item
    return value


def parse_plan_request(message, story_id):
    """Return decoded plan text, None for prose, or raise for invalid protocol data."""
    # This normalization only identifies candidates to REFUSE. Acceptance always
    # parses the original complete JSON, never extracted or repaired fragments.
    normalized = re.sub(r'\\u([0-9a-fA-F]{4})', lambda m: chr(int(m[1], 16)), message)
    candidate = (REQUEST_TYPE in normalized
                 or (re.search(r'"story_id"\s*:', normalized)
                     and re.search(r'"plan"\s*:', normalized)))
    if not candidate:
        return None
    try:
        request = json.loads(message, object_pairs_hook=unique_object)
    except (ValueError, RecursionError) as exc:
        raise ValueError('invalid structured plan request: ' + str(exc)) from exc
    if (not isinstance(request, dict)
            or set(request) != {'type', 'version', 'story_id', 'plan'}
            or request['type'] != REQUEST_TYPE
            or type(request['version']) is not int or request['version'] != 1
            or request['story_id'] != story_id
            or not isinstance(request['plan'], str) or not request['plan'].strip()):
        raise ValueError('invalid structured plan request schema, version, or story identity')
    # Also reject JSON's escaped lone surrogates: the verbatim plan must be
    # representable as UTF-8 for both the story comment and durable hash.
    request['plan'].encode('utf-8')
    return request['plan']
