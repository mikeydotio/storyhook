"""SH-676: positive approval boundaries and conservative Stop behavior."""
import importlib.util
import fcntl
import json
import os
from pathlib import Path
import tempfile
import sys
import unittest
from unittest.mock import Mock, patch

HOOKS = Path(__file__).resolve().parents[1] / 'hooks'
sys.path.insert(0, str(HOOKS))
spec = importlib.util.spec_from_file_location('codex_stop', HOOKS / 'codex_stop.py')
stop = importlib.util.module_from_spec(spec)
spec.loader.exec_module(stop)

PLAN = '''Proposed plan for SH-672:
1. Post this plan verbatim on SH-672 before changing files or running tests.
2. Remove the global cap and preserve per-run limits.
3. Add regressions and run impacted tests, commit, and submit for verification.
MIKEY ACTIONS
1. Reply “Approve” to authorize this plan, “Change” with edits, or “Something else”.'''
APPROVAL = {'decision': 'approve_plan', 'evidence': 'Reply “Approve” to authorize this plan'}


class StopTests(unittest.TestCase):
    """Exercise the production decision with only external answers substituted."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix='sh676-stop-', dir='/tmp')
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.transcript = self.root / 'rollout.jsonl'
        self.env = {'STORYHOOK_AUTO': 'SH-672'}
        self.payload = {'hook_event_name': 'Stop', 'session_id': 'session-1',
                        'turn_id': 'turn-1', 'cwd': str(self.root),
                        'transcript_path': str(self.transcript),
                        'stop_hook_active': False, 'last_assistant_message': PLAN}
        self.transcript_for()
        self.classify = Mock(return_value=APPROVAL)
        self.eligible = Mock(return_value=True)

    def transcript_for(self, mode='default', session='session-1', turn='turn-1', source='cli'):
        self.transcript.write_text('\n'.join(json.dumps(x) for x in [
            {'type': 'session_meta', 'payload': {'id': session, 'cwd': str(self.root), 'source': source}},
            {'type': 'turn_context', 'payload': {'turn_id': turn, 'cwd': str(self.root),
              'collaboration_mode': {'mode': mode}}},
        ]) + '\n')

    def run_hook(self):
        return stop.handle(self.payload, self.env, self.classify, self.eligible)

    def test_observed_default_mode_plan_gets_native_continuation(self):
        result = self.run_hook()
        self.assertEqual(result.get('decision'), 'block')
        self.assertIn('approved automatically', result['reason'])
        self.assertIn('verbatim', result['reason'])
        self.assertIn('SH-672', result['reason'])
        self.classify.assert_called_once_with(PLAN)

    def test_plan_mode_continuation_preserves_the_mode_boundary(self):
        self.transcript_for(mode='plan')
        result = self.run_hook()
        self.assertEqual(result.get('decision'), 'block')
        self.assertIn('<proposed_plan>', result['reason'])
        self.assertIn('Do not implement', result['reason'])

    def test_attended_empty_markers_and_subagents_are_inert(self):
        original = dict(self.payload)
        for env, update in [({}, {}), ({'STORYHOOK_AUTO': ''}, {}),
                            (self.env, {'agent_id': 'child'}),
                            (self.env, {'hook_event_name': 'SubagentStop'}),
                            (self.env, {'stop_hook_active': True}),
                            ({'STORYHOOK_AUTO': 'bad/path'}, {})]:
            with self.subTest(env=env, update=update):
                self.env = env
                self.payload = original | update
                self.assertEqual(self.run_hook(), {})
        self.classify.assert_not_called()
        self.eligible.assert_not_called()

    def test_full_auto_marker_also_activates(self):
        self.env = {'STORYHOOK_FULL_AUTO': 'SH-672'}
        self.assertEqual(self.run_hook().get('decision'), 'block')

    def test_submitted_or_blocked_story_is_inert(self):
        self.eligible.return_value = False
        self.assertEqual(self.run_hook(), {})
        self.classify.assert_not_called()

    def test_story_changed_while_classifying_cannot_resume(self):
        self.eligible.side_effect = [True, False]
        self.assertEqual(self.run_hook(), {})

    def test_session_turn_and_cwd_must_match(self):
        for kwargs in [{'session': 'other'}, {'turn': 'old'}, {'source': {'subagent': {}}}, {'mode': 'unknown'}]:
            with self.subTest(kwargs=kwargs):
                self.transcript_for(**kwargs)
                result = self.run_hook()
                self.assertNotIn('decision', result)
        self.classify.assert_not_called()

    def test_unknown_or_malformed_provider_payload_never_approves(self):
        for value in [None, [], {}, 'Stop', {'hook_event_name': 'Stop'},
                      dict(self.payload, stop_hook_active='false'),
                      dict(self.payload, last_assistant_message=None)]:
            with self.subTest(value=value):
                self.assertNotIn('decision', stop.handle(value, self.env, self.classify, self.eligible))
        self.classify.assert_not_called()

    def test_malformed_transcript_shapes_are_diagnostic(self):
        original = self.transcript.read_text()
        for content in ['[]', '{"type":"session_meta","payload":[]}',
                        original + '{"type":"turn_context","payload":[]}\n',
                        original + '{"type":"turn_context","payload":{"turn_id":"turn-1","cwd":'
                        + json.dumps(str(self.root)) + ',"collaboration_mode":[]}}\n']:
            with self.subTest(content=content):
                self.transcript.write_text(content)
                result = self.run_hook()
                self.assertNotIn('decision', result)
                self.assertIn('systemMessage', result)
        self.classify.assert_not_called()

    def test_native_plan_is_left_to_existing_watcher(self):
        self.payload['last_assistant_message'] = '<proposed_plan>\n' + PLAN + '\n</proposed_plan>'
        self.assertEqual(self.run_hook(), {})
        self.classify.assert_not_called()

    def test_inline_native_tag_documentation_does_not_hide_a_prose_request(self):
        self.payload['last_assistant_message'] = PLAN + '\nThe parser must recognize `<proposed_plan>`.'
        self.assertEqual(self.run_hook().get('decision'), 'block')

    def test_negative_or_uncertain_classifier_never_approves(self):
        for answer in [{'decision': 'other', 'evidence': ''}, {'decision': 'uncertain', 'evidence': ''}]:
            with self.subTest(answer=answer):
                self.classify.return_value = answer
                self.assertNotIn('decision', self.run_hook())

    def test_invalid_or_unanchored_classifier_output_never_approves(self):
        for answer in [None, [], {}, {'decision': 'approve_plan', 'evidence': ''},
                       {'decision': 'approve_plan', 'evidence': 'invented evidence'},
                       dict(APPROVAL, command='touch hacked'), {'decision': True, 'evidence': 'x'}]:
            with self.subTest(answer=answer):
                self.classify.return_value = answer
                self.assertNotIn('decision', self.run_hook())

    def test_classifier_failure_is_diagnostic_without_approval(self):
        self.classify.side_effect = RuntimeError('Luna timed out')
        result = self.run_hook()
        self.assertNotIn('decision', result)
        self.assertIn('Luna timed out', result['systemMessage'])

    def test_one_prose_approval_per_session_even_on_a_new_turn(self):
        self.assertEqual(self.run_hook().get('decision'), 'block')
        self.assertEqual(self.run_hook(), {})
        self.transcript_for(turn='turn-2')
        self.payload['turn_id'] = 'turn-2'
        self.assertEqual(self.run_hook(), {})
        self.classify.assert_called_once()

    def test_foreign_session_does_not_consume_approval(self):
        self.transcript_for(session='other')
        self.assertNotIn('decision', self.run_hook())
        self.transcript_for()
        self.assertEqual(self.run_hook().get('decision'), 'block')

    def test_missing_transcript_cwd_cannot_inherit_the_hook_cwd(self):
        previous = os.getcwd()
        try:
            os.chdir(self.root)
            for index in (0, 1):
                self.transcript_for()
                events = [json.loads(line) for line in self.transcript.read_text().splitlines()]
                del events[index]['payload']['cwd']
                self.transcript.write_text('\n'.join(map(json.dumps, events)) + '\n')
                self.assertNotIn('decision', self.run_hook())
            self.classify.assert_not_called()
        finally:
            os.chdir(previous)

    def test_locked_or_symlinked_receipt_cannot_authorize(self):
        journal = Path(str(self.transcript) + '.storyhook-plan-approval')
        journal.symlink_to(self.transcript)
        self.assertNotIn('decision', self.run_hook())
        journal.unlink()
        with journal.open('w') as record:
            fcntl.flock(record, fcntl.LOCK_EX | fcntl.LOCK_NB)
            self.assertEqual(self.run_hook(), {})
        self.classify.assert_not_called()

    def test_large_message_is_rejected_without_classification(self):
        self.payload['last_assistant_message'] = 'x' * (stop.MAX_MESSAGE + 1)
        self.assertIn('systemMessage', self.run_hook())
        self.classify.assert_not_called()

    def test_turn_changed_during_classification_cannot_resume(self):
        def advance(message):
            self.transcript_for(turn='turn-2')
            return APPROVAL
        self.classify.side_effect = advance
        self.assertNotIn('decision', self.run_hook())

    def test_authoritative_story_state_contract(self):
        for state, superstate, awaiting, blocked, expected in [
            ('working', 'OPEN', None, [], True),
            ('verifying', 'OPEN', None, [], False),
            ('working', 'CLOSED', None, [], False),
            ('working', 'OPEN', 'human input', [], False),
            ('working', 'OPEN', None, [{'story': {'id': 'SH-672'}}], False),
        ]:
            with self.subTest(state=state, superstate=superstate, awaiting=awaiting, blocked=blocked):
                responses = [
                    {'result': 'ok', 'message': 'working (OPEN, active)\nverifying (OPEN, review)'},
                    {'result': 'ok', 'story': {'story': {'id': 'SH-672', 'state': state,
                                                       'superstate': superstate, 'awaiting': awaiting}}},
                    {'result': 'ok', 'stories': blocked},
                ]
                with patch.object(stop, 'story_json', side_effect=responses):
                    self.assertEqual(stop.eligible(str(self.root), 'SH-672'), expected)

    def test_tracker_calls_are_bounded_inside_the_hook_budget(self):
        with patch.object(stop, 'run_process', return_value='{"result":"ok"}') as run:
            stop.story_json(str(self.root), 'show', 'SH-672')
        command = run.call_args.args[0]
        self.assertEqual(command[:3], ['story', '--deadline', '2'])
        self.assertEqual(run.call_args.kwargs['timeout'], 3)

    def test_manifest_wires_synchronous_codex_stop_and_retains_handoff(self):
        manifest = json.loads((HOOKS / 'hooks.json').read_text())
        entries = [hook for group in manifest['hooks']['Stop'] for hook in group['hooks']]
        self.assertTrue(any('stop-handoff.sh' in x['command'] for x in entries))
        continuation = [x for x in entries if 'codex-stop.sh' in x['command']]
        self.assertEqual(len(continuation), 1)
        self.assertFalse(continuation[0].get('async', False))


if __name__ == '__main__':
    unittest.main()
