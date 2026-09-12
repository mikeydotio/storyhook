# SH-705: Verifier fixes were merged before they were deployed

## Outcome and evidence boundary

On 2026-09-12, SH-705 reported that the running StoryHook v2.4.2 daemon
predated the verifier fixes in SH-691, SH-692, SH-695, and SH-697. By this
investigation, the installed binary matched a source tree containing all four
fixes and the daemon had restarted after the story was filed. This session
did not perform that installation or restart. SH-695's stranded worktree
remained, so this report does not establish completion of SH-705.

The incident chronology below is attributed to SH-705's opening comment,
read with `story show SH-705 --json`. Its original PR timeline, event journal,
and gate receipts were not independently re-audited in this session.

## Reported incident

All times in this report are UTC on 2026-09-12.

| Time | Evidence reported by SH-705 |
|---|---|
| 22:56:48 | PR #796 (SH-695) received RED for rustfmt differences in its new orphan regression. |
| 22:56:49 | PR #793 (SH-692) merged the hand-completion and uncertified-merge protections into `dev`. |
| 22:57:52 | SH-695 committed formatting repair `31777a64c`; it subsequently returned to the verification queue. |
| 22:59:52 | PR #796 was hand-merged at its older head, before the verifier pushed the repair. |
| 23:00:09 | SH-695 was marked done without a GREEN, override reason, or uncertified-merge marker. |
| 23:06:18 | PR #798 (SH-691) merged an equivalent formatting correction, `909fc30a9`. |
| 23:15:39 | SH-705 was filed: the daemon still ran build `ad953f399646`, predating the protections. |

The reported causal chain was a deployment gap: the formatting detector
reported RED, but the hand merge bypassed certification and the running
daemon did not yet contain the newly merged completion protections. The
[SH-692 report](sh-692-hand-merge-under-a-running-gate.md) describes those
protections; the [SH-695 report](sh-695-exited-gate-orphan-halt.md) describes
the survivor-cleanup fix whose PR carried the formatting error.

## Independently checked state

These observations were rechecked during implementation of the approved
plan, after that plan was posted verbatim on SH-705.

| Check | Result |
|---|---|
| `story --version` | `story 2.4.2 (build b0e0f32f1305)` |
| Source mapping | Commit `db827a0224a02e2f666fa0c095b4c29f20f0a304` has tree `b0e0f32f130571e5f5f14260f383bd00369775a4`. |
| `story daemon status` | v2.4.2, PID 46830, no binary-mismatch warning; login agent runs `/Users/mikey/.local/bin/story`. |
| Daemon metadata | `started_at=2026-09-12T23:22:35Z`; executable path matches the login agent. |
| Executable identity | Recorded `exe_mtime=1789255326` equals the installed executable's modification time. |
| SH-695 worktree | `.claude/worktrees/SH-695` remains registered on `worktree-SH-695` at `31777a64cb500aa538ba7a47adbcb14214f0a111`; `git status --short` is empty. |
| Formatting equivalence | Both `31777a64c` and `909fc30a9`, restricted to `tests/merge_gate.rs`, have stable patch ID `93ff0a3993cb00f5ad44bc5b908fa98593a32590`. |

The build stamp identifies tracked source content, not a commit ID or a gate
receipt. See [the tracked-tree identity contract](../../scripts/tracked-tree.sh)
and [build stamping](../../build.rs). The daemon status check uses version,
executable path, and modification time through `DaemonInfo::is_this_binary`
in [lifecycle.rs](../../src/daemon/lifecycle.rs). It does not independently
hash the running process. These checks support the deployment conclusion
under the existing identity contract; they do not prove a gate ran, certify
PR #796 retroactively, or identify who installed the new binary.

Each ancestry check used `git merge-base --is-ancestor <merge> db827a022`
and exited zero:

| Required story | Included merge | Existing regression coverage |
|---|---|---|
| SH-691 | `0dbb70b5d` / PR #798 | [merge_gate.rs](../../tests/merge_gate.rs): wrong-base submissions are rejected before running a gate. |
| SH-692 | `e62c2b68d` / PR #793 | [verification_override.rs](../../tests/verification_override.rs): hand completion requires a reason; [merge_gate.rs](../../tests/merge_gate.rs): terminated gates do not become test verdicts. |
| SH-695 | `c0e6e5626` / PR #796 | [test_verifier_lifecycle.py](../../scripts/tests/test_verifier_lifecycle.py): exited-session survivor cleanup; [merge_gate.rs](../../tests/merge_gate.rs): lingering-orphan regression. |
| SH-697 | `5b46c59d9` / PR #795 | [battery_completion.rs](../../tests/battery_completion.rs): battery completion policy. |

These are source-coverage references, not claims that this documentation
session reran those tests or exercised the production daemon's mutation doors.

## Review and remaining delivery

The resumed obviation review returned five in-progress candidates. Their
descriptions, comments, relationships, and available linked evidence were
compared with SH-705:

| Candidate | Requirement distinct from SH-705 |
|---|---|
| SH-698 | Reliable lifecycle-harness waits under load. |
| SH-699 | Isolation and retirement of test-created tmux windows. |
| SH-702 | Preserve gate verdicts when subsequent cleanup fails. |
| SH-703 | Visibility and control of halted verification queues. |
| SH-707 | Provider-compatible output from external hooks. |

None establishes that SH-695's remaining cleanup is complete. Patch
equivalence establishes that its formatting correction landed; it does not
make the stranded commit an ancestor or authorize deleting its branch.

The approved decision is to preserve the deployment and both worktrees, make
no runtime change, and retain SH-705 as open and blocked for the required
SH-695 cleanup outside this lane. The session expressly prohibits worktree
cleanup and release/deployment operations; the central verifier owns normal
lane cleanup. This stranded, unmerged-commit case needs an authorized
cleanup owner to resolve it without losing work. No separate story is filed,
and this report alone is not submitted as completed implementation.

No receipt, GREEN verdict, or override reason is created retroactively.
Optional GitHub ruleset changes remain outside the approved scope. Validation
results and the report's commit identifier are recorded on SH-705.
