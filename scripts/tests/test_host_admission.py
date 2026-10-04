"""Deterministic admission contracts; fixture values are not production policy."""

import copy
import os
from pathlib import Path
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from host_admission.authority import Authority
from host_admission.policy import Policy, Refusal


def policy_value():
    """A deliberately small synthetic host, with a separate repair reserve."""
    return dict(version=1, host="fixture-host", calibration="fixture",
                measurements=["fixture://SH-801"], capacity=dict(cpu=10, memory=1000),
                headroom=dict(cpu=2, memory=100), reserve=dict(cpu=2, memory=200),
                weights=dict(build=1, test=1, release=1, repair=1),
                project_weight=1, sample_ms=10, stale_ms=30, recover_ms=20,
                starvation_ms=100, lease_ms=1000, cleanup_ms=100,
                thresholds=dict(cpu=[500, 800, 950], memory=[100, 500, 900],
                                runnable=[2, 8, 20]),
                workloads=dict(compile=dict(cpu=2, memory=200)))


class Fixture(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="sh868-", dir="/tmp")
        self.addCleanup(self.tmp.cleanup)
        self.path = Path(self.tmp.name) / "state.db"
        self.now = 1000
        self.live = {}
        self.policy = Policy(policy_value(), "fixture-host", fixture=True)
        self.owner = dict(pid=10, start="native:10", boot="boot")
        self.live[10] = self.owner
        self.a = self.open()
        self.sample()

    def open(self, boot="boot"):
        a = Authority(self.path, self.policy, boot, lambda: self.now,
                      lambda owner: self.live.get(owner["pid"]) == owner)
        self.addCleanup(a.close)
        return a

    def sample(self, **changes):
        value = dict(at=self.now, available=2000, cpu=100, memory=0, runnable=1)
        value.update(changes)
        self.a.sample(value)

    def request(self, name, cpu=2, memory=200, project="a", work="build", **kw):
        return self.a.enqueue(dict(id=name, project=project, work=work,
                                   resources=dict(cpu=cpu, memory=memory), **kw), self.owner)

    def lease(self, name):
        return self.a.inspect(name)


class PolicyTests(unittest.TestCase):
    def test_fixture_cannot_activate_production(self):
        with self.assertRaisesRegex(Refusal, "calibration"):
            Policy(policy_value(), "fixture-host")

    def test_invalid_policy_is_never_a_fallback(self):
        for field, value in [("host", "other"), ("capacity", dict(cpu=True, memory=100)),
                             ("version", True),
                             ("sample_ms", 0), ("recover_ms", -1),
                             ("measurements", []), ("weights", dict(build=1)),
                             ("reserve", dict(cpu=20, memory=200))]:
            with self.subTest(field=field):
                p = policy_value(); p[field] = value
                with self.assertRaises(Refusal):
                    Policy(p, "fixture-host", fixture=True)


class AccountingTests(Fixture):
    def test_policy_revision_requires_idle_state_and_keeps_old_evidence(self):
        row = self.request("held")
        before = self.a.events(0)
        p = policy_value(); p["capacity"]["cpu"] += 1
        self.policy = Policy(p, "fixture-host", fixture=True)
        with self.assertRaisesRegex(Refusal, "policy"):
            self.open()
        self.a.finish(row["id"], row["token"])
        revised = self.open()
        self.assertEqual(revised.events(0)[:len(before)], before)
        self.assertNotEqual(revised.status()["authority"], before[0]["authority"])
        self.assertEqual(revised.status()["policy"], self.policy.digest)
        self.assertIn("policy revision", revised.events(0)[-1]["reason"])

    def test_status_retains_wait_peaks_and_recovery_without_capabilities(self):
        held = self.request("held", cpu=6)
        self.request("queued")
        self.now += 11
        self.a.usage("held", held["token"], dict(cpu=None, memory=120))
        status = self.a.status()
        rows = {r["id"]: r for r in status["leases"]}
        self.assertEqual(rows["queued"]["queue_position"], 1)
        self.assertEqual(rows["queued"]["wait_ms"], 11)
        self.assertIsNone(rows["held"]["peaks"]["cpu"])
        self.assertEqual(rows["held"]["peaks"]["memory"], 120)
        self.assertNotIn(held["token"], str(status))
        self.assertEqual(status["sample"]["at"], 1000)

    def test_evidence_pages_are_bounded_and_contiguous(self):
        with self.a.transaction() as state:
            for _ in range(110):
                self.a.event(state, "pressure", reason="fixture observation")
        first = self.a.events(0)
        self.assertEqual(len(first), 100)
        rest = self.a.events(first[-1]["sequence"])
        self.assertEqual(rest[0]["sequence"], first[-1]["sequence"] + 1)
        self.assertEqual(len(first) + len(rest), 111)

    def test_cap_and_reserve_are_shared_across_projects(self):
        self.request("a", 6, 600)
        self.request("b", project="b")
        self.assertEqual(self.lease("a")["state"], "reserved")
        self.assertEqual(self.lease("b")["state"], "queued")
        self.request("repair", work="repair", project="b")
        self.assertEqual(self.lease("repair")["state"], "reserved")
        self.assertEqual(self.a.status()["allocated"], dict(cpu=8, memory=800))

    def test_request_replay_does_not_double_allocate(self):
        first = self.request("a")
        self.assertEqual(self.request("a"), first)
        with self.assertRaisesRegex(Refusal, "changed"):
            self.request("a", cpu=3)
        self.assertEqual(self.a.status()["allocated"]["cpu"], 2)

    def test_invalid_requests_cannot_change_host_policy(self):
        for kw in [dict(cpu=0), dict(cpu=True), dict(memory=-1), dict(cpu=2**64),
                   dict(cpu=7), dict(work="unknown"), dict(capacity=100),
                   dict(project="")]:
            with self.subTest(kw=kw), self.assertRaises(Refusal):
                self.request("bad", **kw)
        self.assertEqual(self.a.status()["allocated"]["cpu"], 0)

    def test_nested_partitions_do_not_double_charge_or_release_parent_early(self):
        parent = self.request("parent", 6, 600)
        child = self.a.subgrant("parent", parent["token"], "child", dict(cpu=4, memory=400))
        self.assertEqual(self.a.status()["allocated"]["cpu"], 6)
        with self.assertRaisesRegex(Refusal, "partition"):
            self.a.subgrant("parent", parent["token"], "overflow", dict(cpu=3, memory=300))
        self.a.cancel("parent", parent["token"])
        self.assertEqual(self.lease("child")["state"], "draining")
        self.assertFalse(self.a.finish("parent", parent["token"]))
        self.assertTrue(self.a.finish("child", child["token"]))
        self.assertTrue(self.a.finish("parent", parent["token"]))
        self.assertEqual(self.a.status()["allocated"]["cpu"], 0)

    def test_subgrant_observations_belong_only_to_that_partition(self):
        parent = self.request("parent", 6, 600)
        self.a.usage("parent", parent["token"], dict(cpu=5, memory=500))
        child = self.a.subgrant("parent", parent["token"], "child", dict(cpu=4, memory=400))
        self.assertIsNone(child.get("peaks"))
        self.a.usage("child", child["token"], dict(cpu=None, memory=100))
        self.assertEqual(self.lease("child")["peaks"], dict(cpu=None, memory=100))
        grandchild = self.a.subgrant("child", child["token"], "grandchild", dict(cpu=2, memory=200))
        self.assertIsNone(grandchild.get("peaks"))
        self.assertEqual(self.lease("parent")["peaks"], dict(cpu=5, memory=500))

    def test_nested_root_upgrade_is_refused_without_waiting(self):
        parent = self.request("p")
        with self.assertRaisesRegex(Refusal, "subgrant"):
            self.request("upgrade", parent=parent["token"])

    def test_queued_cancellation_never_becomes_a_grant(self):
        self.request("busy", 6, 600)
        waiting = self.request("wait")
        self.a.cancel("wait", waiting["token"])
        self.assertEqual(self.lease("wait")["state"], "cancelled")
        self.assertEqual(self.a.status()["allocated"]["cpu"], 6)


class PressureTests(Fixture):
    def test_sensor_failure_and_stale_samples_stop_even_repair_grants(self):
        self.a.sample(None)
        self.request("repair", work="repair")
        self.assertEqual(self.lease("repair")["state"], "queued")
        self.sample()
        self.now += self.policy.value["recover_ms"]
        self.sample()
        self.assertEqual(self.lease("repair")["state"], "reserved")
        self.now += self.policy.value["stale_ms"] + 1
        self.request("stale")
        self.assertEqual(self.lease("stale")["state"], "queued")

    def test_hysteresis_requires_continuous_recovery(self):
        self.sample(cpu=850)
        self.request("a")
        self.now += 10; self.sample()
        self.now += 10; self.sample(cpu=850)
        self.now += 10; self.sample()
        self.assertEqual(self.lease("a")["state"], "queued")
        self.now += 20; self.sample()
        self.assertEqual(self.lease("a")["state"], "reserved")

    def test_external_memory_and_cpu_pressure_are_independent(self):
        for metric in [dict(available=100), dict(memory=600), dict(cpu=900), dict(runnable=9)]:
            with self.subTest(metric=metric):
                self.sample(**metric)
                self.request(str(metric))
                self.assertEqual(self.lease(str(metric))["state"], "queued")

    def test_severe_pressure_drains_but_retains_capacity(self):
        self.request("busy")
        self.sample(memory=950)
        self.assertEqual(self.lease("busy")["state"], "draining")
        self.assertEqual(self.a.status()["allocated"]["cpu"], 2)


class RecoveryTests(Fixture):
    def test_restart_retains_grants_and_idempotency(self):
        grant = self.request("a")
        restarted = self.open()
        self.assertEqual(restarted.inspect("a")["token"], grant["token"])
        self.assertEqual(restarted.status()["allocated"]["cpu"], 2)

    def test_pid_reuse_quarantines_and_never_reclaims(self):
        self.request("a")
        self.live[10] = dict(self.owner, start="other")
        restarted = self.open()
        self.assertEqual(restarted.inspect("a")["state"], "quarantined")
        self.assertEqual(restarted.status()["allocated"]["cpu"], 2)

    def test_confirmed_new_boot_retires_old_reservations(self):
        self.request("a")
        restarted = self.open("new-boot")
        self.assertEqual(restarted.status()["allocated"]["cpu"], 0)
        self.assertEqual(restarted.inspect("a")["reason"], "confirmed reboot")

    def test_events_survive_reopen_with_unique_sequences(self):
        self.request("a")
        events = self.a.events(0)
        self.assertTrue(any(e["event"] == "grant" for e in events))
        restarted = self.open()
        self.assertEqual(restarted.events(0)[:len(events)], events)
        seq = [e["sequence"] for e in restarted.events(0)]
        self.assertEqual(seq, sorted(set(seq)))


class FairnessTests(Fixture):
    def test_work_class_weights_preserve_a_positive_release_share(self):
        p = policy_value(); p["weights"]["build"] = 3
        self.policy = Policy(p, "fixture-host", fixture=True)
        self.a = self.open()
        self.a.sample(None)
        for n in range(40):
            for work in ("build", "release"):
                self.request(f"{work}-{n}", 6, 600, work=work)
        self.sample(); self.now += 20; self.sample()
        order = []
        for _ in range(32):
            held = next(r for r in self.a.status()["leases"] if r["state"] == "reserved")
            order.append(held["work"])
            row = self.lease(held["id"])
            self.a.finish(row["id"], row["token"])
        self.assertEqual(order.count("build"), 24)
        self.assertEqual(order.count("release"), 8)

    def test_full_quantum_requests_rotate_between_projects(self):
        self.a.sample(None)
        for n in range(4):
            for project in ("a", "b"):
                self.request(f"{project}{n}", 8, 800, project=project, work="repair")
        self.sample(); self.now += 20; self.sample()
        order = []
        for _ in range(8):
            held = next(r for r in self.a.status()["leases"] if r["state"] == "reserved")
            order.append(held["project"])
            row = self.lease(held["id"])
            self.a.finish(row["id"], row["token"])
        self.assertEqual(order, ["a", "b"] * 4)

    def test_old_large_request_stops_small_backfill_but_preserves_repair(self):
        first = self.request("first", 4, 400)
        self.request("large", 6, 600, project="b")
        for _ in range(10):
            self.now += 10
            self.sample()
        self.request("small")
        self.assertEqual(self.lease("small")["state"], "queued")
        self.request("repair", work="repair")
        self.assertEqual(self.lease("repair")["state"], "reserved")
        self.a.finish("first", first["token"])
        self.assertEqual(self.lease("large")["state"], "reserved")

    def test_cancellation_and_unknown_owner_cannot_bypass_queue(self):
        row = self.request("first", 6, 600)
        self.request("next", project="b")
        self.a.cancel("first", row["token"])
        self.live.clear()
        self.sample()
        self.assertFalse(self.a.finish("first", row["token"]))
        self.assertEqual(self.lease("next")["state"], "cancelled")
        self.assertEqual(self.a.status()["allocated"]["cpu"], 6)


class ExecutionTests(Fixture):
    def test_attached_execution_requires_independent_settlement(self):
        row = self.request("a")
        child = dict(pid=20, start="native:20", boot="boot")
        self.live[20] = child
        execution = dict(id="e", leader=child, session=20, guard="e.lock")
        self.a.attach("a", row["token"], execution)
        self.assertFalse(self.a.finish("a", row["token"]))
        self.a.settle("a", row["token"], "e", lambda _: False)
        self.assertFalse(self.a.finish("a", row["token"]))
        self.live.pop(20)
        self.a.settle("a", row["token"], "e", lambda _: True)
        self.assertTrue(self.a.finish("a", row["token"]))

    def test_cancel_wins_race_with_launch(self):
        row = self.request("a")
        self.a.cancel("a", row["token"])
        with self.assertRaises(Refusal):
            self.a.attach("a", row["token"], dict(id="e"))

    def test_lost_cleanup_proof_is_quarantine_not_release(self):
        row = self.request("a")
        child = dict(pid=20, start="native:20", boot="boot")
        self.live[20] = child
        self.a.attach("a", row["token"], dict(id="e", leader=child, session=20, guard="e.lock"))
        self.a.settle("a", row["token"], "e", lambda _: None)
        self.assertEqual(self.lease("a")["state"], "quarantined")
        self.assertFalse(self.a.finish("a", row["token"]))

    def test_peak_overrun_cancels_without_reclaim(self):
        row = self.request("a")
        self.a.usage("a", row["token"], dict(cpu=3, memory=201))
        self.assertEqual(self.lease("a")["state"], "draining")
        self.assertEqual(self.a.status()["allocated"]["cpu"], 2)
        self.assertEqual(self.lease("a")["peaks"], dict(cpu=3, memory=201))


class IntegrityTests(Fixture):
    def test_dead_queued_owner_cannot_be_granted_after_capacity_frees(self):
        first = self.request("first", cpu=6)
        other = dict(pid=11, start="native:11", boot="boot")
        self.live[11] = other
        self.a.enqueue(dict(id="orphan", project="b", work="test", resources=dict(cpu=2, memory=100)), other)
        del self.live[11]
        self.sample()
        self.a.finish("first", first["token"])
        self.assertEqual(self.lease("orphan")["state"], "cancelled")
        self.assertEqual(self.a.status()["allocated"]["cpu"], 0)

    def test_repair_backfill_cannot_consume_capacity_needed_by_aged_release(self):
        first = self.request("first", cpu=2)
        self.request("release", cpu=6, work="release")
        # A full-capacity repair request must wait once an ordinary request ages.
        self.now += self.policy.value["starvation_ms"]
        self.sample(); self.now += self.policy.value["recover_ms"]; self.sample()
        self.a.enqueue(dict(id="repair", project="b", work="repair", resources=dict(cpu=6, memory=200)), self.owner)
        self.sample()
        self.a.finish("first", first["token"])
        self.now += self.policy.value["recover_ms"]; self.sample()
        self.assertEqual(self.lease("release")["state"], "reserved")
        self.assertEqual(self.lease("repair")["state"], "queued")

    def test_terminal_execution_with_unproved_cleanup_is_corrupt(self):
        row = self.request("first")
        child = dict(pid=20, start="native:20", boot="boot"); self.live[20] = child
        self.a.attach("first", row["token"], dict(id="e", leader=child, session=20, guard="e.lock"))
        self.a.db.execute("UPDATE authority SET payload=json_set(payload, '$.leases.first.state', 'released')")
        with self.assertRaises(Refusal):
            self.open()

    def test_independent_cleanup_proof_can_recover_a_dead_supervisor_grant(self):
        row = self.request("a")
        child = dict(pid=20, start="native:20", boot="boot")
        self.live[20] = child
        self.a.attach("a", row["token"], dict(id="e", leader=child, session=20, guard="e.lock"))
        self.live.clear()
        self.sample()
        self.assertFalse(self.a.finish("a", row["token"]))
        self.a.settle("a", row["token"], "e", lambda _: True)
        self.sample()
        self.assertTrue(self.a.finish("a", row["token"]))
        self.assertEqual(self.a.status()["allocated"]["cpu"], 0)

    def test_corrupt_negative_reservation_cannot_create_capacity(self):
        self.request("a")
        self.a.db.execute("UPDATE authority SET payload=json_set(payload, '$.leases.a.resources.cpu', -20)")
        with self.assertRaises(Refusal):
            self.open()

    def test_failed_evidence_write_rolls_back_the_grant(self):
        self.a.db.execute("CREATE TRIGGER reject_events BEFORE INSERT ON events BEGIN SELECT RAISE(ABORT, 'disk failure'); END")
        import sqlite3
        with self.assertRaises(sqlite3.Error):
            self.request("a")
        self.assertEqual(self.a.status()["allocated"]["cpu"], 0)
        self.assertEqual(self.a.status()["leases"], [])

    def test_unobserved_recovery_gap_does_not_count_as_healthy(self):
        self.sample(cpu=900)
        self.now += 10; self.sample()
        self.request("a")
        self.now += 100; self.sample()
        self.assertEqual(self.lease("a")["state"], "queued")
        self.now += 20; self.sample()
        self.assertEqual(self.lease("a")["state"], "reserved")


if __name__ == "__main__":
    unittest.main()
