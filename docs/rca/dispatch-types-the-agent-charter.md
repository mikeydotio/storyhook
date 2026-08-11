# Dispatch typed the agent charter into a bare shell, and the shell executed it

- **Date**: 2026-08-10 (failure 2026-08-09)
- **Severity/Impact**: Four stories across two projects (SH-178, SH-170; CAL-31, CAL-33) moved
  to `done` with no work done — no commit, no PR, no comment. 21 minutes of exposure. State
  corruption only: no code, history or store damage, and every affected branch carried zero
  commits not already on `origin/main`. Undetected for roughly a day.
- **Status**: Fixed in `c110b5e`, `46995ff`, `542cf66` (PR #230). Redesign escalated as SH-227.

## Summary

`story.sh dispatch` starts Claude by *typing* a launch command into a fresh tmux pane, then
waits for that pane to become a Claude TUI before typing the agent charter into it. The wait
decided "Claude is ready" from evidence a shell prompt supplies for free — a frame rule, an
idle prompt glyph, and three byte-identical captures. When a launch failed, the pane settled
into a bare shell that satisfied all three in about 0.75 seconds, and the charter was typed
into zsh, which executed its backticked spans — including `story move <n> done`. The fix makes
readiness require a fact from the process table, makes the verdict actually gate the send, and
makes the charter text structurally inert to a shell. **The lesson: a readiness check that
reads rendered characters is not evidence that a process is running, and an unattended
destructive action must be gated by something that cannot be faked by the failure mode it is
supposed to detect.**

## Timeline

| When | What | Anchor |
|---|---|---|
| 2026-07-23 | The two-tier readiness gate is vendored in, forked verbatim from the agentics `issue` plugin. All 56 lines unchanged from this commit until the fix | `03f8e63` |
| 2026-07-23 | The dispatch actuator lands with steps 11-13 and the fail-open `else` present in its introducing diff | `ed4f23d` |
| 2026-07-28 | `--auto` and its charter arrive, adding the `story move <n> done` span. The payload is now armed, but still requires a human to type `--auto` | `1e53c02` |
| 2026-08-08 | `?auto=1` is threaded from the dispatch endpoint to the script's argv. First route by which an **unobserved** caller can send the autonomous charter | `6bca007` |
| 2026-08-09 20:49-21:10Z | Four dispatches from the dashboard. oh-my-zsh's periodic update check is due in newly-spawned panes; its `[Y/n]` prompt eats the launch keystroke's leading `c`. Four stories close | — |
| 2026-08-10 | Noticed. Stories reopened, worktrees cleaned, the progress log corrected | PR #229 |
| 2026-08-10 | RCA: reproduced, forensics, three falsification experiments, diagnosis, fix | SH-226, PR #230 |

## Root cause & trigger

**Defect** — no stage of the dispatch path held the invariant *"nothing is typed into a pane
until Claude is confirmed to be the process in it, and a dispatch that cannot confirm it
fails."* Six sites each assumed another held it:

1. `lib/session.sh` Tier 2 accepted two glyphs plus byte-stability as proof of a Claude TUI.
2. `bin/story.sh` step 12 computed a readiness verdict and then ignored it — the `else` branch
   only slept, and step 13 ran unconditionally.
3. `bin/story.sh` hardcoded `ok: true` once the CAS claim succeeded; an unconfirmed handoff
   downgraded prose into a `warning` and never rolled the claim back — though every *earlier*
   failure path in the same function does roll back.
4. `send_prompt_confirmed` pressed Enter **before** consulting whether the paste had been
   received, so its `false` return could not distinguish "never sent" from "sent blind".
5. `input_box_text` reads the text after the last prompt glyph — at a shell prompt that *is*
   the pasted charter, so the two-phase receipt confirmation was satisfiable by a shell.
6. `prompt_accepted` tests `READY_ACCEPT_PATTERN`, the only Claude-specific discriminator in
   the codebase, but ORs it with a fallback an idle glyph satisfies — and its caller records
   the result without acting on it.

**Infection** — the system's belief that a Claude session existed was never grounded in any
fact about a process.

**Failure** — an autonomous instruction document carrying live shell syntax was delivered to
whatever was in the pane, executed by it, and reported as success.

Sites 1 and 2 are an **AND**, established by toggle: disabling Tier 2 alone left the charter
still submitted; gating step 13 alone changed nothing (the gate affirms in this scenario);
both together stopped it. Site 3 is independent and survived a fix to both.

**ODC**: Function/Class, **Missing**, triggered on **startup/restart via the error path**
(secondary: configuration — the daemon passes no `PATH` and the launchd plist sets none, so
"the binary is not resolvable" is a live production shape). Secondary type **Interface**: the
result JSON meant "the story was claimed" to one caller and "the handoff worked" to the other.

**Why it fired then.** The trigger was environmental and time-boxed, not a code change: every
dispatch in a 21-minute window failed and none outside it did, with nothing deployed between
them. oh-my-zsh's update check became due in newly-spawned panes. The defect had been reachable
since 2026-07-23; `git bisect` against the dashboard `--auto` boundary returned `good_is_bad`,
proving that commit only widened *who* could reach it.

## Contributing factors

Everything below had to align. Removing any one of them makes the incident smaller.

- **The launch is typed, not executed.** A pane's occupant is whatever is listening; a
  keystroke can be eaten by anything that grabs the terminal first.
- **The charter carries live shell syntax.** Its backticks are markdown-ish emphasis to a
  reader and command substitution to a shell. The attended template has the same leak and is
  harmless — its two spans are read-only. Only the `--auto` template carries mutating verbs.
- **Nobody was watching.** From a terminal, the operator sees `command not found: laude` on
  their own screen and intervenes. From the dashboard, the only notice was a **green success
  toast** lasting 4.5 seconds — because `classify()` reads `ok` alone, and `warning` and
  `pane_tail` have no reader anywhere in `src/`.
- **The heuristic is inverted with respect to its own intent.** An idle prompt is *more* stable
  than a live TUI, so the failure case satisfied the stabilisation requirement faster than
  success would have.
- **Nothing tested the tier.** The fake tmux had carried a `structural` fixture since the
  harness was written, and no committed test ever drove it. The fake also documented
  "must NOT confirm" obligations for its `modal` and `busy` fixtures that nothing enforced.

## The fix

Three commits on `rca-fix/dispatch-types-the-agent-charter` (PR #230), each green:

- **`c110b5e`** — the fake tmux learns to model the pane's occupant (derived from the launch
  line, so every existing test keeps passing untouched), a mangled launch, and a failing
  `send-keys`. No production file touched. Without this the fix would have shipped with a test
  that could not fail.
- **`46995ff`** — `wait_ready` requires the pane's foreground command to match
  `READY_PROCESS_PATTERN` before either tier can succeed; step 12 gates on the verdict; the
  Enter is pressed only after receipt; `ok:true` stops being a literal.
- **`542cf66`** — both charter templates are made structurally inert to a shell, enforced by a
  lint.

**Why this is the origin and not the encounter point.** The check lives inside the predicate
whose own contract already claimed to establish that Claude was ready — putting it in the
caller would have left `wait_ready` still asserting something it does not test, which is the
exact shape of the defect. No Rust or dashboard change was needed: `classify()` already maps a
parseable `ok:false` to `Refused` and the dashboard already renders that red.

**One scope correction is worth recording,** because it inverts the obvious fix. Rolling the
claim back on *any* unconfirmed handoff is wrong: if readiness was confirmed and only
*submission* was not, the charter may already be in front of a live agent, and releasing the
story returns it to the ready list for a second dispatch to claim — two agents, one story.
Only the `undelivered` leg rolls back, and that leg is provably safe **only because** Enter is
no longer pressed before receipt.

**Verdict: REDESIGN**, with the narrow patch landing now. Readiness by screen-scrape is not a
contract — `READY_PATTERN` already carries four alternatives because the footer keeps changing.
The redesign is **SH-227**: execute the launch instead of typing it, replace the screen-scrape
with an artifact Claude itself publishes, and read one result contract end to end. It is
modelled on this project's own `await_healthy`, which demands a published artifact, an identity
check, a functional round trip and `child.try_wait()`, and treats timeout as a hard error.

**Tech debt logged** (deliberate and prudent): the patch keeps a content-matching heuristic as
part of readiness, plus a *name*-matching process check that cannot distinguish a non-Claude
process named `claude`/`node`, nor a Claude session that is running but wedged. **Redesign
trigger:** the next time `READY_PATTERN` needs a new alternative, the first false-positive
report under the process check, or the first operator who must set
`STORY_READY_PROCESS_PATTERN` to dispatch at all.

## Preventative action — killing the class

Four guards, all landed:

1. **`tests/test-dispatch-occupant-gate.sh`** — five regression families derived from the ODC
   trigger rather than from the one field scenario. Family A drives the failure by its *cause*
   (a mangled launch), not a hand-built fixture. Family C finally enforces the fake's own
   documented "must NOT confirm" obligations. Family D is the first coverage the
   receipt/submission distinction has ever had.
2. **`tests/test-charter-inert.sh`** — the lint that kills the *class*. It asserts the rendered
   prompt carries no character special to any POSIX-family shell, so the next person who writes
   a backtick or a semicolon into the charter learns it at `make test` rather than in
   production. It also asserts the instructions survive, so inertness can never be bought by
   deleting them.
3. **The fake tmux models process identity**, so any future readiness work has something real
   to assert against.
4. **A fail-closed gate that ships with its own escape hatch.** Every refusal prints
   `STORY_READY_PROCESS_PATTERN`, and `story doctor` reports the occupant it actually observed.

## Lessons

- **Rendered characters are not evidence that a process is running.** A TUI's footer is a
  rendering detail, not a contract. This codebase already knew that on the daemon side — the
  gap was that the shell side never adopted it.
- **A predicate should establish what its name claims.** `wait_ready`'s doc said "poll until
  Claude's TUI is ready" and its comment glossed `launch_gone` as "(claude started)". Neither
  was tested by either tier. A comment asserting a fact the code does not check is a defect
  waiting for the conditions to arrive.
- **An unattended destructive verb needs a gate, not prose.** The codebase already states this
  about itself: `cmd_reap`'s preflight comment says *"the charter's own prose telling it 'never
  reap past a hard stop' is backed by a gate that does not trust the prose alone."* The
  dispatch path is the same shape of risk and had no such gate. During this very investigation,
  an agent handed the charter as evidence executed its spans against an explicit instruction
  not to — the same lesson, at a different layer, within the same week.
- **Text that instructs an agent will eventually be read by something that executes.** Its
  inertness must be structural, not an accident of where a reserved word happens to fall in a
  sentence — which is exactly what it was here, and it differed by shell.
- **Absence of confirmation is not success.** Every stage on this path had that default, and
  the reporting layer flattened the distinction to a green toast.
