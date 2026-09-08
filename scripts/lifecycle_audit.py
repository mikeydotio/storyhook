"""Pure historical lifecycle analysis for the SH-560 report."""

from collections import defaultdict
from datetime import datetime, timezone
import json

STATE_EVENTS = {"StoryCreated", "StoryStateChanged", "StoryClosedAndArchived"}


def timestamp(value):
    """Parse an aware timestamp; malformed evidence remains explicitly unknown."""
    if not isinstance(value, str):
        return None
    try:
        result = datetime.fromisoformat(value.replace("Z", "+00:00"))
        return result.astimezone(timezone.utc) if result.tzinfo else None
    except ValueError:
        return None


def elapsed(start, end):
    """Return nonnegative observed seconds, never inventing absent timing data."""
    a, b = timestamp(start), timestamp(end)
    return (b - a).total_seconds() if a and b and b >= a else None


def _story(number, events, start, end):
    title, state, since = "Untitled", None, None
    transitions, completions, generations, comments, warnings = [], [], [], [], []
    durations = defaultdict(float)
    reopenings = 0
    for event in events:
        kind, at, data = event["kind"], event["at"], event["payload"]
        if timestamp(at) is None:
            warnings.append(f"Event {event['global_seq']} has no valid timestamp")
        if kind in {"StoryCreated", "StoryTitleSet"}:
            title = data["title"]
        if kind in STATE_EVENTS:
            next_state = data["state"]
            # Archive markers repeat the closed state; verification resubmission
            # is different: its event identity is a new reservation generation.
            if next_state != state or (kind == "StoryStateChanged" and state == "verifying"):
                if state is not None:
                    seconds = elapsed(since, at)
                    if seconds is None:
                        warnings.append(f"Unknown {state} interval ending at event {event['global_seq']}")
                    if durations[state] is not None:
                        durations[state] = None if seconds is None else durations[state] + seconds
                    if state == "verifying":
                        generations[-1].update(ended_at=at, residence_seconds=seconds)
                if next_state == "done" and state != "done":
                    completions.append(at)
                if state == "done" and next_state != "done":
                    reopenings += 1
                state, since = next_state, at
                transitions.append(dict(state=state, at=at, event=event["global_seq"]))
                if state == "verifying":
                    generations.append(dict(generation=event["global_seq"], started_at=at,
                                            ended_at=None, residence_seconds=None))
        if kind == "StoryCommentAdded":
            comments.append(dict(at=at, text=data["text"], event=event["global_seq"], retracted=False))
        elif kind == "StoryCommentRetracted":
            for comment in comments:
                if comment["at"] == data["comment_at"] and comment["text"] == data["text"]:
                    comment["retracted"] = True
    window_completions = [at for at in completions if timestamp(at) and start <= timestamp(at) <= end]
    if not window_completions:
        return None
    # Cohort members may reopen before cutoff. Keep the open tail explicitly
    # censored instead of silently extending an observed completion interval.
    if state != "done":
        warnings.append("Reopened at cutoff; state residence excludes the unfinished tail")
    return dict(id=f"SH-{number}", title=title, state=state, events=events,
                transitions=transitions, comments=comments, generations=generations,
                done_entries=len(window_completions), lifetime_done_entries=len(completions),
                reopenings=reopenings, state_seconds=dict(durations),
                cycle_seconds=elapsed(transitions[0]["at"], completions[-1]),
                gate_runtime_seconds=None, warnings=warnings)


def analyze(events, start, end):
    """Return cohort histories and metrics for an inclusive UTC window."""
    start_time, end_time = timestamp(start), timestamp(end)
    if start_time is None or end_time is None or start_time > end_time:
        raise ValueError("invalid audit window")
    grouped, identities, global_ids = defaultdict(list), set(), set()
    for event in sorted(events, key=lambda e: (e["story_no"], e["seq"])):
        identity = (event["story_no"], event["seq"])
        if identity in identities or event["global_seq"] in global_ids:
            raise ValueError(f"duplicate event sequence: {identity}")
        identities.add(identity)
        global_ids.add(event["global_seq"])
        at = timestamp(event["at"])
        if at is None or at <= end_time:
            grouped[event["story_no"]].append(event)
    return [story for number, history in sorted(grouped.items())
            if (story := _story(number, history, start_time, end_time)) is not None]


def embedded_json(value):
    """Encode JSON safely for an HTML script data element."""
    return (json.dumps(value, ensure_ascii=True, sort_keys=True, separators=(",", ":"))
            .replace("<", "\\u003c").replace(">", "\\u003e").replace("&", "\\u0026"))
