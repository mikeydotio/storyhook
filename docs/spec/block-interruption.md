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

### Probe bounds under contention (SH-766)

The interrupt helper (`plugins/story/lib/interrupt-agent.py`) and the two
cleanup helpers that load `stop-dispatch-pane.py` read the process table with
`ps`. Every `ps` or `tmux` call used to have a bare 5 s bound. Under gate load
(load average 300 to 800 on 10 cores) a `ps` spawn takes more than 5 s, so the
interruption contract test went red on trees with no defect, and real interrupts
ended Unreached ("could not bind the dispatched session", SH-772).
See `docs/rca/sh-766-plugin-probe-bounds-under-load.md`.

- **A captured process is judged by its kernel identity.** `alive(pid, identity)`
  compares only the native start token (`process_identity.py`: libproc on macOS,
  `/proc` on Linux). No census, no spawn. An exited or zombie process raises
  `ProcessLookupError`, so exit is told apart from a failed probe. `target()`,
  `signal_known`, every wait and every `finally` use it. The census stays only
  for descendant discovery and for the `ps` `lstart` text that the gate lock
  protocol compares with `machine-lock.sh`'s `started` file. A `finally` that
  resumes a frozen tree therefore cannot fail on the census that failed the
  operation.
- **One budget for each helper operation.** `probe_budget.py`: every helper
  entry point (`stop-dispatch-pane`, `interrupt-agent`, `dropped-cleanup-pane`,
  `agent_identity`, `tmux-env`) runs as one `operation()` of `BUDGET_SECONDS`
  (30 s), and each probe gets what remains. That is two thirds of the tightest
  caller bound: `NOTIFY_TIMEOUT` and `CLEANUP_HELPER_TIMEOUT`, both 45 s, and the
  second kills without SIGTERM. Unit tests beside those constants pin the
  relation. A timeout raises `ProbeTimeout`, a `TimeoutExpired` that names the
  probe, its allowance, the budget spent and the load average.
- **Why not a load multiplier.** Spawn latency is not proportional to load per
  core (SH-643: 250 times the latency at 5 times the contention). A multiplied
  inner bound would also cross the callers' fixed bounds, and the dropped-cleanup
  kill could then land while processes are frozen. Why not a caller deadline in
  the environment: it would reach long-lived descendants (tmux sessions, agent
  shells) unless every launch removed it (the SH-758 class).
- **The interrupt's TERM wait** (`GATE_TERM_GRACE`, 5 s) is policy, not a probe
  bound. It is how long the gate owner's own TERM trap gets before this helper
  freezes and kills the captured tree. Escalation is safe, only less gentle.
- **Fenced.** `tests/timing_assertions.rs` refuses a bare `timeout=` or
  `monotonic() +` literal in any `plugins/story/lib/*.py`. The one exemption is
  `continuation_runtime.py` (SH-798: its resume runs a 120 s dispatch inside a
  125 s bound), and it fails when that file no longer needs the exemption.
- **Fixtures are not stricter than production.** `tests/support/block_interrupt.py`
  and `test-agent-identity.py` give a command the notify bound (`NOTIFY_TIMEOUT` +
  `NOTIFY_TERM_GRACE`). `test-dispatch-pane-readiness.sh` gives the fake pane
  `DISPATCH_TIMEOUT`. Each is graced by contention with
  `scripts/tests/load_grace.py` `patience()` (SH-347: up to 15 minutes, reported
  whenever it applies).
