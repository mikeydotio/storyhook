"""Native pressure-episode receipts; serialized observations are never capabilities."""

import copy

from .ledger import HELD, TERMINAL
from .policy import Refusal, integer, label
from .scheduler import ready

IDENTITY = ("authority", "host", "boot", "policy")
MAX_AFFECTED = 128


def observe(authority, state, old, completed_since=None):
    """Persist episode causality in the same transaction as native sampling."""
    episode = state.get("pressure_episode")
    pressure = state["pressure"]
    changed = pressure != old
    if pressure == "pressure" and (episode is None or episode.get("recovered_sequence") is not None
                                   or episode.get("invalidated")):
        # A legacy active-pressure state needs a new native observation before
        # it can gain restoration authority; never infer an old episode.
        sequence = authority.event(state, "pressure", reason=pressure, sample=state["sample"])
        state["pressure_episode"] = dict(version=1, fault_sequence=sequence,
                                         fault_at=state["now"], recovered_sequence=None,
                                         healthy_since=None, recovered_at=None, invalidated=False)
        return
    if episode is not None and pressure != "ready" and episode.get("recovered_sequence") is not None:
        # A later gap is not recovery of this older pressure episode.
        episode["invalidated"] = True
    details = {}
    if (pressure == "ready" and completed_since is not None and episode is not None
            and not episode.get("invalidated") and episode.get("recovered_sequence") is None):
        details = dict(recovered_from=episode["fault_sequence"], healthy_since=completed_since,
                       recover_ms=authority.policy.value["recover_ms"])
    sequence = None
    if changed:
        sequence = authority.event(state, "pressure", reason=pressure, sample=state["sample"], **details)
    if details and sequence is not None:
        episode.update(recovered_sequence=sequence, healthy_since=completed_since,
                       recovered_at=state["now"])


def _keys(value, expected, what):
    if not isinstance(value, dict) or set(value) != set(expected):
        raise Refusal(f"invalid restoration {what}")


def _binding(value):
    _keys(value, ("attempt_id", "execution_id", "generation"), "binding")
    label(value["attempt_id"], "attempt"); label(value["execution_id"], "execution")
    integer(value["generation"], "generation")


def _event(authority, sequence):
    import json
    found = authority.db.execute("SELECT payload FROM events WHERE sequence=?", (sequence,)).fetchone()
    if found is None:
        raise Refusal("restoration event is absent")
    return dict(json.loads(found[0]), sequence=sequence)


def _same(event, fault):
    return all(event.get(key) == fault[key] for key in IDENTITY)


def _terminal_window(authority, row, fault):
    """Verify both retained boundaries before classifying even an omitted root."""
    start, end = row.get("admission_sequence"), row.get("settlement_sequence")
    if type(start) is not int or type(end) is not int or start <= 0 or end < start:
        raise Refusal("legacy lease lacks authoritative operation window")
    first, last = _event(authority, start), _event(authority, end)
    if any(not _same(event, fault) or event.get("lease") != row["id"]
           or event.get("binding") != row["binding"] for event in (first, last)):
        raise Refusal("foreign operation window evidence")
    if first["event"] not in ("request", "subgrant") or last["event"] not in ("release", "cancel"):
        raise Refusal("invalid operation window boundaries")
    # A cancel of reserved/running work merely starts draining. Only the
    # immutable terminal row state tells which event can close this window.
    if last["event"] != {"released": "release", "cancelled": "cancel"}.get(row["state"]):
        raise Refusal("operation boundary does not prove the retained terminal state")
    return start, end


def _lifetime(authority, row, fault):
    """Link retained terminal state to native admission, attach and cleanup events."""
    start, end = _terminal_window(authority, row, fault)
    executions = []
    for execution in row["executions"]:
        attached, settled = execution.get("attach_sequence"), execution.get("settlement_sequence")
        if (type(attached) is not int or type(settled) is not int
                or not start < attached < settled < end):
            raise Refusal("legacy execution lacks native settlement history")
        for sequence, kind in ((attached, "attach"), (settled, "cleanup")):
            event = _event(authority, sequence)
            if (not _same(event, fault) or event.get("lease") != row["id"]
                    or event.get("binding") != row["binding"] or event.get("event") != kind
                    or event.get("execution") != execution["id"]):
                raise Refusal("foreign execution settlement evidence")
        executions.append(dict(id=execution["id"], leader=copy.deepcopy(execution["leader"]),
                               session=execution["session"], attach_sequence=attached,
                               settlement_sequence=settled))
    return dict(lease=row["id"], admission_sequence=start, settlement_sequence=end, executions=executions)


def _linked(authority, row, fault, start, end):
    for sequence in row.get("pressure_links", []):
        event = _event(authority, sequence)
        if (start <= sequence <= end and _same(event, fault)
                and event.get("lease") == row["id"] and event.get("binding") == row["binding"]
                and event.get("event") in ("denial", "cancel")
                and event.get("pressure_fault_sequence") == fault["sequence"]):
            return True
    return False


def _complete_roots(authority, state, affected, fault, latest):
    """A caller cannot omit a second affected envelope for the same native gate."""
    bindings = [subject["binding"] for subject in affected]
    expected = set()
    for row in state["leases"].values():
        if row["parent"] is not None or row["binding"] not in bindings:
            continue
        start = row.get("admission_sequence")
        end = row.get("settlement_sequence") if row["state"] in TERMINAL else latest
        if type(start) is not int or type(end) is not int or start <= 0 or end < start:
            raise Refusal("same-binding legacy root lacks authoritative operation window")
        first = _event(authority, start)
        if (not _same(first, fault) or first.get("event") != "request"
                or first.get("lease") != row["id"] or first.get("binding") != row["binding"]):
            raise Refusal("same-binding root has foreign admission history")
        if row["state"] in TERMINAL:
            _terminal_window(authority, row, fault)
        if start <= fault["sequence"] <= end or _linked(authority, row, fault, start, end):
            expected.add(row["id"])
    if expected != {subject["lease"] for subject in affected}:
        raise Refusal("affected roots differ from complete native same-binding fault history")


def proof(authority, request, broker_identity):
    """Read native episode/lease facts atomically; never change admission policy."""
    _keys(request, ("nonce", "fault", "window", "affected"), "request")
    nonce, fault, window, affected = (request[key] for key in ("nonce", "fault", "window", "affected"))
    label(nonce, "restoration nonce")
    _keys(fault, (*IDENTITY, "sequence"), "fault locator")
    for key in IDENTITY:
        label(fault[key], key)
    integer(fault["sequence"], "fault sequence")
    _keys(window, ("start_sequence", "end_sequence"), "window")
    for value in window.values():
        integer(value, "window sequence")
    if (not isinstance(affected, list) or not 1 <= len(affected) <= MAX_AFFECTED):
        raise Refusal("invalid affected restoration subjects")
    names = []
    for subject in affected:
        _keys(subject, ("lease", "binding"), "subject")
        names.append(label(subject["lease"], "affected lease"))
        _binding(subject["binding"])
    if names != sorted(set(names)):
        raise Refusal("affected leases must be sorted and unique")
    with authority.read_transaction() as state:
        if any(state[key] != fault[key] for key in IDENTITY) or broker_identity["boot"] != state["boot"]:
            raise Refusal("restoration authority, host, boot or policy changed")
        episode = state.get("pressure_episode")
        if (not isinstance(episode, dict) or episode.get("version") != 1
                or episode.get("fault_sequence") != fault["sequence"] or episode.get("invalidated")
                or episode.get("recovered_sequence") is None):
            raise Refusal("exact native pressure episode has not recovered")
        original = _event(authority, fault["sequence"])
        recovered = _event(authority, episode["recovered_sequence"])
        if (not _same(original, fault) or original.get("event") != "pressure"
                or original.get("reason") != "pressure" or original.get("lease") is not None
                or not _same(recovered, fault) or recovered.get("event") != "pressure"
                or recovered.get("reason") != "ready" or recovered.get("recovered_from") != fault["sequence"]
                or recovered.get("healthy_since") != episode["healthy_since"]
                or recovered.get("recover_ms") != authority.policy.value["recover_ms"]
                or recovered["sequence"] <= original["sequence"]
                or episode["healthy_since"] is None
                or episode["healthy_since"] < original["at"]
                or recovered["at"] - episode["healthy_since"] < authority.policy.value["recover_ms"]):
            raise Refusal("native recovery interval does not prove this fault")
        if not ready(state, authority.policy):
            raise Refusal("restoration requires current fresh healthy sensors")
        for row in state["leases"].values():
            if row["state"] == "quarantined" or (row["state"] in HELD and authority.observe(row["owner"]) is not True):
                raise Refusal("host has quarantined or unproved ownership")
        sequence = authority.db.execute("SELECT COALESCE(MAX(sequence),0) FROM events").fetchone()[0]
        _complete_roots(authority, state, affected, fault, sequence)
        starts, ends, settled = [], [], []
        for subject in affected:
            row = state["leases"].get(subject["lease"])
            if row is None or row["binding"] != subject["binding"] or row["state"] not in TERMINAL:
                raise Refusal("affected lease is not the exact settled subject")
            descendants = {row["id"]}
            for _ in state["leases"]:
                descendants.update(r["id"] for r in state["leases"].values() if r["parent"] in descendants)
            if any(r["state"] not in TERMINAL or any(not e["settled"] for e in r["executions"])
                   for r in state["leases"].values() if r["id"] in descendants):
                raise Refusal("affected descendant lifetime is unsettled")
            lifetimes = [_lifetime(authority, r, fault) for r in state["leases"].values()
                         if r["id"] in descendants]
            start, end = row["admission_sequence"], row["settlement_sequence"]
            if not start <= fault["sequence"] <= end and not _linked(authority, row, fault, start, end):
                raise Refusal("historical pressure is not causal for the affected operation")
            starts.append(start); ends.append(end)
            settled.append(dict(lease=row["id"], binding=copy.deepcopy(row["binding"]),
                                state=row["state"], admission_sequence=start, settlement_sequence=end,
                                lifetimes=sorted(lifetimes, key=lambda item: item["lease"])))
        actual_window = dict(start_sequence=min(starts), end_sequence=max(ends))
        if window != actual_window:
            raise Refusal("caller window differs from authoritative lease history")
        return dict(version=1, kind="host-pressure-restoration", nonce=nonce,
                    broker=copy.deepcopy(broker_identity), fault=copy.deepcopy(fault), window=actual_window,
                    affected=copy.deepcopy(affected), settled=settled, checked_at=state["now"],
                    ledger_sequence=sequence, episode=copy.deepcopy(episode), sample=copy.deepcopy(state["sample"]),
                    timing={key: authority.policy.value[key] for key in ("recover_ms", "stale_ms", "sample_ms")})
