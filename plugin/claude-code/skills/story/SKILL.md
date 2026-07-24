---
name: story
description: "Thin router for the storyhook workflow skills. `/story do <id>` dispatches a ready story to a fresh plan-mode Claude session in a new tmux window + per-story git worktree, refusing if the story isn't in a ready state; `/story work [id]`, `/story context`, `/story setup`, `/story sync`, `/story handoff`, `/story triage`, `/story update`, `/story plan <spec>`, and `/story install` delegate to their own dedicated skills unchanged. Bare `/story` shows current project context. Use this whenever the user wants to work a storyhook story, dispatch one to a background session, or manage the storyhook plugin/project itself."
user-invocable: true
allowed-tools: Bash(story *), Bash(command -v *), Bash(which *), Bash(cargo *), Bash(curl *), Bash(uname *), Bash(bash ${CLAUDE_PLUGIN_ROOT}/bin/story.sh *), Read, Write, AskUserQuestion
argument-hint: "<do <id> | work [id] | context | setup | sync [--since <d>] | handoff [--since <d>] | triage | update | plan <spec-file>|\"description\"> | install>"
---

# Story — storyhook workflow router

You are a **thin router**. Parse `ARGUMENTS` (everything after `/story`) — the first token
is the verb. Every verb except `do` delegates to an existing, unchanged skill: **read that
skill's `SKILL.md` and follow its instructions directly** (the same "router reads the target
skill's file and dispatches inline" pattern `forge:forge` uses in this same marketplace
family) — do not re-derive its behavior from memory, and do not skip steps it defines.

## Dispatch

| ARGUMENTS | Verb | Action |
|-----------|------|--------|
| `do <id>` | Dispatch to a fresh session | **Dispatch** flow below with `<id>`. |
| `work [id]` | Start work in this session | Read `skills/story-work/SKILL.md` and follow it, passing `[id]` through. |
| `context` | Show project state | Read `skills/story-context/SKILL.md` and follow it. |
| `setup` | Initialize/configure | Read `skills/story-setup/SKILL.md` and follow it. |
| `sync [--since <d>]` | Sync git history | Read `skills/story-sync/SKILL.md` and follow it, passing the flag through. |
| `handoff [--since <d>]` | End-of-session handoff | Read `skills/story-handoff/SKILL.md` and follow it, passing the flag through. |
| `triage` | Review the backlog | Read `skills/story-triage/SKILL.md` and follow it. |
| `update` | Update the CLI | Read `skills/story-update/SKILL.md` and follow it. |
| `plan <spec-file>` \| `plan "<description>"` | Decompose into stories | Read `skills/story-plan/SKILL.md` and follow it, passing the argument through. |
| `install` | Install the CLI | Read `skills/story-install/SKILL.md` and follow it. |
| _(empty)_ | Session-start default | Read `skills/story-context/SKILL.md` and follow it — matches the existing "start a session with `story load-context`" convention. |
| anything else | Malformed | Say one line: "Usage: `/story <do <id> \| work [id] \| context \| setup \| sync \| handoff \| triage \| update \| plan <spec> \| install>`", then stop. |

## Dispatch (the `do` flow)

`do` is the one verb with its **own** deterministic implementation — it doesn't delegate to
a subskill. All side-effecting work (tmux, git worktrees, the readiness gate, the storyhook
claim) lives in `bin/story.sh`, which emits **one JSON object** with `ok` + `display`. **Route
and render — never call `story`, `git`, or `tmux` yourself for this verb.**

1. Run `bash ${CLAUDE_PLUGIN_ROOT}/bin/story.sh dispatch <id>`.
2. Render:
   - `ok:false` → show `display`, stop. Common causes: the story doesn't exist, it's closed,
     it's already `in-progress`, or it isn't in a **ready** state (superstate not OPEN,
     `awaiting` set, `obviated-by`'d, or blocked by another still-open story) — `display`
     names the specific reason. This is issue #40's core requirement: `/story do` must refuse
     rather than dispatch a story that isn't actionable.
   - `ok:true` → show `display`. If a `warning` field is present, surface it too — the tmux
     window opened but the handoff (claude readiness and/or prompt submission) couldn't be
     fully confirmed, so the user should glance at the new window. Include a `pane_tail` if
     present, fenced, as diagnostic evidence.

On success, the helper has already: confirmed the story is ready and not already claimed,
claimed it via storyhook's `--if-state` compare-and-swap (`story move <id> in-progress
--if-state <state>` — conflicting rather than erroring if another dispatch won the race),
fetched `origin/<default>` and created a fresh git worktree (`.claude/worktrees/<repo-
prefix>-<id>`, e.g. `sto-STO-7`, on branch `worktree-<repo-prefix>-<id>`) based on that tip,
then opened a new tmux window rooted **in** that worktree, running `claude --permission-mode
plan --model opusplan` in plan mode, prompt already submitted. Nothing further is needed from
you.

## Notes

- **`do` requires tmux** (the helper hard-fails otherwise, unless `STORY_TARGET_SESSION` is
  set for a non-interactive caller); all verbs need the `story` CLI on `PATH`.
- **GitHub-adjacent conventions from `/issue do` don't apply here** — storyhook stories aren't
  GitHub issues. There's no label to apply and no `Closes #N` convention; the claim marker is
  the story's own `state`, and the handoff prompt tells the child session to reference the
  story ID in its PR body and post its plan back via `story comment <id> "..."`.
- **A `complete`/teardown verb (worktree + branch cleanup once a `/story do` session's PR
  merges) is intentionally not part of this skill yet.** `bin/story.sh` is structured so it's a
  same-file addition later; file a follow-up story once `do` is in use, since dispatched
  worktrees otherwise accumulate with no cleanup path.
- `STORY_DRY_RUN=1` previews `dispatch` without side effects (used by the test suite); you
  generally won't need it interactively.
- The launch command and handoff prompt are overridable via `STORY_LAUNCH_CMD`/`STORY_PROMPT`
  (see `bin/story.sh`'s own config block) for advanced/non-interactive callers; the defaults
  are what a normal `/story do` invocation uses.
