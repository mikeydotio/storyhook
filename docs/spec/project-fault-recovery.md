# SH-742: Recover project verification faults through managed repair

## Summary

Replace permanent queue halts for proven project-owned gate faults with durable repair work. Preserve certification, process ownership, operator controls, and generation guards.

Confirmed failure points are `require_certified_by_gate` in `scripts/verify-pr.sh` and gate-configuration handling in `src/daemon/verification.rs`. Existing tests explicitly encode the unwanted permanent-halt behavior.

Obviation review covered SH-739, SH-740, and SH-741. None implements this recovery policy. Preserve SH-740’s label guards and SH-741’s completed work.

## Implementation sequence

1. Run `story comment SH-742 <exact approved plan text>`, posting this entire approved plan verbatim before editing files or running tests.
2. Repeat `story help obviation-review` and `story load-context --story SH-742`; review every candidate. Record the result and implementation decisions using **Context, Question, Decision, Rationale**.
3. Add failing regressions reproducing a successful gate without certification and invalid project gate configuration. Implement the following changes within SH-742, using focused modules and commits with their regression tests.
4. Update `docs/spec/verification-workflow.md`, affected help, dispatch guidance, and diagnostics.
5. Run new and directly impacted tests, commit, record evidence, and finish with `story move SH-742 verifying` as the absolute last action.

## Recovery design

### Structured fault classification and current configuration

Introduce a typed `ProjectFault` outcome shared by the shell protocol and Rust verifier. Retain fault code, owning project, configuration identity, attempt/generation, pinned base/head/tree when available, execution result, log references, and cleanup evidence.

| Evidence | Treatment |
|---|---|
| Invalid project gate configuration | Project repair |
| Missing/non-executable repository-local gate entry point | Project repair |
| Completed successful execution with no qualifying receipt for the exact tree | Project repair; tree remains uncertified |
| Ordinary completed nonzero test execution | Existing test-failure remediation |
| Missing host tool, credentials, access, or unavailable external service | Existing infrastructure classification |
| Invalid verifier protocol, uncertain merge, or unsettled cleanup | Preserve explicit infrastructure/ownership hold |

Classify where the evidence originates. Do not infer ownership from human-readable errors or classify every exit 126/127 as a project defect.

Give receipt inspection a machine-readable result distinguishing absent or insufficient certification from reader failure, invalid references, and tree mismatch. Preserve existing receipt validity rules and shell callers.

Resolve gate configuration from the pinned proposed merge snapshot before executing it. Keep registered-checkout identity and origin authorization separate. This lets a repair branch fix its gate without first changing the registered checkout or bypassing certification. Preserve the default gate and plain-argv validation.

### Durable coordination

Add an additive store migration for fault records, affected submissions, repair decisions, and pending delivery effects. Persist:

- Stable recovery ID and revision.
- Fault identity, immutable observations, and original unjudged submission evidence.
- Assessment owner and dispatch lease.
- Scope decision and repair story identity.
- Affected stories, recovery-owned dependency edges, and resume disposition.
- Attempt counts, delivery status, and terminal diagnostics.

Deduplicate by owning project, typed fault code, and repository-relative gate/configuration locus. Store changing heads and configuration digests as observations rather than creating new repair identities. Permit only one active coordinator per fault; retain lineage through repair attempts.

Create or reuse repairs, attach relationships, record decisions, and enqueue delivery intent transactionally. Run external effects after commit. Reconcile interrupted effects against existing dispatch identities before retrying. This follows SQLite’s transaction boundaries and explicit idempotency-key practice. [SQLite transactions](https://www.sqlite.org/lang_transaction.html), [AWS idempotent APIs](https://aws.amazon.com/builders-library/making-retries-safe-with-idempotent-APIs/)

Use the existing change bus and periodic recovery wake to resume unfinished coordination after restart. Recovery work must run independently of verifier workspace ownership.

### Story deletion, prefix changes and record isolation (SH-848)

A recovery record keeps story numbers and exact event sequences for as long as it lives, active or retired, and `read_view` fails closed (`StoreError::Corrupt`) when one no longer resolves. Three rules keep a supported command from producing that state, and keep one invalid record from stopping unrelated work.

- **Delete refuses a named story.** `story delete` and its preview refuse any story a recovery names (`project_recovery::references::stories`, plus the record's observation rows) and point at `story close`. Tolerating the missing story was rejected: no command retires a recovery, so one whose repair or assessor vanished would stay held forever.
- **A prefix change keeps retained leases valid.** A retained managed lease records the story id its worktree, branch and pane were created under. Validation compares the number that id was minted with, not the id under the current prefix.
- **Readers validate only the records that name their story.** `attempts::owner` (admission, completion, observation, landing), `owns_resume` (block-edge derivation in every story write), `owns_coordination` (Full Auto lanes) and `reconcile_incident` read only the records that name their story, or whose own observations corroborate the incident. The queue reads judged generations from the immutable observation rows. For a valid store every answer is unchanged. An invalid record still fails closed for the stories it names, for a new fault with its code and locus, and in `repair show` and the status snapshot; status-snapshot isolation is SH-851.

### Bounded agent scope judgment

Use the stopped story’s managed agent to assess scope. Reuse notification and proven-absent resume dispatch; do not add an autonomous fixer inside the verifier.

Add shared CLI/RPC operations:

- `story verifier repair show <recovery-id> --json`
- `story verifier repair decide <recovery-id> --input <json-file>`

The versioned decision input contains recovery revision, originating generation, dispatch identity, scope (`same-story`, `separate-story`, or `external`), owning project, evidence references, and nonempty Context/Question/Decision/Rationale. Separate-work decisions also supply a repair description and acceptance criteria.

Validate current authority, ownership, labels, and decision consistency transactionally. Identical replay returns the recorded result; conflicting or stale input is rejected. Scope advice cannot authorize certification, credentials, project reassignment, or permission overrides.

The assessment charter requires the scope rubric, source inspection, and a decision before edits:

- **Same story:** return it with diagnosis and instructions to repair, run new/impacted tests, commit, and resubmit.
- **Separate story:** create or reuse one critical bug in the proven owning project and attach real `blocked-by` relationships to affected stories. Dispatch through existing managed dispatch and lease machinery.
- **External:** retain a contextual hold naming the actual prerequisite.

Document critical priority as this recovery path’s explicit policy exception. Do not reprioritize unrelated stories.

Limit assessment delivery to three proven failed attempts, using existing bounded dispatch deadlines. Allow a delivered assessment 30 minutes to return its decision; expiry records an affected-story hold rather than launching a competing agent. An ambiguous live agent remains protected.

### Queue release and automatic resumption

Settle owned children and restoration before releasing verifier ownership. A recoverable project fault then releases the queue immediately; neither assessment nor repair holds the verifier lock.

For separate repair, retain the original submission and create only recovery-owned dependency edges. Do not globally block all project stories: attach additional affected stories when structured evidence identifies the same fault. The repair story must never depend on itself or acquire a dependency cycle.

After the repair passes central verification and lands:

1. Reconcile durable repair completion.
2. Release only recovery-owned holds; preserve unrelated blockers and operator settings.
3. Resume affected agents through the managed resume path.
4. Refresh source/configuration, reconcile with the current base, and resubmit from the existing worktree.
5. Admit a fresh verification generation and certify the newly computed merge tree.

A dropped, manually closed, or merely unblocked repair is not proof of verified repair completion.

Allow at most three completed repair submissions per recovery lineage, each requiring changed committed input. Repeated unchanged submissions do not consume another gate run. A repair that encounters its own fault updates the same lineage rather than creating another repair story. Exhaustion produces a durable affected-story hold with all attempts; unrelated eligible work continues.

Honor manual stop, `human-only`, and `no-auto` before assessment, dispatch, decision acceptance, and resumption. Preserve uncertain landing intents and resource quarantine.

### Release of enrollment policy holds (SH-860)

A retained assessment initially held by a reservation or verifier stop is
reconsidered on recovery-worker wake and daemon restart. Remove the reservation
with `story unlabel <story-id> <label>`; use `story verifier start` to lift a
verifier stop. Both controls must permit recovery. Status evaluates the current
hold without writing, and names eligibility for managed reconciliation once
policy clears. Existing version-1 records use this path without migration.

Under the existing target and origin workspace locks, a service transaction
rechecks the original generation, state event, reservation history, interruption
history, dependencies, awaiting state, resets, quarantine and landing ownership.
An unchanged submission that enrollment left in verifying is returned through
the ordinary state-transition mechanism. Its mutable state and label authority
are updated with that return and the assessment becomes pending atomically.
The immutable observation, candidate, recovery ID, dispatch identity and original
unjudged generation remain intact. The ordinary claim and delivery monitor
still recheck authority before external effects.

Only removal of the reservation present at enrollment may renew initial label
authority. Successive removal of two reserved labels is allowed; addition or
re-addition is a new reservation episode and revokes the older authority.
Already-returned assessments and held repair/resume effects require unchanged
authority. They cannot clear a retained terminal awaiting disposition, even if
someone has subsequently cleared the story's awaiting text.

Automatic rearming requires no previous delivery or a proven failed delivery
below the existing attempt limit. A confirmed, uncertain or interrupted delivery,
response expiry, exhausted budget, or terminal disposition never becomes fresh
delivery merely because policy clears. This also applies when the stored policy
hold masks the earlier transport result. Status directs inspection through
`story verifier repair show <recovery-id> --json` when ownership reconciliation
is required. There is no blanket retry or unblock command.

Repeated checks of an unchanged hold append no events and revise no record.
Previously observed generations stay excluded from the verifier queue; recovery
requires a scope decision and later fresh submission, never replay of the old gate.

For existing incidents, automatically convert only those whose retained structured execution, receipt, generation, and cleanup evidence proves a supported project fault. Preserve the old incident as historical evidence. Never clear an ambiguous legacy incident by matching its diagnostic text.

## Interfaces, diagnostics, and tests

Expose additive recovery status in the shared CLI/dashboard snapshot: fault, affected stories, assessment/repair owner, repair link, phase, attempt budget, and next action. Distinguish repair dependency from infrastructure halt. Existing payloads deserialize with no recovery records.

Test through production service, dispatch, subprocess, and queue paths, mocking external endpoints or provider responses rather than recovery behavior:

- Missing receipt, insufficient receipt tier, invalid configuration, and missing repository-local command.
- Same-story and separate-story recovery through fresh certified submission.
- Repair configuration read from the proposed merge while the registered checkout remains stale.
- Required regression execution before receipt creation; no fabricated receipt or reduced gate coverage.
- Duplicate observations, concurrent decisions, restart at each durable/effect boundary, dispatch failure, and absent-agent recovery.
- Dependency completion, unrelated blockers, dropped repairs, unchanged resubmission, exhausted budgets, and recursive-fault prevention.
- Manual stop, reserved labels, rapid authority changes, stale decisions, cancellation, uncertain landing, and cleanup survivors.
- Other projects and unrelated candidates advance while repair is pending.
- Proven legacy incident conversion and refusal of ambiguous legacy evidence.
- CLI/dashboard rendering and migration compatibility.

Run `scripts/select-tests.sh` against the actual changed tree before selecting direct commands. Run new and directly impacted Rust, shell/plugin, and browser cases, plus applicable formatting and warning checks. If selection returns `ALL`, record why and retain the targeted-test boundary. Use `/tmp` for scratch fixtures.

## Delivery boundaries

Keep all assigned recovery work in SH-742. Record later decisions immediately and adopt nearby defects according to the scope rubric, with separate commits and regressions.

The existing v3.0.2 tag/commit mismatch is diagnostic-only. Do not patch webtail directly, modify SH-741’s branch, release, push, open a PR, or run the full suite.

After commits and the final evidence comment, run `story move SH-742 verifying` from this worktree and stop. The centralized verifier owns submission, full-suite testing, merge, completion, and cleanup.
