"""Validated host policy; no production capacity is inferred or defaulted."""

import copy
import hashlib
import json

MAX_INT = 2**63 - 1
CLASSES = ("build", "test", "release", "repair")


def integer(value, name, minimum=1):
    """Reject bools, coercions and counters that cannot fit the durable wire type."""
    if type(value) is not int or not minimum <= value <= MAX_INT:
        raise Refusal(f"invalid {name}: expected integer in {minimum}..{MAX_INT}")
    return value


def resources(value, name="resources", minimum=1):
    """Validate both resource dimensions; unknown dimensions cannot be ignored."""
    if not isinstance(value, dict) or set(value) != {"cpu", "memory"}:
        raise Refusal(f"invalid {name}: require cpu and memory")
    return {key: integer(value[key], f"{name}.{key}", minimum) for key in value}


def label(value, name):
    """Validate an opaque identifier without giving it filesystem meaning."""
    if not isinstance(value, str) or not value.strip() or len(value) > 4096:
        raise Refusal(f"invalid {name}")
    return value


class Refusal(RuntimeError):
    """An admission invariant cannot be established safely."""


class Policy:
    """An immutable, measured host resource policy."""

    def __init__(self, value, host, *, fixture=False):
        fields = {"version", "host", "calibration", "measurements", "capacity",
                  "headroom", "reserve", "weights", "project_weight", "sample_ms",
                  "stale_ms", "recover_ms", "starvation_ms", "lease_ms", "cleanup_ms",
                  "thresholds", "workloads"}
        if not isinstance(value, dict) or set(value) != fields or value["version"] != 1:
            raise Refusal("incomplete or unsupported host policy")
        if value["host"] != host:
            raise Refusal("policy belongs to another host")
        if value["calibration"] != ("fixture" if fixture else "measured"):
            raise Refusal("host calibration is not valid for this authority")
        refs = value["measurements"]
        if not isinstance(refs, list) or not refs:
            raise Refusal("calibration requires measurement references")
        for ref in refs:
            label(ref, "measurement reference")
            if not fixture and not ref.startswith("sha256:"):
                raise Refusal("production calibration requires content-addressed evidence")
        self.value = copy.deepcopy(value)
        capacity = resources(value["capacity"], "capacity")
        headroom = resources(value["headroom"], "headroom")
        reserve = resources(value["reserve"], "reserve")
        self.cap = {k: capacity[k] - headroom[k] for k in capacity}
        self.normal = {k: self.cap[k] - reserve[k] for k in capacity}
        if min(self.normal.values()) <= 0:
            raise Refusal("headroom and reserve exhaust host capacity")
        weights = value["weights"]
        if not isinstance(weights, dict) or set(weights) != set(CLASSES):
            raise Refusal("positive weights required for every work class")
        for key, weight in weights.items():
            integer(weight, f"weight.{key}")
        for key in fields & {"project_weight", "sample_ms", "stale_ms", "recover_ms",
                             "starvation_ms", "lease_ms", "cleanup_ms"}:
            integer(value[key], key)
        if value["stale_ms"] < value["sample_ms"] or value["recover_ms"] < value["sample_ms"]:
            raise Refusal("sensor freshness and recovery must span a sample")
        thresholds = value["thresholds"]
        if not isinstance(thresholds, dict) or set(thresholds) != {"cpu", "memory", "runnable"}:
            raise Refusal("missing pressure thresholds")
        for key, levels in thresholds.items():
            if not isinstance(levels, list) or len(levels) != 3:
                raise Refusal(f"invalid {key} thresholds")
            for level in levels:
                integer(level, key, 0)
            if not levels[0] < levels[1] < levels[2]:
                raise Refusal(f"unordered {key} thresholds")
            if key != "runnable" and levels[2] > 1000:
                raise Refusal(f"{key} pressure is measured in permille")
        workloads = value["workloads"]
        if not isinstance(workloads, dict) or not workloads:
            raise Refusal("missing measured workload costs")
        for key, amount in workloads.items():
            label(key, "workload")
            resources(amount, "workload cost")
        self.digest = hashlib.sha256(json.dumps(value, sort_keys=True,
                                               separators=(",", ":")).encode()).hexdigest()

    def cost(self, amount):
        """Dominant resource share in integer millionths, rounded upward."""
        return max((amount[k] * 1_000_000 + self.cap[k] - 1) // self.cap[k]
                   for k in self.cap)
