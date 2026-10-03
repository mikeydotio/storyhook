"""Persisted hierarchical deficit scheduling and pressure admission."""

from .ledger import HELD, allocated


def ready(state, policy):
    """Require a current healthy sample; silence cannot manufacture capacity."""
    sample = state["sample"]
    return (state["pressure"] == "ready" and sample is not None
            and 0 <= state["now"] - sample["at"] <= policy.value["stale_ms"])


def fits(row, state, policy):
    """Honor host, non-repair and observed available-memory limits together."""
    used = allocated(state)
    normal = allocated(state, [r for r in state["leases"].values() if r["work"] != "repair"])
    amount = row["resources"]
    if any(used[k] + amount[k] > policy.cap[k] for k in used):
        return False
    if row["work"] != "repair" and any(normal[k] + amount[k] > policy.normal[k] for k in used):
        return False
    # Conservatively count reservations against available memory too: an allocation
    # may not yet have faulted its pages when the sensor reads the host.
    return used["memory"] + amount["memory"] <= state["sample"]["available"] - policy.value["headroom"]["memory"]


def _visit(lane, names, weight):
    """Start a visit once; carry unused deficit to the next round."""
    if lane.get("current") not in names:
        lane.update(current=names[0], visiting=False)
    name = lane["current"]
    credits = lane.setdefault("credits", {})
    if not lane.get("visiting"):
        credits[name] = credits.get(name, 0) + 1_000_000 * weight(name)
        lane["visiting"] = True
    return name, credits[name]


def _advance(lane, names):
    lane.update(current=names[(names.index(lane["current"]) + 1) % len(names)], visiting=False)


def _pick(rows, state, policy):
    """Visit project then class queues with independent persistent deficit counters."""
    sched = state["scheduler"]
    projects = sorted({r["project"] for r in rows})
    if not projects:
        return None
    for _ in range(2 * len(projects)):
        project, budget = _visit(sched, projects, lambda _: policy.value["project_weight"])
        candidates = [r for r in rows if r["project"] == project]
        classes = sorted({r["work"] for r in candidates})
        lane = sched.setdefault("classes", {}).setdefault(project, {})
        for _ in range(2 * len(classes)):
            work, credit = _visit(lane, classes, lambda name: policy.value["weights"][name])
            row = next(r for r in candidates if r["work"] == work)
            cost = policy.cost(row["resources"])
            if cost <= credit and cost <= budget:
                lane["credits"][work] -= cost
                sched["credits"][project] -= cost
                if lane["credits"][work] == 0:
                    _advance(lane, classes)
                if sched["credits"][project] == 0:
                    _advance(sched, projects)
                return row
            if cost <= credit and cost > budget:
                break
            _advance(lane, classes)
        _advance(sched, projects)
    return None


def schedule(state, policy, event):
    """Grant eligible work without bypassing an aged request with new backfill."""
    if not ready(state, policy):
        return
    if any(r["state"] == "quarantined" for r in state["leases"].values()):
        return
    while True:
        queued = [r for r in state["leases"].values() if r["state"] == "queued"]
        if not queued:
            return
        aged = next((r for r in queued if state["now"] - r["queued_at"] >= policy.value["starvation_ms"]), None)
        eligible = [r for r in queued if fits(r, state, policy)]
        if aged:
            # Reserved repair capacity stays usable while ordinary capacity drains.
            repair = allocated(state, [r for r in state["leases"].values() if r["work"] == "repair"])
            eligible = [r for r in eligible if r is aged or (
                r["work"] == "repair" and aged["work"] != "repair"
                and all(repair[k] + r["resources"][k] <= policy.value["reserve"][k] for k in repair))]
        row = _pick(eligible, state, policy)
        if row is None:
            return
        row.update(state="reserved", granted_at=state["now"], reason=None)
        event(state, "grant", row, wait_ms=state["now"] - row["queued_at"])


def drain(state, row, reason, event):
    """Propagate cancellation without confusing it with settled descendants."""
    if row["state"] not in HELD | {"queued"}:
        return
    if row["state"] != "quarantined":
        row["state"] = "cancelled" if row["state"] == "queued" else "draining"
    row["reason"] = reason
    event(state, "cancel", row, reason=reason)
    for child in list(state["leases"].values()):
        if child["parent"] == row["id"]:
            drain(state, child, reason, event)
