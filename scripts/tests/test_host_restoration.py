"""SH-871 native restoration provenance; private fixtures never contact a broker."""

import copy
from contextlib import ExitStack
import json
from pathlib import Path
import sys
from types import SimpleNamespace
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from host_admission import client, native
from host_admission.broker import Broker
from host_admission.policy import Refusal
from host_admission.restoration import proof
from test_host_admission import Fixture


BINDING = dict(attempt_id="attempt", execution_id="gate", generation=7)
BROKER = dict(pid=20, start="native:20", boot="boot")


class RestorationFixture(Fixture):
    def bound(self, name="work", **kwargs):
        return self.request(name, binding=dict(BINDING), **kwargs)

    def pressure(self):
        self.sample(cpu=850)
        event = next(e for e in reversed(self.a.events(0))
                     if e["event"] == "pressure" and e["reason"] == "pressure")
        return {k: event[k] for k in ("authority", "host", "boot", "policy", "sequence")}

    def recover(self):
        self.now += 1; self.sample()
        self.now += self.policy.value["recover_ms"]; self.sample()

    def finish(self, row):
        self.assertTrue(self.a.finish(row["id"], row["token"]))

    def request_proof(self, fault, *names):
        rows = [self.lease(name) for name in sorted(names)]
        return dict(nonce="one-use-native-request", fault=fault,
                    window=dict(start_sequence=min(r["admission_sequence"] for r in rows),
                                end_sequence=max(r["settlement_sequence"] for r in rows)),
                    affected=[dict(lease=r["id"], binding=r["binding"]) for r in rows])

    def complete(self):
        row = self.bound(); fault = self.pressure(); self.finish(row); self.recover()
        return row, self.request_proof(fault, row["id"])

class RestorationTests(RestorationFixture):
    def test_exact_episode_proof_is_read_only_and_contains_no_capabilities(self):
        row, request = self.complete()
        before = self.path.read_bytes()
        self.now += 1
        statements = []
        self.a.db.set_trace_callback(statements.append)
        result = proof(self.a, request, BROKER)
        self.a.db.set_trace_callback(None)
        self.assertEqual(self.path.read_bytes(), before)
        self.assertTrue(all(s.split()[0] in {"BEGIN", "SELECT", "COMMIT"} for s in statements))
        self.assertEqual(result["window"], request["window"])
        self.assertEqual(result["affected"], request["affected"])
        self.assertEqual(result["broker"], BROKER)
        self.assertEqual(result["checked_at"], self.now)
        self.assertGreater(result["episode"]["recovered_sequence"], request["fault"]["sequence"])
        self.assertNotIn(row["token"], json.dumps(result))

    def test_initial_ready_and_incomplete_hysteresis_are_not_restoration(self):
        row = self.bound(); self.finish(row)
        initial = self.a.events(0)[0]
        fault = {k: initial[k] for k in ("authority", "host", "boot", "policy", "sequence")}
        with self.assertRaisesRegex(Refusal, "has not recovered"):
            proof(self.a, self.request_proof(fault, row["id"]), BROKER)
        row = self.bound("second"); fault = self.pressure(); self.finish(row)
        self.now += 1; self.sample()
        self.now += self.policy.value["recover_ms"] - 1; self.sample()
        with self.assertRaisesRegex(Refusal, "has not recovered"):
            proof(self.a, self.request_proof(fault, row["id"]), BROKER)

    def test_native_denial_link_connects_an_earlier_active_pressure_episode(self):
        fault = self.pressure(); row = self.bound()
        self.assertEqual(row["state"], "queued")
        self.a.cancel(row["id"], row["token"])
        self.recover()
        request = self.request_proof(fault, row["id"])
        self.assertLess(fault["sequence"], request["window"]["start_sequence"])
        result = proof(self.a, request, BROKER)
        self.assertEqual(result["settled"][0]["state"], "cancelled")
        links = self.lease(row["id"])["pressure_links"]
        self.assertTrue(links)
        self.assertEqual(next(e for e in self.a.events(0) if e["sequence"] == links[0])
                         ["pressure_fault_sequence"], fault["sequence"])

    def test_historical_pressure_cannot_be_rebound_to_a_later_operation(self):
        fault = self.pressure(); self.recover()
        row = self.bound(); self.finish(row)
        with self.assertRaisesRegex(Refusal, "complete native same-binding"):
            proof(self.a, self.request_proof(fault, row["id"]), BROKER)

    def test_window_binding_subject_and_native_epoch_disagreement_refuse(self):
        _, request = self.complete()
        changes = [lambda r: r["window"].update(start_sequence=1),
                   lambda r: r["window"].update(end_sequence=999),
                   lambda r: r["affected"][0]["binding"].update(generation=8),
                   lambda r: r["affected"].append(copy.deepcopy(r["affected"][0]))]
        changes += [lambda r, k=k: r["fault"].update({k: "other"})
                    for k in ("authority", "host", "boot", "policy")]
        for change in changes:
            candidate = copy.deepcopy(request); change(candidate)
            with self.subTest(candidate=candidate), self.assertRaises(Refusal):
                proof(self.a, candidate, BROKER)

    def test_stale_restarted_sample_and_reboot_never_prove_recovery(self):
        _, request = self.complete()
        self.now += self.policy.value["stale_ms"] + 1
        self.a = self.open()
        with self.assertRaisesRegex(Refusal, "fresh healthy"):
            proof(self.a, request, BROKER)
        self.a = self.open("new-boot")
        with self.assertRaisesRegex(Refusal, "changed"):
            proof(self.a, request, dict(BROKER, boot="new-boot"))

    def test_a_gap_after_recovery_stays_invalid_after_later_green_samples(self):
        _, request = self.complete()
        self.now += self.policy.value["stale_ms"] + 1; self.sample()
        self.now += self.policy.value["recover_ms"]; self.sample()
        self.assertEqual(self.a.status()["pressure"], "ready")
        with self.assertRaisesRegex(Refusal, "has not recovered"):
            proof(self.a, request, BROKER)

    def test_unavailable_sensors_during_hysteresis_invalidate_original_episode(self):
        row = self.bound(); fault = self.pressure(); self.finish(row)
        self.now += 1; self.sample()
        self.a.sample(None); self.recover()
        self.assertEqual(self.a.status()["pressure"], "ready")
        with self.assertRaisesRegex(Refusal, "has not recovered"):
            proof(self.a, self.request_proof(fault, row["id"]), BROKER)

    def test_new_pressure_supersedes_old_episode_even_after_it_recovers(self):
        _, request = self.complete()
        later = self.pressure(); self.recover()
        self.assertGreater(later["sequence"], request["fault"]["sequence"])
        with self.assertRaisesRegex(Refusal, "has not recovered"):
            proof(self.a, request, BROKER)

    def test_unrelated_quarantine_blocks_proof_across_projects(self):
        _, request = self.complete()
        other = self.bound("other", project="another-project")
        self.a.quarantine(other["id"], "unknown native descendant")
        self.assertEqual(self.a.status()["pressure"], "ready")
        with self.assertRaisesRegex(Refusal, "quarantined"):
            proof(self.a, request, BROKER)

    def test_live_descendant_requires_native_settlement_before_parent_release(self):
        row = self.bound()
        child = self.a.subgrant(row["id"], row["token"], "child", dict(cpu=1, memory=100))
        execution = dict(id="child-execution", leader=self.owner, session=42, guard="fixture-guard")
        self.a.attach(child["id"], child["token"], execution)
        fault = self.pressure(); self.recover()
        self.assertFalse(self.a.finish(row["id"], row["token"]))
        candidate = dict(nonce="held", fault=fault, window=dict(start_sequence=1, end_sequence=999),
                         affected=[dict(lease=row["id"], binding=BINDING)])
        with self.assertRaisesRegex(Refusal, "settled subject"):
            proof(self.a, candidate, BROKER)
        self.a.settle(child["id"], child["token"], execution["id"], lambda _: True)
        self.finish(child); self.finish(row)
        result = proof(self.a, self.request_proof(fault, row["id"]), BROKER)
        lifetimes = result["settled"][0]["lifetimes"]
        self.assertEqual([r["lease"] for r in lifetimes], ["child", "work"])
        self.assertEqual(lifetimes[0]["executions"][0]["leader"], self.owner)
        self.assertNotIn("fixture-guard", json.dumps(result))

    def test_legacy_leases_and_execution_booleans_without_event_links_refuse(self):
        row = self.bound()
        execution = dict(id="run", leader=self.owner, session=42, guard="fixture-guard")
        self.a.attach(row["id"], row["token"], execution)
        fault = self.pressure()
        self.a.settle(row["id"], row["token"], execution["id"], lambda _: True)
        self.finish(row); self.recover()
        request = self.request_proof(fault, row["id"])
        proof(self.a, request, BROKER)
        with self.a.transaction() as state:
            saved = state["leases"][row["id"]]["executions"][0].pop("settlement_sequence")
        with self.assertRaisesRegex(Refusal, "legacy execution"):
            proof(self.a, request, BROKER)
        with self.a.transaction() as state:
            state["leases"][row["id"]]["executions"][0]["settlement_sequence"] = saved
            state["leases"][row["id"]].pop("admission_sequence")
        with self.assertRaisesRegex(Refusal, "legacy root"):
            proof(self.a, request, BROKER)

    def test_omitted_same_binding_affected_root_refuses_but_unrelated_work_is_allowed(self):
        first = self.bound("first"); second = self.bound("second")
        unrelated = self.request("unrelated", binding=dict(BINDING, generation=8))
        fault = self.pressure(); self.finish(first); self.recover()
        with self.assertRaisesRegex(Refusal, "complete native same-binding"):
            proof(self.a, self.request_proof(fault, first["id"]), BROKER)
        self.finish(second)
        result = proof(self.a, self.request_proof(fault, first["id"], second["id"]), BROKER)
        self.assertEqual([r["lease"] for r in result["affected"]], ["first", "second"])
        self.assertEqual(self.lease(unrelated["id"])["state"], "reserved")

    def test_replayed_native_settlement_after_release_keeps_first_cleanup_boundary(self):
        row = self.bound()
        execution = dict(id="run", leader=self.owner, session=42, guard="fixture-guard")
        self.a.attach(row["id"], row["token"], execution)
        fault = self.pressure()
        self.a.settle(row["id"], row["token"], execution["id"], lambda _: True)
        self.finish(row); self.recover()
        request = self.request_proof(fault, row["id"])
        before = proof(self.a, request, BROKER)["settled"]
        event_count = len(self.a.events(0))
        self.a.settle(row["id"], row["token"], execution["id"],
                      lambda _: self.fail("completed native lifetime was probed again"))
        self.assertEqual(len(self.a.events(0)), event_count)
        self.assertEqual(proof(self.a, request, BROKER)["settled"], before)

    def test_omitted_terminal_root_cannot_hide_behind_a_false_early_boundary(self):
        first = self.bound("first"); second = self.bound("second")
        fault = self.pressure(); self.finish(first); self.finish(second); self.recover()
        request = self.request_proof(fault, first["id"])
        with self.a.transaction() as state:
            row = state["leases"][second["id"]]
            saved = row["settlement_sequence"]
            row["settlement_sequence"] = row["admission_sequence"]
        with self.assertRaisesRegex(Refusal, "operation window boundaries"):
            proof(self.a, request, BROKER)
        with self.a.transaction() as state:
            state["leases"][second["id"]]["settlement_sequence"] = saved
        result = proof(self.a, self.request_proof(fault, first["id"], second["id"]), BROKER)
        self.assertEqual(len(result["settled"]), 2)

    def test_real_pre_pressure_drain_event_cannot_hide_a_later_released_root(self):
        first = self.bound("first"); second = self.bound("second")
        self.a.cancel(second["id"], second["token"])
        self.assertEqual(self.lease(second["id"])["state"], "draining")
        cancellation = next(e["sequence"] for e in self.a.events(0)
                            if e["event"] == "cancel" and e["lease"] == second["id"])
        fault = self.pressure(); self.finish(first); self.finish(second); self.recover()
        with self.a.transaction() as state:
            state["leases"][second["id"]]["settlement_sequence"] = cancellation
        with self.assertRaisesRegex(Refusal, "retained terminal state"):
            proof(self.a, self.request_proof(fault, first["id"]), BROKER)

    def test_broker_dispatch_refuses_unknown_fields_and_unobservable_incarnation(self):
        _, request = self.complete()
        broker = SimpleNamespace(authority=self.a, identity=BROKER, boot="boot")
        message = dict(version=1, operation="restoration-proof", **request)
        with patch.object(native, "observe", return_value=True):
            result = Broker.dispatch(broker, message, self.owner)
            self.assertEqual(result["nonce"], request["nonce"])
            with self.assertRaisesRegex(Refusal, "fields"):
                Broker.dispatch(broker, dict(message, restored=True), self.owner)
        with patch.object(native, "observe", return_value=None):
            with self.assertRaisesRegex(Refusal, "incarnation"):
                Broker.dispatch(broker, message, self.owner)


class ReplySocket:
    """A bounded in-memory transport, not a fake production restoration authority."""
    def __init__(self, answer):
        self.answer = json.dumps(dict(version=1, value=answer)).encode() + b"\n"
        self.sent = None

    def __enter__(self): return self
    def __exit__(self, *_): return False
    def settimeout(self, _): pass
    def connect(self, _): pass
    def sendall(self, payload): self.sent = json.loads(payload)
    def recv(self, _):
        data, self.answer = self.answer, b""
        return data


class RestorationClientTests(RestorationFixture):
    def transport(self, request, *, mutate=None, peers=None, inode_change=False, policy_change=False, receipt_delay=0, changed_boot=False):
        result = proof(self.a, request, BROKER)
        if mutate: mutate(result)
        channel = ReplySocket(result)
        stats = [SimpleNamespace(st_dev=1, st_ino=2),
                 SimpleNamespace(st_dev=1, st_ino=3 if inode_change else 2)]
        policies = [self.policy, SimpleNamespace(digest="changed") if policy_change else self.policy]
        with ExitStack() as stack:
            stack.enter_context(patch.object(client, "ROOT", self.tmp.name))
            stack.enter_context(patch.object(client, "directory"))
            stack.enter_context(patch.object(client, "check_file", side_effect=stats))
            stack.enter_context(patch.object(client.socket, "socket", return_value=channel))
            stack.enter_context(patch.object(native, "host_identity", return_value="fixture-host"))
            stack.enter_context(patch.object(native, "boot_identity", side_effect=["boot", "new-boot" if changed_boot else "boot"]))
            stack.enter_context(patch.object(client.time, "monotonic_ns", return_value=(self.now + receipt_delay) * 1_000_000))
            stack.enter_context(patch.object(client.time, "monotonic", return_value=self.now / 1000))
            stack.enter_context(patch.object(native, "peer_identity", side_effect=peers or [BROKER, BROKER]))
            stack.enter_context(patch("host_admission.activation.load_policy", side_effect=policies))
            answer = client.Client(self.tmp.name).restoration_proof(**request)
        self.assertEqual(channel.sent["operation"], "restoration-proof")
        return answer

    def test_live_client_accepts_only_matching_nonce_peer_and_measured_policy(self):
        _, request = self.complete()
        self.assertEqual(self.transport(request)["nonce"], request["nonce"])
        mutations = [lambda r: r.update(nonce="replayed"),
                     lambda r: r.update(broker=dict(BROKER, start="reused")),
                     lambda r: r["fault"].update(sequence=999),
                     lambda r: r["affected"][0]["binding"].update(generation=8)]
        for mutation in mutations:
            with self.subTest(mutation=mutation), self.assertRaisesRegex(Refusal, "live native request"):
                self.transport(request, mutate=mutation)
        with self.assertRaisesRegex(Refusal, "measured policy changed"):
            self.transport(request, policy_change=True)
        with self.assertRaisesRegex(Refusal, "boot or measured policy changed"):
            self.transport(request, changed_boot=True)

    def test_sample_must_still_be_fresh_at_receipt_not_only_when_broker_checked(self):
        _, request = self.complete()
        self.assertEqual(self.transport(request, receipt_delay=self.policy.value["stale_ms"])["nonce"], request["nonce"])
        with self.assertRaisesRegex(Refusal, "stale or unhealthy at native receipt"):
            self.transport(request, receipt_delay=self.policy.value["stale_ms"] + 1)
        with self.assertRaisesRegex(Refusal, "valid native sample clock"):
            self.transport(request, mutate=lambda r: r.update(checked_at=True))

    def test_native_peer_and_endpoint_replacement_refuse(self):
        _, request = self.complete()
        with self.assertRaisesRegex(Refusal, "incarnation changed"):
            self.transport(request, peers=[BROKER, dict(BROKER, start="new")])
        with self.assertRaisesRegex(Refusal, "endpoint changed"):
            self.transport(request, inode_change=True)

    def test_fixture_endpoint_cannot_be_used_as_production_restoration_authority(self):
        _, request = self.complete()
        with self.assertRaisesRegex(Refusal, "canonical host endpoint"):
            client.Client(self.tmp.name).restoration_proof(**request)


if __name__ == "__main__":
    unittest.main()
