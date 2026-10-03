"""Durable admission state, independent of transport and process observation."""

import copy
import secrets

from .ledger import HELD, TERMINAL, Ledger, allocated
from .policy import Refusal, integer, label, resources
from .scheduler import drain, ready, schedule


class Authority(Ledger):
    """One transactional resource ledger for one host."""

    def __init__(self, path, policy, boot, clock, observe):
        self.observe = observe
        super().__init__(path, policy, label(boot, "boot identity"), clock)
        with self.transaction() as state:
            if state["boot"] != boot:
                for row in state["leases"].values():
                    if row["state"] not in TERMINAL:
                        row.update(state="released", reason="confirmed reboot")
                        self.event(state, "recovery", row, reason="confirmed reboot")
                state.update(boot=boot, pressure="initial", recovery=None, sample=None, scheduler={})
            else:
                self._reconcile(state)

    def _reconcile(self, state):
        for row in state["leases"].values():
            if row["state"] in HELD and self.observe(row["owner"]) is not True:
                if row["state"] != "quarantined":
                    row.update(state="quarantined", reason="supervisor identity is absent or unknown")
                    self.event(state, "quarantine", row, reason=row["reason"])

    def _row(self, state, identity, token):
        row = state["leases"].get(identity)
        if row is None or not isinstance(token, str) or not secrets.compare_digest(row["token"], token):
            raise Refusal("unknown lease capability")
        return row

    def enqueue(self, request, owner):
        """Atomically retain a request before scheduling or acknowledging it."""
        if "parent" in request:
            raise Refusal("nested work must use a subgrant; release before requesting a new root")
        if set(request) - {"id", "project", "work", "resources", "binding"}:
            raise Refusal("unknown request fields cannot change host policy")
        identity = label(request.get("id"), "request id")
        project = label(request.get("project"), "project")
        work = request.get("work")
        if work not in self.policy.value["weights"]:
            raise Refusal("unknown work class")
        amount = resources(request.get("resources"))
        limit = self.policy.cap if work == "repair" else self.policy.normal
        if any(amount[k] > limit[k] for k in amount):
            raise Refusal("request exceeds permanent host capacity")
        if self.observe(owner) is not True or owner.get("boot") != self.boot:
            raise Refusal("request supervisor identity is not live on this boot")
        binding = request.get("binding")
        if binding is not None:
            if not isinstance(binding, dict) or set(binding) != {"attempt_id", "execution_id", "generation"}:
                raise Refusal("invalid gate evidence binding")
            label(binding["attempt_id"], "attempt"); label(binding["execution_id"], "execution")
            integer(binding["generation"], "generation")
        with self.transaction() as state:
            old = state["leases"].get(identity)
            if old:
                if old["request"] != request or old["owner"] != owner:
                    raise Refusal("request identity changed on replay")
                return copy.deepcopy(old)
            row = dict(id=identity, project=project, work=work, resources=amount,
                       token=secrets.token_hex(32), owner=copy.deepcopy(owner),
                       request=copy.deepcopy(request), binding=binding, parent=None,
                       state="queued", queued_at=state["now"], granted_at=None,
                       executions=[], reason=None)
            state["leases"][identity] = row
            self.event(state, "request", row)
            schedule(state, self.policy, self.event)
            if row["state"] == "queued":
                self.event(state, "denial", row, reason=self._reason(state), wait_ms=0)
            return copy.deepcopy(row)

    def subgrant(self, parent, token, identity, amount, owner=None):
        """Partition a live envelope; never wait while holding its capacity."""
        label(identity, "subgrant id"); amount = resources(amount)
        with self.transaction() as state:
            row = self._row(state, parent, token)
            owner = row["owner"] if owner is None else owner
            if self.observe(owner) is not True:
                raise Refusal("subgrant supervisor is not live")
            old = state["leases"].get(identity)
            if old:
                if old["parent"] == parent and old["resources"] == amount and old["owner"] == owner:
                    return copy.deepcopy(old)
                raise Refusal("subgrant identity changed")
            if row["state"] not in {"reserved", "running"}:
                raise Refusal("parent is not accepting subgrants")
            children = [r for r in state["leases"].values() if r["parent"] == parent and r["state"] in HELD]
            if any(amount[k] + sum(r["resources"][k] for r in children) > row["resources"][k] for k in amount):
                raise Refusal("subgrant exceeds parent partition; cannot wait for an upgrade")
            child = dict(copy.deepcopy(row), id=identity, parent=parent, resources=amount,
                         owner=copy.deepcopy(owner),
                         token=secrets.token_hex(32), executions=[], state="reserved",
                         queued_at=state["now"], granted_at=state["now"])
            state["leases"][identity] = child
            self.event(state, "subgrant", child, wait_ms=0)
            return copy.deepcopy(child)

    def cancel(self, identity, token):
        """Request draining; capacity remains charged until independent cleanup proof."""
        with self.transaction() as state:
            drain(state, self._row(state, identity, token), "client cancellation", self.event)

    def attach(self, identity, token, execution):
        """Commit a blocked launch identity before the supervisor releases its child."""
        with self.transaction() as state:
            row = self._row(state, identity, token)
            if row["state"] not in {"reserved", "running"}:
                raise Refusal("lease cannot attach after cancellation or release")
            if not isinstance(execution, dict) or set(execution) != {"id", "leader", "session", "guard"}:
                raise Refusal("invalid execution attachment")
            label(execution["id"], "execution id")
            label(execution["guard"], "lifetime guard")
            integer(execution["session"], "execution session", 2)
            if self.observe(execution["leader"]) is not True:
                raise Refusal("execution incarnation is not live")
            old = next((e for e in row["executions"] if e["id"] == execution["id"]), None)
            if old:
                if all(old[k] == v for k, v in execution.items()):
                    return
                raise Refusal("execution identity changed")
            if any(not e["settled"] for e in row["executions"]):
                raise Refusal("parallel execution needs an explicit subgrant")
            row["executions"].append(dict(copy.deepcopy(execution), settled=False))
            row["state"] = "running"
            self.event(state, "attach", row, execution=execution["id"])

    def settle(self, identity, token, execution_id, prove):
        """Consume a broker-owned process/descriptor probe, never client-supplied success."""
        with self.transaction() as state:
            row = self._row(state, identity, token)
            execution = next((e for e in row["executions"] if e["id"] == execution_id), None)
            if execution is None:
                raise Refusal("unknown execution settlement")
            result = prove(copy.deepcopy(execution))
            if result is None:
                row.update(state="quarantined", reason="descendant cleanup is unknown")
                self.event(state, "quarantine", row, reason=row["reason"])
            elif result is True:
                execution["settled"] = True
                self.event(state, "cleanup", row, execution=execution_id, reason="session and lifetime guard settled")
                if row["state"] == "quarantined" and self.observe(row["owner"]) is True:
                    row.update(state="draining", reason="cleanup proof restored")

    def usage(self, identity, token, sample):
        """Retain measured peaks; an overrun cancels but never frees the envelope."""
        sample = resources(sample, "resource observation", minimum=0)
        with self.transaction() as state:
            row = self._row(state, identity, token)
            if row["state"] not in HELD:
                raise Refusal("resource observation is outside a live lease")
            old = row.get("peaks", dict(cpu=0, memory=0))
            row["peaks"] = {k: max(old[k], sample[k]) for k in sample}
            self.event(state, "usage", row, sample=sample, peaks=row["peaks"])
            if any(sample[k] > row["resources"][k] for k in sample):
                drain(state, row, "resource envelope exceeded", self.event)

    def finish(self, identity, token):
        """Release only a never-launched or proven-settled envelope and its descendants."""
        with self.transaction() as state:
            row = self._row(state, identity, token)
            if row["state"] in TERMINAL:
                return True
            if row["state"] == "quarantined" and not row["executions"]:
                return False
            if any(r["parent"] == identity and r["state"] not in TERMINAL for r in state["leases"].values()):
                return False
            if any(not execution.get("settled") for execution in row["executions"]):
                return False
            row.update(state="released", reason="descendants settled")
            self.event(state, "release", row, reason=row["reason"])
            schedule(state, self.policy, self.event)
            return True

    def sample(self, sample):
        """Update pressure and hysteresis; invalid samples never become idle values."""
        with self.transaction() as state:
            self._reconcile(state)
            valid = isinstance(sample, dict) and set(sample) == {"at", "available", "cpu", "memory", "runnable"}
            if valid:
                valid = all(type(v) is int and 0 <= v <= 2**63 - 1 for v in sample.values())
                valid = valid and 0 <= state["now"] - sample["at"] <= self.policy.value["stale_ms"]
                valid = valid and sample["cpu"] <= 1000 and sample["memory"] <= 1000
            old = state["pressure"]
            prior = state["sample"]
            if prior and state["now"] - prior["at"] > self.policy.value["stale_ms"]:
                state.update(pressure="sensor gap", recovery=None)
                old = "sensor gap"
            state["sample"] = copy.deepcopy(sample) if valid else None
            levels = self.policy.value["thresholds"]
            if not valid:
                state.update(pressure="sensor unavailable", recovery=None)
            elif any(sample[k] >= levels[k][1] for k in levels) or sample["available"] <= self.policy.value["headroom"]["memory"]:
                state.update(pressure="pressure", recovery=None)
            elif old in {"ready", "initial"}:
                state.update(pressure="ready", recovery=None)
            elif all(sample[k] <= levels[k][0] for k in levels):
                if state["recovery"] is None:
                    state["recovery"] = state["now"]
                if state["now"] - state["recovery"] >= self.policy.value["recover_ms"]:
                    state.update(pressure="ready", recovery=None)
            else:
                state["recovery"] = None
            if state["pressure"] != old:
                self.event(state, "pressure", reason=state["pressure"], sample=state["sample"])
            severe = valid and any(sample[k] >= levels[k][2] for k in levels)
            for row in state["leases"].values():
                expired = row["granted_at"] is not None and state["now"] - row["granted_at"] >= self.policy.value["lease_ms"]
                if row["parent"] is None and row["state"] in {"reserved", "running"} and (severe or expired):
                    drain(state, row, "severe pressure" if severe else "lease deadline", self.event)
            schedule(state, self.policy, self.event)

    def inspect(self, identity):
        """Return one retained request, without altering ownership."""
        with self.transaction() as state:
            if identity not in state["leases"]:
                raise Refusal("unknown request")
            return copy.deepcopy(state["leases"][identity])

    def _reason(self, state):
        if any(r["state"] == "quarantined" for r in state["leases"].values()):
            return "quarantine requires proven cleanup or reboot"
        if not ready(state, self.policy):
            return "wait for fresh healthy sensors and recovery hysteresis"
        return "wait for capacity and fair queue turn"

    def status(self):
        """Expose allocation, pressure and queue order without bearer capabilities."""
        with self.transaction() as state:
            return dict(authority=state["authority"], policy=state["policy"], boot=state["boot"],
                        timing={k: self.policy.value[k] for k in ("sample_ms", "lease_ms", "cleanup_ms")},
                        allocated=allocated(state), pressure=state["pressure"],
                        next_action=self._reason(state),
                        leases=[{k: r[k] for k in ("id", "parent", "project", "work", "state", "resources", "reason")}
                                for r in state["leases"].values()])
