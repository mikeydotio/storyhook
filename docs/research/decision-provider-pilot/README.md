# SH-895 offline shadow preparation

This is a reviewable offline research packet for SH-895. Start with the
[human review guide](REVIEW.md) and [blank worksheet](human-labels.csv). It makes
no provider calls and reads no credentials. It cannot run a live pilot: there is
no HTTP transport, SDK, environment lookup or credential loader. No native lane,
production configuration or release is involved. Publication does not authorize
a pilot, select a provider, or complete SH-895.

## Review the proposed pilot

- [pilot-proposal.json](pilot-proposal.json): limits, prices, exact model strings, missing approval
  fields, taxonomy questions and live-execution prerequisites.
- [requests.jsonl](requests.jsonl): all 80 proposed request bodies and SHA-256 hashes, with local
  metadata outside each `body`. Only `body` would be sent to the named endpoint.
- [human-labels.csv](human-labels.csv): all 40 case texts, blank columns for two independent human
  reviews, adjudication, target snapshot, applicability and guard disposition.
- [provisional-review.json](provisional-review.json) and [provisional-labels.csv](provisional-labels.csv): one independent AI
  review, explicitly **not human-adjudicated ground truth**. The reviewer read
  the seeds, protocol and rubric, without author notes or prior proposed labels.
- [mock-results.json](mock-results.json): scripted fixture outputs, **not provider predictions**.
  Runtime and complexity each retain separate Decisions/Jev groups. No accuracy,
  calibration or human agreement scores are computed.

The two human reviewers should each work from a separate copy of the blank
worksheet before looking at the provisional suggestions or one another's labels.
Adjudicate afterward. There are 20 families with two variants each; all are
exposed development material, regardless of the old seed `split` fields. The
40 rows do not establish 40 independent observations.

## Label decisions needing attention

The reviewer proposed 34 labels and left these six rows null:

| Cases | Question for human adjudication | Proposed handling to review |
|---|---|---|
| run-10-1, run-10-2 | A definitive protected-branch refusal is known, but no existing class names policy refusal. | Keep the hold independently; either explicitly map policy refusal to insufficient evidence for causal classification or revise the taxonomy. Never infer candidate fault or merge success. |
| com-09-1, com-09-2 | Does complexity describe the submitted snapshot or the later scope? | Label the submitted snapshot and mark the result stale/inapplicable; evaluate the changed scope only as a new case. |
| com-10-1, com-10-2 | A typed epic has no executable scope to assess. | Mark inapplicable, preserving child assessment. Decide whether its advisory class is insufficient evidence or whether it should be excluded from the assessment corpus. |

The manual-medium pair also needs a distinction between inferred complexity
and the observed manual choice. Whatever the advisory label, the manual value
must remain intact. Human taxonomy changes require regenerating and reviewing
request hashes; this draft does not silently resolve them.

## Request and spending bounds

Proposed models are `gpt-6-luna` at `/v1/decisions` and `jev-1.13.0` at
`/v1/systemone`. A published immutable Decisions snapshot was not established;
the request name is exact, but it is not a claim of immutable weights. Record
returned model identity and stop an arm on mismatch; never substitute an alias.

At most 80 attempts: 40 per provider, 20 per use case/provider, one at a time,
one attempt per pair, no retry. Limits: 900 seconds overall; 3 seconds per runtime
case and 10 per complexity case, bounded by remaining campaign time; 8,000 billed
input tokens and 64 KiB response per request; $1 total.

Published input rates checked 2026-10-09 are $0.10/M for Decisions and $0.042/M
for Jev, with no output charge for these endpoints. At the token ceilings, the
base estimate is $0.04544. Account-specific terms and adjustments are unverified.
[Decisions pricing](https://developers.openai.com/api/docs/guides/decisions),
[Jev pricing](https://docs.typesafe.ai/models).

The mock runner reserves $0.0125 before every attempt, never refunds unknown
usage, and refuses reservations above $1. Before any live attempt, a future
executor must establish that the request's worst-case bill fits that reservation
using verified price terms and a billed-token bound. The script's 8,000-byte
preparation limit is **not** a tokenizer or proof of the 8,000-token ceiling.
Unknown price/token bounds block live execution. Reservations and request IDs
must be persisted before sending; crash/restart must not reset them or reissue
uncertain requests. A real executor must also enforce bounded streaming and
absolute connection/read/cancellation deadlines. The mock clock proves only
policy arithmetic, not real socket behavior.

The future approval should name accounts, region/retention, these exact payload
hashes and models, applicable price bounds, adjudication receipt, and the
80-request/$1/15-minute limits. It grants no production activation or provider
selection. Access/authentication failures stop the affected arm without retry.

## Reproduce the offline checks

From this directory:

```sh
python3 -m unittest -v test_shadow
python3 prepare_packet.py
```

Only this packet's new focused Python tests run. `prepare_packet.py` rebuilds
the request previews, blank human worksheet, provisional CSV, proposal and mock
results; do not run it over a worksheet that humans have edited. Keep completed
review copies outside the generated filenames.

The original 27 mock-plan scenarios are covered by the focused tests. Further
checks cover provider model drift, byte/response bounds, duplicate JSON keys,
hash drift, per-provider quotas, and separate result groups. The local development log retained the initial missing implementation and a
corrected missing-model response classification. The repository validation record
reports only the published focused checks; no broad gate receipt is claimed.

## Approval and remaining work

Publication and offline script/test preparation are explicitly authorized.
Production integration and external execution remain outside scope.

Remaining work is human taxonomy/label adjudication and a specific future paid
pilot authorization, followed by a separately tested bounded live executor.
The larger independent held-out study remains necessary for production-quality
selection claims. This packet does not finish SH-895 or unblock SH-896/SH-897.
