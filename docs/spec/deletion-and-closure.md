# Deletion is permanent; closure is the record

Design of record for **SH-503** and its three children — SH-505 (the `closed` state and
`story close`), SH-498 (`story delete` becomes a hard delete) and SH-506 (`<DELETED>`
references). User determination, 2026-08-27, taken while planning SH-498.

## The conflation this removes

`story delete` means two incompatible things today.

*This story was a mistake* — filed under the wrong project, a duplicate, a typo. Nothing
about it is worth keeping, and the only honest disposal is removal.

*We decided not to do this* — an approach abandoned, a plan superseded. The story is
exactly what a tracker exists to hold: a record of a choice, with the reasoning attached.

Soft delete serves the second badly and the first not at all. Nothing is ever really
removed, so a mistake keeps its id and its row forever; and the story keeps every
relationship it had, so live stories go on claiming `parent-of`, `blocked-by` and
`relates-to` edges into something no board will show. SH-498 was filed against that
second symptom.

## Why the original SH-498 contract was not enough

As filed, SH-498 made `story delete` refuse when other stories held edges into the
target, listing them, with `--force` to override and unmake them. The invariant behind
it — **we must never allow orphan relationships** — is the part that mattered.

That contract holds the invariant at exactly one door. `story relate <live> blocks
<soft-deleted>` was still permitted: SH-207 deliberately relaxed the *target* guard to
"closed", and a soft-deleted story is merely closed. So an orphan edge stayed creatable
from the other side, and the invariant would have been true only of the door that
enforced it.

Hard deletion closes the class instead of guarding it. The far end does not exist, so an
orphan edge stops being a state the store can hold. The guard becomes unnecessary rather
than merely correct.

## The two verbs

### `story delete <id> [--force]`

Destroys the story: its events, its row, its id. This is today's `story purge` body,
promoted to the primary verb and freed of its "must be soft-deleted first" antechamber.
Its two-step ordering is already correct and already documented — retract every surviving
claim with real `StoryRelationshipRemoved` events, *then* `WriteOps::purge_story` — and
that first step is what unmakes the relationships.

`surviving_claims` is reused **unchanged**. It deliberately does not filter deleted or
archived claimants, which is exactly right here: every edge into the story must be
retracted, whoever holds it.

**No reason argument.** Nothing survives to carry one — a story's events are its only
record and they are destroyed. The reason now belongs to `close`.

**Confirmation on every interactive route; `--force` is the bypass.** The gate is a typed
token (the story id), matching `ConfirmationPlan::Purge` today: the act is a one-way
door, so the gate matches the act. Non-interactively — `--json`, no terminal — the
existing machinery already refuses and names `--force`, which is the shape SH-498's own
example asked for.

### `story close <id> "<reason>"`

What soft delete was reaching for. A closed story was intentionally not completed and is
retained for the historical record. It keeps all its data and **all its relationships**.

`closed` is a real state in the CLOSED superstate that behaves *exactly* like `done`. No
freeze rules, no new guards: SH-261 (a closed story takes comments) and SH-207 (a closed
story can be a relation target) are untouched. A separate, stricter rule for `closed`
was considered and rejected by user determination — "Done's rules are better."

The verb is parse-time sugar producing `Invocation::SetState { state: "closed", comment:
Some(reason) }`. No new `Invocation` variant, no new event kind, no new snapshot field,
no new MCP or wire surface. Two consequences worth stating rather than discovering:
`set_state` routes through `resolve_open_story`, so `close` is **not idempotent** — it
refuses an already-closed story, exactly as `story move <id> done` does; and the reason
must travel as `comment`, never as `awaiting`, because `set_state` refuses a `--reason`
on a CLOSED target.

## Placement: why `closed` goes after `done`

`closed` is a fifth entry in `domain::REQUIRED_STATES`, placed **after** `done`. Three
functions answer "the CLOSED state" with a bare `.find()` that today can only return
`done`:

- `service::project::closed_state` — names the state in every generated `AGENTS.md`;
- `service::pr_check` — a merged PR closes its story, where `done` is the right answer
  and abandonment would be a lie;
- `domain::resting_state_for_deleted`, renamed `resting_state_for_closure` here.

Ordering keeps the first two correct. The third is the one that *should* now answer
`closed`, and it is made explicit rather than left to ordering: a `CLOSED_STATE_SLUG`
constant read by both `REQUIRED_STATES` and the resolver, in the `UNCLAIM_FALLBACK_STATE`
style. A reframe that depended on which literal came first in an array would be one edit
away from silently reversing itself.

`default_states` has **three** hand-written twins of that array —
`service::project::default_states`, `storyhook_test_support`'s, and
`tests/store_support`'s. All three gain `closed`, or every fixture sits below the
required-states floor.

## The legacy fold

`StoryDeleted` stays in the event log. It is permanent, append-only history and there is
nothing to rewrite; it is simply **read differently**. A story whose log carries it folds
to state `closed`, `closed_at`, and `hidden_at` — closed, and archived, so it stays as
invisible as it is today.

`StorySnapshot.deleted` and `.deleted_reason` go, along with the `stories.deleted` column
and `StoryQuery::deleted` — but **with SH-498, not with the fold**. Removing the flag
forces the delete verb's own change with it, and the "As built" section below records why.
The human record survives their removal untouched: today's `delete` already writes a
`[deleted] <reason>` **comment** beside the event.

Three things about this fold are load-bearing.

**`hidden_at` is stamped inside the `StoryDeleted` arm, never after the loop.** This is
the most dangerous bug the design makes available. Stamped post-loop, `story unarchive`
on a migrated story appends `StoryUnhidden`, the replay clears `hidden_at`, and the
post-loop stamp puts it straight back — a silent, permanent no-op on exactly the
population the migration creates. In the arm, `StoryUnhidden` wins by ordinary replay
order.

**`resting_state_for_closure` keeps a fallback chain rather than hard-coding `closed`.**
Prefer `closed` when defined *and* CLOSED; else the existing chain (`done` if defined and
CLOSED, else the alphabetically first CLOSED state); else keep the current slug and force
`SuperState::Closed`. `fold_story` is not permitted to fail on its own history, and a
catalog with no `closed` is reachable three ways: a legacy tree read through
`storage::load_state_map`, a store not yet migrated, and `service::migrate`'s pre-repair
catalog. The chain is also what lets a project that already owns an OPEN state named
`closed` stay coherent — see below.

**A move into an OPEN state now clears `hidden_at` as well as `closed_at`.** Without it,
`[StoryDeleted, StateChanged(open), ClosedAndArchived]` — delete, undelete, later close
for real — folds *hidden*, because the post-loop `if superstate == Open { hidden_at =
None }` retraction cannot fire on a story that ends CLOSED. This is a behaviour change
beyond the determination's own scope, adopted deliberately: it makes `hidden_at`
symmetric with `closed_at`, which the same arm has always cleared. Nothing pinned the old
behaviour (`fold_story_reopening_clears_hidden_at` ends OPEN and is covered by the
post-loop rule either way).

## Migration 21

**A soft-deleted story is already in `done`/CLOSED, not `todo`.** Migration 4 made
`stories.superstate` a pure function of the slug and the catalog, and the composite
foreign key enforces it — inserting `('todo','CLOSED')` is refused. So this migration is
not repairing an illegal pair. It moves a legal row from `done` to `closed` and stamps
`hidden_at`. `superstate`, `archived`, `closed_at`, `head_seq`, `head_global_seq` and
`updated_at` are untouched, and **no event is appended**, so `projects.next_global_seq`
stays put. That is the single biggest reason this migration is cheap, and it belongs in
the migration's own header.

Order is required, not merely tidy. The foreign key is `DEFERRABLE INITIALLY DEFERRED`,
so the wrong order survives to COMMIT and *then* fails with `FOREIGN KEY constraint
failed (19)`, naming neither table nor row and rolling the whole migration back.

1. Insert `closed`/`CLOSED` into every project's `project_states` at `MAX(position)+1`,
   with `role` and `description` NULL — matching `with_required_states` exactly, so a
   migrated project and a `doctor --fix`ed project cannot disagree.
2. Repoint every soft-deleted story: `state`, `hidden_at`, and the `snapshot` blob.
3. `ALTER TABLE stories DROP COLUMN deleted`.

**No table rebuild.** `DROP COLUMN` succeeds against the bundled SQLite (3.51.0,
measured): the column-level `CHECK (deleted IN (0,1))` drops with the column, and no
table CHECK, index, foreign key, trigger body or view names it. `foreign_keys_off` stays
`false`, and there is **no** `DROP TRIGGER events_reject_delete` bracket — the schema-5
hazard applies only to a `DROP TABLE`/`RENAME` rebuild.

**Key the repoint on `stories.deleted = 1`, never on `EXISTS(kind='StoryDeleted')`.**
Migration 16's lesson, on a different fact: `[StoryDeleted, StateChanged(todo)]` is a
live, reachable log — delete, then `reopen --force` — whose story is *not* deleted, and
`EXISTS` would archive it. The column is already the head-bounded, fold-authoritative
answer.

**`hidden_at` comes from the `StoryDeleted` event's own `at`**, bounded by `seq <=
stories.head_seq` (migration 15's rule), not from `closed_at`. The two differ whenever a
story was closed before being deleted — a case
`fold_story_deleted_while_closed_keeps_original_closed_at` already pins.

**In the snapshot patch, `$.state` and `$.hidden_at` are correctness; stripping
`$.deleted`/`$.deleted_reason` is hygiene.** `diff_rebuilt` compares the *deserialized*
struct and `StorySnapshot` has no `deny_unknown_fields`, so leftover keys are invisible
to the oracle and key order is irrelevant. They are stripped anyway, per migration 4's
rule that a repair leaving the document behind fixes the query surface and leaves the
displayed story wrong — but the header says which half the oracle actually depends on, so
the next reader does not infer a constraint that is not there.

### A project that already defines an OPEN state named `closed`

`story state add closed --super OPEN` is legal today, and `with_required_states` refuses
to reclassify an existing state under a new superstate. The migration must not flip it
either. The `UPDATE` is guarded on `EXISTS (slug='closed' AND superstate='CLOSED')`, so
those rows **stay in `done`** — and the fold's fallback chain agrees with them, answering
`done` for the same reason. `diff_rebuilt` is clean, and `story doctor` reports the
catalog problem as a `RequiredStates` finding that `--fix` correctly declines to repair.

`story close` **refuses** on such a project rather than moving stories into an OPEN state
and reporting success.

An earlier draft had the migration abort here. Leaving the store readable and letting
the existing catalog machinery report the problem is strictly better, and the fallback
chain is what makes it possible.

## A defect this change activates

`service::migrate` folds each story against the **legacy tree's own** catalog rather than
the repaired catalog `write_states_repairing` has just written. That is benign today —
every repair is the addition of a state no story sits in — and not benign afterwards: a
legacy tree has no `closed`, so a soft-deleted story folds to `done` and is written
pointing at `done`, while `diff_rebuilt` folds against the repaired catalog and answers
`closed`. Every `story migrate` of a tree holding a soft-deleted story would produce a
fresh divergence, the frozen baseline tree included.

The fix is to re-read the catalog after the repair, which
`service::transfer::import_project` already does correctly.

## An epic whose children were all abandoned

`live_children` and `children_of` filter `!child.deleted` (SH-497). Removing the field
removes that filter, and those children become ordinary `closed` stories that keep their
`child-of` edges — so such an epic now computes `closed`.

This is not SH-497 reopening. SH-497's defect was the epic reading **done**: a completion
nobody chose, carrying `state_computed: true` so it read as authoritative. An epic all of
whose children were abandoned reading **closed** is a true statement about that epic.
Adopted deliberately.

## What this closes without building it

The `story doctor` orphan-edge sweep SH-498 proposed is deliberately not filed. After
this lands there are no orphan edges to sweep for: a hard delete unmakes them, and every
legacy soft-deleted story becomes a `closed` story that keeps its edges legitimately.
`DanglingRelation` already covers an edge to a story that is genuinely absent.

## `<DELETED>` references (SH-506)

Once a hard delete is possible, prose can name a story that no longer exists. The
dashboard's `linkifyStoryIds` renders an unresolvable id as plain text today; it renders
`<DELETED>` instead.

**Render-time, never a rewrite.** Nothing stored is touched. This preserves `story
project set-prefix`'s recorded rule — "there is no grammar in this codebase for a story-id
reference inside prose, so rewriting one would be a guess dressed up as a fact" — and it
is strictly more correct besides, because it also covers a reference written *after* the
deletion, which no rewrite could reach. Comments could not be rewritten in any case: the
event log is append-only and there is no edit-comment event.

**The predicate is not "absent from `state.data.stories`"**, which is also true of a
draft, an archived story, or an id belonging to another project. It is: the id parses
under *this* project's prefix, is at or below the project's highest minted story number,
and resolves in none of the client's arrays. Story numbers are never reused, so the
answer is permanent. `/data` therefore carries one new integer, the watermark from
`projects.next_story_no`; the client's arrays already cover the whole project, since
`report_data` is built from `story_views` and `project_data_json` filters out only
deleted stories and drafts, routing drafts into `drafts_json`.

The CLI renders prose verbatim and has no id grammar at all. Deliberately untouched.

## Consequences accepted rather than fixed

- **"As invisible as today" is nearly, not exactly, true.** `is_visible` drops deleted
  stories with no flag able to reach them; a hidden `closed` story *is* reachable through
  `--include-archived` and `--all`. `build_visibility_message`'s "N deleted stories are
  not listed" disclosure disappears with the concept it described.
- **`story reopen` lands a formerly-deleted story in the default open state**, not
  wherever it was. Already true, but with `--force`/`UndeletePlan` gone this is the only
  route back to OPEN; the original state survives only in the story's own event log.
- **`story close` loses the `[deleted] ` comment prefix.** The reason is an ordinary
  comment now, so nothing marks it as the closing one to a reader scanning `story log`.

## As built

### SH-505 narrowed: the `deleted` field stays until SH-498

The plan had SH-505 remove `StorySnapshot.deleted`, `deleted_reason`, the
`stories.deleted` column and `StoryQuery::deleted` alongside the fold change. It cannot:
removing the flag forces the delete verb's own change with it. `resolve_purgeable_story`
gates `story purge` on "has been soft-deleted", `reopen_plan` gates the undelete on the
same flag, and `service::history` writes a `StoryDeleted` to redo one — so a SH-505 that
removed the field would either ship a broken `story purge` or drag the whole of SH-498
into it.

Discovered by doing it: the removal was implemented, produced 21 compiler errors across
12 files, and every one of them was about a verb SH-498 owns. So SH-505 is exactly the
additive half — the state, the verb, the fold, the migration — and migration 21 keeps the
`deleted` column, which still describes something real until deletion becomes permanent.
That also keeps each wave's history bisectable, which a combined one would not have been.

### The migration skips a colliding project rather than aborting

The plan had migration 21 **abort** when a project already defines a state named `closed`,
on the grounds that silently reclassifying it would move every story in that state into
the CLOSED superstate. Aborting turned out to be unnecessary, and worse than the
alternative: it would leave the store at the old schema for a condition that harms
nothing.

What ships instead: the INSERT skips a project that already owns the slug, and the UPDATE
is guarded on the slug being CLOSED, so those stories stay in `done` — and
`resting_state_for_closure`'s second rung answers `done` for exactly those catalogs, so a
fresh fold and the stored row still agree. `story doctor` then reports the real problem,
a catalog below the required-states floor, as the `RequiredStates` finding it is. The
fallback rung was already required for other reasons; this is a second thing it buys.

### `hidden_at` is cleared by a move into an OPEN state

Not in the plan, and forced by the fold change. With `hidden_at` stamped in the
`StoryDeleted` arm, the history `[StoryDeleted, StateChanged(open), ClosedAndArchived]` —
delete, undelete, later close for real — ends CLOSED, so the post-loop
`superstate == OPEN` retraction never fires and the story silently re-archives itself.
The `→ OPEN` arm now clears `hidden_at` alongside `closed_at`, which it has always
cleared. A behaviour change beyond the determination's own scope, taken deliberately;
nothing pinned the older behaviour.

### Two guards the plan did not know were missing

`service::project::default_states` claims in its own doc comment to be "exactly
`REQUIRED_STATES`, in that order", and nothing checked it — nor the two test-support
twins. The claim is load-bearing: catalog order is what `closed_state` and `pr_check`
read when they take "the first CLOSED state", so a twin that drifted into listing
`closed` first would scaffold an AGENTS.md telling every agent to finish its work by
abandoning the story, with the floor itself still correct and every test of the floor
still green. `tests/required_states.rs` now pins both twins, and
`tests/service_system.rs` pins the consequence at the surface an agent would feel.

### One unrelated defect adopted

`store_properties`' relation property asserted `parents <= 1`, pinning a unique index
SH-446 removed on purpose. Unreachable until this change perturbed the generator's RNG
stream. Fixed in its own commit rather than filed, per `story help scope-rubric` — and
the first replacement for it was wrong in an instructive way: "a story may not be its own
ancestor" is a `RelationService` rule, and that harness bypasses the service on purpose to
exercise the store. Symmetry is the whole of what the store itself promises.

### Not yet built

SH-498 (hard delete) and SH-506 (`<DELETED>` references) are unstarted. The dashboard has
no Close action yet — the CLI verb, the state and the migration land first, since the
dashboard has nothing to offer until `closed` exists.
