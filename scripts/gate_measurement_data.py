"""Validated, restartable evidence for verifier scheduling measurements."""

import json
import math
import statistics

from verifier_state import Refusal


def schedule(pairs=10):
    """Return the alternating control/utility schedule, excluding warmups."""
    if type(pairs) is not int or pairs < 1:
        raise Refusal("sample pairs must be a positive integer")
    return ["control", "utility"] * pairs


def summarize(manifest, records):
    """Summarize a complete cohort without hiding invalid or failed attempts."""
    order = schedule(manifest["pairs"])
    samples = [row for row in records if row.get("kind") == "sample"]
    starts = [row.get('index') for row in records if row.get('kind') == 'start']
    if starts and (any(type(i) is not int for i in starts)
                   or starts != list(range(len(starts))) or len(starts) > len(order)):
        raise Refusal('invalid, duplicate or out-of-order sample starts')
    seen = set()
    accepted = {condition: [] for condition in ("control", "utility")}
    attempts = {condition: [] for condition in accepted}
    for row in samples:
        index = row.get("index")
        if type(index) is not int or not 0 <= index < len(order):
            raise Refusal(f"invalid sample index: {index!r}")
        if index in seen:
            raise Refusal(f"duplicate sample index {index}; retries cannot replace evidence")
        if index != len(seen):
            raise Refusal(f"sample {index} is out of order")
        seen.add(index)
        condition = order[index]
        if row.get("condition") != condition:
            raise Refusal(f"sample {index} violates the alternating schedule")
        if any(row.get(key) != manifest[key] for key in ("tree", "day")):
            raise Refusal(f"sample {index} changes the cohort tree or day")
        finite_seconds(row.get("wall_seconds"))
        if type(row.get("exit_code")) is not int:
            raise Refusal(f"sample {index} has no observed process exit")
        attempts[condition].append(row)
        probes = row.get("probes", [])
        good_probes = (len(probes) == 2
                       and {p.get("name") for p in probes} == {"list", "hook"}
                       and all(p.get("ok") is True and p.get("overlap") is True for p in probes))
        for probe in probes:
            finite_seconds(probe.get("seconds"))
        if (row.get("valid") is True and row["exit_code"] == 0
                and row.get("cleanup") == "complete" and good_probes):
            accepted[condition].append(row)
    if starts and any(i not in starts for i in seen):
        raise Refusal('sample has no start observation')
    unresolved = {condition: sum(order[i] == condition and i not in seen for i in starts)
                  for condition in accepted}
    result = {"version": 1, "complete": all(len(v) == manifest["pairs"] for v in accepted.values())}
    for condition, rows in accepted.items():
        wall = [row["wall_seconds"] for row in rows]
        result[condition] = {
            "attempted": len(attempts[condition]) + unresolved[condition], "valid": len(rows),
            "unresolved": unresolved[condition],
            "failed": sum(row["exit_code"] != 0 for row in attempts[condition]),
            "gate": {"median": statistics.median(wall), "min": min(wall), "max": max(wall)} if wall else None,
            "probes": {name: statistics.median([p["seconds"] for r in rows for p in r["probes"] if p["name"] == name])
                       for name in ("list", "hook")} if rows else None,
        }
    control, utility = result["control"]["gate"], result["utility"]["gate"]
    result["median_change_percent"] = (100 * (utility["median"] / control["median"] - 1)
                                       if control and utility and control["median"] > 0 else None)
    return result


def finite_seconds(value):
    """Reject booleans, missing values, infinities and negative elapsed times."""
    if type(value) not in (int, float) or not math.isfinite(value) or value < 0:
        raise Refusal(f"invalid elapsed seconds: {value!r}")
    return value


def parse_wall(text):
    """Read exactly one finite nonnegative real duration from time -p output."""
    rows = [line.split() for line in text.splitlines() if line.startswith("real")]
    try:
        if len(rows) != 1 or len(rows[0]) != 2 or rows[0][0] != "real":
            raise ValueError("expected one real duration")
        return finite_seconds(float(rows[0][1]))
    except ValueError as error:
        raise Refusal(f"invalid time -p output: {text!r}") from error


def hook_context(raw):
    """Require the real SessionStart envelope instead of accepting degraded {}."""
    try:
        result = json.loads(raw)
        context = result["hookSpecificOutput"]
        if context["hookEventName"] != "SessionStart":
            raise ValueError("wrong hook event")
        text = context["additionalContext"]
        if not isinstance(text, str) or not text.strip():
            raise ValueError("empty context")
        return text
    except (ValueError, KeyError, TypeError) as error:
        raise Refusal("SessionStart returned missing or degraded context") from error


class IdleWindow:
    """Require a continuous idle interval; unknown pressure breaks the interval."""

    def __init__(self, seconds=60, ratio=0.5):
        self.seconds = seconds
        self.ratio = ratio
        self.since = None
        self.last = None

    def observe(self, now, load, cores):
        """Return whether the measured load has qualified for the whole interval."""
        finite_seconds(now)
        if self.last is not None and now < self.last:
            raise Refusal("idle observation clock moved backwards")
        self.last = now
        if (type(load) not in (int, float) or not math.isfinite(load) or load < 0
                or type(cores) is not int or cores <= 0 or load / cores >= self.ratio):
            self.since = None
            return False
        if self.since is None:
            self.since = now
        return now - self.since >= self.seconds
