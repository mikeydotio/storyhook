# Blocked state transitions (SH-656)

An open `blocked-by` target prevents a story from advancing. The rule belongs
to domain admission, shared by every live writer and verifier eligibility.
Historical folds remain readable. The approved plan and council verdict are
recorded on SH-656, outside this disposable worktree.

## Contract

Use the existing readiness predicate's interpretation: a target whose effective
superstate is OPEN blocks; an absent target does not. Extract that predicate so
readiness, state admission, and landing cannot disagree. `obviated-by` retains
its unconditional readiness restriction without becoming a transition blocker.

Configured catalog order determines advancement. Backward and same-state moves
are allowed. The exact `closed` slug remains available for abandonment; `done`
is forbidden while blocked even if reordered earlier. Entering reserved
`blocked` is always allowed; leaving it uses the latest non-blocked pipeline
state as the comparison point. A story created in `blocked` uses the first
configured non-blocked OPEN state. This prevents a detour through `blocked`
from manufacturing progress. Unknown comparison states fail with repair context.

Validate proposed events in order against their preceding folded snapshot.
Admission and persistence share one write transaction. Refusal names the story,
source, destination, blockers, and the available abandonment action. No partial
events, comments, PR status updates, or read-model changes may survive refusal.

## Replay and maintenance

Pure `fold_story` reconstructs accepted history without new admission checks.
Raw import/migration and deterministic restore defaults are replay/repair paths.
Catalog occupant migration and integrity repair have explicit maintenance entry
points. Live undo is compensating input and obeys admission. Computed epic
rollups remain projections; effective epic states resolve dependency blockers.
An architectural regression prevents new live callers bypassing admission.

## Verification and durable landing

Blocked submissions remain visible in `verifying` with their PR and generation.
Runnable selection skips them, allowing unrelated work and automatic eligibility
when dependencies resolve. Verification produces a certification without merging.

`begin_landing` rechecks the submission and blockers in a short write transaction
and persists an intent identifying project/story, generation, exact PR, expected
head, and certified tree. Only that intent authorizes the separate landing call.
The database transaction ends before external work starts.

While unresolved, the intent prevents mutations invalidating that identity or
introducing open blockers, including reopening dependencies, materializing absent
targets, and administrative replacement. Unrelated writes remain available.
These checks apply at transaction commit, including maintenance paths, so replay
exemption cannot invalidate a live merge already authorized.

Recovery reconciles unresolved intents before rescheduling their stories.
Completion and intent release are atomic. A definitive refusal before a merge
request permits release; timeout, connection loss, or a currently OPEN PR do not
prove that an earlier request cannot still complete. Ambiguous intents remain
visible and fenced, without automatic expiry or starvation of unrelated work.
Independent actors merging directly on GitHub are outside this local guarantee.

The three-seat council selected durable intent by ranked-choice majority (2–1).
A transient SQLite lock releases on process death and cannot fence a remote
request that survives it. References: [SQLite transactions](https://www.sqlite.org/lang_transaction.html),
[GitHub merge API](https://docs.github.com/en/rest/pulls/pulls#merge-a-pull-request),
[event sourcing](https://learn.microsoft.com/en-us/azure/architecture/patterns/event-sourcing).

## Validation and delivery

Regressions cover order permutations, the blocked detour, exact abandonment,
undo, historical replay, maintenance, batch rollback, queue visibility, blockers
arriving during tests or landing, and restart with ambiguous external outcomes.
Mutation-check the exact `closed` exception. Run the impacted-test selector on
the changed tree and only new/directly impacted tests. The centralized verifier
owns the full suite and merge. Submit one linked SH-656 PR and move to verifying
as the last action.
