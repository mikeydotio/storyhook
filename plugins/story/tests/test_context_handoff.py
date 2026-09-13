"""SH-711: typed context handoffs must reach the supervisor at root Stop."""

import importlib.util
import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

HOOKS = Path(__file__).resolve().parents[1] / 'hooks'
sys.path.insert(0, str(HOOKS))
spec = importlib.util.spec_from_file_location('handoff_codex_stop', HOOKS / 'codex_stop.py')
stop = importlib.util.module_from_spec(spec)
spec.loader.exec_module(stop)
import codex_classifier


class ContextHandoffRegression(unittest.TestCase):
    """Run the real handler, eligibility decoder, classifier, and transcript checks."""

    def test_eligible_root_context_handoff_reaches_supervisor_in_both_modes(self):
        """An administrative handoff survives the same boundary as ordinary plans."""
        for mode in ('default', 'plan'):
            with self.subTest(mode=mode), tempfile.TemporaryDirectory(
                    prefix='sh711-context-stop-', dir='/tmp') as directory:
                root = Path(directory)
                transcript = root / 'rollout.jsonl'
                transcript.write_text('\n'.join(map(json.dumps, [
                    {'type': 'session_meta', 'payload': {
                        'id': 'session-1', 'cwd': directory, 'source': 'cli'}},
                    {'type': 'turn_context', 'payload': {
                        'turn_id': 'turn-1', 'cwd': directory,
                        'collaboration_mode': {'mode': mode}}},
                ])) + '\n')
                payload = {
                    'hook_event_name': 'Stop', 'session_id': 'session-1',
                    'turn_id': 'turn-1', 'cwd': directory,
                    'transcript_path': str(transcript), 'stop_hook_active': False,
                }
                handoff = {
                    'type': 'storyhook.session-handoff', 'version': 1,
                    'story_id': 'SH-1', 'kind': 'context',
                    'evidence': {
                        'context': 'Context is exhausted; the approved parser fix remains open.',
                        'outstanding_work': 'Implement the approved parser fix and run its regression.',
                    },
                }
                calls = []

                def external_process(argv, **kwargs):
                    """Substitute external process responses, never hook decisions."""
                    calls.append((argv, kwargs))
                    if argv[:2] == ['codex', '--version']:
                        return codex_classifier.SUPPORTED_VERSION
                    if argv[:2] == ['codex', 'exec']:
                        return '\n'.join(map(json.dumps, [
                            {'type': 'item.completed', 'item': {
                                'type': 'agent_message', 'text': json.dumps({
                                    'decision': 'other', 'evidence': ''})}},
                            {'type': 'turn.completed'},
                        ]))
                    if 'session-eligibility' in argv:
                        return json.dumps({'result': 'ok', 'session_eligibility': {
                            'schema_version': 1, 'story_id': 'SH-1',
                            'eligible': True, 'reason': 'eligible'}})
                    if 'continuation' in argv and 'request' in argv:
                        return json.dumps({'result': 'ok', 'native_feedback': True, 'continuation': {
                            'id': 'request-1', 'story_id': 'SH-1',
                            'status': 'awaiting-ack', 'phase': 'native-continuation'}})
                    raise AssertionError(f'Unexpected external command: {argv!r}')

                with patch.object(stop, 'run_process', side_effect=external_process), \
                        patch.object(codex_classifier, 'run_process', side_effect=external_process):
                    # Establish that this identity/store fixture permits ordinary
                    # approval without consuming the handoff session's receipt.
                    control = root / 'control.jsonl'
                    control.write_text(transcript.read_text())
                    plan_result = stop.handle(payload | {
                        'transcript_path': str(control),
                        'last_assistant_message': json.dumps({
                            'type': 'storyhook.implementation-plan', 'version': 1,
                            'story_id': 'SH-1', 'plan': 'Fix the parser and test the regression.'}),
                    }, {'STORYHOOK_AUTO': 'SH-1'})
                    self.assertEqual(plan_result.get('decision'), 'block', plan_result)
                    result = stop.handle(payload | {
                        'last_assistant_message': json.dumps(handoff),
                    }, {'STORYHOOK_AUTO': 'SH-1'})
                    calls_before = len(calls)
                    self.assertEqual(stop.handle(payload | {
                        'stop_hook_active': True,
                        'last_assistant_message': json.dumps(handoff),
                    }, {'STORYHOOK_AUTO': 'SH-1'}), {})
                    self.assertEqual(len(calls), calls_before)
                    self.assertEqual(stop.handle(payload | {
                        'stop_hook_active': True,
                        'last_assistant_message': json.dumps(handoff | {
                            'kind': 'obviation-review', 'evidence': {
                                'context': 'Receiving review found a candidate.',
                                'candidates': ['SH-2'], 'original_state': 'in-progress'}}),
                    }, {'STORYHOOK_AUTO': 'SH-1'}), {})
                    self.assertEqual(len(calls), calls_before + 1)

                self.assertTrue(
                    any('continuation' in argv and 'request' in argv for argv, _ in calls),
                    f'Valid {mode} root context handoff was silently discarded: {result!r}; '
                    'eligibility is true and the ordinary plan control was approved',
                )
                self.assertFalse(any(argv[:2] == ['codex', 'exec'] for argv, _ in calls),
                                 'Administrative handoffs must never reach the plan classifier')
                self.assertFalse(Path(str(transcript) + '.storyhook-plan-approval').exists(),
                                 'Administrative delivery cannot consume plan approval authority')
                self.assertEqual(result.get('decision'), 'block', result)
                self.assertIn('Preserve the current collaboration mode', result['reason'])
                self.assertIn('request-1', result['reason'])
                self.assertNotIn('plan is approved', result['reason'])


if __name__ == '__main__':
    unittest.main()
