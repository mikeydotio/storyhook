"""PostCompact receipts bind native provider events to durable requests."""

import json
from pathlib import Path
import sys
import unittest
from unittest.mock import Mock

HOOKS = Path(__file__).resolve().parents[1] / 'hooks'
sys.path.insert(0, str(HOOKS))
import compact_receipt as compact


class CompactReceiptTests(unittest.TestCase):
    """The service validates lifecycle evidence; hooks never guess request ownership."""

    def setUp(self):
        self.payload = {'hook_event_name': 'PostCompact', 'session_id': 'session-1',
                        'cwd': '/tmp/checkout', 'transcript_path': '/tmp/transcript.jsonl',
                        'trigger': 'manual', 'turn_id': 'compact-turn'}
        self.env = {'STORYHOOK_AUTO': 'SH-1', 'TMUX': '/tmp/socket,123,0', 'TMUX_PANE': '%2'}
        self.record = {'id': 'request-1', 'story_id': 'SH-1', 'phase': 'native-continuation',
                       'status': 'awaiting-ack', 'capture': {
                           'provider': 'codex', 'session_id': 'session-1',
                           'transcript_path': '/tmp/transcript.jsonl'}}
        self.process = Mock(side_effect=[json.dumps({'result': 'ok', 'story_id': 'SH-1',
                                                    'requests': [self.record]}),
                                        json.dumps({'result': 'ok'})])

    def test_codex_compact_has_its_own_turn_and_emits_a_receipt(self):
        self.assertEqual(compact.handle(self.payload, self.env, self.process), {})
        args, kwargs = self.process.call_args
        self.assertEqual(args[0], ['story', '--deadline', '2', 'continuation', 'receipt',
                                  'SH-1', 'request-1', '--stdin', '--json'])
        receipt = json.loads(kwargs['text'])
        self.assertEqual(receipt['event'], 'post-compact')
        self.assertEqual(receipt['provider'], 'codex')
        self.assertEqual(receipt['session_id'], 'session-1')
        self.assertEqual(receipt['origin']['turn_id'], 'compact-turn')
        self.assertEqual(receipt['origin']['tmux_pane'], '%2')

    def test_claude_auto_compact_uses_the_same_receipt_contract(self):
        self.payload.pop('turn_id')
        self.payload.update(trigger='auto', compact_summary='A compacted summary.')
        self.record['capture']['provider'] = 'claude'
        self.process.side_effect = [json.dumps({'result': 'ok', 'story_id': 'SH-1',
                                               'requests': [self.record]}),
                                    json.dumps({'result': 'ok'})]
        self.assertEqual(compact.handle(self.payload, self.env, self.process), {})
        self.assertEqual(json.loads(self.process.call_args.kwargs['text'])['provider'], 'claude')

    def test_unrelated_and_completed_requests_cannot_receive_acknowledgements(self):
        for patch in ({'phase': 'administrative'}, {'status': 'acknowledged'},
                      {'story_id': 'SH-2'}, {'capture': self.record['capture'] | {
                          'session_id': 'foreign'}}, {'capture': self.record['capture'] | {
                              'provider': 'claude'}}, {'capture': self.record['capture'] | {
                                  'transcript_path': '/tmp/foreign'}}):
            with self.subTest(patch=patch):
                process = Mock(return_value=json.dumps({'result': 'ok', 'story_id': 'SH-1',
                                                        'requests': [self.record | patch]}))
                self.assertEqual(compact.handle(self.payload, self.env, process), {})
                self.assertEqual(process.call_count, 1)

    def test_ambiguous_requests_and_invalid_status_fail_visibly(self):
        for response in ({'result': 'ok', 'story_id': 'SH-1', 'requests': [self.record] * 2},
                         {'result': 'ok', 'story_id': 'SH-2', 'requests': [self.record]},
                         {'result': 'ok', 'story_id': 'SH-1', 'requests': None},
                         {'result': 'error', 'error': 'daemon unavailable'}):
            with self.subTest(response=response):
                process = Mock(return_value=json.dumps(response))
                result = compact.handle(self.payload, self.env, process)
                self.assertIn('systemMessage', result)
                self.assertNotIn('decision', result)
                self.assertEqual(process.call_count, 1)

    def test_inert_contexts_do_not_query_or_mutate(self):
        for env, patch in (({}, {}), (self.env, {'agent_id': 'child'}),
                           (self.env, {'hook_event_name': 'PreCompact'})):
            with self.subTest(env=env, patch=patch):
                self.assertEqual(compact.handle(self.payload | patch, env, self.process), {})
                self.process.assert_not_called()

    def test_invalid_event_is_not_forwarded(self):
        for patch in ({'session_id': ''}, {'cwd': 'relative'},
                      {'transcript_path': None}, {'turn_id': None}, {'trigger': 'unknown'}):
            with self.subTest(patch=patch):
                result = compact.handle(self.payload | patch, self.env, self.process)
                self.assertIn('systemMessage', result)
                self.process.assert_not_called()

    def test_receipt_error_is_not_retried(self):
        self.process.side_effect = [json.dumps({'result': 'ok', 'story_id': 'SH-1',
                                               'requests': [self.record]}),
                                    RuntimeError('receipt delivery uncertain')]
        result = compact.handle(self.payload, self.env, self.process)
        self.assertIn('receipt delivery uncertain', result['systemMessage'])
        self.assertNotIn('continue', result)
        self.assertEqual(self.process.call_count, 2)


if __name__ == '__main__':
    unittest.main()
