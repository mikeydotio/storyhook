# Codex autonomous prose plan continuation — SH-676

## Defect and evidence

Some autonomous Codex sessions end with a completed implementation plan and a
prose request for “Approve” or “Approved”. No question tool runs and no native
plan-review menu appears. The existing PreToolUse denial and exact-menu watcher
therefore have nothing to intercept. The built-in autonomous charter reinforced
this failure by saying that the user approves the plan once.

Local Codex 0.154.0 rollouts from September 10–11, 2026 establish two sightings:

| Story | Turn mode | Prose request (UTC) | Human approval (UTC) |
|---|---|---|---|
| SH-672 | Default | September 11, 05:51:15 | 05:55:14 |
| SH-673 | Default | September 11, 05:52:53 | 05:54:42 |

Both followed a retry in Default mode. This proves the failing approval boundary;
it does not establish why every intermittent dispatch reaches that mode. The
regressions retain sanitized approval patterns, without copying private rollouts.

## Behavior

The autonomous charter now says StoryHook approves the implementation plan
automatically. Investigation and presentation still precede implementation, and
the exact approved plan must be the first implementation comment. Attended
dispatch and explicit custom prompts retain their existing behavior.

A synchronous Codex Stop command supplements the native menu watcher. It accepts
only a root Stop with a valid STORYHOOK_AUTO or STORYHOOK_FULL_AUTO story ID,
explicit stop_hook_active=false, and matching session, turn, cwd, and mode in the
provider transcript. The authoritative tracker must report an active, open,
unblocked story with no awaiting request. It checks identity and state again
after classification. Submitted, blocked, attended, and subagent sessions do not
receive continuation. Complete proposed_plan envelopes remain with the watcher.

GPT-5.6 Luna classifies the complete assistant message into approve_plan, other,
or uncertain with a strict JSON schema. Only a concrete implementation plan
explicitly awaiting approval qualifies. Completion, substantive choices,
operational permissions, quoted requests, and ambiguous output do not. Positive
evidence must be an exact nonempty substring of the assistant message. The model
never supplies the continuation instructions or commands.

| Current mode | Native Stop response |
|---|---|
| Default | Block stopping; give Codex a fixed continuation approving this plan and requiring its verbatim story comment first. |
| Plan | Block stopping; require the same plan inside proposed_plan tags. Explicitly preserve the prohibition on implementation until the native plan-review transition. |

Stop feedback creates a native continuation prompt; it does not itself change
collaboration mode. The existing exact-pane watcher still accepts the Plan-mode
review UI. No free-text approval is typed into a terminal.

An owned, no-symlink, flock-protected receipt next to the provider transcript
allows at most one prose continuation per root session, including concurrent
delivery and later turns. The receipt is fsynced before returning the decision.
An interrupted delivery therefore favors at-most-once authorization over retry.
Unknown receipt contents suppress continuation. This also prevents repeated
Stop feedback from becoming an approval loop.

## Classifier isolation and compatibility

The three-seat council unanimously chose bounded codex exec, conditional on
demonstrated tool isolation. Its verdict and follow-up findings are persisted as
SH-676 comments; the worktree-local .council trail is supplementary.

The child uses the existing Codex login, a fresh /tmp cwd, an ephemeral session,
ignored user configuration, zero project-document budget, disabled skill
instructions, a fixed classification instruction file, and a restricted public
Luna model catalog. It disables hooks, shell, apply_patch, agents, image tools,
browser, apps, plugins, web search, memory, goals, and question/plan tools. Parent
autonomy markers and session/plugin variables are excluded from its environment.
No authentication tokens are read or copied by StoryHook.

The real wire probe found that disabling shell and multi_agent alone was
insufficient: deferred subagent tools and code-mode tools remained. Explicit
agents.enabled=false plus normal tool mode, disabled shell, no apply_patch, and
disabled integrations produces **zero tools** in the outgoing request, including
additional_tools. Tool or error events in classifier output invalidate the answer.

Codex independently preserves global user AGENTS instructions. Those user-owned
constraints remain; project rules and skill catalogs do not reach classification.
The fixed classifier instruction and absence of capabilities prevent repository
execution. The model catalog contains public metadata, not credentials.

The capability contract is measured for **codex-cli 0.154.0**. Other versions emit
a diagnostic and receive no prose approval until their capability contract is
measured and the compatibility guard updated. This is deliberate: undocumented
catalog switches must not silently gain authority across a runtime upgrade.
The native menu watcher remains available independently.

## Bounds and failure behavior

| Bound | Limit |
|---|---|
| Complete assistant message | 64 KiB; reject oversized messages without truncating |
| Transcript tail / input payload | 4 MiB |
| Tracker lookup | 2-second CLI deadline, 3-second process deadline; three checks before and after classification |
| Codex version / classifier | 2 / 20 seconds |
| Returned child output | 1 MiB per stream |
| Stop hook | 50 seconds, against at most 40 seconds of subprocess waits |

Each child owns a process group, killed on completion, timeout, or parent SIGTERM.
Missing executables/authentication, changed schemas or runtime, malformed data,
unknown modes, excessive output, and uncertain classifications never manufacture
approval. They emit a contextual systemMessage. A project without an unambiguous
active state role cannot use the prose fallback. Tracker snapshots are not a
transaction with the provider; the final recheck narrows that race without
claiming atomic cross-process authorization.

## Validation

- test-codex-stop.sh exercises the production decision and runner, including
  observed prose patterns, mode boundaries, identity, story state, repeat events,
  malformed data, classifier refusal, and real child-process cleanup.
- probe-codex-stop.sh is an opt-in, loopback-only provider test. The installed
  Codex, production hooks, classifier runner, and isolated tracker are real; only
  model response data is supplied by an HTTP fixture. It asserts native
  continuation and the actual outgoing tool inventory and instruction isolation.
- eval_plan_classifier.py is an opt-in live Luna evaluation of 11 sanitized
  positive/negative cases. All passed on September 11, 2026, taking 2.9–5.9
  seconds per classification. This sample is behavioral evidence, not a guarantee
  of perfect classification; runtime guards do not depend on the model's judgment.
- Existing dispatch, provider, full-auto, plan-watcher, and hook-budget tests cover
  adjacent behavior. Run the impacted-test selector on the actual changed tree;
  its missing-coverage-map ALL fallback belongs to the centralized verifier.

## Primary references

- [Codex hook protocol](https://learn.chatgpt.com/docs/hooks): Stop payload,
  blocking feedback, recursive Stop marker, and command hook execution.
- [Codex source, rust-v0.154.0](https://github.com/openai/codex/tree/rust-v0.154.0):
  core tool configuration, collaboration modes, instruction loading, and exec
  output protocol. Local app-server schemas generated by the same installed
  runtime establish the Plan-mode probe's request contract.
