# Remaining backlog: evidence and acceptance decisions

**Draft for review, 10 October 2026. No decisions applied.** All 18 stories remain
open: seven Verifying, four In Progress, seven Todo. Source baseline is dev
`73257ca56328679f34ad554e2d9c204a9639e1ed` (v3.0.3). Local build 118/schema 59
passed its installation smoke checks; that is not a full release gate.

This packet preserves the requested features and the Verifying hold. It proposes
running the required final correctness/release gate **before** marking the seven
landed stories Done. It does not propose a gate waiver to resolve sequencing.
Formal release/publication remains after truthful backlog disposition. No gate is
started by this documentation PR. Any required clarification concerns the order
of validation and board completion, not whether all release tests must run.

After one explicitly authorized replacement, the investigation is again closed:
**nine charged attempts, zero accepted
original cohort samples, one successful observer diagnostic**. No further campaign
is proposed. Automations remain off and the verifier stopped. No production
activation, provider selection, paid call, story closure or merge follows from
this packet. Cancellation or scope reduction is never reported as implemented
functionality.

## Evidence: landed stories awaiting final correctness

Historical focused passes below apply to their recorded source heads, not to the
final combined release tree. The [evidence index](2026-10-10-backlog-evidence.json)
records exact PR heads/merges and hashes of retained validation records. Private
raw logs, process identities and local paths are not published. Counts are not
summed across stories because some tests overlap.

| Story | Original requirement | Verified result | Missing evidence | Proposed disposition |
|---|---|---|---|---|
| SH-757 | Review CLI redesign proposals; record approved compatibility, migration and acceptance requirements. | PR984 landed the approved compatible package A; package B remains deferred. Documentation-only, no added tests. | Final acceptance review of the delivered A surface and applicable integrated correctness gate. | Keep Verifying; review A and its implementation children, run required gate, then Done only if accepted. |
| SH-835 | Project-configured reclamation only after verifying handoff and settled agent build; automatic rebuild on return; guarded tests. | PR985 landed opt-in reclamation with ownership/settlement guards and return/rebuild fixtures; 16 native plus 42 Python added tests passed. | Final integrated correctness gate. Production opt-in remains inactive; fixtures do not prove live activation. | Keep Verifying; validate the delivered mechanism at the gate. No live cache removal or enablement implied. |
| SH-871 | Deduplicate shared-fault ownership, pause correct scope, admit repair safely, resume fresh submissions, survive retries/restarts and expose cumulative status. | PR983: 206 Rust cases and 55 nested Python cases across 205 focused commands passed. PR995 later landed observation-clock, held-retry and native-custody corrections with focused validation. | Required final integrated gate after all corrections; no production rollout cohort is claimed. | Keep Verifying through gate and acceptance review, then Done if passed. |
| SH-898 | One exhaustive typed command model; parser/help/dispatch parity and legacy compatibility. | PR987 landed at resolved integration head; 13 command-model plus eight JSON-input focused tests passed. | Final integrated correctness gate. | Keep Verifying through gate and command-surface review. |
| SH-899 | Explicit `set --input-json`, legacy input compatibility, output independence and atomic actionable refusal of invalid/duplicate input. | PR986 landed; eight added parser/real-CLI mutation/refusal tests passed. | Final integrated correctness gate. | Keep Verifying through gate and acceptance review. |
| SH-900 | Offline versioned discovery, correct audiences, handler capabilities and honest output contracts without daemon startup. | PR988 landed; 12 new discovery tests passed. | Final integrated gate and dependency completion. | Keep Verifying; gate and resolve accepted dependencies before Done. |
| SH-901 | Audit compatible package A against real consumers and preserve legacy output, aliases, dry-run/refusal and terminator behavior. | PR989 landed; nine added consumer fixtures passed; [consumer audit](SH-901-cli-compatible-audit.md) records scope. | Final integrated gate and dependency completion. | Keep Verifying; review audit and gate before Done. |

## Evidence: research, conditional rollout and provider work

| Story | Original requirement | Verified result | Missing evidence | Proposed disposition |
|---|---|---|---|---|
| SH-797 | Explain the historical 1.6–1.9× dispatch slowdown using real historical/current dispatch and shim comparisons. | Local shim run completed 240 timed operations; added medians were 31.207, 30.153 and 49.312 ms across three fixture cases. Static dispatch source analysis exists. | Historical/current production comparison and causal attribution. Local empty-repository timings are not real dispatch timings. | Document inconclusive production attribution. Owner may accept a narrower research outcome; otherwise remain open. |
| SH-801 | Matched normal/utility gate comparisons with interactive probes under representative load. | Collector/wrapper implementation and focused evidence exist in draft PRs991–993. Observer diagnostic completed 12 snapshots/36 helpers. | Accepted matched gate cohort and measured interactive baseline. Observer ran without compilation load. | Report methodology and missing results; accept inconclusive research only by explicit scope decision, otherwise open. |
| SH-872 | Measured cold/warm/reuse cost ranking and causal optimization within host/900-second budget while retaining detectors. | PR990 fingerprint repair and PR995 correctness fixes landed. Nine charged attempts; zero accepted original cohorts. Cold attempt lower bound 977.967 s. | Matched cohort, causal speedup, complete cost ranking and target compliance. | Record target unmet and terminal findings. Explicit bounded-research disposition or remain open; never certify optimization from these results. |
| SH-873 | Decide coverage from measured cost, detector inventory, placement, dependencies and detection delay; preserve honest full coverage when unknown. | Static inventory prepared; existing coverage remains unchanged. Browser checks remain release-tier. | Measured costs and evidence that any proposed policy/capacity meets 900 s. | Recommend retaining existing full coverage. Owner must decide whether incomplete measured analysis is an acceptable research disposition; no exclusions or receipt relabeling. |
| SH-874 | Enforce truthful 900-second admission-to-verdict holds/settlement, validate required cohorts and thresholds, observe 30 consecutive production attempts and complete release coverage. | Landed resource/recovery mechanisms have focused evidence. Neither that evidence nor the observer demonstrates this rollout contract. | Required cohorts, approved interactive thresholds, 30-attempt production series and validated performance/enforcement acceptance. | Remain open under original criteria. Optional explicit withdrawal/re-scope may produce a disabled-readiness assessment, but is not completed rollout or implementation. |
| SH-866 | Derived epic organizing SH-867–874. | Earlier children landed; SH-871 awaits gate and SH-872–874 retain gaps above. | Truthful child completion/disposition. | Let state derive from children. Do not manually close the epic to empty the board. |
| SH-841 | Enable batching only after qualifying exact-commit pair-green trigger; cap first enablement at two and supervise trial. | Mechanism from SH-831/832 exists dormant. Retained preview summaries do not establish the required trigger. | Qualifying live cohort: ≥50% pair-green over ≥30 eligible dequeues, or ≥62% per-story green after bisection; cap test and supervised activation. | Keep disabled and open. Optional no-go/dormant scope disposition records conditional work not performed; it is not activation. |
| SH-845 | Enable union smoothing only when it adds a member in ≥3 of 30 eligible dequeues at live cap; model resolver requires material payoff and its own review. | Dormant mechanism exists. Retained cap-4 observations do not establish the required live-cap cohort. | SH-841 live batching and qualified smoothing denominator/payoff; no model-resolver justification. | Keep disabled and open. Optional no-go disposition is not completed enablement. |
| SH-895 | Compare documented provider contracts, define labeled fixtures/metrics/settings/guarded integration, recommend and record independent provider choices. | PR981 comparison and PR994 offline packet landed; 24 offline tests passed. Forty human labels remain blank; 34 provisional AI proposals and six unresolved cases. | Human adjudication, provider quality/calibration evidence, independent runtime/complexity choices and accepted evaluation criteria. | Complete decision review to unblock requested implementation. No quality winner is established. Retain-current is an optional explicit scope choice, not the default recommendation. |
| SH-896 | Independent persistent runtime toggle and selected-provider integration; typed bounded advice, guarded recovery/preventative proposals, deduplication and shadow tests. | Provider-neutral design and offline policy fixtures exist; requested web setting and production integration are not implemented. | Provider decision, implementation authority, feature code, integration fixtures and spike-defined quality acceptance before rollout. | Follow runtime implementation route below after choice/authority. Keep open until actual acceptance. Optional cancellation is scope reduction, never delivery. |
| SH-897 | Independent persistent complexity toggle and selected-provider integration at creation; manual overrides, stale/duplicate protection, fallback and shadow tests. | Entry-point/current-assessor trace and offline policy fixtures exist; requested setting and integration are not implemented. | Independent provider choice, implementation authority, feature code/fixtures and quality acceptance before rollout. | Follow complexity implementation route below after choice/authority. Keep open until actual acceptance; cancellation is not implementation. |

### Terminal investigation details

The successful shim study measured source `2c6d57dc76bf7efdc57a765dc9a0bf205e348a7d`,
not current dev. Forty alternating pairs per case, 24 warmups and 11 other commands
settled; the empty local repository fixture included no provider/network calls.
Its descriptive medians cannot explain the historical production regression.

The final cold attempt at source `da65607cda7e540fc593483a0051b93ae820a8d7`
failed when a bounded process-census helper exhausted its allowance. Successful
helper timing spikes and host load were observed, but the failed helper lacks
stage timing; the underlying OS delay is not established. The 977.967-second
lower bound is a budget breach, not a completed gate duration or test failure.

Slot 7's observer completed in 56.248 seconds, with no compilation workload.
Slot 8 failed in 1.409 seconds **before input capture or `make test`** because the
task adapter passed `pathlib.Path` to a validator comparing a stored string path.
Implementation and review missed that real boundary. The successful observer is
not a replacement accepted sample. That eight-attempt outcome remains retained; the later replacement is recorded below.
All original failure records are retained; the original failed
helper's nonterminal durable record remains preserved despite observed empty
native session. The task-only adapter is not delivered source; its frozen copy
and unapplied correction remain evidence. It is not being shipped or reused.

### Authorized replacement, slot 9 — terminal update

At 19:26 UTC the owner authorized exactly one replacement partial-warm attempt,
preserving the original deadline, 7,200-second run limit and overall cap of 20.
The isolated adapter normalized the three Path/string boundaries. Fifteen policy
checks and two launcher-to-real-validator regression cases passed; the latter use
real manifest/owner/Git checks and stop before gate execution. Independent review
approved the exact corrected freeze. Fresh custody/source/input checks allowed
reuse of observer slot 7 without another diagnostic invocation.

Slot 9 ran from 19:32:04 to 19:34:27 UTC (143.599 seconds to terminal observation).
The repaired boundary worked: `make test` started, formatting passed in 3 seconds,
and Clippy passed in 1 second. During Rust compilation, the same process-census
helper exhausted its unchanged 30-second allowance. Admission took 0.299 seconds;
the command phase began 1.803 seconds after the allowance started. There was no
command-finished event; supervision reported failure at 34.484 seconds. This
locates the unresolved delay after command-phase entry, not inside a proven OS
or `ps` execution interval. No compiler defect or test assertion failure was
established. No complete gate or accepted cohort resulted.

Supervised gate cleanup escalated from TERM to KILL. Fresh native settlement at
19:34:49 UTC found 72 replacement custody records with empty sessions and released
guards: 71 finished records and one preserved nonterminal failed-helper record.
All 11 build-product records were finished; all 241 frozen files were unchanged.
The original failed helper also remains preserved. Nine attempts are now charged,
zero original cohort samples accepted, and no further attempt is authorized.
The same full release prerequisites and all 18 open story dispositions remain.

## Proposed actions and owner decisions — separate from evidence

### 1. Preserve final correctness before Done

Recommended sequence: settle the remaining implementation/scope decisions; build
an exact integrated candidate; run the required final correctness/release gate
while the seven landed stories stay Verifying; fix failures and revalidate the
final tree; then mark individually accepted stories Done. Perform the formal
release/publication after backlog disposition. If release tooling requires a
version-bumped candidate to run the release gate, distinguish preparation and
validation from publication and agree that ordering explicitly. Do not weaken
the hold or call the backlog complete before evidence exists.

All required Rust, contract, plugin and configured browser coverage remains
mandatory on the actual release tree (`make test-full` in the trusted workflow).
Audit skips and stale receipts. A correctness pass does not satisfy the separate
900-second or 30-production-attempt criteria. This PR starts neither a gate nor
a new performance investigation.

### 2. Decide research outcomes one story at a time

For SH-797/801/872, choose **accept the documented bounded, inconclusive research
outcome** or **retain the original unmet criteria and leave open**. The original
requirements remain visible either way. For SH-873, retain full coverage and
decide whether unmeasured cost analysis can have that same bounded disposition.
For SH-874, the original rollout cannot finish under the closed investigation and
disabled-production constraints: keep it open, or explicitly withdraw/re-scope
it. There is no passing-performance option supported by existing evidence.

For SH-841/845, retain the conditional stories open and disabled unless the owner
explicitly prefers a recorded no-go/dormant disposition. Existing records may be
audited without generating new production runs; absent or ineligible records do
not prove the trigger false or satisfied. Any cancellation/dormancy must be
recorded as such rather than successful enablement.

### 3. Choose providers, then implement the requested features

The recommended route preserves SH-896 and SH-897 as implementation work. Review
[the dated comparison](../spec/decision-provider-evaluation.md) and
[offline worksheet](../research/decision-provider-pilot/REVIEW.md); select runtime
and complexity providers independently. Existing documentation does not establish
an accuracy winner or current account access. This packet makes neither choice
and does not recommend purchasing a pilot. Record the selected provider/model,
typed contract, acceptable data boundary and criteria for progression from offline
implementation to evaluated feature. Provider choice/implementation authority is
separate from credentials, paid calls, production data and activation authority.

| Route | Concrete implementation after provider choice and authority | Required focused validation and remaining boundary |
|---|---|---|
| SH-896 runtime | Add independent global web toggle and durable server-side setting/readback; selected-provider adapter with typed bounded requests, abstention and deterministic fallback; connect incident identity/evidence to guarded recovery advice and deduplicated preventative-story proposals. Preserve existing owners and receipt authority. | New tests for persistence/restart, off mode/no calls, independent toggles, unsafe suggestions, missing/stale/duplicate evidence, invalid output, timeouts/errors and manual review. Automations off forbids automatic effects; model output never certifies a gate or authorizes cleanup. |
| SH-897 complexity | Add its own durable global toggle and selected-provider adapter; coordinate all creation entry points, preserve explicit manual complexity, coalesce duplicates and reject late results after input/config changes; retain current fallback without blocking story creation. | New tests for each complexity class, ambiguity, overrides, repeated creation, stale results, malformed/timeout/unavailable output, independent persistence and current-assessor interaction. Existing stories are not silently reassessed. |

Shared implementation may provide settings storage and transport contracts, but
neither toggle enables the other or starts project automations/verifier. Both
default off; configuration does not itself authorize an external call. Implement
against fake transport first and run only added/changed feature tests. A functioning
disabled integration can be reviewed without live calls; mock results cannot pass
provider correctness/calibration or rollout criteria. Resolve those unmet criteria
explicitly rather than silently dropping them or marking the features Done.

The latest retained assessor trace identifies human/agent rubric assignment and
native medium-unassessed fallback; installed plugins provide the rubric and the
observed project had no event hooks. This supersedes the comparison document's
earlier unresolved-assessor note for that observed setup; it does not prove every
other installation has no external assessor. Check the integration source again
when implementing, without changing other projects.

Optional **retain-current/no-provider** is permitted by SH-895's original decision
scope. If selected for either use case, explicitly decide whether the corresponding
feature stays blocked, is deferred or is cancelled/re-scoped. None of those is
successful implementation of its requested toggle and integration.

## Review and delivery boundary

This isolated documentation PR contains this report and a sanitized evidence index.
It does not merge draft PRs991–993 or copy their experimental collector into dev.
Existing raw records stay local, with fingerprints for traceability. Preparing
this packet ran only source/receipt, completeness, link and whitespace checks:
no tests were added or run, no campaign resumed, and no board states changed.
PR approval alone does not accept any proposed story disposition or authorize a
merge. Record each chosen action separately before changing the board or scope.
