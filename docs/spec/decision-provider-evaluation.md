# Decision-provider evaluation proposal (SH-895)

Research date: 2026-10-08. Source baseline: StoryHook v3.0.3, `2995774b`.
This is a documentation study and proposed experiment, not a measured evaluation
or an implementation. No provider requests, account setup, credential inspection,
configuration changes or activation were performed. Claude-specific Mods remain
outside scope.

| Use case | Provider decision | Current disposition |
|---|---|---|
| Runtime issue classification (SH-896) | **Pending** | Preserve current behavior; no new provider enabled |
| Initial story complexity (SH-897) | **Pending** | Preserve current behavior; no new provider enabled |

Pending is not a selection of “no provider.” Record a separate explicit provider
or retain-current decision for each use case before unblocking these stories.

## Documented products and limits

The following are public documentation facts, not proof of this account's access.

| Candidate | Documented contract | Important limits |
|---|---|---|
| OpenAI Decisions | Public beta; `POST https://api.openai.com/v1/decisions`, currently `gpt-6-luna`. Predicate probability, discrete choice and ordered score; distributions accompany choice/score. | Text/images; dependent questions need separate requests. Per-question refusal is possible. Arbitrary JSON/explanations use Responses instead. |
| TypeSafe System One / Jev | `POST https://api.typesafe.ai/v1/systemone`; current version `jev-1.13.0`. Choice, Score and Noul over shared state. | Text only; 64k total request tokens and 32k state plus longest question. Mutable aliases can change answers; pin the version. |
| OpenAI Responses with Structured Outputs | `gpt-6-luna` supports Responses and structured JSON output. A strict enum/schema can represent a bounded classification and evidence references. | Schema validity does not establish semantic correctness. Handle refusal/incomplete output; explanations and generated confidence can be wrong. |
| Retain current behavior | Existing native diagnosis, managed recovery assessment, manual complexity and medium-unassessed fallback. | No new external inference or data export; no measured automatic initial-assessment benefit. |

Sources: [Decisions guide](https://developers.openai.com/api/docs/guides/decisions),
[Decisions reference](https://developers.openai.com/api/reference/cli/resources/decisions/methods/create),
[System One API](https://docs.typesafe.ai/api),
[Jev models](https://docs.typesafe.ai/models),
[Luna capabilities](https://developers.openai.com/api/docs/models/gpt-6-luna),
[Structured Outputs](https://developers.openai.com/api/docs/guides/structured-outputs).

### Correctness, confidence and abstention

No StoryHook accuracy or calibration measurements exist from this study.
TypeSafe's confidence is a statistic of its probability distribution, not a
second independent correctness estimate. Choice confidence normalizes the largest
probability above a uniform distribution; Score uses a different statistic, and
Noul has no separate confidence field. Do not transfer one numeric confidence
threshold between providers or question types.
[TypeSafe confidence](https://docs.typesafe.ai/confidence).

Jev's documented weaknesses include numeric/date reasoning, indirection,
irrelevant context, adversarial input and choice ordering. Keep exact identifier,
time, ownership and permission comparisons in code.
[Jev 1.13 limitations](https://docs.typesafe.ai/model-jaggedness/jev-1.13).

**Vendor claims, unmeasured here:** OpenAI describes Decisions as approximately
10 times faster than Responses. TypeSafe's launch claims roughly 70–500 ms and
40–200 times speedups; its benchmark reference answers use other models and its
methodology has disclosed limitations. Neither is a StoryHook latency SLA or
human-labeled correctness result. Type-constrained output can still select the
wrong permitted answer.
[OpenAI guide](https://developers.openai.com/api/docs/guides/decisions),
[TypeSafe launch](https://typesafe.ai/blog/introducing-system-one-models-and-jev).

### Cost, privacy and operational maturity

| Candidate | Published pricing | Data/access considerations |
|---|---|---|
| Decisions | $0.10/M input tokens; no output/cache charges. Regional and long-context adjustments apply. | Default abuse-monitoring retention up to 30 days; eligible ZDR with limitations, including prompt-cache and image exceptions. Public beta; account entitlement untested. |
| Jev | $0.042/M input tokens; output free. | Documentation says customer requests/responses are not training data. Enterprise ZDR requires an arrangement; standard payload/log deletion interval and deployment region remain unresolved. Published rate limits can change without notice. |
| Responses Luna | Standard short context: $0.10/M input and $0.50/M output, with separate cache pricing. | Default application-state retention is 30 days. `store=false` minimizes state but does not itself establish ZDR or eliminate all monitoring/cache retention. |

Sources: [OpenAI pricing](https://developers.openai.com/api/docs/pricing),
[OpenAI data controls](https://developers.openai.com/api/docs/guides/your-data),
[Jev model pricing](https://docs.typesafe.ai/models),
[TypeSafe legal index](https://docs.typesafe.ai/legal),
[TypeSafe DPA](https://typesafe.ai/legal/data-processing).
The DPA describes retention by processing necessity and law, not a fixed API
payload TTL. Public documentation is not an executed agreement or account check.

Illustration only: 2,000 billed input tokens cost $0.0002 for Decisions or
$0.000084 for Jev. Responses with another 100 billed output tokens costs $0.00025
before cache, regional, retry or other adjustments. Tokenizers and question
overhead differ; measure actual usage and billed cost.

The OpenAI Python SDK defaults to two retries and a ten-minute timeout; TypeSafe
exposes configurable retry counts, backoff and a total retry budget. Supply an
application-owned deadline and cancellation rather than inheriting long defaults.
Initially disable SDK retries. No supported local/offline deployment for the
named products was established, and a Codex/ChatGPT login does not prove API
billing access. Rust integration remains a design choice; a public SDK example
does not establish an existing StoryHook adapter.
[OpenAI SDK](https://github.com/openai/openai-python),
[TypeSafe retry policy](https://docs.typesafe.ai/sdk/python/api/retries).

## Proposed synthetic evaluation

All counts and thresholds below are **proposals for review**, not approved
budgets, achieved results or product guarantees. External evaluation requires
separate authorization naming provider/account, synthetic payload and total spend.

Prepare 1,200 synthetic incidents and 900 synthetic new stories. Split by scenario
and template family: 30% development, 20% calibration, 50% held out. Keep paraphrases
and repeats in the same split. Two reviewers label independently and adjudicate
before scoring; another model alone is not ground truth. Compare all authorized
arms against retained current behavior. Freeze prompts, schemas, model versions,
payload minimization and scoring before the held-out run.

Incident families must distinguish successful execution, candidate-caused failure
with causal evidence, shared-project fault, host/external fault, integration
conflict and insufficient evidence. Include unstarted/cancelled tests, missing
receipts, definitive merge refusal versus timeout, stale heads, duplicate delivery,
unsettled children, unprobeable ownership and manual pause. Story cases cover the
canonical low/medium/high rubric, insufficient context, explicit manual values,
scope changes, epics versus executable children, and urgency/line-count distractors.
At least 20% should be ambiguous, stale, contradictory or adversarial. Include
choice permutations, irrelevant padding and nearly identical IDs.

| Measure | Proposed acceptance threshold |
|---|---|
| Runtime classification | Held-out macro F1 ≥0.90; accepted-answer precision ≥0.98; coverage ≥0.70; insufficient/OOD abstention recall ≥0.95. Also require ≥300 accepted independent held-out cases and a 95% Wilson upper bound on accepted-answer error ≤0.02. |
| Complexity | Macro F1 ≥0.85; weighted kappa ≥0.80; high-complexity recall ≥0.95; zero high-to-low outcomes in the deliberately hazardous subset. |
| Calibration | Ten-bin equal-mass ECE ≤0.05; Brier score no worse than the empirical-class-frequency baseline. Tune abstention on calibration cases only. |
| Latency | Runtime p95 ≤1 s, absolute ceiling 3 s; asynchronous initial complexity p95 ≤3 s, ceiling 10 s. Include queue/network/validation. Timeouts count as errors/abstentions. |
| Cost | First authorized synthetic run ≤$5 total and mean ≤$0.001/case including retries; stop before either cap. These are not spending authorizations. |
| Authority | 100% preservation of off/manual/stale guards; zero model-issued receipts, merges, resets, cleanup, host grants, hold releases, permission changes or duplicate writes. |

Report confusion matrices, class/subgroup counts and uncertainty intervals,
coverage-risk curves, repeat/ordering instability, p50/p95/p99 latency,
error/refusal frequency and actual tokens/cost. Insufficient samples cannot pass.
Record requested/returned model, schema/prompt versions, synthetic case and payload
hash, bounded response/error, attempt/deadline/cancellation, and shadow disposition.
No synthetic result changes a story or authorizes an effect. Any later real-data
shadow study needs separate payload/privacy authorization; no automatic activation
follows a passing study.

Fake-transport contract checks should cover off/unconfigured, 401/403, 422,
429/529/5xx, disconnect, timeout, refusal, malformed/truncated output, wrong or
duplicate question IDs, invalid probabilities, cancellation and late responses.
Initially make one attempt. Any later approved transient retry must fit the
original deadline and spend cap; no cross-provider retry cascade. This document
adds no executable tests and claims none ran.

## Proposed StoryHook integration constraints

Use two independent persistent installation settings exposed through authenticated
global web settings: runtime classification and initial complexity assessment.
Both default off, with separate provider/model and configuration revision. Store
them through a shared service in SQLite, following
[dispatch-policy persistence](story-complexity.md), not browser-token preferences.
Credentials remain server-side references. Proposed setting names are not an
existing API.

Off means no new queueing or outbound calls and no later catch-up burst. Disabling
cancels owned advisory work and invalidates result application. Show disabled,
unconfigured, shadow, unavailable, abstained and stale outcomes distinctly, plus
last model/time, minimized provenance, cost and latency. Neither toggle changes
project automations, verifier state, host policy or provider sessions.
[Manual mode](project-manual-mode.md) remains authoritative.

Runtime output is an allowlisted recommendation with local evidence references.
It cannot construct native capabilities, invent observations, certify a tree or
grant destructive permission. The existing recovery owner remains the sole effect
coordinator:

| Advisory classification | Guarded consumer |
|---|---|
| Possible shared fault | Native causal comparison, then existing managed scope assessment; no source ownership inferred solely from text. |
| Possible host/external fault | Native admission evidence or existing prerequisite path; no model-created capacity or release attestation. |
| Possible integration conflict | Managed integration after fresh PR/head/base/policy proof; ambiguous semantics remain held. |
| Recurring preventable defect | Deduplicated draft proposal tied to verified evidence; ordinary authorized story creation, not automatic filing from JSON. |
| Uncertain merge, cleanup, owner or access | Preserve the existing hold and show the diagnostic; no replay of uncertain effects or permission bypass. |

These constraints follow [project recovery](project-fault-recovery.md),
[verification recovery](verification-throughput-and-recovery.md),
`src/service/attribution/contrast.rs`, native attribution proof, and the versioned
`project_recovery::DecisionInput` authority checks. SH-871's active native
integration/readmission design was inspected at `4318a527`; it is a downstream
constraint, not evidence that a decision provider has shipped.

Initial complexity must use one coordinator for CLI, web, TUI, plugin and
decomposition creation paths. Creation keeps medium-unassessed and never waits
on inference. Apply only while the same story/input revision is still unassessed,
the configuration generation matches and the task still owns its result. Explicit
manual complexity always wins, including explicit medium. Coalesce duplicate
events; discard stale results after scope changes. Preserve priority, dispatch
overrides and the [canonical complexity rubric](../../src/help/complexity-rubric.txt).

**Assessor identification is incomplete:** source inspection found the existing
managed recovery scope assessor and deterministic priority triage, but no dedicated
automatic initial-complexity worker in the inspected native/plugin paths. An
externally configured assessor mentioned by the story was not inspected. Identify
its owner and precedence before SH-897 implementation; do not duplicate, disable
or silently replace it. Provider failure leaves native classification/Unknown and
existing holds intact, or complexity at its manual value/medium-unassessed fallback.

## Decision still required

Documentation supports comparing both named APIs and the Structured Outputs
alternative; it does not establish a correctness winner. Jev offers explicit
version pinning and lower listed input price; Decisions documents refusal and
data-control behavior but is beta. Retain-current remains a valid outcome for
either use case if measured benefit or access/privacy requirements are unmet.

Before recording each selection, resolve account availability and relevant data
terms, identify the current initial assessor, and either authorize the bounded
synthetic experiment or explicitly choose retain-current with a rationale. No
provider selection, unblocking, implementation or activation follows from this
report alone.
