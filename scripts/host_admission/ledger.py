"""Atomic host records and events; a missing acknowledgement never undoes a grant."""

from contextlib import contextmanager
import json
import sqlite3
import uuid

from .policy import Refusal, integer, resources

HELD = {"reserved", "running", "draining", "quarantined"}
TERMINAL = {"released", "cancelled"}


def allocated(state, rows=None):
    """Charge a root envelope once, including quarantined descendants."""
    rows = state["leases"].values() if rows is None else rows
    return {k: sum(row["resources"][k] for row in rows
                   if row["state"] in HELD and row["parent"] is None)
            for k in ("cpu", "memory")}


class Ledger:
    """Serialize state transitions and their evidence in one local SQLite transaction."""

    def __init__(self, path, policy, boot, clock, *, initialize=True):
        self.policy, self.boot, self.clock = policy, boot, clock
        self.db = sqlite3.connect(path, isolation_level=None, timeout=0)
        self.db.execute("PRAGMA synchronous=FULL")
        self.db.execute("PRAGMA journal_mode=DELETE")
        if not initialize:
            tables = {r[0] for r in self.db.execute("SELECT name FROM sqlite_master WHERE type='table'")}
            if not {"authority", "events"} <= tables:
                self.db.close()
                raise Refusal("initialized authority schema disappeared")
        self.db.executescript("""
            CREATE TABLE IF NOT EXISTS authority (id INTEGER PRIMARY KEY CHECK(id=1), payload TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS events (sequence INTEGER PRIMARY KEY AUTOINCREMENT, payload TEXT NOT NULL);
        """)
        self.db.execute("BEGIN IMMEDIATE")
        try:
            found = self.db.execute("SELECT payload FROM authority WHERE id=1").fetchone()
            if found is None:
                if not initialize:
                    raise Refusal("initialized authority state disappeared")
                state = dict(version=1, authority=uuid.uuid4().hex, host=policy.value["host"],
                             boot=boot, policy=policy.digest, leases={}, scheduler={},
                             pressure="initial", recovery=None, sample=None, pressure_episode=None, now=clock())
                self.db.execute("INSERT INTO authority VALUES(1,?)", (json.dumps(state),))
            else:
                state = json.loads(found[0])
                if state.get("version") != 1 or state.get("host") != policy.value["host"]:
                    raise Refusal("foreign or unsupported authority state")
                if state.get("policy") != policy.digest:
                    if any(r["state"] not in TERMINAL or any(not e["settled"] for e in r["executions"])
                           for r in state["leases"].values()):
                        raise Refusal("host policy changed; retained leases require their original policy")
                    previous = state["policy"]
                    state.update(policy=policy.digest, authority=uuid.uuid4().hex, scheduler={},
                                 pressure="initial", recovery=None, sample=None, pressure_episode=None)
                    self.validate(state)
                    self.event(state, "pressure", reason="idle policy revision activated", previous_policy=previous)
                    self.db.execute("UPDATE authority SET payload=? WHERE id=1", (json.dumps(state),))
            self.db.execute("COMMIT")
        except BaseException:
            self.db.execute("ROLLBACK")
            self.db.close()
            raise

    @contextmanager
    def transaction(self):
        """A failed transition rolls back both allocation and its evidence."""
        self.db.execute("BEGIN IMMEDIATE")
        try:
            row = self.db.execute("SELECT payload FROM authority WHERE id=1").fetchone()
            if row is None:
                raise Refusal("authority state disappeared")
            state = json.loads(row[0])
            self.validate(state)
            if state["boot"] == self.boot and self.clock() < state["now"]:
                raise Refusal("monotonic host clock moved backward")
            state["now"] = self.clock()
            yield state
            self.validate(state)
            self.db.execute("UPDATE authority SET payload=? WHERE id=1", (json.dumps(state),))
            self.db.execute("COMMIT")
        except BaseException:
            self.db.execute("ROLLBACK")
            raise

    @contextmanager
    def read_transaction(self):
        """Read one current snapshot without updating its clock or any durable row."""
        self.db.execute("BEGIN")
        try:
            row = self.db.execute("SELECT payload FROM authority WHERE id=1").fetchone()
            if row is None:
                raise Refusal("authority state disappeared")
            state = json.loads(row[0])
            self.validate(state)
            now = self.clock()
            if state["boot"] != self.boot or now < state["now"]:
                raise Refusal("native snapshot boot or monotonic clock changed")
            state["now"] = now
            yield state
            self.db.execute("COMMIT")
        except BaseException:
            self.db.execute("ROLLBACK")
            raise

    def validate(self, state):
        """Reject impossible retained accounting before it can authorize more work."""
        try:
            if state["version"] != 1 or state["policy"] != self.policy.digest or state["host"] != self.policy.value["host"]:
                raise Refusal("authority identity or policy changed")
            integer(state["now"], "retained clock", 0)
            rows = state["leases"]
            for key, row in rows.items():
                if row["id"] != key or row["state"] not in HELD | TERMINAL | {"queued"}:
                    raise Refusal("invalid retained lease identity or state")
                resources(row["resources"], "retained allocation")
                if row["work"] not in self.policy.value["weights"]:
                    raise Refusal("invalid retained work class")
                ancestors, parent = {key}, row["parent"]
                while parent is not None:
                    if parent in ancestors or parent not in rows:
                        raise Refusal("invalid retained lease ancestry")
                    ancestors.add(parent)
                    parent = rows[parent]["parent"]
                children = [r for r in rows.values() if r["parent"] == key and r["state"] in HELD]
                if any(sum(r["resources"][k] for r in children) > row["resources"][k] for k in ("cpu", "memory")):
                    raise Refusal("retained subgrants exceed parent envelope")
                if row["state"] in TERMINAL and children:
                    raise Refusal("released parent still owns descendants")
                if row["state"] in TERMINAL and any(not e["settled"] for e in row["executions"]):
                    raise Refusal("terminal lease has unproved execution cleanup")
            used = allocated(state)
            if any(used[k] > self.policy.cap[k] for k in used):
                raise Refusal("host allocation invariant violated")
        except (KeyError, TypeError, AttributeError) as error:
            raise Refusal(f"malformed retained authority: {error}") from error

    def event(self, state, event, lease=None, **details):
        """Retain a non-authoritative observation with its authority and policy identity."""
        value = dict(version=1, authority=state["authority"], host=state["host"],
                     boot=state["boot"], policy=state["policy"], at=state["now"],
                     event=event, lease=lease["id"] if lease else None, **details)
        if lease:
            value.update(parent=lease["parent"], project=lease["project"], work=lease["work"],
                         resources=lease["resources"], binding=lease["binding"])
        episode = state.get("pressure_episode")
        if (lease and episode and not episode.get("invalidated")
                and episode.get("recovered_sequence") is None
                and state["pressure"] == "pressure"
                and ((event == "denial" and details.get("reason") == "wait for fresh healthy sensors and recovery hysteresis")
                     or (event == "cancel" and details.get("reason") == "severe pressure"))):
            value["pressure_fault_sequence"] = episode["fault_sequence"]
        inserted = self.db.execute("INSERT INTO events(payload) VALUES(?)", (json.dumps(value),))
        sequence = inserted.lastrowid
        if lease:
            if event in ("request", "subgrant"):
                lease["admission_sequence"] = sequence
            if event in ("release", "cancel") and lease["state"] in TERMINAL:
                lease["settlement_sequence"] = sequence
            if event in ("attach", "cleanup"):
                execution = next(e for e in lease["executions"] if e["id"] == details["execution"])
                # A lost acknowledgement can replay cleanup after release. Keep
                # the first successful native proof inside the operation window.
                execution.setdefault("attach_sequence" if event == "attach" else "settlement_sequence", sequence)
            if "pressure_fault_sequence" in value:
                lease.setdefault("pressure_links", []).append(sequence)
        return sequence

    def events(self, after):
        """Return at most 100 observations after a sequence, without acknowledging delivery."""
        return [dict(json.loads(payload), sequence=seq) for seq, payload in
                self.db.execute("SELECT sequence,payload FROM events WHERE sequence>? ORDER BY sequence LIMIT 100", (after,))]

    def close(self):
        """Close this database connection without releasing any process reservation."""
        self.db.close()
