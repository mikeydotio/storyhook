"""Provider boundary regressions for durable autonomous session handoffs."""

import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import Mock

HOOKS = Path(__file__).resolve().parents[1] / 'hooks'
sys.path.insert(0, str(HOOKS))
import session_handoff as handoff


def request():
    """Return a fresh explicit context request for each mutation case."""
    return {'type': 'storyhook.session-handoff', 'version': 1,
            'story_id': 'SH-1', 'kind': 'context',
            'evidence': {'context': 'Context is exhausted.',
                         'outstanding_work': 'Finish the approved regression.'}}


class HandoffSchemaTests(unittest.TestCase):
    """Malformed administrative requests never fall through to plan approval."""

    def test_complete_context_and_unrelated_prose(self):
        self.assertEqual(handoff.parse_request(json.dumps(request()), 'SH-1'), request())
        self.assertIsNone(handoff.parse_request('Work is complete.', 'SH-1'))

    def test_invalid_candidates_are_rejected(self):
        base = request()
        candidates = [
            json.dumps(base | {'version': True}),
            json.dumps(base | {'version': 2}),
            json.dumps(base | {'story_id': 'SH-2'}),
            json.dumps(base | {'kind': 'permission'}),
            json.dumps(base | {'type': 'storyhook.session-handoff-v2'}),
            json.dumps(base | {'type': 'typo'}),
            json.dumps(base | {'extra': 'authority'}),
            json.dumps(base | {'evidence': []}),
            json.dumps(base | {'evidence': {'context': ' ', 'outstanding_work': 'x'}}),
            json.dumps(base | {'evidence': {'context': 'x'}}),
            json.dumps(base | {'evidence': {'context': 'x', 'outstanding_work': 3}}),
            json.dumps(base | {'evidence': {'context': '\ud800', 'outstanding_work': 'x'}}),
            '```json\n' + json.dumps(base) + '\n```',
            json.dumps(base) + '\nImplement this plan?',
            json.dumps(base).replace('"version": 1', '"version": 1, "version": 1'),
            json.dumps(base).replace('"context":', '"context":"duplicate", "context":'),
            json.dumps(base).replace('storyhook.session-handoff', r'storyhook.\u0073ession-handoff')[:-1],
        ]
        for text in candidates:
            with self.subTest(text=text), self.assertRaises(ValueError):
                handoff.parse_request(text, 'SH-1')

    def test_obviation_evidence_is_an_administrative_request(self):
        value = request() | {'kind': 'obviation-review', 'evidence': {
            'context': 'Both candidates implement this acceptance criterion.',
            'candidates': ['SH-2', 'SH-3'], 'original_state': 'in-progress'}}
        self.assertEqual(handoff.parse_request(json.dumps(value), 'SH-1'), value)
        for patch in ({'candidates': []}, {'candidates': ['SH-2', 'SH-2']},
                      {'candidates': ['SH-1']}, {'candidates': ['not-a-story']},
                      {'candidates': 'SH-2'}, {'original_state': ''},
                      {'original_state': '../in-progress'}):
            with self.subTest(patch=patch), self.assertRaises(ValueError):
                handoff.parse_request(json.dumps(value | {
                    'evidence': value['evidence'] | patch}), 'SH-1')


class HandoffProviderTests(unittest.TestCase):
    """Validate root transcript evidence before forwarding supervisor requests."""

    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix='sh711-hooks-', dir='/tmp')
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.transcript = self.root / 'transcript.jsonl'
        self.message = json.dumps(request())
        self.payload = {'hook_event_name': 'Stop', 'session_id': 'session-1',
                        'cwd': str(self.root), 'transcript_path': str(self.transcript),
                        'stop_hook_active': False, 'last_assistant_message': self.message,
                        'permission_mode': 'plan'}
        self.event = {'type': 'assistant', 'sessionId': 'session-1',
                      'cwd': str(self.root), 'isSidechain': False, 'uuid': 'turn-1',
                      'message': {'role': 'assistant', 'content': [
                          {'type': 'text', 'text': self.message}]}}
        self.write_transcript(self.event)
        self.process = Mock(return_value=json.dumps({'result': 'ok', 'native_feedback': True, 'continuation': {
            'id': 'request-1', 'story_id': 'SH-1', 'status': 'awaiting-ack',
            'phase': 'native-continuation'}}))

    def write_transcript(self, *events):
        """Use provider-shaped records; no production validation is mocked."""
        self.transcript.write_text(''.join(json.dumps(event) + '\n' for event in events))

    def test_claude_stop_forwards_current_root_without_approval(self):
        result = handoff.handle_stop(self.payload, {'STORYHOOK_AUTO': 'SH-1'},
                                     'claude', self.process)
        self.assertEqual(result.get('decision'), 'block', result)
        self.assertIn('Preserve the current collaboration mode', result['reason'])
        args, kwargs = self.process.call_args
        self.assertEqual(args[0], ['story', '--deadline', '2', 'continuation',
                                  'request', 'SH-1', '--stdin', '--json'])
        document = json.loads(kwargs['text'])
        self.assertEqual(document['handoff'], request())
        self.assertEqual(document['provider'], 'claude')
        self.assertEqual(document['origin']['turn_id'], 'turn-1')
        self.assertEqual(document['origin']['collaboration_mode'], 'plan')
        self.assertIn('Do not acknowledge while in Plan mode', result['reason'])
        self.assertIn('after ordinary plan approval switches to Default mode', result['reason'])
        self.assertIn('before implementation', result['reason'])

    def test_only_accepted_native_context_receipt_continues(self):
        for patch in ({'id': ''}, {'story_id': 'SH-2'}, {'status': 'needs-attention'},
                      {'phase': 'observe'}, {'status': 'acknowledged'}):
            with self.subTest(patch=patch):
                self.process.return_value = json.dumps({'result': 'ok', 'native_feedback': True, 'continuation': {
                    'id': 'request-1', 'story_id': 'SH-1', 'status': 'awaiting-ack',
                    'phase': 'native-continuation'} | patch})
                result = handoff.handle_stop(self.payload, {'STORYHOOK_AUTO': 'SH-1'},
                                             'claude', self.process)
                self.assertNotIn('decision', result)
                self.assertIn('systemMessage', result)

    def test_only_atomic_first_delivery_gets_native_feedback(self):
        answer = json.loads(self.process.return_value)
        self.process.return_value = json.dumps(answer | {'native_feedback': False})
        self.assertEqual(handoff.handle_stop(self.payload, {'STORYHOOK_AUTO': 'SH-1'},
                                            'claude', self.process), {})
        for value in (None, 1, 'true'):
            with self.subTest(value=value):
                self.process.return_value = json.dumps(answer | {'native_feedback': value})
                result = handoff.handle_stop(self.payload, {'STORYHOOK_AUTO': 'SH-1'},
                                             'claude', self.process)
                self.assertNotIn('decision', result)
                self.assertIn('systemMessage', result)
        answer.pop('native_feedback')
        self.process.return_value = json.dumps(answer)
        self.assertIn('systemMessage', handoff.handle_stop(
            self.payload, {'STORYHOOK_AUTO': 'SH-1'}, 'claude', self.process))

    def test_obviation_has_no_native_continuation_or_plan_approval(self):
        value = request() | {'kind': 'obviation-review', 'evidence': {
            'context': 'Existing delivery may cover this work.',
            'candidates': ['SH-2'], 'original_state': 'in-progress'}}
        self.payload['last_assistant_message'] = json.dumps(value)
        self.event['message']['content'][0]['text'] = json.dumps(value)
        self.write_transcript(self.event)
        self.process.return_value = json.dumps({'result': 'ok', 'continuation': {
            'id': 'request-1', 'story_id': 'SH-1', 'status': 'acknowledged',
            'phase': 'administrative'}})
        self.assertEqual(handoff.handle_stop(self.payload, {'STORYHOOK_AUTO': 'SH-1'},
                                            'claude', self.process), {})
        self.process.reset_mock()
        self.assertEqual(handoff.handle_stop(self.payload | {'stop_hook_active': True},
                                            {'STORYHOOK_AUTO': 'SH-1'},
                                            'claude', self.process), {})
        self.process.assert_called_once()

    def test_claude_root_identity_and_last_message_must_match(self):
        for change in ({'sessionId': 'foreign'}, {'cwd': '/foreign'},
                       {'isSidechain': True}, {'isSidechain': None},
                       {'uuid': ''}, {'message': {'role': 'assistant', 'content': [
                           {'type': 'text', 'text': 'older handoff'}]}}):
            with self.subTest(change=change):
                self.write_transcript(self.event | change)
                result = handoff.handle_stop(self.payload, {'STORYHOOK_AUTO': 'SH-1'},
                                             'claude', self.process)
                self.assertIn('systemMessage', result)
                self.process.assert_not_called()

    def test_claude_rejects_a_newer_user_turn(self):
        self.write_transcript(self.event, self.event | {'type': 'user', 'uuid': 'turn-2'})
        result = handoff.handle_stop(self.payload, {'STORYHOOK_AUTO': 'SH-1'},
                                     'claude', self.process)
        self.assertIn('systemMessage', result)
        self.process.assert_not_called()

    def test_attended_subagent_and_other_provider_remain_inert(self):
        for env, patch in (({}, {}), ({'STORYHOOK_AUTO': 'SH-1'}, {'agent_id': 'child'}),
                           ({'STORYHOOK_AUTO': 'SH-1'}, {'stop_hook_active': True}),
                           ({'STORYHOOK_AUTO': 'SH-1'}, {'hook_event_name': 'SubagentStop'}),
                           ({'STORYHOOK_AUTO': 'SH-1'}, {'turn_id': 'codex-turn'})):
            with self.subTest(patch=patch, env=env):
                self.assertIsNone(handoff.handle_stop(self.payload | patch, env,
                                                       'claude', self.process))
                self.process.assert_not_called()

    def test_request_failure_never_approves_or_requests_a_retry(self):
        self.process.side_effect = RuntimeError('daemon delivery unknown')
        result = handoff.handle_stop(self.payload, {'STORYHOOK_AUTO': 'SH-1'},
                                     'claude', self.process)
        self.assertIn('daemon delivery unknown', result['systemMessage'])
        self.assertNotIn('decision', result)
        self.assertEqual(self.process.call_count, 1)


if __name__ == '__main__':
    unittest.main()
