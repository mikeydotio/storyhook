"""Create an offline SH-895 approval packet and explicitly simulated results."""
import csv
import hashlib
import json
from pathlib import Path

from shadow import CRITERIA, PROVIDERS, MockTransport, Runner, canonical, prepare

ROOT = Path(__file__).resolve().parent
SOURCE = ROOT / 'fixtures'


def write_json(name, value):
    """Write a task-owned evidence file; never touch the original seed packet."""
    (ROOT / name).write_text(json.dumps(value,indent=2,ensure_ascii=False)+'\n')


def main():
    """Build exact request previews, blank human worksheets, and mock-only evidence."""
    cases=[json.loads(line) for line in (SOURCE/'seed-cases.jsonl').read_text().splitlines()]
    review=json.loads((ROOT/'provisional-review.json').read_text())
    labels={row['case_id']:row for row in review['cases']}
    if len(cases)!=40 or len(labels)!=40 or set(labels)!={c['case_id'] for c in cases}:
        raise ValueError('Review and seed identities do not match the 40-case proposal')
    worksheet=ROOT/'human-labels.csv'
    if worksheet.exists():
        with worksheet.open(newline='') as handle:
            for row in csv.DictReader(handle):
                if any(value for key,value in row.items()
                       if key not in ('case_id','use_case','family_id','variant','text')):
                    raise ValueError('Human review exists; preserve it before regenerating this packet')
    requests=[prepare(case,p) for case in cases for p in PROVIDERS]
    (ROOT/'requests.jsonl').write_text(''.join(canonical(req)+'\n' for req in requests))
    fields=['case_id','use_case','family_id','variant','text','reviewer_a_label',
            'reviewer_a_notes','reviewer_b_label','reviewer_b_notes','adjudicated_label',
            'adjudication_notes','guard_disposition','target_snapshot','applicability']
    with (ROOT/'human-labels.csv').open('w',newline='') as handle:
        writer=csv.DictWriter(handle,fieldnames=fields,lineterminator='\n');writer.writeheader()
        for case in cases:
            row={k:case[k] for k in fields[:4]};row['text']=case['input']['text'];writer.writerow(row)
    with (ROOT/'provisional-labels.csv').open('w',newline='') as handle:
        fields=['case_id','proposed_label','alternatives','ambiguity_rationale','required_guard','status']
        writer=csv.DictWriter(handle,fieldnames=fields,lineterminator='\n');writer.writeheader()
        for case in cases:
            row=labels[case['case_id']]
            writer.writerow({'case_id':case['case_id'],'proposed_label':row['proposed_label'],
                'alternatives':canonical(row['alternatives']),'ambiguity_rationale':row['ambiguity_rationale'],
                'required_guard':row['guard_disposition']['required_disposition'],
                'status':'AI provisional; not human-adjudicated ground truth'})
    sources=[{'path':'fixtures/'+name,'sha256':hashlib.sha256((SOURCE/name).read_bytes()).hexdigest()}
             for name in ('seed-cases.jsonl','protocol.json','human-labels.jsonl')]
    write_json('pilot-proposal.json',{
        'status':'DRAFT; external execution not authorized; offline preparation only',
        'story':'SH-895','schema_version':1,'source_files':sources,
        'request_manifest_sha256':hashlib.sha256((ROOT/'requests.jsonl').read_bytes()).hexdigest(),
        'models':PROVIDERS,
        'model_identity_limit':'Jev uses the published version ID jev-1.13.0. Decisions publishes gpt-6-luna; an immutable snapshot was not established. Record the returned model; stop that arm on any mismatch. No silent alias/version substitution.',
        'data':'Only synthetic subject_id, task_kind and text, plus frozen rubric/class instructions. No labels, author notes, family/split metadata, local paths, real stories, repository code or logs in request bodies.',
        'request_order':'Case order from the source packet, Decisions then Jev per case. Serial; 20 cases per provider per use case; descriptive latency only, not a randomized performance comparison.',
        'cases':{'runtime':20,'complexity':20,'families':20,'variants_per_family':2,'all_development':True},
        'maximum_requests':80,'maximum_per_provider':40,'attempts_per_pair':1,'retries':0,'concurrency':1,
        'limits':{'campaign_wall_seconds':900,'runtime_deadline_seconds':3,'complexity_deadline_seconds':10,
                  'input_tokens_per_request':8000,'prepared_body_bytes':8000,'response_bytes':65536,'spend_usd':'1.00'},
        'price_basis':{'verified_date':'2026-10-09','decisions_input_usd_per_million':'0.10',
          'jev_input_usd_per_million':'0.042','output_charge':'None for these decision endpoints',
          'maximum_base_estimate_usd':'0.04544','calculation':'40 * 8000 * (0.10 + 0.042) / 1000000',
          'sources':['https://developers.openai.com/api/docs/guides/decisions','https://docs.typesafe.ai/models'],
          'limitations':'Base published rates only; account contract, region, token overhead, taxes or surcharges are not verified. No provider request was made to check them.'},
        'cap_enforcement':{
          'mock_implemented':'Before each attempt, permanently reserve $0.0125. Never refund failed, timed-out or unknown usage. Refuse before attempt 81, provider attempt 41, or a reservation over $1. Stop provider on authentication refusal, model mismatch or reported token-bound violation. All attempts counted; no automatic retry.',
          'live_prerequisites':'A separately approved live executor must prove a worst-case all-in request cost <= $0.0125 using an established billed-token bound and applicable price contract BEFORE sending. Byte size is not a tokenizer. Stop if a bound cannot be proven. Persist reservations and request IDs before sending; crash/restart may not reset budget or resend uncertain requests.',
          'deadline':'Use an absolute monotonic campaign deadline and per-call minimum of remaining campaign time and 3s/10s. Network transport must enforce total connection/read/cancellation deadlines and bounded body streaming. Mock arithmetic does not prove OS cancellation.'},
        'missing_approval_fields':{'approved_accounts':None,'approved_region_and_retention':None,
          'approved_price_adjustments_and_token_bound':None,'approved_taxonomy_and_prompt_hashes':None,
          'human_adjudication_receipt':None,'explicit_user_paid_pilot_authorization':None},
        'scope_limit':'No production integration, provider choice, global setting, story write or native lane. Runtime and complexity groups remain separate. No quality scores from mock replies or provisional labels.',
        'future_executor':'Not implemented. This packet has no credential loader or live HTTP adapter. Later external execution requires the missing approval fields and a tested bounded live executor; approval alone cannot make this offline script send requests.',
        'taxonomy_pending':review['taxonomy_review'],
        'design_approval':'This offline script/test preparation is explicitly authorized. Production integration remains outside scope; no provider selection or live execution is authorized.'})
    events=[]
    for req in requests:
        probabilities={c:float(c=='insufficient_evidence') for c in req['classes']}
        answer={'type':'choice','choice':'insufficient_evidence','confidence':1,'probabilities':probabilities}
        if req['provider']=='decisions':
            answer['name']=req['use_case']
            answer['probabilities']=[{'value':c,'probability':p} for c,p in probabilities.items()]
            answers=[answer]
        else: answers={req['use_case']:answer}
        events.append({'body':{'model':req['body']['model'],'answers':answers,
                       'usage':{'input_tokens':200,'output_tokens':0}},'elapsed':.01})
    runner=Runner(MockTransport(events))
    for req in requests: runner.run(req)
    write_json('mock-results.json',{'warning':'Fabricated fixture responses, NOT provider results or human labels.',
                                  'summary':runner.summary(),'results':runner.results})


if __name__=='__main__':
    main()
