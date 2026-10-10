"""Worker protocol, mandatory-loop independence and disposable launcher fixture."""
import hashlib
import json
import os
from pathlib import Path
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import time
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch

SCRIPTS=Path(__file__).resolve().parents[1]
sys.path.insert(0,str(SCRIPTS))
import gate_measurement_worker as worker
import gate_measurement_execution as execution
from gate_measurement_optional import CONTRACT, complete
from verifier_state import Refusal
from test_gate_measurement_cohorts import identity

GAP={'version':1,'complete':False,'gaps':[{'helper':'processes','reason':'local-timeout-quiescent'}]}

class Fixture(unittest.TestCase):
    def setUp(self):
        self.tmp=tempfile.TemporaryDirectory(prefix='optional-worker-',dir='/tmp')
        self.addCleanup(self.tmp.cleanup);self.root=Path(self.tmp.name).resolve()

class Protocol(Fixture):
    def setUp(self):
        super().setUp()
        self.worker=worker.Worker.__new__(worker.Worker);w=self.worker
        w.directory=self.root;w.end=time.monotonic()+100;w.manifest_path='/fixture/manifest.json'
        w.sequence=0;w.pending=w.disabled=w.closed=False;w.ready=True;w.receipts=[];w.summary=complete()
        w.process=SimpleNamespace(_exited=lambda:False,finished=False,close=Mock(),wait=Mock(return_value=0))
        w.channel,self.peer=socket.socketpair(socket.AF_UNIX,socket.SOCK_DGRAM);w.channel.setblocking(False)
        self.addCleanup(w.channel.close);self.addCleanup(self.peer.close)
        p=patch.object(worker,'running',return_value=True);self.running=p.start();self.addCleanup(p.stop)

    def reply(self,value):self.peer.send(json.dumps(value).encode());self.worker.poll()

    def test_requests_are_single_flight_and_never_start_before_root(self):
        self.running.return_value=False;self.worker.request();self.assertFalse(self.worker.pending)
        self.running.return_value=True
        for _ in range(20):self.worker.request()
        self.assertEqual(self.worker.sequence,1);self.assertEqual(self.peer.recv(16),b'R')
        self.peer.setblocking(False)
        with self.assertRaises(BlockingIOError):self.peer.recv(16)
        for _ in range(20):self.worker.poll()
        self.assertTrue(self.worker.pending)

    def test_first_proved_gap_latches_and_stops_all_future_requests(self):
        self.worker.request();self.peer.recv(16)
        proof={'directory':str(self.root/'helpers'/'build-fixture')}
        with patch.object(worker,'quiescence',return_value=proof) as check:
            self.reply(dict(state='timeout',sequence=1,sha256='a'*64,helper='processes',proof=proof))
            check.assert_called_once_with(Path(proof['directory']),expected=proof)
        self.assertEqual(self.worker.summary,GAP)
        for _ in range(20):self.worker.request()
        self.assertEqual(self.worker.sequence,1)
        with self.assertRaises(Refusal):self.reply(dict(state='complete',sequence=1,sha256='b'*64))
        self.assertEqual(self.worker.summary,GAP)
        # Worker already terminated after its one gap. Close must not send a
        # datagram to a closed peer, nor reset completeness.
        self.peer.close()
        with patch.object(self.worker,'verify_receipts'):
            self.assertEqual(self.worker.close(),GAP)

    def test_close_consumes_terminal_gap_queued_after_gate_exit(self):
        self.worker.request();self.peer.recv(16)
        proof={'directory':str(self.root/'helpers'/'build-fixture')}
        self.peer.send(json.dumps(dict(state='timeout',sequence=1,sha256='a'*64,
                                       helper='processes',proof=proof)).encode())
        self.peer.close()
        with patch.object(worker,'quiescence',return_value=proof),patch.object(self.worker,'verify_receipts'):
            self.assertEqual(self.worker.close(),GAP)

    def test_unproved_timeout_foreign_record_and_unknown_exit_are_fatal(self):
        self.worker.request();self.peer.recv(16)
        row=dict(state='timeout',sequence=1,sha256='a'*64,helper='processes',proof={'directory':str(self.root/'helpers'/'build-x')})
        with patch.object(worker,'quiescence',side_effect=Refusal('held guard')):
            with self.assertRaisesRegex(Refusal,'held guard'):self.reply(row)
        row['proof']['directory']='/foreign/build-x'
        with self.assertRaisesRegex(Refusal,'foreign'):self.reply(row)
        self.worker.process._exited=lambda:True
        with self.assertRaisesRegex(Refusal,'without a terminal'):self.worker.poll()

    def test_root_end_reply_requires_actual_end_and_has_no_fake_gap(self):
        self.worker.request();self.peer.recv(16)
        with self.assertRaises(Refusal):self.reply(dict(state='root-ended',sequence=1))
        self.running.return_value=False;self.reply(dict(state='root-ended',sequence=1))
        self.assertTrue(self.worker.disabled);self.assertEqual(self.worker.summary,complete())

    def test_success_fields_are_classified_against_collector_not_worker(self):
        from subprocess import CompletedProcess
        values=['10 1 0.0 collector\n11 10 0.0 cargo\n12 1 0.0 rustc\n',
                '10 1 1.0 2 collector\n11 10 2.0 3 cargo\n12 1 4.0 5 rustc\n']
        with patch('gate_measurement_command.bounded',side_effect=[CompletedProcess([],0,text,'') for text in values]):
            value=worker.sample(self.root,time.monotonic()+100,10)
        self.assertEqual(value['sample']['competing_pids'],[12])
        self.assertEqual(value['sample']['owned_resources']['rss_bytes_sum'],5*1024)

    def test_nonzero_parse_and_second_command_failure_stay_fatal(self):
        from subprocess import CompletedProcess
        for responses in ([CompletedProcess([],1,'','')], [CompletedProcess([],0,'bad','')]*2,
                          [CompletedProcess([],0,'10 1 0.0 collector\n',''),Refusal('mandatory failure')]):
            with patch('gate_measurement_command.bounded',side_effect=responses):
                with self.assertRaises((Refusal,ValueError)):worker.sample(self.root,time.monotonic()+100,10)

    def test_sample_packet_digest_and_timeout_proof_are_bound(self):
        proof={'directory':str(self.root/'helpers'/'build-x')}
        row={'state':'timeout','sequence':1,'helper':'processes','proof':proof}
        digest=worker.save_sample(self.root,1,row)
        self.worker.receipts=[dict(row,sha256=digest)]
        self.worker.verify_receipts()
        self.worker.receipts[0]['proof']={'directory':'changed'}
        with self.assertRaisesRegex(Refusal,'differs'):self.worker.verify_receipts()
        (self.root/'sample-0001.json').write_text('{}')
        with self.assertRaisesRegex(Refusal,'changed'):self.worker.verify_receipts()

class GateLoop(Fixture):
    def run_gate(self, *, failure=False):
        value=identity();directory=self.root/'measurement-results-v1'/'baseline'/'slot-00'
        directory.mkdir(parents=True);manifest=self.root/'baseline'/'manifest.json'
        manifest.parent.mkdir()
        (self.root/'common').mkdir()
        info={'kind':'gate-throughput-measurement','commit':value['source_commit'],'tree':value['source_tree'],
              'common':str(self.root/'common'),'worktree':str(manifest.parent/'worktree'),
              'campaign_root':str(self.root),'gate':{'argv':['make','test']},'policy':{'optional_telemetry':CONTRACT}}
        now=[0];child=Mock();child.returncode=0;child.poll.side_effect=lambda:None if now[0]<16 else 0
        def wait(**_):now[0]=16;return 0
        child.wait.side_effect=wait
        sampler=Mock();sampler.ready=True;sampler.closed=False;sampler.close.return_value=GAP
        safety=Mock(side_effect=Refusal('mandatory pressure failure') if failure else None)
        health=Mock()
        with patch.object(execution,'validate',return_value=info), \
             patch.object(execution,'Worker',return_value=sampler), \
             patch.object(execution.subprocess,'Popen',return_value=child), \
             patch.object(execution.time,'monotonic',side_effect=lambda:now[0]), \
             patch.object(execution.time,'sleep',side_effect=lambda seconds:now.__setitem__(0,now[0]+seconds)), \
             patch.object(execution,'execution',return_value={'state':'completed','exit_status':0}), \
             patch.object(execution,'read',return_value={'gate_started':False,'gate_session':None}), \
             patch.dict(os.environ,STORYHOOK_VERIFIER_OWNER='fixture',STORYHOOK_GATE_PROGRESS=str(self.root/'outer')):
            gate=execution.OwnedGate(str(manifest),['make','test'],health=health,running_health=safety)
            if failure:
                with self.assertRaisesRegex(Refusal,'mandatory pressure'):gate({'identity':value},directory,100)
                child.send_signal.assert_called_once_with(signal.SIGTERM)
                sampler.close.assert_called_once_with(cancel=True)
            else:
                result=gate({'identity':value},directory,100)
                self.assertEqual(result['telemetry'],GAP);self.assertEqual(result['exit_code'],0)
                self.assertGreaterEqual(safety.call_count,3)
                self.assertGreaterEqual(sampler.poll.call_count,30)
                self.assertGreaterEqual(sampler.request.call_count,3)
                child.send_signal.assert_not_called()
            health.assert_called_once_with()

    def test_running_gate_keeps_safety_and_progress_polling_with_pending_sampler(self):self.run_gate()
    def test_mandatory_failure_cancels_gate_and_sampler(self):self.run_gate(failure=True)

class OwnerBinding(Fixture):
    def test_separate_session_worker_is_bound_to_exact_live_collector(self):
        manifest=self.root/'manifest.json';manifest.write_text('fixture')
        value={'common':str(self.root),'worktree':str(self.root/'worktree'),'policy':{'optional_telemetry':CONTRACT}}
        collector={'pid':123,'start':'birth','boot':'fixture'}
        row=dict(common=value['common'],worktree=value['worktree'],nonce='nonce',boot='fixture',session=12,
                 measurement=str(manifest),measurement_sha256=hashlib.sha256(manifest.read_bytes()).hexdigest(),
                 gate_started=True,gate_session=999)
        with patch('gate_measurement_context.manifest',return_value=value),patch.object(worker,'read',return_value=row), \
             patch.object(worker.native,'boot_identity',return_value='fixture'), \
             patch.object(worker.native,'identity',return_value=collector),patch.object(worker.os,'getppid',return_value=123), \
             patch.object(worker.os,'getsid',return_value=12),patch.dict(os.environ,STORYHOOK_VERIFIER_OWNER='nonce'):
            self.assertTrue(worker.running(str(manifest),collector))
            for change in ({'start':'reused'},{'pid':124},{'boot':'old'}):
                with self.assertRaises(Refusal):worker.running(str(manifest),dict(collector,**change))
            for field,value in [('nonce','other'),('session',None),('measurement_sha256','changed'),('gate_session',None)]:
                old=row[field];row[field]=value
                if field=='gate_session':self.assertFalse(worker.running(str(manifest),collector))
                else:
                    with self.assertRaises(Refusal):worker.running(str(manifest),collector)
                row[field]=old

    @unittest.skipUnless(sys.platform=='darwin','native private macOS launcher fixture')
    def test_exact_managed_worker_launcher_validates_clean_pinned_workspace(self):
        # No ps, compilation, make, campaign entry, or charged gate is launched.
        source=self.root/'source';source.mkdir();(source/'fixture').write_text('pinned')
        env={k:v for k,v in os.environ.items() if not k.startswith(('STORYHOOK_','GIT_'))}
        env.update(GIT_CONFIG_NOSYSTEM='1',GIT_CONFIG_GLOBAL='/dev/null',GIT_AUTHOR_NAME='Fixture',
                   GIT_AUTHOR_EMAIL='fixture@example.invalid',GIT_COMMITTER_NAME='Fixture',
                   GIT_COMMITTER_EMAIL='fixture@example.invalid',PYTHONDONTWRITEBYTECODE='1')
        def git(*args):return subprocess.check_output(['git','-C',str(source),*args],env=env,text=True).strip()
        git('init','-q');git('add','.');git('commit','-qm','fixture')
        revision=self.root/'campaign'/'baseline';revision.mkdir(parents=True);worktree=revision/'worktree'
        commit,tree=git('rev-parse','HEAD'),git('rev-parse','HEAD^{tree}')
        git('worktree','add','-q','--detach',str(worktree),commit)
        manifest=revision/'manifest.json'
        manifest.write_text(json.dumps(dict(version=1,kind='gate-throughput-measurement',campaign_root=str(revision.parent),
            revision='baseline',worktree=str(worktree),common=str(source/'.git'),commit=commit,tree=tree,
            policy={'optional_telemetry':CONTRACT})))
        attempt=revision.parent/'measurement-results-v1'/'baseline'/'slot-00';attempt.mkdir(parents=True)
        env.update(STORYHOOK_GATE_MEASUREMENT=str(manifest))
        body=f'''import sys,time
sys.path.insert(0,{str(SCRIPTS)!r})
from gate_measurement_worker import Worker
from gate_measurement_optional import complete
w=Worker({str(attempt)!r},{str(manifest)!r},time.monotonic()+15)
try:
    end=time.monotonic()+10
    while not w.ready:
        assert time.monotonic()<end, 'launcher validation stalled'
        w.poll();time.sleep(.01)
    w.request()
    assert not w.pending, 'worker sampled before gate admission'
    assert w.close()==complete()
finally:
    if not w.closed:w.close(cancel=True)
print('exact worker bootstrap passed')
'''
        result=subprocess.run([sys.executable,'-B',str(SCRIPTS/'verifier-owner.py'),'measurement-run',str(source/'.git'),
                              str(worktree),'--',sys.executable,'-B','-c',body],cwd=worktree,env=env,
                              text=True,capture_output=True,timeout=25)
        self.assertEqual(result.returncode,0,result.stdout+result.stderr)
        self.assertIn('exact worker bootstrap passed',result.stdout)
        self.assertFalse((attempt/'telemetry'/'helpers').exists())
        record,=(attempt/'telemetry').glob('build-*/record.json')
        self.assertEqual(json.loads(record.read_text())['state'],'finished')

if __name__=='__main__':unittest.main()
