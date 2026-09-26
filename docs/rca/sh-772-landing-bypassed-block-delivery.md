# SH-772: A verifier landing unblocked a working story and never told it

- **Date**: 2026-09-24 10:05:56Z (the missed resume investigated); latent since
  2026-09-11 (SH-690, the block-delivery outbox, shipped without the landing door)
- **Severity/Impact**: Silent loss of an agent kickstart. When the central verifier
  landed a story, every in-progress story it unblocked stayed parked: no Resume
  row, no `AGENT BLOCK DELIVERY` comment, nothing in the dashboard. moshtail MT-32
  sat idle 37 hours until a person resumed it by hand. No store data was wrong;
  the missing record was the delivery effect itself.
- **Status**: Fixed on `worktree-SH-772`: the landing, the doctor drift heal and the
  prefix rename derive block edges; a runtime backstop refuses the class.
  A Resume whose interrupt was never acknowledged now reaches the story's registered
  session (council decision D5). Follow-ups filed: SH-779 (atomic blocked filing),
  SH-780 (the same idle-composer guard for the pinned and remediation paths), SH-781
  (test harness leaks a Full Auto lane identity).

## Reported failure

SH-772, filed 2026-09-25: "When MT-13 closed, its blocking relationship of MT-32
cleared, but MT-32 was not notified its tmux window to resume work. Other
notify-on-unblock mechanisms seem to work normally."

## Evidence

Read-only from the live store (`~/.local/share/storyhook/store.db`) and
`story log`. Times are UTC.

| Time | Fact | Source |
|---|---|---|
| 2026-09-24 08:41:50 | MT-32 created (`new`) and claimed by Full Auto in the same second (state change with no command or actor) | `story log MT-32` seq 1-6 |
| 08:41:58 | `relate MT-32 blocked-by MT-13`: MT-32 is in-progress and now blocked | seq 8 |
| 08:42:12 | `story.sh:dispatch` launches MT-32's session | seq 9 |
| 08:42:50 | Delivery #92 `interrupt` `unreached`: "could not bind the dispatched session: … ps … timed out after 5 seconds" | `block_deliveries`, seq 10 |
| 08:46:38 | The MT-32 session finds the block itself and parks | seq 13 |
| 10:05:56 | The verifier lands MT-13 (PR #226); MT-32 records "no longer blocked-by MT-13" with no command or actor | seq 16 |
| 10:05:56 onward | No `resume` row for MT-32 exists, and no delivery comment | `block_deliveries` |
| 2026-09-25 22:50:45 | MT-32 resumed by hand | seq 17 |

Other doors worked: MT-22's resume #93 followed a `web:clear-awaiting`. Three
older resumes had failed a second way — MT-20 #32, WT-1 #73 and #75, "no agent
reached: no acknowledged interrupted session to resume".

## Root cause & trigger

`VerificationQueue::complete_landing_guarded` (`src/service/landing.rs`) closed the
landed story in a plain `store.write`. Its `append_state_transition` retracted the
`blocked-by` edges the story imposed, inside that transaction. Only
`Ctx::write_stories` compared effective blocking before and after a transaction
and enqueued the Resume and Interrupt rows, so a landing never did. Its sibling
`record_merged` closed stories through `write_stories` and was correct. The same
bypass missed an epic whose last open child the landing closed, and left the
landed story's own pending effects unretired. The trigger is any verifier landing
of a story that another in-progress story is `blocked-by`.

The landing door (`c9043c63`, SH-656) predates the outbox (`9d2e17ee`, SH-690) by
18 hours. SH-690 wrapped every event writer it found, and its detector,
`tests/block_delivery_paths.rs`, is how it found them: a scan of `src/service`
for three helper names, checked per file. `landing.rs` called a fourth helper,
`append_state_transition`, so the scan never listed it. Files that mix wrapped
and plain writes also passed.

A sibling bypass came out of the sweep: `story doctor --fix` heals a drifted row
with `store::repair_read_model`, a plain write. Every reader reads the rows, so a
row that drifted into a hold held its agent as surely as a real hold, and healing
it was an unblock nobody heard about.

A second, separate cause kept MT-32 from being told even with the landing fixed:
the worker sent a Resume only to a session that a Delivered interrupt had
acknowledged. MT-32's interrupt (#92) had failed on a 5 s `ps` bound under load
(SH-766), so with the landing fixed its Resume would still have ended "no agent
reached: no acknowledged interrupted session to resume". MT-20 and WT-1 had
already lost resumes that way. The rule came from the SH-718 council, whose
concern was a delayed effect reaching a replacement session; it never weighed the
case where no interrupt was acknowledged at all.

ODC classification: **Interface / Missing**, triggered by **Build/Package/Merge**
(two features landed the same day; the later one's fence did not see the earlier
one's door).

## Contributing factors

- The class detector keyed on helper names, not on what a transaction does.
- `(unrecorded)` in `story log` is what a daemon-internal write looks like, so the
  edge removal looked ordinary.
- MT-32's interrupt had already failed on a 5 s `ps` bound under load (SH-766), so
  even a correct landing would have produced only an unreached resume.
- MT-32 was dispatched while blocked because `story new` cannot file a blocked
  story atomically (SH-779).

## What now guards the class

- `service::block_delivery::derive_block_edges` is the one derivation;
  `Ctx::write_stories`, the landing, the doctor heal and the prefix rename all
  run inside it.
- The service write funnel refuses a block-relevant event outside derivation,
  and `refold_story` refuses to run outside it. `domain::BLOCK_INERT_EVENT_KINDS`
  names the 17 kinds that cannot change blocking, so a new kind is relevant
  until argued otherwise. Pinned by `service::block_delivery::tests` and
  `exactly_the_listed_kinds_are_block_inert`.
- `tests/block_delivery_paths.rs` scans the indirect helpers too, and fences the
  derivation flag's setter and direct row writes to their known doors.
- Regression tests: `tests/landing_block_delivery.rs` (including the production
  flow from a verifier tick to the helper), `tests/doctor_block_delivery.rs`,
  `set_prefix_neither_records_nor_retires_block_deliveries`, and
  `landing_completion_resumes_the_dependents_it_unblocks`.
- The resume rule: `tests/block_delivery_resume.rs` (unreached, uncertain and
  superseded interrupts; episode scoping; the SH-718 pin kept; an acknowledgement
  without a target stays Uncertain; the remedy), the MT-32 regression
  `a_landing_resumes_an_agent_whose_interrupt_never_arrived`, the doctor heal driven
  to Delivered, and the plugin test `test-notify-registered-session.sh` (busy
  composer, dropped paste, delivery, no adoption; mutation-checked).
- `every_session_registration_first_revokes_pending_deliveries_under_the_lock` pins
  the property the resume rule rests on: dispatch's `register` and notify's `adopt`
  both reserve the workspace and revoke pending deliveries first.

## Decisions

Recorded on SH-772: D1 (adopt the doctor heal and the resume rule), D2 (the
runtime backstop over a wider fence or a type-level wrapper), D3 (landing,
rename and heal skip the pre-write Git evidence), D4 (an inert-kind list, safe
by default), and D5 (the resume rule: council-vote 2f510ebc. All three seats
proposed variants of the same rule, and all three voted for seat 2's version in
both vote rounds. The sitting still aborted, because the chair recorded the ballots
after each deadline, so D5 is the fallback decision taking that proposal).
