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
from host_admission.restoration import fault_proof, proof
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
    def transport(self, request, *, mutate=None, peers=None, inode_change=False, policy_change=False, receipt_delay=0, changed_boot=False, fault_only=False):
        result = (fault_proof if fault_only else proof)(self.a, request, BROKER)
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
            transport = client.Client(self.tmp.name)
            answer = (transport.fault_proof if fault_only else transport.restoration_proof)(**request)
        self.assertEqual(channel.sent["operation"], "fault-proof" if fault_only else "restoration-proof")
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


class FaultProofTests(RestorationFixture):
    transport = RestorationClientTests.transport

    def fault_case(self):
        fault = self.pressure()
        row = self.bound(project="native-project", work="repair")
        self.a.cancel(row["id"], row["token"])
        return row, self.request_proof(fault, row["id"])

    def test_fault_enrollment_under_pressure_is_read_only_and_not_restoration(self):
        row, request = self.fault_case()
        self.a.sample(None)  # Fault enrollment must not invent sensor recovery.
        before = self.path.read_bytes()
        result = fault_proof(self.a, request, BROKER)
        self.assertEqual(self.path.read_bytes(), before)
        self.assertEqual(result["kind"], "host-pressure-fault")
        self.assertNotIn("episode", result); self.assertNotIn("sample", result)
        self.assertEqual(result["timing"], {"stale_ms": self.policy.value["stale_ms"]})
        subject = result["settled"][0]
        self.assertEqual((subject["project"], subject["work"]), ("native-project", "repair"))
        self.assertEqual(subject["pressure_links"], [dict(sequence=row["pressure_links"][0], event="denial",
            reason="wait for fresh healthy sensors and recovery hysteresis",
            pressure_fault_sequence=request["fault"]["sequence"])])
        self.assertNotIn(row["token"], json.dumps(result))
        with self.assertRaisesRegex(Refusal, "has not recovered"):
            proof(self.a, request, BROKER)

    def test_overlap_only_can_restore_but_cannot_causally_enroll(self):
        _, request = self.complete()
        self.assertEqual(proof(self.a, request, BROKER)["settled"][0]["pressure_links"], [])
        with self.assertRaisesRegex(Refusal, "native causal"):
            fault_proof(self.a, request, BROKER)

    def test_each_binding_needs_native_cause_while_companion_roots_are_retained(self):
        companion = self.bound("companion")
        other = self.request("other", binding=dict(BINDING, generation=8))
        fault = self.pressure(); denied = self.bound("denied")
        self.finish(companion); self.finish(other)
        self.a.cancel(denied["id"], denied["token"])
        result = fault_proof(self.a, self.request_proof(fault, "companion", "denied"), BROKER)
        self.assertEqual(result["settled"][0]["pressure_links"], [])
        self.assertTrue(result["settled"][1]["pressure_links"])
        with self.assertRaisesRegex(Refusal, "every binding"):
            fault_proof(self.a, self.request_proof(fault, "companion", "denied", "other"), BROKER)
        other_denied = self.request("other-denied", binding=dict(BINDING, generation=8))
        self.a.cancel(other_denied["id"], other_denied["token"])
        result = fault_proof(self.a, self.request_proof(fault, "companion", "denied", "other", "other-denied"), BROKER)
        self.assertEqual(len(result["settled"]), 4)

    def test_forged_and_foreign_causal_links_are_not_native_enrollment(self):
        fault = self.pressure(); row = self.bound()
        foreign = self.request("foreign", binding=dict(BINDING, generation=8))
        self.a.cancel(row["id"], row["token"]); self.a.cancel(foreign["id"], foreign["token"])
        request = self.request_proof(fault, row["id"])
        fault_proof(self.a, request, BROKER)
        with self.assertRaisesRegex(Refusal, "request"):
            fault_proof(self.a, dict(request, pressure_links=[dict(sequence=123)]), BROKER)
        for sequence in [foreign["pressure_links"][0], row["admission_sequence"]]:
            with self.a.transaction() as state:
                state["leases"][row["id"]]["pressure_links"] = [sequence]
            with self.subTest(sequence=sequence), self.assertRaisesRegex(Refusal, "native pressure link"):
                fault_proof(self.a, request, BROKER)

    def test_native_project_and_work_cannot_be_relabelled(self):
        row, request = self.fault_case()
        for field, replacement in [("project", "other-project"), ("work", "test")]:
            with self.a.transaction() as state:
                original = state["leases"][row["id"]][field]
                state["leases"][row["id"]][field] = replacement
            with self.subTest(field=field), self.assertRaisesRegex(Refusal, "foreign operation window"):
                fault_proof(self.a, request, BROKER)
            with self.a.transaction() as state:
                state["leases"][row["id"]][field] = original

    def test_fault_enrollment_keeps_native_subtree_and_epoch_custody(self):
        row = self.bound(); child = self.a.subgrant(row["id"], row["token"], "child", dict(cpu=1, memory=100))
        execution = dict(id="run", leader=self.owner, session=42, guard="fixture-guard")
        self.a.attach(child["id"], child["token"], execution)
        fault = self.pressure(); self.sample(cpu=960)  # Same episode, actual native severe cancellation.
        candidate = dict(nonce="held", fault=fault, window=dict(start_sequence=1, end_sequence=999),
                         affected=[dict(lease=row["id"], binding=BINDING)])
        self.assertFalse(self.a.finish(row["id"], row["token"]))
        with self.assertRaisesRegex(Refusal, "settled subject"):
            fault_proof(self.a, candidate, BROKER)
        self.a.settle(child["id"], child["token"], execution["id"], lambda _: True)
        self.finish(child); self.finish(row)
        request = self.request_proof(fault, row["id"])
        result = fault_proof(self.a, request, BROKER)
        self.assertEqual(result["settled"][0]["pressure_links"][0]["event"], "cancel")
        self.assertEqual(len(result["settled"][0]["lifetimes"]), 2)
        self.a = self.open("new-boot")
        with self.assertRaisesRegex(Refusal, "changed"):
            fault_proof(self.a, request, dict(BROKER, boot="new-boot"))

    def test_unrelated_quarantine_does_not_prevent_fault_hold_but_blocks_restoration(self):
        unrelated = self.request("unrelated", project="another", binding=dict(BINDING, generation=8))
        row, request = self.fault_case()
        self.a.quarantine(unrelated["id"], "native lifetime unknown")
        fault_proof(self.a, request, BROKER)
        self.recover()
        with self.assertRaisesRegex(Refusal, "quarantined"):
            proof(self.a, request, BROKER)

    def test_fault_and_restoration_receipts_share_only_native_causal_links(self):
        _, request = self.fault_case()
        enrolled = fault_proof(self.a, request, BROKER)
        self.recover()
        restored = proof(self.a, request, BROKER)
        self.assertEqual(enrolled["settled"], restored["settled"])
        self.assertIn("episode", restored); self.assertNotIn("episode", enrolled)
        broker = SimpleNamespace(authority=self.a, identity=BROKER, boot="boot")
        with patch.object(native, "observe", return_value=True):
            result = Broker.dispatch(broker, dict(version=1, operation="fault-proof", **request), self.owner)
        self.assertEqual(result["kind"], "host-pressure-fault")

    def test_fault_client_requires_its_own_kind_live_peer_policy_and_fresh_clock(self):
        _, request = self.fault_case()
        self.assertEqual(self.transport(request, fault_only=True)["kind"], "host-pressure-fault")
        for mutation in [lambda r: r.update(kind="host-pressure-restoration"),
                         lambda r: r.update(nonce="old"), lambda r: r.update(episode={}),
                         lambda r: r.update(timing={"stale_ms": 999}),
                         lambda r: r.update(checked_at=True)]:
            with self.subTest(mutation=mutation), self.assertRaises(Refusal):
                self.transport(request, fault_only=True, mutate=mutation)
        for kwargs in [dict(peers=[BROKER, dict(BROKER, start="reused")]),
                       dict(changed_boot=True), dict(policy_change=True), dict(inode_change=True),
                       dict(receipt_delay=self.policy.value["stale_ms"] + 1)]:
            with self.subTest(kwargs=kwargs), self.assertRaises(Refusal):
                self.transport(request, fault_only=True, **kwargs)
        with self.assertRaisesRegex(Refusal, "canonical host endpoint"):
            client.Client(self.tmp.name).fault_proof(**request)


if __name__ == "__main__":
    unittest.main()
