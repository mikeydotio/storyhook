# Codex autonomous plan approval could lose its only continuation

- **Date**: 2026-09-05
- **Severity/Impact**: One of eight measured Codex 0.153.2 autonomous sessions stopped at
  plan review until a person supplied approval 22.471 seconds later. The failure did not
  corrupt data, but an unattended lane could remain stalled indefinitely.
- **Status**: Fixed in `4133420a2` and `546c24671`

## Summary

StoryHook's asynchronous plan watcher treated a tmux operation result as proof that Codex had
left its plan-review dialog. A transient observation failure, rejected Return, or Return
accepted by tmux but not consumed by the provider could therefore terminate the only
continuation while the original Codex process still awaited approval. The fix binds the
watcher to the dispatch-owned pane PID, bounds transport and input retries, and completes only
after a settled capture acknowledges that the exact dialog disappeared. The same defect class
was present in the Claude sibling and was corrected separately.

## Timeline

| Date | Event | Anchor |
|---|---|---|
| 2026-08-29 | Autonomous provider plan approval introduced the asynchronous watcher, immediate exit after a capture failure, and unacknowledged Return completion | `03c62c64e` |
| 2026-09-04 | Provider scheduling was unified and Claude received the original pane PID; Codex retained its one-shot observation and send contract | `ae7df15c9` |
| 2026-09-05 | SH-568 stopped at Codex plan review; manual approval resumed implementation after 22.471 seconds, while seven same-version peers advanced in 0.085-0.972 seconds | SH-568 |
| 2026-09-05 | SH-570 reproduced the transient-capture manifestation in 20 of 20 runs and verified independent capture-retry and transition-acknowledgement toggles | SH-570 |
| 2026-09-05 | Codex PID binding, bounded retries, and provider-transition acknowledgement landed with the focused regression matrix | `4133420a2` |
| 2026-09-05 | The adopted Claude sibling received the same acknowledgement contract and its own provider regression matrix | `546c24671` |

## Root cause & trigger

The verified defect→infection→failure chain was:

1. **Defect:** `plugins/story/hooks/full-auto.sh::approve_codex_plan` exited after one failed
   capture, swallowed a failed `send-keys`, and exited after any accepted Return without
   observing the provider transition. `plugins/story/bin/story.sh::schedule_plan_approval`
   already owned the original pane PID but did not pass it to the Codex watcher.
2. **Infection:** the sole asynchronous continuation disappeared while the original Codex
   process remained at, or later reached, the exact plan-review dialog.
3. **Failure:** option 1 received no effective Return, so the autonomous story waited until a
   person supplied approval.

The transient-capture test reproduced the failure in 20 of 20 runs. Changing only capture
failure termination to retry moved the test failure→pass→failure. A second controlled
experiment made tmux accept and absorb the first Return; changing only immediate completion to
settled re-observation again moved failure→pass→failure. Official Codex 0.153.2 source and
snapshots matched StoryHook's exact prompt predicate, which ruled out prompt drift for the
reported cohort.

**ODC classification:** **Timing / Serialization**, qualifier **Missing**, triggered by the
recovery/error path plus provider UI transition timing. The necessary trigger conditions were
autonomous dispatch, the exact plan dialog, a still-live original pane, an unavailable or
ineffective first observation/send, and no transition acknowledgement. SH-568 retained no
watcher diagnostic, so the defect class is verified with high confidence but its particular
capture, send, absorbed-send, or child-start manifestation cannot be identified retrospectively.

## Contributing factors

- `tmux run-shell -b` acknowledges job acceptance, not watcher lifetime or completion.
- A successful `tmux send-keys` reports that tmux accepted input, not that the provider TUI
  consumed it during a state transition.
- Retry logic followed only successful nonmatching captures; operation errors bypassed it.
- Static hook tests covered an immediately readable prompt but did not inject transport
  failures, absorbed input, pane replacement, exhaustion, or concurrent watchers.
- The watcher emitted no durable outcome evidence, limiting exact attribution after SH-568.
- Claude had a pane-identity check but shared the same unacknowledged Return-completion
  assumption, exposing a direct sibling of the defect class.
- **Residual risks:** asynchronous watcher-child startup has no acknowledgement or durable
  diagnostic; duplicate watchers for one pane and PID are not serialized; future prompt drift
  deliberately fails closed and requires an updated exact matcher.

## The fix

The verdict was **SURGICAL** because the defect was localized to the provider watcher and
scheduler lifetime contract; no public protocol or stored data changed.

- **`4133420a2`** passes Codex the pane PID captured by dispatch. Before every observation and
  input attempt, the watcher requires the same live PID. It retries up to three consecutive
  observation failures and at most three Return attempts, settles after each accepted or
  rejected send, and completes only when a subsequent capture shows the exact dialog has left.
  Dead, replaced, malformed, or unknown pane identity remains fail-closed.
- **`546c24671`** applies the same origin-level correction to Claude in a separate behavior
  commit and regression test. This adopted sibling preserves two-hat commit separation while
  removing the same latent continuation-loss contract from the only other provider watcher.

The change fixes the origin instead of adding a timeout at the stalled prompt: the continuation
now lives for the original process lifetime and requires provider-state acknowledgement before
it can declare success.

## Preventative action — killing the class

- **`plugins/story/tests/test-codex-plan-approval-resilience.sh`** is the executable Codex
  contract. It covers transient and exhausted identity/capture failures, rejected and absorbed
  Returns, transition acknowledgement, dead or replaced panes before and after send, malformed
  arguments, exact-prompt fail-closed behavior, and independent concurrent pane budgets.
- **`plugins/story/tests/test-claude-plan-approval-resilience.sh`** independently applies the
  same matrix to Claude, preventing the provider sibling from retaining an equivalent defect.
- Provider dispatch tests require the already-captured pane PID in each watcher command, and
  the watcher comments document the invariant: a Return is complete only after a settled
  capture observes the exact dialog leave the same original live process.

These guards make transport success insufficient as a completion signal and make any future
change that drops identity, bounded recovery, or transition acknowledgement fail locally.

## Lessons

- Asynchronous UI automation must acknowledge the state transition it intends to cause;
  process launch, transport acceptance, and input delivery are separate facts.
- Pane IDs are reusable locators, not process identity. Long-lived watchers must carry and
  revalidate the dispatch-owned pane PID immediately before observation and input.
- Retry budgets belong to independent failure domains: polling for a long-running plan remains
  unbounded in production, while transport failures and Return attempts must be bounded.
- Exact prompt matching is a safety boundary. Unknown provider UI should remain inert even
  when fail-closed behavior means a future prompt change needs explicit maintenance.
