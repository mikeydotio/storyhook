"""Load only a host-local measured policy whose calibration bytes and inputs match."""

import hashlib
import json
import os
import re

from .namespace import directory, open_private
from .policy import Policy, Refusal


def _read(path):
    fd = open_private(path)
    with os.fdopen(fd, "rb") as stream:
        return stream.read()


def load_policy(root, host):
    """Refuse activation without a retained matching calibration record."""
    try:
        root = directory(root)
        value = json.loads(_read(root / "policy.json"))
    except FileNotFoundError:
        raise Refusal("production admission disabled: measured host calibration is absent") from None
    policy = Policy(value, host)
    parameters = {k: v for k, v in value.items() if k not in ("calibration", "measurements")}
    sources = set()
    for reference in value["measurements"]:
        if not re.fullmatch(r"sha256:[0-9a-f]{64}", reference):
            raise Refusal("invalid calibration content digest")
        digest = reference.removeprefix("sha256:")
        data = _read(root / f"calibration-{digest}.json")
        if hashlib.sha256(data).hexdigest() != digest:
            raise Refusal("calibration digest does not match retained bytes")
        evidence = json.loads(data)
        if evidence.get("version") != 1 or evidence.get("host") != host or evidence.get("result") != "calibrated":
            raise Refusal("foreign or incomplete calibration evidence")
        if evidence.get("parameters") != parameters:
            raise Refusal("calibration parameters differ from host policy")
        if not isinstance(evidence.get("observations"), list) or not evidence["observations"]:
            raise Refusal("calibration observations are absent")
        if not isinstance(evidence.get("sources"), list) or not all(isinstance(s, str) for s in evidence["sources"]):
            raise Refusal("calibration source references are absent")
        sources.update(evidence["sources"])
    if not {"SH-801", "SH-867"}.issubset(sources):
        raise Refusal("calibration must retain SH-801 and SH-867 source references")
    return policy
