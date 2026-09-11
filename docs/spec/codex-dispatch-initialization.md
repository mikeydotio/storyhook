# Codex dispatch initialization — SH-675

## Problem and scope

In v2.4.2, autonomous Codex dispatch waited for SessionStart before sending any
input. Codex 0.154.0 runs SessionStart only when the first turn starts. Correctly
installed hooks therefore timed out, and rollback could remove a live startup
process's working directory while leaving its tagged pane consuming lane capacity.
The fake terminal published the sentinel at launch and concealed this deadlock.

This fix covers `--auto` and engine-owned `--full-auto` Codex initialization and
pre-charter rollback for either provider. Attended Codex remains screen-gated;
Claude retains its existing sentinel gate. Hook identity protocol 2 is unchanged.

## Handoff contract

1. Create an attempt nonce using the required Python runtime and write a pending
   receipt in the worktree's private Git directory. Pass its path only to this
   launch through `STORYHOOK_CODEX_BOOTSTRAP`.
2. Confirm rendered readiness, the originally captured live pane PID and Codex
   process, then confirm Plan mode before submitting any input.
3. Submit one fixed initialization message authorizing no tools, questions,
   planning or story work. Do not retry an uncertain submission.
4. SessionStart runs the ordinary CLI sentinel writer. The packaged hook verifies
   the pending attempt, canonical cwd, session, Plan-mode turn and exact sentinel
   root/session, then atomically replaces the request with a stopped receipt. It
   returns `continue:false` without ordinary context. Invalid initialization
   evidence still stops the turn, but cannot produce a successful receipt. Stop
   suppresses ordinary handoff generation while the attempt file exists.
5. Require the existing protocol-2 root binding and original process evidence.
   Separately require the matching transcript `task_complete`, with no assistant
   message, tool call, newer turn or malformed transcript. An empty input box alone
   cannot establish completion.
6. Reconfirm an empty input, Plan mode and original live owner; remove the private
   receipt, arm the existing exact-pane approval watcher and submit the charter.
   Subsequent hooks use their ordinary behavior. The original charter and its
   first implementation step remain unchanged.

No provider config is modified. A custom launch command must satisfy the same
observable contract; hook trust alone does not select the correct plugin. A
provider version without the required hook/completion semantics refuses before
receiving the charter. On a missing-hook path the fixed primer may reach the
model, but never contains story instructions or authorizes tools; this is why the
separate successful hook and stopped-turn proof is mandatory.

## Failure ownership

Capture the pane tail and reason before teardown. For a pre-charter refusal,
verify the exact pane/PID, freeze and census its process descendants until the
set stabilizes, kill that pane and remaining captured processes, and confirm
termination before removing only Git resources created by this attempt. A
sibling pane is not a teardown target. Release a new claim only after successful
cleanup; report claim-release failures as still claimed.

An exited/replaced owner, unrecognized process evidence, failed termination or
incomplete Git cleanup preserves the claim and remaining resources with explicit
diagnostics. A failed termination resumes any still-owned frozen processes. A
possibly submitted charter retains the existing conservative handoff behavior;
it is outside pre-charter rollback. Resume preserves pre-existing Git work and
force preserves pre-existing claims.

## Evidence and regression coverage

An isolated Codex 0.154.0 probe used a private CODEX_HOME, fixture repo, owned tmux
session and local model endpoint. No hook ran while idle. At the first submission,
the hook observed `task_started` in Plan mode and returned `continue:false`; the
same turn completed with a null assistant message and no model request. Running
the production dispatch helper against that fixture then completed initialization
and delivered the charter. Model requests were the charter and automatic title
request; the primer made none.

| Boundary | Regression coverage |
| --- | --- |
| First-turn hook timing | Fake invokes the production hook only after initialization submission; exactly one primer and one charter |
| Hook identity | Missing, legacy, malformed and wrong-root sentinels refuse; attended Codex remains usable |
| Attempt and completion | Wrong nonce/root/session/turn/version/phase/path, incomplete or malformed transcript, newer turn, assistant/tool output and non-Plan mode refuse |
| Pending turn | A stopped receipt without task completion submits no charter and is not retried |
| Hook isolation | Initialization Stop emits no handoff; normal provider hooks remain compatible |
| Resource ownership | Native tmux parent/child exit before rollback; unrelated sibling survives |
| Cleanup refusal | Replaced/exited owner, failed pane kill, locked worktree and failed claim release retain accurate state |
| Existing flows | Resume, force/reused claims, launch overrides, lane budget, Auto/Full Auto and undelivered handoff tests |

The impacted-test selector had no coverage map and conservatively returned ALL.
Only new and directly impacted shell/Rust suites were run in this worktree; the
central verifier owns full-suite validation and merge.

Primary implementation references: Codex's
[SessionStart turn handling](https://github.com/openai/codex/blob/main/codex-rs/core/src/session/turn.rs)
and [hook integration tests](https://github.com/openai/codex/blob/main/codex-rs/core/tests/suite/hooks.rs),
and Python's [OS-backed token API](https://docs.python.org/3/library/secrets.html#secrets.token_hex).
The three-seat council's decision, live evidence and approved plan are persisted
as SH-675 comments; the worktree's council files are supplementary.
