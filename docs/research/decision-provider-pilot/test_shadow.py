"""Focused, offline contract tests added for SH-895."""
import copy
import json
import tempfile
from pathlib import Path
import unittest
from unittest.mock import patch

from shadow import prepare, decode, MockTransport, Runner, canonical

SEEDS = Path(__file__).resolve().parent / 'fixtures/seed-cases.jsonl'
CASES = [json.loads(s) for s in SEEDS.read_text().splitlines()]


def reply(request):
    """Construct a neutral mock response, not a model prediction or gold label."""
    classes = request['classes']
    probs = {c: 1 / len(classes) for c in classes}
    answer = {'type': 'choice', 'choice': classes[0], 'confidence': 0,
              'probabilities': probs}
    if request['provider'] == 'decisions':
        answer['name'] = request['use_case']
        answer['probabilities'] = [{'value': c, 'probability': p} for c, p in probs.items()]
        answers = [answer]
    else:
        answers = {request['use_case']: answer}
    return {'model': request['body']['model'], 'answers': answers,
            'usage': {'input_tokens': 200, 'output_tokens': 0}}


class ShadowTests(unittest.TestCase):
    """Exercise admission, response validation, and accounting without external effects."""
    def setUp(self):
        self.req = prepare(CASES[0], 'decisions')

    def run_one(self, event=None, **kwargs):
        transport = MockTransport([event or {'body': reply(self.req)}])
        runner = Runner(transport)
        return runner.run(self.req, **kwargs), runner, transport

    def test_payload_is_allowlisted_and_independent_of_labels(self):
        polluted = copy.deepcopy(CASES[0])
        polluted['author_label'] = 'SECRET_LABEL'
        polluted['input']['private_context'] = 'SECRET_CONTEXT'
        for provider in ('decisions', 'jev'):
            request = prepare(polluted, provider)
            self.assertNotIn('SECRET', canonical(request['body']))
            self.assertNotIn('family_id', canonical(request['body']))
            self.assertEqual(request['body']['model'], {'decisions':'gpt-6-luna','jev':'jev-1.13.0'}[provider])

    def test_real_or_malformed_case_is_refused(self):
        for field, value in [('synthetic', False), ('use_case', 'other')]:
            case = dict(CASES[0], **{field:value})
            with self.subTest(field=field), self.assertRaises(ValueError):
                prepare(case, 'decisions')

    def test_both_provider_shapes_and_tie_order(self):
        for provider in ('decisions', 'jev'):
            req = prepare(CASES[0], provider)
            result = decode(req, canonical(reply(req)).encode())
            self.assertEqual(result['label'], 'success')
            self.assertEqual(result['status'], 'valid_advisory')

    def test_invalid_vectors_and_identity(self):
        mutations = {
            'missing_class': lambda a: a['probabilities'].pop(),
            'duplicate_class': lambda a: a['probabilities'].append(a['probabilities'][0]),
            'nan_probability': lambda a: a['probabilities'][0].update(probability=float('nan')),
            'negative_probability': lambda a: a['probabilities'][0].update(probability=-.1),
            'over_one_probability': lambda a: a['probabilities'][0].update(probability=1.1),
            'sum_outside_tolerance': lambda a: a['probabilities'][0].update(probability=.5),
            'wrong_question_id': lambda a: a.update(name='wrong'),
            'boolean_probability': lambda a: a['probabilities'][0].update(probability=True),
            'wrong_choice': lambda a: a.update(choice='bogus'),
        }
        for name, mutate in mutations.items():
            obj = reply(self.req); mutate(obj['answers'][0])
            with self.subTest(name=name):
                result, _, _ = self.run_one({'body':obj})
                self.assertEqual(result['status'], 'invalid_response')

    def test_duplicate_question_and_json_keys_are_refused(self):
        obj = reply(self.req); obj['answers'] *= 2
        self.assertEqual(self.run_one({'body':obj})[0]['status'], 'invalid_response')
        for raw in (b'{"answers":[],"answers":[]}', b'{', b'{}'):
            self.assertEqual(self.run_one({'raw':raw})[0]['status'], 'invalid_response')

    def test_refusal_and_response_size_limit(self):
        obj = reply(self.req); obj['answers'] = [{'name':'runtime','type':'refusal'}]
        self.assertEqual(self.run_one({'body':obj})[0]['status'], 'refusal')
        self.assertEqual(self.run_one({'raw':b' ' * 65537})[0]['status'], 'response_too_large')

    def test_rounding_retains_raw_vector(self):
        obj = reply(self.req); obj['answers'][0]['probabilities'][0]['probability'] += .0005
        result = decode(self.req, canonical(obj).encode())
        self.assertAlmostEqual(sum(result['probabilities'].values()), 1)
        self.assertNotEqual(result['raw_probabilities'], result['probabilities'])

    def test_http_errors_never_retry_and_auth_stops_arm(self):
        for status in (401,403,422,429,529,500):
            result, runner, transport = self.run_one({'http_status':status})
            self.assertEqual(result['status'], 'http_error')
            self.assertEqual(transport.calls, 1)
            second = prepare(CASES[1], 'decisions')
            if status in (401,403):
                self.assertEqual(runner.run(second)['status'], 'arm_stopped')
                self.assertEqual(transport.calls, 1)

    def test_disconnect_and_timeout_never_retry(self):
        for error in ('disconnect','timeout'):
            result, _, transport = self.run_one({'error':error})
            self.assertEqual(result['status'], error)
            self.assertEqual(transport.calls, 1)

    def test_guards_make_zero_calls(self):
        for guard in ('disabled','unconfigured','cancelled','manual_override',
                      'stale_input_revision','stale_configuration_generation'):
            result, _, transport = self.run_one(guard=guard)
            self.assertEqual(result['status'], guard)
            self.assertEqual(transport.calls, 0)

    def test_guards_rechecked_after_delivery(self):
        for guard in ('cancelled','manual_override','stale_input_revision','stale_configuration_generation'):
            result, _, transport = self.run_one({'body':reply(self.req),'after_guard':guard})
            self.assertEqual(result['status'], guard)
            self.assertEqual(transport.calls, 1)

    def test_deadlines_reject_late_result_and_prevent_late_start(self):
        result, runner, transport = self.run_one({'body':reply(self.req),'elapsed':3})
        self.assertEqual(result['status'], 'late_result')
        runner.elapsed = 900
        self.assertEqual(runner.run(prepare(CASES[1],'decisions'))['status'], 'campaign_deadline')
        self.assertEqual(transport.calls, 1)

    def test_campaign_remaining_budget_shortens_request(self):
        transport = MockTransport([{'body':reply(self.req),'elapsed':2}])
        runner = Runner(transport); runner.elapsed = 899
        self.assertEqual(runner.run(self.req)['status'], 'late_result')
        self.assertEqual(transport.deadlines, [1])

    def test_duplicate_delivery_not_reissued(self):
        result, runner, transport = self.run_one()
        self.assertEqual(runner.run(self.req)['status'], 'duplicate_delivery')
        self.assertEqual(transport.calls, 1)

    def test_unknown_cost_keeps_reservation_and_caps_apply_before_send(self):
        _, runner, transport = self.run_one({'error':'timeout'})
        self.assertEqual(str(runner.reserved), '0.0125')
        runner.reserved = runner.cap
        self.assertEqual(runner.run(prepare(CASES[1],'decisions'))['status'], 'spend_cap')
        self.assertEqual(transport.calls, 1)

    def test_count_and_token_caps(self):
        for field,value,expected in [('attempts',80,'request_cap')]:
            transport=MockTransport([]); runner=Runner(transport); setattr(runner,field,value)
            self.assertEqual(runner.run(self.req)['status'],expected)
            self.assertEqual(transport.calls,0)
        for tokens in (8001,-1,True):
            obj=reply(self.req); obj['usage']['input_tokens']=tokens
            self.assertEqual(self.run_one({'body':obj})[0]['status'],'usage_bound_violation')

    def test_model_drift_stops_arm(self):
        obj=reply(self.req);obj['model']='different-model'
        result, runner, _=self.run_one({'body':obj})
        self.assertEqual(result['status'],'model_drift')
        self.assertIn('decisions',runner.stopped)

    def test_no_live_transport_and_invalid_mock_is_loud(self):
        with self.assertRaises(TypeError):
            Runner(lambda request: None)
        with self.assertRaises(ValueError):
            self.run_one({'elapsed':-1})

    def test_jev_duplicate_keys_missing_class_and_wrong_question(self):
        req=prepare(CASES[0],'jev')
        for mutation in ('missing','wrong_question','bad_confidence','inconsistent_choice'):
            obj=reply(req);answer=obj['answers']['runtime']
            if mutation=='missing': answer['probabilities'].pop('success')
            if mutation=='wrong_question': obj['answers']['wrong']=obj['answers'].pop('runtime')
            if mutation=='bad_confidence': answer['confidence']=float('inf')
            if mutation=='inconsistent_choice': answer['choice']='wrong'
            with self.subTest(mutation=mutation):
                self.assertEqual(decode(req,canonical(obj).encode())['status'],'invalid_response')
        raw=canonical(reply(req)).replace('"success":', '"success":0,"success":').encode()
        self.assertEqual(decode(req,raw)['status'],'invalid_response')

    def test_refused_request_never_consumes_event_and_hash_binds_body(self):
        req=copy.deepcopy(self.req);req['body']['input']='changed after preview'
        transport=MockTransport([]);runner=Runner(transport)
        with self.assertRaises(ValueError): runner.run(req)
        self.assertEqual(transport.calls,0)
        case=copy.deepcopy(CASES[0]);case['input']['text']='x'*8001
        with self.assertRaises(ValueError): prepare(case,'decisions')

    def test_per_provider_limit_does_not_stop_other_arm(self):
        req=prepare(CASES[0],'jev')
        transport=MockTransport([{'body':reply(req)}]);runner=Runner(transport)
        runner.counts['decisions']=40
        self.assertEqual(runner.run(self.req)['status'],'request_cap')
        self.assertEqual(runner.run(req)['status'],'valid_advisory')
        self.assertEqual(transport.calls,1)

    def test_complexity_deadline_and_missing_usage(self):
        req=prepare(CASES[20],'jev')
        runner=Runner(MockTransport([{'body':reply(req),'elapsed':9.99}]))
        result=runner.run(req)
        self.assertEqual(result['status'],'valid_advisory')
        self.assertEqual(result['deadline_seconds'],10)
        obj=reply(req);del obj['usage']
        self.assertEqual(decode(req,canonical(obj).encode())['status'],'invalid_response')

    def test_eighty_requests_separate_use_cases_and_providers(self):
        requests=[prepare(case,p) for case in CASES for p in ('decisions','jev')]
        transport=MockTransport([{'body':reply(r)} for r in requests]);runner=Runner(transport)
        for req in requests:
            result=runner.run(req)
            self.assertEqual(result['story_writes'],0)
            self.assertFalse(result['authority'])
        summary=runner.summary()
        self.assertEqual(summary['mode'],'mock_only')
        self.assertEqual(summary['provider_calls'],0)
        self.assertEqual(runner.attempts,80)
        self.assertEqual(str(runner.reserved),'1.0000')
        for use in ('runtime','complexity'):
            for p in ('decisions','jev'):
                self.assertEqual(summary['groups'][use][p]['attempts'],20)

    def test_packet_regeneration_preserves_human_review(self):
        import prepare_packet
        with tempfile.TemporaryDirectory(dir='/tmp') as directory:
            target=Path(directory)
            (target/'provisional-review.json').write_bytes(
                (SEEDS.parent.parent/'provisional-review.json').read_bytes())
            worksheet=target/'human-labels.csv'
            worksheet.write_text('case_id,reviewer_a_label\nrun-01-1,success\n')
            original=worksheet.read_bytes()
            with patch.object(prepare_packet,'ROOT',target), self.assertRaises(ValueError):
                prepare_packet.main()
            self.assertEqual(worksheet.read_bytes(),original)
            self.assertFalse((target/'requests.jsonl').exists())


if __name__ == '__main__':
    unittest.main()
