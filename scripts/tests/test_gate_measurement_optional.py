"""Narrow timeout policy, retained custody and versioned replay; no campaign."""
import copy
import fcntl
import json
import os
from pathlib import Path
import signal
import sys
import tempfile
import time
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import build_products
import gate_measurement_command as command
import gate_measurement_optional as optional
from gate_measurement_cohorts import Cohort
from gate_measurement_bounds import Deadline
from host_admission import supervisor
from host_admission.policy import Refusal as CustodyRefusal
from verifier_state import Refusal
from test_gate_measurement_cohorts import identity


class Fixture(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix='optional-policy-', dir='/tmp')
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name).resolve()


class Quiescence(Fixture):
    def setUp(self):
        super().setUp()
        self.directory = self.root / 'build-fixture'; self.directory.mkdir(mode=0o700)
        token = 'a' * 32
        self.guard = self.directory / ('lease-' + token + '.lock')
        self.guard.touch(mode=0o600)
        self.row = dict(version=1, id=token, token=token, state='running',
                        command=optional.COMMANDS['processes'],
                        executions=[dict(session=123, leader=dict(pid=123, start='native', boot='fixture'),
                                         guard=self.guard.name, id='execution')])
        self.path = self.directory / 'record.json'
        self.path.write_text(json.dumps(self.row)); self.path.chmod(0o600)
        self.before = self.path.read_bytes()
        for name, value in [('boot_identity', 'fixture'), ('session_members', [])]:
            p = patch.object(optional.native, name, return_value=value)
            p.start(); self.addCleanup(p.stop)

    def test_proof_retains_failed_record_and_revalidates_native_session_and_inode(self):
        proof = optional.quiescence(self.directory)
        self.assertEqual(proof, optional.quiescence(self.directory, expected=proof))
        self.assertEqual(self.path.read_bytes(), self.before)
        self.assertEqual(proof['guard_inode'], self.guard.stat().st_ino)
        with patch.object(optional.native, 'session_members', return_value=[456]):
            with self.assertRaisesRegex(Refusal, 'surviving'): optional.quiescence(self.directory)

    def test_escaped_helper_guard_blocks_even_when_native_session_is_empty(self):
        fd = os.open(self.guard, os.O_RDWR); self.addCleanup(os.close, fd)
        fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        with self.assertRaisesRegex(Refusal, 'guard remains held'): optional.quiescence(self.directory)
        self.assertEqual(self.path.read_bytes(), self.before)

    def test_unknown_native_result_is_fatal_and_replaced_guard_rejects_replay(self):
        with patch.object(optional.native, 'session_members', side_effect=PermissionError('denied')):
            with self.assertRaises(PermissionError): optional.quiescence(self.directory)
        proof = optional.quiescence(self.directory)
        other = self.directory / 'replacement'; other.touch(mode=0o600); other.replace(self.guard)
        with self.assertRaisesRegex(Refusal, 'proof changed'): optional.quiescence(self.directory, expected=proof)

    def test_wrong_boot_session_command_or_finished_record_cannot_prove_exception(self):
        for mutate in (lambda r: r.update(state='finished'), lambda r: r.update(command=['memory_pressure','-Q']),
                       lambda r: r['executions'][0].update(session=124),
                       lambda r: r['executions'][0]['leader'].update(boot='old')):
            row = copy.deepcopy(self.row); mutate(row); self.path.write_text(json.dumps(row))
            with self.assertRaises(Refusal): optional.quiescence(self.directory)


class TimeoutClassification(Fixture):
    def exercise(self, *, optional_mode=True, now=30, failure=None, cancelled=False,
                 observation_failure=None, drain_reason=None, finished=True, proof_error=None):
        clock = [0]
        process = SimpleNamespace(close=Mock(), failure_cause=None, finished=finished,
                                  cancelled=cancelled, observation_failure=observation_failure,
                                  drain_reason=drain_reason, child=SimpleNamespace(returncode=0), leader_signals=set())
        def managed(custody, lease, argv, **kwargs):
            def wait():
                clock[0] = now
                try:
                    kwargs['publisher'].publish()
                except CustodyRefusal as error:
                    process.failure_cause = error if failure is None else failure
                    raise CustodyRefusal('drained') from error
                raise CustodyRefusal('different failure')
            process.wait = wait
            return process
        def deadline(seconds, end=None): return Deadline(seconds, clock=lambda: clock[0], end=end)
        with patch.object(build_products.native, 'identity', return_value={'pid':123}), \
             patch.object(build_products.native, 'boot_identity', return_value='fixture'), \
             patch('gate_measurement_bounds.Deadline', side_effect=deadline), \
             patch.object(command.time, 'monotonic', side_effect=lambda: clock[0]), \
             patch.object(supervisor, 'ManagedProcess', side_effect=managed), \
             patch.object(optional, 'quiescence', side_effect=proof_error, return_value={'proved':True}) as proof:
            try:
                command.bounded(optional.COMMANDS['processes'], root=self.root,
                                optional_overall_end=100 if optional_mode else None)
            finally:
                process.close.assert_called_once_with()
                self.last_proof_calls = proof.call_count

    def test_local_timeout_is_typed_only_after_positive_cleanup(self):
        with self.assertRaises(optional.ExposureTimeout) as error: self.exercise()
        self.assertEqual(error.exception.proof, {'proved':True})
        self.assertEqual(self.last_proof_calls, 1)

    def test_global_deadline_cancel_unknown_custody_and_nonlocal_errors_remain_fatal(self):
        for kwargs in ({'optional_mode':False}, {'now':100}, {'cancelled':True},
                       {'finished':False}, {'observation_failure':'unknown'},
                       {'drain_reason':'revoked'}, {'failure':CustodyRefusal('other')}, {'now':29}):
            # Each attempt needs its own namespace: retained journals cannot be replaced.
            with self.subTest(kwargs=kwargs):
                with self.assertRaises(Refusal) as error: self.exercise(**kwargs)
                self.assertNotIsInstance(error.exception, optional.ExposureTimeout)
                self.assertEqual(self.last_proof_calls, 0)

    def test_unproved_cleanup_is_never_optional(self):
        with self.assertRaisesRegex(Refusal, 'guard held') as error:
            self.exercise(proof_error=Refusal('guard held'))
        self.assertNotIsInstance(error.exception, optional.ExposureTimeout)

    def test_allowlist_and_local_ceiling_are_exact(self):
        with patch.object(optional.time, 'monotonic', return_value=0):
            for argv in optional.COMMANDS.values(): optional.require_optional(argv,30,100)
            for argv, seconds, end in [(['/bin/ps','-axo','stat='],30,100),
                                        (['/usr/bin/memory_pressure','-Q'],30,100),
                                        (optional.COMMANDS['processes'],31,100),
                                        (optional.COMMANDS['processes'],30,float('inf'))]:
                with self.assertRaises(Refusal): optional.require_optional(argv,seconds,end)

    def test_boundary_failures_cannot_be_reclassified_as_local_timeouts(self):
        # Constructor failures include custody fsync, descriptor capture and exec
        # handshakes: without a completed supervisor they retain strict semantics.
        for phase in ('custody_fsync','descriptor_capture','spawn','exec_handshake','result_fsync'):
            with self.subTest(phase=phase), \
                 patch.object(build_products.native,'identity',return_value={'pid':123}), \
                 patch.object(build_products.native,'boot_identity',return_value='fixture'), \
                 patch.object(supervisor,'ManagedProcess',side_effect=OSError(phase)), \
                 patch.object(optional,'quiescence') as proof:
                with self.assertRaises(Refusal) as error:
                    command.bounded(optional.COMMANDS['processes'],root=self.root,optional_overall_end=10**12)
                self.assertNotIsInstance(error.exception, optional.ExposureTimeout)
                proof.assert_not_called()


@unittest.skipUnless(sys.platform == 'darwin', 'native private macOS helper fixtures')
class WrapperTimeouts(Fixture):
    def test_real_owned_wrapper_stalls_only_downgrade_after_native_quiescence(self):
        # Advance an injected deadline at each observed child boundary. The real
        # launcher, supervision, TERM/drain, custody record and guard are used;
        # the ps executable is a disposable printf fixture, never a host census.
        from host_admission.diagnostics import Trace
        bin_dir=self.root/'bin';bin_dir.mkdir()
        fake=bin_dir/'ps';fake.write_text('#!/bin/sh\nprintf "fixture output\\n"\n');fake.chmod(0o700)
        real_managed=supervisor.ManagedProcess
        for phase in ('descriptor_capture','command_spawn','command_wait','result_fsync'):
            with self.subTest(phase=phase):
                trace=Trace();self.addCleanup(trace.close)
                def managed(custody,lease,argv,**kwargs):
                    shim="import sys,time;sys.path.insert(0,"+repr(str(Path(command.__file__).parent))+ ");import gate_measurement_command as c;import host_admission.supervisor as s;"
                    if phase=='descriptor_capture':shim+="s.inherited_descriptors=lambda:time.sleep(60);"
                    elif phase=='command_spawn':shim+="c.subprocess.Popen=lambda *a,**k:time.sleep(60);"
                    elif phase=='command_wait':shim+="c.subprocess.Popen.wait=lambda *a,**k:time.sleep(60);"
                    else:shim+="c.os.fsync=lambda *a:time.sleep(60);"
                    shim+="raise SystemExit(c.execute(sys.argv[1],int(sys.argv[2])))"
                    replacement=[sys.executable,'-B','-c',shim,argv[-2],argv[-1]]
                    return real_managed(custody,lease,replacement,**kwargs)
                started=time.monotonic()
                class InjectedDeadline:
                    def __init__(self,seconds,end=None):
                        self.seconds=seconds
                    def require(self,label):
                        row=trace.snapshot()['latest'].get('child:'+phase)
                        if row and row[2]=='begin':raise Refusal('injected local deadline at '+phase)
                        if time.monotonic()-started>5:raise AssertionError('fixture failed to reach '+phase)
                env=dict(os.environ,PATH=str(bin_dir)+':/usr/bin:/bin')
                with patch.object(command,'Trace',return_value=trace), \
                     patch.object(supervisor,'ManagedProcess',side_effect=managed), \
                     patch('gate_measurement_bounds.Deadline',InjectedDeadline):
                    with self.assertRaises(optional.ExposureTimeout) as error:
                        command.bounded(optional.COMMANDS['processes'],root=self.root/phase,
                                        env=env,optional_overall_end=time.monotonic()+30)
                directory=Path(error.exception.proof['directory'])
                self.assertEqual(build_products.read_record(directory/'record.json')['state'],'running')
                self.assertEqual(optional.quiescence(directory),error.exception.proof)
                snapshot=json.loads((directory/'boundaries.json').read_text())
                self.assertEqual(snapshot['latest']['child:'+phase][2],'begin')

    def test_already_exited_nonzero_or_unprompted_signal_remains_fatal_at_deadline(self):
        real_managed=supervisor.ManagedProcess
        for failure in ('command-nonzero','result-fsync','unprompted-signal'):
            with self.subTest(failure=failure):
                bin_dir=self.root/failure;bin_dir.mkdir()
                fake=bin_dir/'ps';fake.write_text('#!/bin/sh\nexit '+('7' if failure=='command-nonzero' else '0')+'\n');fake.chmod(0o700)
                process=[]
                def managed(custody,lease,argv,**kwargs):
                    if failure!='command-nonzero':
                        shim="import sys,os,signal;sys.path.insert(0,"+repr(str(Path(command.__file__).parent))+ ");import gate_measurement_command as c;"
                        if failure=='result-fsync':shim+="c.os.fsync=lambda *a:(_ for _ in ()).throw(OSError('injected fsync failure'));"
                        else:shim+="os.kill(os.getpid(),signal.SIGTERM);"
                        shim+="raise SystemExit(c.execute(sys.argv[1],int(sys.argv[2])))"
                        argv=[sys.executable,'-B','-c',shim,argv[-2],argv[-1]]
                    result=real_managed(custody,lease,argv,**kwargs);process.append(result);return result
                class InjectedDeadline:
                    def __init__(self,*a,**k):pass
                    def require(self,label):
                        if process and process[0]._exited():raise Refusal('deadline coincides with prior failure')
                with patch.object(supervisor,'ManagedProcess',side_effect=managed), \
                     patch('gate_measurement_bounds.Deadline',InjectedDeadline):
                    with self.assertRaises(Refusal) as error:
                        command.bounded(optional.COMMANDS['processes'],root=bin_dir/'operations',
                            env=dict(os.environ,PATH=str(bin_dir)+':/usr/bin:/bin'),optional_overall_end=time.monotonic()+30)
                self.assertNotIsInstance(error.exception,optional.ExposureTimeout)
                self.assertTrue(process[0].finished)
                self.assertEqual(process[0].leader_signals,set())


class Acceptance(Fixture):
    def test_successful_root_with_gap_is_terminal_unaccepted_and_cannot_retry_or_reuse(self):
        cohort=Cohort(self.root,'baseline'); value=identity()
        cohort.begin(value,remaining_window=36000,remaining_campaign=72000)
        telemetry={'version':1,'complete':False,'gaps':[{'helper':'processes','reason':'local-timeout-quiescent'}]}
        row=cohort.finish(exit_code=0,settled=True,executed=value['applicable_legs'],reused=[],elapsed=12,telemetry=telemetry)
        self.assertEqual(row['exit_code'],0); self.assertTrue(row['settled']); self.assertFalse(row['accepted'])
        self.assertEqual(cohort.history()[2],1)
        with self.assertRaises(Refusal): cohort.begin(value,remaining_window=36000,remaining_campaign=72000)
        with self.assertRaises(Refusal): cohort.reuse_key(value,'fmt')
        rows=cohort.path.read_text().splitlines(); forged=json.loads(rows[1]);forged['accepted']=True
        cohort.path.write_text(rows[0]+'\n'+json.dumps(forged)+'\n')
        with self.assertRaises(Refusal): cohort.history()

    def test_report_reads_versioned_samples_and_retains_incomplete_terminal(self):
        from gate_measurement_worker import save_sample
        from gate_measurement_campaign import report
        cohort=Cohort(self.root,'baseline');value=identity()
        cohort.begin(value,remaining_window=36000,remaining_campaign=72000)
        telemetry={'version':1,'complete':False,'gaps':[{'helper':'processes','reason':'local-timeout-quiescent'}]}
        cohort.finish(exit_code=0,settled=True,executed=value['applicable_legs'],reused=[],elapsed=12,telemetry=telemetry)
        samples=cohort.root/'slot-00'/'telemetry';samples.mkdir(parents=True)
        resources={'cpu_percent_sum':2,'rss_bytes_sum':1024,'process_count':1}
        save_sample(samples,1,{'state':'complete','sequence':1,'sample':{'owned_resources':resources}})
        save_sample(samples,2,{'state':'timeout','sequence':2,'helper':'processes','proof':{}})
        (self.root/'baseline').mkdir()
        result=report(self.root,'baseline')
        self.assertFalse(result['complete']);self.assertEqual(result['observed_peak_resources'],resources)
        self.assertEqual(result['telemetry'],[dict(slot=0,**telemetry)])
        self.assertEqual(result['modes']['cold']['accepted'],0)

    def test_missing_old_or_contradictory_completeness_rejects(self):
        for row in (None,{},dict(version=1,complete=True,gaps=[{'helper':'processes','reason':'local-timeout-quiescent'}]),
                    dict(version=1,complete=False,gaps=[]),dict(version=2,complete=True,gaps=[])):
            with self.subTest(row=row),self.assertRaises(Refusal):optional.completeness(row)
        cohort=Cohort(self.root,'baseline'); row=cohort.begin(identity(),remaining_window=36000,remaining_campaign=72000)
        row.pop('version');cohort.path.write_text(json.dumps(row)+'\n')
        with self.assertRaisesRegex(Refusal,'predates'):cohort.history()

    def test_running_health_still_requires_native_pressure_and_memory_command(self):
        from gate_measurement_runtime import pressure
        with patch('gate_measurement_exposure.snapshot',return_value={'native_memory_pressure':1}), \
             patch('gate_measurement_runtime.capture',return_value='memory') as capture:
            result=pressure(include_processes=False)
            capture.assert_called_once_with(['/usr/bin/memory_pressure','-Q'])
            self.assertNotIn('processes',result)
        with patch('gate_measurement_exposure.snapshot',side_effect=Refusal('unsafe pressure')):
            with self.assertRaisesRegex(Refusal,'unsafe pressure'):pressure(include_processes=False)


if __name__ == '__main__': unittest.main()
