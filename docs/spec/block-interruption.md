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
to the interrupted session, so replacement panes do not inherit old prompts.
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
