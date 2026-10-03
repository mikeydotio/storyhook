Approved by Mikey on 2026-10-03. The approved defaults are: the 900-second limit starts at verifier admission and includes resource waits; project faults pause affected projects, and host faults pause affected machine work. Further coverage reductions require their later concrete measured proposal. The original plan below is retained verbatim as the approval record.

Organizational epic. Execute the child stories. Approved-design candidate follows; preserve its decision status until review.

# Verification throughput and recovery

Status: proposed for review. StoryHook v3.0.3, 2026-10-03.
Proposed repository path: docs/spec/verification-throughput-and-recovery.md.
Research snapshot: origin/dev c2a315a7cd54d89838bc067573ad82586c37c6c8.
No live configuration, story state, gate, or test policy changed during planning.

## Requested outcomes

1. Each story's merge gate takes less than 15 minutes. A longer gate is a process defect.
2. Tests must not exhaust machine resources and cause their own failures.
3. Return a story only for a defect attributable to its submitted change. For other faults, retain the submission, pause the affected verification, repair the cause, and resume automatically.

## Working assumptions for review

The time limit starts at verifier admission, before workspace preparation, build, or resource waits. Queue time is a separate visible measure. Report submission-to-verdict as well. This is a proposed interpretation; Mikey was asked whether queue time belongs inside the limit.

Pause affected projects for project faults. Pause all affected machine work for host resource or tooling faults. An unrelated project may continue only when it does not need the failed resource. Mikey was asked to confirm this scope.

A pause does not erase elapsed time. A held submission is not a successful verdict or a completed gate. Every attempt at or above 900 seconds is a breach; retain cumulative time across retries, holds, and automatic re-admissions. Show gate service time, diagnosis time, repair time, queue time, and total elapsed time separately. The target applies to the maximum, not only the median or an amortized batch cost.

## Evidence and limits

| Observation | Evidence | Implication |
|---|---|---|
| Recent completed gates had a 49m55s median | Earlier session snapshot: 17 certified or test-failed attempts in the 24 hours ending 2026-10-03 15:41:16 UTC; reused legs included | Use this as operating history, not a fresh-run benchmark |
| A recent successful gate took 46m46s | SH-823, attempt 5a6584f7-2c3e-4d3c-bfad-1fbd159c1659, ended 15:50:40 UTC | The long duration also affects successful work |
| That gate spent 17m36s in core Rust, 16m34s in contracts, and 10m27s in plugin tests | Its retained progress journal; fmt 3s, clippy 59s, build 36s | Both Rust legs independently exceed 15 minutes; moving browser tests cannot solve this |
| Browser tests already run at the release tier | Makefile: test versus test-full; docs/spec/test-tiers.md | Any further split needs a new coverage decision |
| Compiler admission and test admission are separate | scripts/rustc-slot.py, scripts/test-pool.py, plugin runner, browser runner | Eight compiler slots plus independent test pools do not form a machine resource budget |
| Higher local concurrency already produced failures | docs/spec/test-audit.md: core at 8 threads 694s/green; 16 threads 351s/7 failures | More workers alone is not a valid optimization |
| Ordinary test failures are returned directly | src/daemon/verification.rs:2871; scripts/verify-pr.sh:504 | Existing recovery does not first establish author responsibility |
| Project recovery already exists | docs/spec/project-fault-recovery.md | Extend its durable records and managed repair machinery |
| Prior 30-return analysis counted 26 housekeeping/integration returns | /tmp/storyhook-return-causes-xfxr5sa3.md | This is a failure-type breakdown, not a blame breakdown. Some fixtures were authored incorrectly. Two historical causes remained provisional |

The operating history contains different trees, toolchains, loads, and cache states. It does not prove the speedup from any proposed option. Do not rerun the full suite in an implementation lane to fill this gap. Use the central verifier and the authorized measurement path already requested by SH-801.

## Options to compare

| Option | Likely benefit | Cost or constraint | Proposed treatment |
|---|---|---|---|
| Remove repeated discovery, setup, process launch, and compilation work | Less work with the same assertions | Must measure current hot paths; several earlier optimizations already landed | First choice after profiling |
| Use one resource budget and schedule the critical path | Avoid overload; allow safe overlap of independent work | Lower admission alone can increase duration; local worker counts are not comparable units | Required foundation; enable overlap only on evidence |
| Reuse valid results at a finer granularity | Avoid re-running unaffected checks | Requires complete inputs, toolchain/environment identity, and invalidation proofs | Extend current receipts only where equivalence is proved |
| Use mandatory core checks plus proven impacted tests at merge; run exhaustive matrices at release | Potentially the largest reduction | Changes what a merge certifies; missing impact data must fail closed | Conditional option with a concrete test inventory before approval |
| Improve test build layout or adopt a different runner | Could reduce linking or improve scheduling | Can destroy shared per-binary fixtures and multiply daemon starts | Benchmark only after profiling shows a bottleneck |
| Batch stories | More completions per successful gate | Does not make an individual gate shorter; poor green rate costs bisection time | Keep SH-841's existing evidence trigger |
| Move heavy execution to a dedicated worker | Isolate interactive use and provide capacity | Adds a host, environment parity, and operating cost | Fallback if safe local execution cannot meet the limit; needs an actual resource decision |

Do not treat a new runner as a substitute for cross-process admission. Nextest supports weighted tests and group limits, but replacing this runner needs fixture-cost measurements: https://nexte.st/docs/configuration/test-groups/ and https://nexte.st/docs/configuration/threads-required/.
Cargo exposes profile and incremental-build controls; measure their effects before changing this repository's existing packed debug-info choice: https://doc.rust-lang.org/cargo/reference/profiles.html.

## Gate budget and evidence

Persist monotonic durations within an attempt, with durable timestamps and restart accounting. Pin story submission, head, base, proposed merge tree, gate contract version, platform/toolchain, resource grant, and cache state. Record preparation, lock/admission wait, discovery, build/link, execution, cleanup, and certification separately. Link exact failure names and logs to this record.

At the 900-second deadline, record a process-budget fault. Stop new work, settle owned children safely, and hold the submission. Cleanup may need additional time; report that excess and retain ownership until cleanup is proved. Never write a receipt for an incomplete gate. A timeout is not an author defect. Diagnosis can later establish an attributable hang with a controlled reproduction.

Record budget faults from the first instrumentation rollout. Turn on the termination policy after the replacement path is validated; enabling it immediately on today's 50-minute gate would halt every story without fixing throughput. Until then, every excess is visible and remains a process defect. This rollout stage is not acceptance of the final target.

Avoid new whole-gate retries for diagnostic questions that an exact failed case can answer. Retain valid completed-leg evidence without accepting a stale final-tree receipt.

## Machine resource admission

Use one host-scoped admission authority shared by verifier work, release work, implementation test runners, and compiler wrappers. Do not derive its identity from an isolated test HOME or data directory. Cover concurrent checkouts and projects that participate in StoryHook's runner protocol. Unmanaged applications remain external load; measure them and reduce managed admission rather than trying to control arbitrary user processes.

Grant weighted CPU and memory capacity with process/file-descriptor and I/O-heavy workload limits where measurement supports them. Account for descendant work, nested Python workers, browser workers, linkers, and test daemons. A nested runner uses its parent's grant or obtains an explicit subgrant; it must not silently multiply capacity. Avoid parent-holds-all/child-waits deadlocks. Do not use thread counts as a proxy for equal workload cost.

Reserve interactive headroom and capacity for diagnosis/repair. Use measured available memory, memory pressure, and runnable/CPU pressure; load average alone is insufficient. Define thresholds, hysteresis, minimum progress, starvation limits, and sensor-failure behavior from measurements. Keep utility QoS as scheduling policy; it is not admission control. Release work must receive a bounded share so deferred checks do not wait forever.

Lease ownership must survive supervisor restart without granting capacity twice. Verify process identity and release only after descendants settle. Unknown ownership blocks new conflicting work. A project cannot change its local config to exceed the host cap. Reduce admission or cancel a managed attempt safely when external pressure exceeds the supported envelope; do not suspend arbitrary children in ways that keep timers or locks running.

## Failure attribution

Separate the kind of failure from who caused it. A failed assertion, failed launch, or nonzero exit is an observation. It is not a cause classification.

| Supported cause | Story disposition | Recovery owner |
|---|---|---|
| Submitted change causes the defect under the supported environment | Return with the exact reproduction and affected requirement | Implementer |
| Submitted test, fixture, format, or audit entry is incorrect | Return when causal evidence identifies the submitted change | Implementer |
| Same defect already exists in the pinned base | Keep Verifying; hold on a shared repair | Project recovery |
| Unsupported resource pressure, missing host tool, tooling crash, or external service failure | Keep Verifying; hold the affected scope | Host or external recovery |
| Conflict caused by base movement | Keep Verifying; attempt controlled integration repair | Integration recovery; request a semantic decision if needed |
| Cause is uncertain | Keep Verifying; hold for diagnosis | Recovery assessor |

Use exact candidate and pinned-base probes with equivalent toolchain, fixtures, resource envelope, and failure signature. Where the test exists only on the candidate, transplant the detector or use a controlled revert/ablation that preserves the test's meaning. A base pass and candidate fail are evidence, but one run is not proof against a flake. Require a reproducible causal contrast or another retained deterministic causal proof. High load, unchanged filenames, a familiar error string, or a later green retry alone are insufficient.

Mixed failures retain separate records. Return only for the proven candidate-caused component; do not ask the author to repair the shared component or charge it as author rework. An assessor must provide validated evidence references and a structured decision. An LLM diagnosis alone cannot mark a tree green or clear a hold.

Apply this contract to every route that currently returns a story, including failed repair stories, compile/lint failures, submission refusals, conflicts, cleanup problems, and batch culprit handling. Unknown batch responsibility stays held.

## Hold, heal, resume

Retain Verifying with a visible reason and immutable submission identity. Use durable recovery records with one owner per fault; do not launch one repair agent per waiting story. Release the verifier workspace and machine resources only after cleanup. Preserve operator pause, other blockers, labels, changed heads, uncertain landing intents, and resource quarantine.

For a known safe host fault, perform the supported repair and verify the prerequisite. For a source or harness defect, create or reuse a scoped repair story and use the normal managed work and verifier flow. Reserve repair admission so the failed project gate does not block its own repair. A repair must still prove the gate it changes; there is no bypass certificate. Do not delete arbitrary files, disable security checks, invent credentials, or retry until green. External prerequisites stay held with a specific action when the system cannot repair them.

Deduplicate only faults with matching typed evidence and scope. A repeated symptom at a different base, toolchain, or causal source can require a new incident. Bound diagnosis attempts, repair attempts, and retry time. Exhaustion stays visible as a hold; it never becomes an author return just to free the queue.

After verified repair lands, rebuild the proposed merge against the current base and admit a fresh attempt linked to the retained submission. Automatically revalidate any evidence invalidated by the repair or changed base. No implementer resubmission is needed for unrelated repair. If integration requires semantic edits, use the managed integration owner; preserve the original work and ask for a decision when meaning is ambiguous. Never replay an old receipt against a new merge tree.

A resource-recovery boundary is not a fresh 15-minute success metric. Retain total wall time and cumulative service cost. Status must show owner, cause, blocked submissions, attempt count, next action, and elapsed time. One corrupt recovery row must not hide other work; use SH-851 rather than duplicate it. External recovery retirement must use SH-849's eventual contract.

## Merge and release coverage decision

Keep today's coverage while reducing avoidable work. If it still cannot meet 900 seconds, prepare a test-by-test tier proposal, with measured cost and the defect each check detects. Candidate categories:

- Always merge: buildability, lint/format, core behavior, persistence/migration safety, verification/receipt integrity, and critical resource/recovery invariants.
- Also merge: new or changed tests and all demonstrably impacted integration, plugin, and checkout-contract tests.
- Candidate release-only: exhaustive platform/provider matrices, long stress/soak repetitions, and isolation/order permutations when their core detector remains at merge.

These are candidates, not an approved exclusion list. Reusing make test-changed unchanged is not sound authorization: current policy explicitly refuses its receipt for merge. Unknown dependency input must retain full coverage or hold verification; it must not silently select fewer tests to meet the clock. If that full run cannot meet the budget, it is an unresolved capacity/process defect.

A revised contract needs a distinct policy version and an honest receipt describing what ran or was validly reused. The release gate must run the complete required battery for the exact release tree. Deferred checks need a scheduled owner, visible lag, and resource capacity between releases. A release-tier failure enters shared recovery. Whether a named release check must also block further affected merges belongs in the concrete tier proposal.

## Validation and rollout

Use deterministic fake-pressure and fake-clock tests for admission, nested work, deadlines, restart, and ownership. Use controlled failure fixtures for attribution and replay the 30-return sample as evidence cases; do not turn provisional historical judgments into ground truth. Verify real production subprocess boundaries with isolated stores and workspaces.

Use the central verifier for full measurements. Include cold builds, warm builds without verdict reuse, and valid reuse; small and broad edits; one and multiple active projects; typical external load; cleanup and recovery. Report each class separately. Gate acceptance requires every measured completed ordinary gate to be below 900 seconds, not merely its median. Any breach keeps the process issue open.

Before declaring completion, gather at least 30 consecutive production attempts with the new policy. Report maximum and p50/p95, all pauses and breaches, cumulative service/repair time, resource peaks, interactive latency, and attributable versus unrelated returns. Require zero unsupported author returns. Thirty observations validate a rollout sample, not a mathematical guarantee; the permanent deadline and resource guards enforce the continuing contract.

Set numeric interactive latency and pressure thresholds from SH-801's measured baseline before enabling the resource policy. They are unresolved measurements, not arbitrary constants hidden in implementation. If no safe local configuration satisfies the timing target, present the exact remaining workload and cost of a dedicated worker or coverage change.

## Existing work to preserve

| Story | Use in this program |
|---|---|
| SH-801 | Reuse its gate/QoS/interactive benchmark. It needs a verifier-owned measurement path, not another prohibited full run in an agent lane |
| SH-797 | Coordinate plugin startup investigation with gate optimization |
| SH-829 | Keep the stable test-binary lease work; integrate it with runner coverage |
| SH-835 | Disk reclamation remains its own existing hook story |
| SH-849 / SH-851 | External recovery resolution and resilient status are prerequisites for complete recovery rollout |
| SH-839 / SH-840 / SH-846 / SH-855 / SH-862 | Reproduce and fix these known fixture/load/cleanup cases; use them to validate attribution |
| SH-812 / SH-814 | Existing browser scheduling and isolation work supports the release tier |
| SH-841 / SH-845 | Keep batching and smoothing behind their existing measured triggers |

Do not change active assignments, close stories as obsolete, or enable dormant batching merely because this plan exists. Run obviation review before implementing each approved story.

## Proposed stories

One typed epic contains eight executable stories. The epic has no implementation steps. The detailed descriptions and exact dependency graph are in stories.json. A through H are planning keys, not assigned StoryHook IDs.

| Key | Work | Priority / complexity | Blocked by |
|---|---|---|---|
| A | Record gate cost and budget breaches | medium / medium | — |
| B | Add host resource admission and safe leases | high / high | A |
| C | Route builds and all managed test runners through admission | high / high | B |
| D | Require causal evidence before an implementer return | high / high | A |
| E | Heal shared faults and automatically resume held submissions | high / high | D, SH-849, SH-851 |
| F | Reduce measured gate overhead within the resource budget | medium / high | C |
| G | Decide the measured merge/release coverage contract | medium / high | F |
| H | Enforce and validate the 15-minute gate in production | medium / high | E, G |

B, C, D, and E address a population of faulty verification decisions and load flakes: high under the priority rubric. A, F, G, and H address visible delay and process failures: medium. Derived blocker priority will propagate through real dependencies. None needs critical priority for planning.

G requires a concrete measured inventory and review before changing the coverage boundary; it carries no-auto. H carries no-auto because its production observation and measurement work belongs to the central verifier, not a Full Auto implementation lane. Other stories become executable when their real dependencies clear. A commits this approved specification as its first documentation change.

Creation note: story decompose provides the title/description/priority preview. Its current YAML schema does not express explicit dependencies or typed epics. The prepared JSON import expresses both and the service writes the entire batch and its relationships in one transaction. Use that atomic import after approval, preserving the requirement that blocked work never appears ready between writes. No stories have been created yet.


## SH-867 observation-stage implementation

The approval at the start of this document supersedes the original proposed
status retained above. Queue time is separate. Admission-to-verdict includes
preparation, resource waits and cleanup. At 900 seconds, record a sticky process
budget breach. Observation does not cancel or hold work. Completed exact-tree
certification and budget compliance are separate results. No incomplete gate
receives certification. SH-874 owns enforcement.

SH-801 owns the verifier measurement operation adopted on 2026-10-03. SH-867
owns the durable evidence model and observer. Neither duplicates the other
session. Historical aggregate timings above are retained observations, not
newly measured samples or a controlled benchmark.

Evidence is versioned and bound to project, story, submission generation and
attempt. SQLite records survive replacement of the live journal. Missing
measurements are unknown, not zero. Monotonic durations describe uninterrupted
execution; UTC restart gaps are estimates, with clock anomalies explicit.
Retries, holds and new attempts retain prior costs and breaches. Raw producer
evidence cannot write the store, establish ownership or certify a tree.

### Read retained evidence

Use `story verifier evidence SH-<n>` for a compact table, or add `--json` for
the versioned record. The command works without a live verifier. It includes
admissions owned by another story when a shared physical execution contains
the requested submission. The admission table names its owning generation;
the submission summaries name the requested story's generations.

`story verifier status --json` has an optional `cost` field for the current
admission. Old payloads omit it. Current-generation progress comments show
cumulative cost, including breaches on earlier retries. A missing record
does not become zero elapsed time. Cost publication does not change
`last_evidence_at`, output freshness, or the silence watchdog baseline.

### Durable identity and clocks

Schema migration 54 stores version-1 admission JSON in `gate_attempts`,
scoped to a project. A submission is a story plus its verifying generation.
Legacy missing generations stay unknown. Each admission has a unique ID,
revision, predecessor, admission time, elapsed checkpoint, budget outcome,
independent gate result, lifecycle intervals and nested physical executions.
Each real gate or bisection probe has a separate execution ID. Reusing a
retained probe result does not create another physical execution.

Admission is written in the admission transaction, before preparation. A
scoped daemon observer checkpoints at one-second intervals, even without
status readers or progress output. It checks again at finalization. The
first persisted elapsed value at or above 900000 ms makes the breach sticky
and publishes a project-change notice. It does not cancel a child, create a
hold, or classify the submitter's work as defective.

Live elapsed time uses `Instant`. A restart closes an unfinished admission
as interrupted, retaining a checked UTC gap as an estimate. Malformed,
reversed or overflowing clock evidence retains the prior lower bound and a
diagnostic. A lost physical end remains unknown. Restart cannot infer
quiescence, test completion, or certification. Completed records are
immutable; updates to active records use revision compare-and-swap.

Cumulative wall time runs from submission through the last retained
observation. It includes retry and hold gaps within those boundaries, but
does not claim that a later unobserved hold has ended. Admission cost is the
sum of distinct admissions. Physical service cost is the sum of distinct
completed physical executions. An incomplete execution makes the full sum
unknown; a separate known lower bound remains available. Shared batch costs
are retained at full value for each member reference, never divided. Do not
sum member summaries as project service cost. Do not sum overlapping phase
intervals as wall time.

### Producer protocol and coverage

The daemon initializes each physical journal with a `run` record containing
`attempt_id`, `execution_id`, and `generation`. The importer requires that
binding and consumes complete newline-terminated records only. A partial
tail stays unread until complete. Foreign bindings, malformed records,
changed input identity, duplicate starts, unmatched ends and clock errors
retain diagnostics. None grants gate authority.

| Record | Evidence |
|---|---|
| `context` | Pinned head, base and merge tree; exact command argv; platform and available tool versions; inherited limits and scheduling argv |
| `cost` | `start` or `end`, phase, unique interval ID, path, UTC time and monotonic nanoseconds |
| `case` | Outcome and exact name; Rust target or Playwright ID and original title array when available |
| `item` | Leg outcome, measured duration and reuse receipt fingerprint when present |
| `output` | Attempt-bound raw log reference |

The scheduled launcher reads process limits after the scheduling wrappers
run. Tool probes have bounded waits; a failed probe retains an unknown value
and diagnostic. Build-cache warmth is `unmeasured`; leg receipts establish
validation reuse without claiming a warm build. This stage does not invent
a resource grant or change existing scheduling limits.

Workspace preparation and restoration are measured in the speculative
checkout owner. Gate-lock and compiler-slot waits have separate intervals.
The Cargo diagnostics adapter measures build-only commands; binary listing
measures discovery. Rust pool, plugin runner and Playwright reporter measure
runner execution, including their fixture and runner overhead. The Rust
pool already rejects an unexpected rebuild. Arbitrary external commands
and serial/doctest paths can mix compile and execution; their whole physical
duration is retained, and missing finer phases stay unknown. A producer can
use `STORYHOOK_GATE_PROGRESS_WRITER cost start|end <phase> <id> <leg>` to
supply finer boundaries. Endpoints describe real work, not periodic liveness.

Preparation and completed execution journals are copied to unique synced
archive files before the live path is replaced. Raw output and receipt
references remain attached to the execution. A durable-write or archive
failure is an infrastructure error through existing supervision, distinct
from a budget breach. It cannot silently become a successful disposition.
The bundle includes the cost helper, and changes invalidate affected leg
receipts through the existing contract fingerprint.
