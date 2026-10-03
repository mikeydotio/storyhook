"""A restored UUID can bind only the dispatch its source snapshot proves."""

import copy
import json
import shlex
from pathlib import Path
import sys
import unittest

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'lib'))
import restored_dispatch as restore


class SourceTests(unittest.TestCase):
    """Exercise the ownership join independently of process and transport adapters."""

    def setUp(self):
        self.target = dict(socket='/tmp/public', endpoint='/tmp/.rv-' + 'b'*32 + '/s', generation='b'*32)
        self.old = '/tmp/.rv-' + 'a'*32 + '/s'
        self.lease = dict(version=1, project_slug='project', story_id='SH-1',
                          repository_path='/repo', worktree_path='/repo/.codex/worktrees/SH-1', branch='worktree-SH-1',
                          tmux=dict(socket_path=self.old, revivify=dict(logical_socket='/tmp/public', origin_generation='a'*32)))
        self.identity = dict(version=1, project='project', story='SH-1', common='/repo/.git',
                             worktree=self.lease['worktree_path'], pane='%1', socket=self.old, provider='codex',
                             process=dict(pid=99, start='old', executable='/bin/codex'))
        self.agent = dict(kind='codex', session_id='conversation', resume_cwd=self.lease['worktree_path'], old_pid=99)
        self.saved = dict(pane_id='%8', pane=dict(uuid='uuid', cwd=self.lease['worktree_path'],
                          options={'@storyhook-identity-v1':json.dumps(self.identity)}, agent=self.agent),
                          window=dict(name='SH-1', options={'@storyhook-agent':'codex'}), sessions={'project':'$2'})
        self.evidence = dict(source_generation='a'*32, source_endpoint=self.old, source_history=[],
                             snapshot_id='snapshot', panes={'uuid':self.saved})

    def match(self):
        return restore.source_dispatch(self.target, self.evidence, self.lease, '/repo/.git', 'SH-1')

    def test_unique_dispatch_joins_uuid_snapshot_process_conversation_and_lease(self):
        source = self.match()
        self.assertEqual(source['uuid'], 'uuid')
        self.assertEqual(source['saved']['pane_id'], '%8')
        self.assertEqual(source['identity'], self.identity)
        self.assertEqual(source['session_id'], 'conversation')
        self.assertEqual(source['lease']['tmux']['socket_path'], self.target['endpoint'])
        self.assertEqual(source['lease']['tmux']['revivify'], self.lease['tmux']['revivify'])
        self.assertEqual(self.lease['tmux']['socket_path'], self.old)

    def test_foreign_identity_and_provider_conflicts_refuse(self):
        for key, value in [('project','foreign'), ('common','/other/.git'), ('worktree','/other'),
                           ('socket','/foreign'), ('provider','claude'), ('version',True)]:
            with self.subTest(key=key):
                self.saved['pane']['options']['@storyhook-identity-v1'] = json.dumps(dict(self.identity, **{key:value}))
                with self.assertRaises(RuntimeError): self.match()

    def test_ambiguous_dispatches_and_renamed_windows_refuse(self):
        self.evidence['panes']['other'] = copy.deepcopy(self.saved)
        self.evidence['panes']['other']['pane']['uuid'] = 'other'
        with self.assertRaises(RuntimeError): self.match()
        del self.evidence['panes']['other']
        self.saved['window']['name'] = 'foreign'
        with self.assertRaises(RuntimeError): self.match()

    def test_missing_session_wrong_cwd_or_snapshot_process_refuse(self):
        for change in [dict(session_id=None),dict(kind='claude'),dict(resume_cwd='/other'),dict(old_pid=100)]:
            with self.subTest(change=change):
                self.saved['pane']['agent'] = dict(self.agent, **change)
                with self.assertRaises(RuntimeError): self.match()

    def test_foreign_origin_or_lease_socket_refuses(self):
        self.lease['tmux']['revivify']['origin_generation'] = 'c'*32
        with self.assertRaises(RuntimeError): self.match()
        self.lease['tmux']['revivify']['origin_generation'] = 'a'*32
        self.lease['tmux']['socket_path'] = '/other'
        with self.assertRaises(RuntimeError): self.match()

    def test_legacy_lease_needs_independent_source_proof(self):
        del self.lease['tmux']['revivify']
        source = self.match()
        self.assertEqual(source['lease']['tmux']['revivify']['origin_generation'], 'a'*32)
        self.evidence['source_generation'] = None
        self.evidence['source_endpoint'] = None
        with self.assertRaises(RuntimeError): self.match()
        self.identity['socket'] = self.target['socket']
        self.lease['tmux']['socket_path'] = self.target['socket']
        self.saved['pane']['options']['@storyhook-identity-v1'] = json.dumps(self.identity)
        self.assertNotIn('revivify', self.match()['lease']['tmux'])

    def test_missing_story_is_distinct_from_a_damaged_story_snapshot(self):
        self.evidence['panes'] = {}
        self.assertIsNone(self.match())
        self.evidence['panes'] = {'uuid':self.saved}
        del self.saved['pane']['options']['@storyhook-identity-v1']
        with self.assertRaises(RuntimeError): self.match()

    def test_previous_readoption_uses_its_provider_child_identity(self):
        self.identity['restored'] = dict(provider_process=dict(pid=101, start='old-child', executable='/bin/codex'))
        self.saved['pane']['options']['@storyhook-identity-v1'] = json.dumps(self.identity)
        self.agent['old_pid'] = 101
        self.assertEqual(self.match()['provider_process']['pid'], 101)


class ProviderTests(unittest.TestCase):
    """The adapter supplies native observations, never screen or name guesses."""

    def setUp(self):
        self.root = dict(pid=10, start='root', executable='/bin/sh')
        self.provider = dict(pid=11, start='provider', executable='/bin/codex')
        self.table = {10:1, 11:10, 12:1}
        self.rows = {
            10:dict(process=self.root, parent=1, cwd='/work', argv=['sh']),
            11:dict(process=self.provider, parent=10, cwd='/work', argv=['codex','resume','session']),
            12:dict(process=dict(self.provider, pid=12), parent=1, cwd='/work', argv=['codex','resume','session'])}
        self.source = dict(provider='codex', provider_process=dict(self.provider, pid=3),
                           session_id='session', lease={'worktree_path':'/work'})

    def match(self, observe=None):
        return restore.live_provider(self.source, self.root, self.table, '/bin/codex', observe or self.rows.__getitem__)

    def test_unique_child_belongs_to_root_and_retains_conversation(self):
        self.assertEqual(self.match(), self.rows[11])
        self.rows[11]['argv'] = ['codex','resume','different']
        with self.assertRaises(RuntimeError): self.match()

    def test_same_named_foreign_process_does_not_count_but_second_child_refuses(self):
        self.assertEqual(self.match()['process'], self.provider)
        self.table[12] = 10
        self.rows[12]['parent'] = 10
        with self.assertRaises(RuntimeError): self.match()

    def test_wrong_worktree_or_parent_or_executable_refuses(self):
        for key,value in [('cwd','/other'),('parent',20),('process',dict(self.provider,executable='/foreign'))]:
            old = self.rows[11][key]
            self.rows[11][key] = value
            with self.assertRaises(RuntimeError): self.match()
            self.rows[11][key] = old

    def test_snapshot_and_live_resume_must_be_exact_argument_values(self):
        for args in [['codex','resume session'],['codex','resume','session-extra'],
                     ['codex','exec','resume','session'],['codex','resume','session','--fork']]:
            self.rows[11]['argv'] = args
            with self.assertRaises(RuntimeError): self.match()

    def test_claude_requires_explicit_resume_and_rejects_forks(self):
        self.source['provider'] = 'claude'
        self.source['provider_process']['executable'] = '/bin/claude'
        self.rows[11]['process'] = dict(self.provider, executable='/bin/claude')
        for args in [['claude','--resume','session'],['claude','--resume=session']]:
            self.rows[11]['argv'] = args
            self.assertEqual(self.match(), self.rows[11])
        for args in [['claude','--resume'],['claude','--resume','session','--fork-session'],
                     ['claude','--resume','session','--resume','different']]:
            self.rows[11]['argv'] = args
            with self.assertRaises(RuntimeError): self.match()

    def test_changed_provider_or_root_during_proof_refuses(self):
        for changed in (10,11):
            calls = {}
            def observer(pid):
                calls[pid] = calls.get(pid,0) + 1
                row = copy.deepcopy(self.rows[pid])
                if pid == changed and calls[pid] > 1: row['process']['start'] = 'replacement'
                return row
            with self.assertRaises(RuntimeError): self.match(observer)

    def test_node_provider_requires_the_exact_installed_entry_point(self):
        self.source['provider_process']['executable'] = '/bin/node'
        self.rows[11]['process'] = dict(self.provider, executable='/bin/node')
        self.rows[11]['argv'] = ['node','/bin/codex','resume','session']
        self.assertEqual(self.match(), self.rows[11])
        self.rows[11]['argv'][1] = '/foreign/codex'
        with self.assertRaises(RuntimeError): self.match()


class LaunchTests(unittest.TestCase):
    """Old pane options cannot grant a manually replaced command restore authority."""

    def test_ticket_and_wrapper_must_belong_to_the_captured_generation(self):
        target = dict(executable='/provider/bin/revivify', state_dir='/state/generations/new')
        argv = ['/provider/libexec/revivify-pane-init', '/state/generations/new/tickets/uuid', '/bin/tmux', '/bin/zsh', '']
        self.assertIsNone(restore.restored_launch(target, 'uuid', shlex.join(argv)))
        for index,value in [(0,'/foreign/revivify-pane-init'), (1,'/state/generations/old/tickets/uuid'),
                            (1,'/state/generations/new/tickets/other')]:
            changed = argv.copy()
            changed[index] = value
            with self.assertRaises(RuntimeError): restore.restored_launch(target, 'uuid', shlex.join(changed))
        for command in ['claude --resume session', '"unterminated', shlex.join(argv) + ' ; echo extra']:
            with self.assertRaises(RuntimeError): restore.restored_launch(target, 'uuid', command)


if __name__ == '__main__':
    unittest.main()
