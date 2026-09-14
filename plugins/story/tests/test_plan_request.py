"""SH-687: exercise structured requests through the production Stop boundary."""
import json
import unittest

import test_codex_stop as fixture

PLAN = fixture.PLAN


class StructuredPlanTests(unittest.TestCase):
    """A protocol declaration bypasses classification, never the approval guards."""

    setUp = fixture.StopTests.setUp
    transcript_for = fixture.StopTests.transcript_for
    run_hook = fixture.StopTests.run_hook

    def request(self, **changes):
        return json.dumps(dict(type='storyhook.implementation-plan', version=1,
                               story_id='SH-672', plan=PLAN) | changes)

    def test_default_request_bypasses_classifier_and_preserves_plan_bytes(self):
        plan = '  First comment this plan.\nThen test `$(literal)` and Unicode: π.\n'
        self.payload['last_assistant_message'] = self.request(plan=plan)
        self.classify.side_effect = AssertionError('structured requests must not spend a model call')
        result = self.run_hook()
        self.assertEqual(result.get('decision'), 'block')
        self.assertIn('decoded plan', result['reason'])
        self.assertIn('operational', result['reason'])
        self.assertIn('verbatim', result['reason'])
        self.assertEqual(self.eligible.call_count, 2)
        journal = json.loads(self.transcript.with_name(self.transcript.name + '.storyhook-plan-approval').read_text())
        import hashlib
        self.assertEqual(journal['plan_sha256'], hashlib.sha256(plan.encode()).hexdigest())
        self.assertEqual(journal['source'], 'structured')
        self.assertEqual(self.run_hook(), {})

    def test_plan_mode_redirects_to_native_review_without_implementation(self):
        self.transcript_for(mode='plan')
        self.payload['last_assistant_message'] = self.request()
        result = self.run_hook()
        self.assertEqual(result.get('decision'), 'block')
        self.assertIn('<proposed_plan>', result['reason'])
        self.assertIn('Do not implement', result['reason'])
        self.assertIn('decoded plan', result['reason'])
        self.classify.assert_not_called()

    def test_malformed_or_quoted_requests_never_fall_through_to_classification(self):
        good = self.request()
        cases = [self.request(version=v) for v in [0, 2, True, 1.0, '1', None]]
        cases += [self.request(plan=p) for p in ['', ' \n', None, [], {}]]
        cases += [self.request(story_id='SH-999'), self.request(command='deploy'),
                  self.request(type='unknown'),
                  json.dumps({'version': 1, 'story_id': 'SH-672', 'plan': PLAN}),
                  good[:-1], good + good, '```json\n' + good + '\n```',
                  '> ' + good, 'Example: ' + good, json.dumps(good),
                  good.replace('"version": 1', '"version": 1, "version": 1'),
                  self.request(type='storyhook.implementation-plan-v2')]
        for message in cases:
            with self.subTest(message=message):
                self.payload['last_assistant_message'] = message
                result = self.run_hook()
                self.assertNotIn('decision', result)
                self.assertIn('systemMessage', result)
        self.classify.assert_not_called()

    def test_json_escaping_and_property_order_do_not_change_the_protocol(self):
        self.payload['last_assistant_message'] = self.request().replace('storyhook', 'story\\u0068ook')
        self.assertEqual(self.run_hook().get('decision'), 'block')
        self.classify.assert_not_called()

    def test_structured_request_shares_identity_and_state_guards(self):
        self.payload['last_assistant_message'] = self.request()
        self.transcript_for(turn='old')
        self.assertNotIn('decision', self.run_hook())
        self.transcript_for()
        self.eligible.return_value = False
        self.assertEqual(self.run_hook(), {})
        self.eligible.side_effect = [True, False]
        self.assertEqual(self.run_hook(), {})
        self.classify.assert_not_called()

    def test_attended_and_subagent_requests_are_inert(self):
        self.payload['last_assistant_message'] = self.request()
        env = self.env
        self.env = {}
        self.assertEqual(self.run_hook(), {})
        self.env = env
        self.payload['agent_id'] = 'child'
        self.assertEqual(self.run_hook(), {})
        self.classify.assert_not_called()


if __name__ == '__main__':
    unittest.main()
