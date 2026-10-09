# Human review and pilot authorization

This packet is **offline preparation**, not a provider evaluation. No API requests
have been made. A separate AI reviewer proposed 34 labels and left six unresolved;
none is human-adjudicated ground truth. Runtime classification and initial
complexity are separate decisions and may select different providers.

## Label the cases

1. Give each of two human reviewers a separate copy of [human-labels.csv](human-labels.csv).
   Read the 40 complete synthetic texts. Use the runtime class definitions in
   [shadow.py](shadow.py) and the canonical [complexity rubric](../../spec/story-complexity.md).
   Record a label, rationale, applicability, target snapshot and guard disposition.
2. Work independently before opening the [AI proposals](provisional-labels.csv)
   or the other reviewer's answers. Then compare both reviews and adjudicate.
3. Resolve the six cases below before freezing labels and request hashes.
   Label ambiguity never relaxes a manual hold, ownership check or stale-result guard.

| Unresolved cases | Decision needed |
|---|---|
| `run-10-1`, `run-10-2` | How does a definitive protected-branch refusal map into the causal-class taxonomy? It does not establish candidate fault, success or a content conflict. |
| `com-09-1`, `com-09-2` | Does the assessment target the submitted snapshot or revised scope? Keep freshness/applicability separate from complexity. |
| `com-10-1`, `com-10-2` | How should non-executable epics be represented or excluded? Assess executable children separately. |

Also preserve the explicit manual-medium rating in `com-08-*` regardless of the
advisory label. [Full review provenance and rationale](provisional-review.json)
state exactly what the AI reviewer read. The author hypotheses were not read.
All 20 families are exposed development material; paired variants are not
independent observations. No production-quality score can be inferred from them.

## Proposed future authorization — not yet granted

| Item | Proposed scope |
|---|---|
| OpenAI | Decisions endpoint `https://api.openai.com/v1/decisions`, model `gpt-6-luna`. No immutable snapshot ID was established; log returned identity and stop on mismatch. |
| TypeSafe AI | System One endpoint `https://api.typesafe.ai/v1/systemone`, pinned model `jev-1.13.0`. No alias substitution. |
| Data | Only the synthetic subject ID, task kind and case text, plus rubric/class instructions in the [80 exact request previews](requests.jsonl). Send only each entry's `body`. No labels, source code, real stories, logs, machine paths or credentials in the body. |
| Attempt limit | At most **80 requests total**, 40 per provider and 20 per provider/use case; one at a time, one attempt per pair, **zero retries**. |
| Cost/time | At most **$1 and 15 minutes**; per-request deadline 3 seconds for runtime or 10 for complexity, shortened by remaining campaign time. At most 8,000 billed input tokens and 64 KiB response per request. |
| Published base estimate | At the token ceilings, $0.04544 before unverified account/region adjustments. The proposal records the dated price sources; the $1 cap is not authorization to spend. |

Before requesting execution approval, name the authorized API account/project for
each provider and confirm endpoint/model entitlement, billing access, applicable
price adjustments, region and retention terms. Existing ChatGPT/Codex access
does not establish API billing or TypeSafe access. Do not place secrets in the PR
or worksheet. Human adjudication and the final taxonomy/prompt/payload hashes
must also be recorded.

The [full proposal](pilot-proposal.json) defines conservative $0.0125 reservations
per attempt, including failures and unknown usage. A future live executor must
prove the worst-case request price fits that reservation before sending, enforce
real network deadlines and response limits, and durably retain reservations and
request identities across failure. The current mock-only harness cannot execute
the pilot. Approval would permit only that bounded synthetic evaluation, not
provider selection, production activation or merge.
