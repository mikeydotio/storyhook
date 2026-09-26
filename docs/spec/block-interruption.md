# Block interruption — SH-690

An effective block interrupts the dispatched agent's current turn using its
provider's native interrupt. It sends no message and retains the session,
claim and worktree. Clearing the block sends the following exact prompt only
when the resulting state is `in-progress`:

> Your story experienced a temporary block, which has been lifted. The environment and dev branch may have changed. Please reread your story, its comments, and its relationships to understand the changes, and adjust your work accordingly. If the change is significant, resetting & rebasing the worktree and restarting the story may be appropriate.

Blocking means an OPEN story with state `blocked`, an awaiting hold, or a
`blocked-by` target whose effective superstate is OPEN. Drafts and obviation
alone are different predicates. Effective epic states use the existing project
projection. Multiple holds produce one block edge; removing only one does not
resume work. Closed/deleted stories never receive a resume prompt.

Service mutation transactions compare the complete project before and after
writing, including dependent stories, and atomically record ordered delivery
intents. Intermediate states in multi-story batches are never delivered.
The daemon consumes these records; ChangeBus supplies wakeups, with bounded
recovery independent of traffic. Delivery outcomes are durable and visible as
story comments. Missing or unmarked panes do not roll back the block.

`story.sh notify` remains the single provider/pane identity boundary. Its
interrupt-only operation captures verified agent-owned gate wrapper identities
before sending Escape, then requests cancellation through the lock holder's
existing bounded cleanup. Protocol-1 gate holders honor a guard during this
interval, including if native cancellation kills the wrapper before its trap.
Captured PID/start evidence stays in the guard. Once every captured process is
quiescent, the controller revalidates and atomically retires only that owned
lock. Failure retains a visible guard; older holders that cannot honor it are
refused with a diagnostic. Helper protocol 5 prevents older helpers from
pasting the interrupt flag as text. Resume binds
to the interrupted session when the block's interrupt acknowledged one, so
replacement panes do not inherit old prompts; otherwise it reaches the story's
registered session through protocol 6's `--registered-session` (SH-772, below).
Tmux cannot acknowledge delivery atomically with SQLite: interrupted deliveries
are reported as uncertain after restart and are never blindly replayed.

The same blocking predicate governs verifier selection and transactional
outcome authority. A candidate also captures the latest durable block revision,
so even a block lifted between observer reads withdraws that attempt.
SH-686 observes authority loss, cancels the active attempt,
retains ownership until cleanup, and advances to the next eligible candidate.
SH-687's separate nested verifier-owner cleanup repair is not duplicated here.

Tests cover durable transaction edges and rollback, rapid/multiple/reopened
blocks, exact prompt and stale identity, missing/unmarked panes, real gate child
termination and lock admission order, and blocked verifier queue progression.
Only new and impacted tests run in this lane; centralized verification owns the
full suite and submission.

The three-member council selected this transaction outbox unanimously. See
SH-690's comments for the durable decision and rationale. Reference:
[transactional outbox](https://docs.aws.amazon.com/prescriptive-guidance/latest/cloud-design-patterns/transactional-outbox.html).

## As-built

The worker's idle passes are reads (SH-693). `recover` and `process_one` in
`src/daemon/block_delivery.rs` each read the deliveries they would act on and
open a write transaction only when one exists; `finish`'s compare-and-swap on
the expected status protects that write against a row that moved between the
read and the lock. The reason is the store's fault model: every fault point
fires inside every commit, an empty transaction included, so a write opened
merely to look kills an armed daemon during its own start-up, before it has
accepted the client command the fault was armed for. SH-690 shipped exactly
that under a gate that was terminated before it reported (SH-692), and `dev`
was red until SH-693 landed. The class detector is
`tests/fault_injection.rs::an_armed_daemon_left_idle_is_not_killed_by_its_own_housekeeping`;
the per-function pins are in `tests/block_delivery.rs`.

### Every door derives, and the funnel refuses one that does not (SH-772)

The paragraph above says every service mutation compares the complete project
before and after. SH-690 enforced that with a source scan, and one door slipped
past it: the verifier's landing (`VerificationQueue::complete_landing_guarded`)
closed the landed story in a plain write, so the in-progress stories it unblocked
never got their Resume. moshtail MT-32 sat idle 37 hours that way. `story doctor
--fix`'s drift heal had the same gap. See `docs/rca/sh-772-landing-bypassed-block-delivery.md`.

- The derivation is one function, `service::block_delivery::derive_block_edges`.
  `Ctx::write_stories` is the ordinary door; the landing, the doctor heal and the
  prefix rename call the function inside their own writes. Its `SubmissionGate`
  says whether a story newly entering `verifying` is checked as a submission:
  those three pass `NotASubmission`, so they skip the four pre-write git calls.
- The derivation keys stories by row number, so it reads a drifted row without
  failing, and it refuses to nest.
- The write funnel (`append_and_fold_maintenance`) refuses any event whose kind is
  not in `domain::BLOCK_INERT_EVENT_KINDS` when the transaction is not inside the
  derivation, and `refold_story` refuses outside it. The inert list is explicit,
  so a new event kind is block-relevant until someone argues otherwise.
- `tests/block_delivery_paths.rs` still lists every writer, now including the
  indirect helpers, and fences the derivation flag's setter and direct row writes.

The resume rule changed with it (council decision D5 on SH-772). A Resume's
authority is its own: it is still Pending while the worker holds the workspace
lock, and every door that registers a session (dispatch `register`, notify
`adopt`) reserves that lock and revokes pending rows first. So the episode's
interrupt now only decides how the session is named:

- the latest interrupt in the Resume's own episode (the rows after the story's
  previous Resume) was Delivered: `--expected-target <target>`, exactly as before;
- anything else (unreached, uncertain, superseded, or no interrupt): helper
  protocol 6's `--registered-session`, which never adopts, types nothing unless the
  composer reads idle (a dialog's `❯ 1. Yes` row reads as text, and Enter would
  approve it), submits only after it sees the prompt, and names the session it
  bound. An acknowledgement that names none stays Uncertain.

Every Unreached or Uncertain Resume records the operator's remedy.
`every_session_registration_first_revokes_pending_deliveries_under_the_lock` in
`tests/block_delivery_paths.rs` pins the revocation property the rule rests on.

### Every resume types only into an idle composer (SH-780)

SH-772 guarded only `--registered-session`. The `--expected-target` resume, and
the verifier's remediation (`notify <id> <message>`), still pasted and pressed
the submit key without looking. A Claude or Codex dialog draws its cursor with
the composer's glyph (`❯ 1. Yes`), so that key approved a permission or a plan
for the person. An interrupt's Escape does not make the screen safe: the agent
may open a dialog later, and a provider may restore the interrupted prompt into
the composer. See `docs/rca/sh-780-notify-submit-on-dialog.md`.

Every form that types a prompt now runs one block in `cmd_notify`:

1. `input_state <pane> strict` must read `empty`: a composer is drawn and holds
   only faint placeholder text. Otherwise `composer-busy`, and nothing is typed.
2. Paste, then `poll_composer_holds`: the composer must show this prompt (its
   first line, or the provider's collapsed-paste placeholder), not just any
   text. Otherwise `delivery-failed`, and no submit key.
3. Revalidate the identity (`pane-changed`).
4. Read `composer_holds` again before the submit key and before each re-send.

The composer reader (`lib/composer.awk`, used by `input_state`) reads
`capture-pane -e`. Faint text is not input: Claude's predicted next prompt and
Codex's placeholder are drawn faint, and read as a draft before. NBSP padding
(Claude's `❯` is followed by U+00A0) reads as a space in every locale. Doubt
resolves to `text`, so storyhook does not type.

`tests/notify_reasons.rs` lists every place the plugin presses a key in a pane
(`KEY_SENDERS`) and pins `cmd_notify`'s single guarded delivery. A remaining
window, not closed: a dialog that opens in the milliseconds between the last
`composer_holds` read and the key. SH-799 applies the same receipt to dispatch
(`send_prompt_confirmed`).
