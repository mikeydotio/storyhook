# Shared verifier lifecycle — SH-683

The verifier temporarily checks out a speculative merge using private Git
administration and objects. Its ordinary registration stays at a published
commit so sibling Git processes can walk refs and fetch without private
object overrides. Ownership covers that entire transaction.

## Evidence and scope

The v2.4.2 incident left `.git` pointing into `merge-watch-objects.*`, while
Git registered the checkout as `verification-worktree1`. Startup attempted
forced removal and Git refused its inconsistent backlink. Restoring only the
pointer then paired the shared index with speculative files; the private index
showed one real gate edit hidden among apparent checkout differences. The
original basename collision and file truncation causes remain unestablished.

`verify-pr.sh` is the creator and preflight owner; `merge-watch.sh` is the
borrower and speculative executor. Both enter the same lifecycle. Daemon bundle
projection ships the helpers; story-worktree cleanup, browser-watch and
coverage-watch own different paths and do not participate. This policy applies
canonical naming only to `<common>/storyhook/verification-worktree`, never to
arbitrary user worktrees or the generic isolated speculative test checkout.

## Ownership

Order: project `gate` lock → permanent workspace `flock` → existing `merge`
lock. Both first locks precede preflight, fetch, pointer changes and recovery.
The outer acquisition reports gate progress. Nested gate calls retain the
existing reentrant environment contract.

`verifier-owner.py` supervises controlled lifecycle code in a dedicated session.
A pipe handshake prevents execution until its session identity is durably
recorded. A separate supervised session wraps arbitrary gate execution.
The supervisor retains an inherited flock descriptor; session inspection also
finds surviving children that closed that descriptor. It never unlinks the
lock inode. Missing permissions or malformed process evidence fail closed.

Records under `<common>/storyhook/verifier-lifecycle/<worktree-hash>` carry:

| File | Meaning |
|---|---|
| `.lock` | Permanent kernel lock inode |
| `.owner` | Version, exact paths, nonce, boot UUID, supervisor and session identities, gate-launch/completion state, the gate leader's exit code once observed |
| `.state` | Exact workspace/admin/lease mappings, pinned base, allocation phase and pending recovery operations |

Reentrancy requires the exact nonce, paths, boot identity and recorded session.
PID or session reuse can cause a conservative refusal; it never grants an old
owner new authority. Process age is not evidence of abandonment.

Before recovery, acquire the flock and exclude all recorded surviving sessions.
Automatic same-boot recovery requires evidence that arbitrary gate execution
never began or that its supervised session finished. A verified boot change
also proves prior processes cannot survive. The gate supervisor records its
leader's exit code the moment it observes it; a recorded leader exit whose
session census is empty is that "finished" evidence. An interrupted arbitrary
gate — started, no leader exit recorded — with uncertain descendants is
retained and refused even when its leader is gone. Survivors of an exited
leader hold no authority: they are signalled and reaped within the cleanup
grace (SH-695, below). Gates must not daemonize out of their supervised
session. Portable inspection cannot exclude an arbitrary detached descendant
that also closes inherited descriptors; this is an explicit limit, not an
automatic-recovery claim.

## Recovery transitions

`verifier-worktree.py` is the single reader/writer for recovery. JSON intent and
pointer replacement use complete staged files, `fsync`, atomic rename and
parent-directory sync. Every pending move reconciles actual source/destination
presence; conflicting destinations are preserved and refused.

| State | Action |
|---|---|
| Healthy canonical registration | Validate reciprocal pointers, common directory and ordinary ref/reflog resolution; clear disposable inputs before the next attempt |
| Owned suffixed registration, canonical free | Journal and rename administration; replace forward pointer; preserve HEAD, index, refs and reflogs |
| Prior owned canonical registration with a stale duplicate backlink | Retain stale administration, then normalize the unique current reciprocal mapping |
| Valid foreign canonical collision or ambiguous mapping | Refuse and name both mappings |
| Interrupted private allocation | Remove only the recorded unpublished lease after checking the shared pointer |
| Clean recorded private checkout | Restore pinned base with private index/objects; restore shared pointer; check shared state; remove lease |
| Dirty/unreadable private state or obstructed base checkout | Retain checkout, private index/objects and shared administration together; never reset edits |
| Interrupted retention/rename/deletion | Resume recorded operations and verify their postconditions before admission |
| Ownerless legacy private pointer | Preserve it and identify the missing ownership proof; do not attempt forced removal |

Retention directories are `<common>/storyhook/verification-recovery-*`.
Complete retained units contain `worktree/`, `admin/`, optional `lease/`, a
manifest and inspection notes. Backlinks are relocated; HEAD, index, refs,
reflogs and working files are preserved. Stale administration has its own
`stale-admin/` and manifest. Retained objects are never deleted automatically.
A stale format marker alone does not authorize discarding a healthy checkout.

A gate completion record is written only after successful restoration and
cleanup. Retention or uncertain supervision remains an infrastructure failure,
including when the gate command itself exited zero. A receipt cannot turn
failed restoration into a test verdict.

## Clean startup — SH-684

The canonical managed verifier starts each attempt with a clean detached
checkout of the resolved base commit. Startup recovery runs first under the
same lifecycle owner. Tracked or index changes, unreadable owned state, and
unfinished Git operations trigger retention of the complete diagnostic unit
before replacement. This includes accompanying untracked and ignored files.
Replacement is bounded: a newly created checkout that is still damaged fails
admission with its evidence intact instead of repeatedly creating archives.

For healthy tracked state, startup records `startup_cleanup` with the pinned
base in the existing state journal, runs `git clean -ffdx` without exclusions,
and checks out that base. Both tracked comparisons, a dry-run clean including
ignored files and nested repositories, and detached HEAD identity must pass
before the intent is cleared. A deletion or checkout failure leaves the intent
and a contextual diagnostic. Restart revalidates ownership, mappings and tracked
damage before repeating cleanup; the journal never authorizes blind deletion.

Untracked and ignored files in this dedicated checkout are disposable,
including dependency directories, build caches, empty directories and nested
repositories. Symlink entries are removed without following their targets.
Caches may rebuild; routine outputs are not retained as diagnostic archives.
This policy does not apply to generic pollers, foreign worktrees, logs, receipts,
or recovery archives outside the verifier checkout. It does not authorize
cleanup under a live or ambiguous owner. Post-gate recovery retains its existing
contract: tracked damage or obstructed restoration invalidates that attempt.

The unanimous startup-policy council verdict is recorded in `story show SH-684`
(2026-09-11). It chose preservation of damaged units plus cleanup of disposable
extras over archiving every successful run's generated files, which would grow
without a retention mechanism. Git documents these force/ignore semantics in
[git-clean](https://git-scm.com/docs/git-clean).

## Operator recovery

Diagnostics name the exact owner record, checkout and conflicting mappings.
For an ambiguous legacy lease or interrupted arbitrary gate, first establish
writer quiescence outside the verifier. Preserve the complete checkout,
registered administration, private lease and ownership records together before
repairing anything. Do not delete the permanent lock file, run broad worktree
prune, or repair only `.git`. A boot change supplies kernel evidence for a
recorded interrupted owner; unknown legacy identity still needs inspection.
Recovery archives must remain available after a replacement checkout is made.

## Validation

### Cancellation ownership — SH-686

Rust supplies its cleanup grace as `STORYHOOK_VERIFIER_CLEANUP_GRACE_MS`
(normally 30 seconds). The gate session receives one quarter for TERM cleanup,
the lifecycle session one half, the outer machine-lock wrapper three quarters,
and Rust the full budget. Session owners reserve another eighth for bounded
reaping after KILL. The wrapper's explicit `--termination-grace` leaves other
callers' existing policy unchanged. The bundled shell entry requires at least
four seconds so each layer has a positive whole-second wrapper budget.

The outer entry execs its lock wrapper. Python installs signal handlers before
launch admission; handlers only latch cancellation. Supervisors inspect and
signal their recorded sessions, including surviving children in other process
groups, then prove quiescence before marking execution complete. Healthy work
uses nonblocking waitpid without repeatedly scanning the process table. Unknown
or unkillable execution retains its records and inherited ownership lock;
recovery never guesses that it finished. Tracked gate edits retain the existing
archive behavior, including their private Git administration and object lease.

The complete council decision and nested-budget reasoning are recorded on
SH-686. Authority monitoring stops before verifier-owned story transitions;
this process contract also applies to manual cancellation and timeouts.

### Exited-session reaping — SH-695

A supervised leader that exits while its session still has members is not an
ambiguity: the leader has answered, and what survives it is a leak. Both
supervisors send TERM to the session's confirmed members, wait their own
cancellation grace, escalate to KILL at the deadline, and refuse only when
members outlive the reaping eighth. The outer supervisor also signals the
recorded gate session when the gate supervisor is provably gone. One stderr
line names the session, the exit code and the survivors, so the leak stays
visible in the gate log without halting the queue. The gate supervisor records
`gate_leader_exit` at the moment it observes its leader's exit, normal or
cancelled, and clears it with the other gate fields once the session is
settled. Same-boot admission refuses a started gate with no recorded leader
exit as interrupted; a started gate with a recorded exit is admitted once the
census of its recorded session is empty, the same instrument the completed
path uses. A recorded exit without a started gate and its integer session is
refused as an incomplete identity; records written before this change carry no
such field and read as unknown. The cleanup budget is validated before the
record says a gate started, so a refused policy cannot leave an interrupted
gate behind. Where Python offers `waitid` with `WNOWAIT`, the leader's exit is
observed without reaping it, so its pid and session identity stay pinned while
survivors are signalled; older interpreters reap at observation and keep that
reuse window, a stated portable limit. The three `sleep 300` orphans the hook
tests leave on purpose are the motivating case and are unchanged; the decisions
are recorded on SH-695.

`tests/verifier_lifecycle.rs` runs isolated real-Git Python cases, including
production entry points, concurrent preflight, killed owners with surviving
children, a SIGKILL after the actual pointer swap, and crashes at filesystem
retention boundaries. Git behavior is real; fault injection terminates the
process around real filesystem operations. `merge_gate`, `portable_receipt`,
`verifier_bundle`, and `verifier_foreign_checkout` cover gate verdicts, signal
cleanup, object isolation, progress, reentrancy and packaged foreign projects.
The central verifier owns the full suite.

## Decision and references

The native Codex council selected gate-first kernel ownership and durable
recovery intent, with explicit quiescence thresholds, by three first-place
votes after one deliberation. Its complete verdict is recorded on SH-683.

- [Git worktree administration](https://git-scm.com/docs/git-worktree#_details)
- [Git repository layout](https://git-scm.com/docs/gitrepository-layout)
- [Python file locking](https://docs.python.org/3/library/fcntl.html)
- [flock inheritance and last-close semantics](https://man7.org/linux/man-pages/man2/flock.2.html)
