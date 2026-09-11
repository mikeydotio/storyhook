# Codex dispatch adapter

Load this adapter only when the active agent host is Codex. The shared `story` skill has
already resolved the installed plugin root and its absolute `<story-helper>` path.

## Dispatch (`do <id> [--auto] [--force] [--resume] [--agent=claude|codex]`)

Run `bash "<story-helper>" dispatch <id> --agent=<agent>`, adding `--auto`, `--force`, or
`--resume` only when the user requested them. Use the user's explicit agent when present; otherwise use
`--agent=codex`. `--force` reuses a named story's existing claim without
another state transition; it does not override any worktree, branch, tmux, or provider
safety gate.

When the first result is `ok:false` with `reason:"resume-available"` and the user did not
already pass `--resume`, show `display` and `resources`, then ask exactly once whether to
resume the preserved work or leave it untouched. Yes reruns the identical command with
`--resume`; no stops. Never ask for any other refusal. Resume preserves the existing branch,
dirty worktree, and commits while rebuilding only missing resources; it can replace an
abandoned story-pane occupant but refuses the current pane and inconsistent identities.

For a typed epic, the same helper invocation has a separate boundary: `--auto` starts a
daemon-owned Full Auto run scoped to the epic and returns `kind:"engine-run"`; it does not
claim the epic or launch a Codex pane directly. Show `display` and stop. Without `--auto`, the
helper refuses and names the engine remedy. Never add the engine-private `--full-auto` lane
marker here.

- `ok:false`: except for the one `resume-available` interaction above, show `display` and stop. The helper refuses before story-charter delivery when the
  story, worktree, Codex process, readiness proof, or Plan-mode footer is unsafe.
- `ok:true`: show `display` verbatim. Surface `warning` and a fenced `pane_tail` when present.

The helper owns the compare-and-swap claim, fresh base, `.codex/worktrees/<id>` worktree,
tmux window, `codex --no-alt-screen` launch, readiness, Shift+Tab transition into Plan mode,
bracketed paste, and Tab submission. Do not repeat those side effects.

With `--auto`, the helper adds `--approve-for-me` and
`--dangerously-bypass-hook-trust`, and gives the child
`STORYHOOK_AUTO=<story-id>`. After confirming Plan mode, it arms a watcher bound
to the original live pane PID. The watcher sends Return only when Codex's exact
three-option plan review is visible with “Yes, implement this plan” selected;
it retries bounded tmux/TUI failures and completes only after a settled capture
confirms that dialog disappeared. Automatic workspace-write review handles
later tool approvals; the trusted packaged hook refuses
`request_user_input` so the unattended session cannot wait for a person. A
custom `STORY_LAUNCH_CMD` remains wholesale and is reported as potentially
weakening that guarantee. Autonomous dispatch first confirms the original live
Codex process and Plan mode, then submits one task-free initialization turn:
Codex invokes SessionStart only when its first turn begins. The matching packaged
hook returns `continue:false`, suppressing ordinary context and stopping that turn
before model work. A private attempt receipt binds the hook root, session and turn;
the transcript must separately confirm completion with no assistant/tool work.
Only then does the helper arm the watcher and deliver the story charter once.

The protocol-2 SessionStart sentinel must still name the exact package that owns
the helper. Missing, legacy, malformed, or mismatched hooks, an incomplete stopped
turn, or incompatible launch overrides refuse before the charter. The refusal
includes the initialization phase, reason and captured pane tail. Before rolling
back new Git resources and the claim, the helper terminates the exact owned startup
pane and its descendants. Uncertain ownership or cleanup preserves resources and
reports the failure. Existing work and claims remain preserved on resume/force.
Attended Codex continues to use screen readiness. See
[the initialization contract](../../../docs/spec/codex-dispatch-initialization.md)
for compatibility evidence and the failure matrix.

Codex can surface a Claude-compatible `ExitPlanMode` hook event, but a bare
`permissionDecision: "allow"` is unsupported and is not its plan-approval
boundary. The hook recognizes Codex's required `turn_id` field and emits no
decision; the exact-pane watcher approves the review UI. Storyhook's built-in
Codex prompts therefore require the plan presented for approval to make posting
that exact plan to the story its first implementation step. Custom `STORY_PROMPT`,
`STORY_AUTO_PROMPT`, and `STORY_AUTO_PROMPT_SOLO` values are wholesale overrides
and must carry any equivalent requirement themselves; `STORY_PROMPT_EXTRA` still
appends after the built-in requirement.

SH-676 supplements that menu with a synchronous, autonomous-only Codex Stop hook.
A bounded, tool-free Luna classifier recognizes completed plans awaiting prose
approval. In Default mode it supplies native continuation; in Plan mode it asks
for the same plan in `proposed_plan` tags so the existing menu watcher can approve.
Session/story checks and a persistent receipt allow at most one prose continuation.
The classifier capability contract is currently verified only for Codex 0.154.0;
other runtimes emit a diagnostic and retain the native menu path. See
[the design and probe contract](../../../docs/spec/codex-auto-plan-continuation.md).

SH-687 adds an explicit handoff for built-in autonomous Default-mode sessions:

```json
{"type":"storyhook.implementation-plan","version":1,"story_id":"SH-123","plan":"Complete implementation plan text"}
```

The entire assistant message must be this object, with exactly these fields and
the assigned story ID. Valid requests bypass Luna. Malformed, embedded, quoted,
unknown-version and mismatched-story requests do not fall back to classification.
The approved content is the decoded `plan` text; posting it verbatim remains the
first implementation step. The same receipt limits structured and prose paths
together. Native Plan mode continues to use `proposed_plan`; JSON there redirects
to native review without authorizing implementation. A typed transactional
`story session-eligibility <id> --json` query supplies tracker facts before and
after recognition. Schema validation records readiness, not proof of plan
completeness or extra operational permission. Attended and custom prompts are
unchanged; ordinary prose keeps its bounded classifier fallback.

Codex has no stable machine-readable skill inventory. In `--auto`, council discovery
therefore defaults to the safe solo charter; `STORY_COUNCIL=on` is the explicit opt-in.

## Capture and doctor

- `capture <id>`: run `bash "<story-helper>" capture <id>` and show `display`. The stable
  launcher runs the helper as Codex, so no `STORY_AGENT` prefix is needed — and neither the
  installed-artifact guard nor Codex's own command rule admits one.
- `doctor`: run `bash "<story-helper>" doctor` and show `display`. It reports
  the selected provider and independently confirms readiness, Plan mode, bracketed paste,
  and Storyhook project integrity.

## Release (`unclaim <id>`, `reset <id>`)

Reached as `$story unclaim <id>` and `$story reset <id>`. Run
`bash "<story-helper>" unclaim <id>` or `bash "<story-helper>" reset <id>`, adding `--force`
(reset only) and `--comment <text>` / `--no-comment` only when the user asked for them. Show
`display` and stop. Both close the story's tmux window, which is why they are documented per
host rather than only in the shared router.

- `unclaim` leaves the `.codex/worktrees/<id>` worktree and its branch exactly as found;
  `reset` deletes both.
- From the story's own window, `unclaim` still releases the claim and reports that it left the
  window open, while `reset` refuses with `self-window` and `--force` does not override it.

## Hooks and trust

Codex discovers this installed plugin's `hooks/hooks.json`. The SessionStart,
PreToolUse, PostToolUse(Bash), and Stop hook protocol is shared with Claude Code. A locally
installed, non-managed plugin may require explicit trust/review in Codex before its hooks run.
Trust permits hooks to execute; it does not bind an independently resolved helper to the
plugin version Codex loaded. The SessionStart identity sentinel supplies that binding for
autonomous Codex dispatch.

The autonomous entries are implemented by `hooks/full-auto.sh`. They are inert unless
`STORYHOOK_AUTO` or `STORYHOOK_FULL_AUTO` is set. In an autonomous session they approve
Claude's plan tool call and refuse the provider's question-asking tool, handing the model an
instruction to decide or convene a council instead of waiting for a person who is not there.
Dispatch arms both providers' exact-gated plan-review watchers before prompt submission.
Codex's arm was measured live rather than assumed (SH-459, CLI 0.149.0): a matcher named
`request_user_input` runs before the question UI, and `permissionDecisionReason` is returned to
the model as the blocking reason. On both hosts a PreToolUse hook fails OPEN at its timeout, so
a lane whose denial times out asks anyway and stalls — caught by the engine's stall ceiling and
quarantined, never silent.
