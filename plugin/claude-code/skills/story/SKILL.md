---
name: story
description: "Storyhook story lifecycle toolkit. `/story do <id>` dispatches a ready story to a fresh plan-mode Claude session in a new tmux window + per-story git worktree, refusing if the story isn't ready; `/story new <desc>` interrogates you then files a story; `/story view <id>` prints a story and stops; `/story complete <id>` closes it and safely cleans up merged branches + worktrees; `/story <id>` views it then offers to work on it; bare `/story` lists ready stories to pick from. `/story work [id]`, `/story context`, `/story setup`, `/story sync`, `/story handoff`, `/story triage`, `/story update`, `/story plan <spec>`, and `/story install` delegate to their own dedicated skills unchanged. Use whenever the user wants to file, view, start, or wrap up a storyhook story, or manage the storyhook plugin/project itself. Deterministic work lives in bin/story.sh; requires the story CLI (and tmux for `do`, `capture`, and `doctor`)."
user-invocable: true
allowed-tools: Bash(story *), Bash(command -v *), Bash(which *), Bash(cargo *), Bash(curl *), Bash(uname *), Bash(bash ${CLAUDE_PLUGIN_ROOT}/bin/story.sh *), Read, Write, Grep, Glob, AskUserQuestion
argument-hint: "<do <id> | new <desc> | view <id> | complete <id> | <id> | work [id] | context | setup | sync | handoff | triage | update | plan <spec> | install | doctor | capture <id>>"
---

# Story — storyhook story lifecycle router

You are a **thin router**. Parse `ARGUMENTS` (everything after `/story`) — the first token
is the verb.

Deterministic work lives in `bash ${CLAUDE_PLUGIN_ROOT}/bin/story.sh <subcommand> …`.
**Route and render — never call `story`, `git`, or `tmux` yourself for those verbs.**

**Universal rule:** every `story.sh` run returns exactly one JSON object. If `ok` is `false`,
show the `display` string and **stop**. On `ok:true`, show `display` (plus any `warning` /
`pane_tail`).

The remaining verbs delegate to an existing, unchanged skill: **read that skill's `SKILL.md`
and follow its instructions directly** (the same "router reads the target skill's file and
dispatches inline" pattern `forge:forge` uses in this marketplace family) — do not re-derive
its behavior from memory, and do not skip steps it defines.

## Dispatch

| ARGUMENTS | Verb | Action |
|-----------|------|--------|
| _(empty)_ | List → Pick | **List → Pick** flow below. |
| `<id>` (e.g. `SH-45`) | View + Offer | **View + Offer** flow below. A bare first token is a story id only if it matches `^[A-Za-z0-9]+-[0-9]+$`; otherwise it is a malformed verb. |
| `do <id>` | Dispatch to a fresh session | **Dispatch** flow below. |
| `view <id>` | Show a story | Run `story.sh view <id>`, show `display`, stop. |
| `new <description>` | File a story | Read `references/story-new.md` and follow it. |
| `complete <id>` | Close + clean up | Read `references/story-complete.md` and follow it. |
| `capture <id>` | Peek at a dispatched session | Run `story.sh capture <id>`, show `display`, stop. Read-only. |
| `doctor` | Self-test | Run `story.sh doctor`, show `display`. Checks project data integrity **and** whether this Claude build's readiness/paste path is still recognised. |
| `work [id]` | Start work in this session | Read `skills/story-work/SKILL.md` and follow it, passing `[id]` through. |
| `context` | Show project state | Read `skills/story-context/SKILL.md` and follow it. |
| `setup` | Initialize/configure | Read `skills/story-setup/SKILL.md` and follow it. |
| `sync [--since <d>]` | Sync git history | Read `skills/story-sync/SKILL.md` and follow it, passing the flag through. |
| `handoff [--since <d>]` | End-of-session handoff | Read `skills/story-handoff/SKILL.md` and follow it, passing the flag through. |
| `triage` | Review the backlog | Read `skills/story-triage/SKILL.md` and follow it. |
| `update` | Update the CLI | Read `skills/story-update/SKILL.md` and follow it. |
| `plan <spec-file>` \| `plan "<description>"` | Decompose into stories | Read `skills/story-plan/SKILL.md` and follow it, passing the argument through. |
| `install` | Install the CLI | Read `skills/story-install/SKILL.md` and follow it. |
| anything else | Malformed | Say one line: "Usage: `/story <do <id> \| new <desc> \| view <id> \| complete <id> \| <id>>`", then stop. |

## List → Pick (bare `/story`)

1. Run `bash ${CLAUDE_PLUGIN_ROOT}/bin/story.sh list`. `ok:false` → show `display`, stop.
2. If `count` is `0` → show `display` and **stop**. Do **not** open an `AskUserQuestion`.
3. Otherwise ask **exactly one** `AskUserQuestion`, `header: "Which story"`. The tool allows
   only 2–4 options, so tier by `count` (rows arrive highest-priority first, each carrying a
   pre-built `option` object):

   | `count` | Presentation |
   |---|---|
   | 1 | the single `option`, plus `{label: "Other", description: "Enter a different story id"}` |
   | 2–4 | every row's `option` object, in order |
   | >4 | first print the full list as plain text (`<id> — <title>`), then ask with the **first 3** options (the tool adds "Other" itself) |

4. Map the answer back to an id — prefer matching the chosen option to its `stories[]` entry
   and reading `.id`. For a free-form answer, use it as the id. If it doesn't look like a
   story id, re-run List → Pick.
5. Run **View + Offer** on that id.

## View + Offer (`/story <id>`)

1. Run `bash ${CLAUDE_PLUGIN_ROOT}/bin/story.sh view <id>`. `ok:false` → show `display`, stop.
2. Show `display`.
3. Ask **exactly one** `AskUserQuestion`, `header: "Story <id>"`, question "Work on story
   `<id>` now?", options **Yes** / **No**.
4. **No** → stop. **Yes** → run the **Dispatch** flow with `<id>`.

## Dispatch (the `do` flow)

All side-effecting work (tmux, git worktrees, the readiness gate, the storyhook claim) lives
in `bin/story.sh`.

1. Run `bash ${CLAUDE_PLUGIN_ROOT}/bin/story.sh dispatch <id>`.
2. Render:
   - `ok:false` → show `display`, stop. Common causes: the story doesn't exist, it's closed,
     it's already `in-progress`, or it isn't in a **ready** state (superstate not OPEN,
     `awaiting` set, `obviated-by`'d, or blocked by another still-open story) — `display`
     names the specific reason. `/story do` must refuse rather than dispatch a story that
     isn't actionable.
   - `ok:true` → show `display`. If a `warning` field is present, surface it too — the tmux
     window opened but the handoff (claude readiness and/or prompt submission) couldn't be
     fully confirmed, so the user should glance at the new window. Include a `pane_tail` if
     present, fenced, as diagnostic evidence.

On success, the helper has already: confirmed the story is ready and not already claimed,
claimed it via storyhook's `--if-state` compare-and-swap (conflicting rather than erroring if
another dispatch won the race), fetched `origin/<default>` and created a fresh git worktree
(`.claude/worktrees/<repo-prefix>-<id>`, e.g. `sto-SH-45`, on branch
`worktree-<repo-prefix>-<id>`) based on that tip, then opened a new tmux window rooted **in**
that worktree, running `claude --permission-mode plan --model opusplan` in plan mode, prompt
already submitted. Nothing further is needed from you.

## Notes

- **`do`, `capture`, and `doctor` require tmux** (the helper hard-fails otherwise, unless
  `STORY_TARGET_SESSION` is set for a non-interactive caller); all verbs need the `story` CLI
  on `PATH`.
- **Every verb is anchored to the main worktree's tracker.** `.storyhook/` is
  version-controlled, so a dispatched worktree carries its own copy; the helper always reads
  and writes the main repo's, so `view`/`list`/`complete` can't disagree with `do`.
- **GitHub-adjacent conventions from `/issue` don't apply here** — storyhook stories aren't
  GitHub issues. There's no label to apply and no `Closes #N` convention; the claim marker is
  the story's own `state`, and the handoff prompt tells the child session to reference the
  story ID in its PR body and post its plan back via `story comment <id> "..."`.
- `STORY_DRY_RUN=1` previews the side-effecting verbs without touching anything (used by the
  test suite); you generally won't need it interactively.
- The launch command and handoff prompt are overridable via `STORY_LAUNCH_CMD`/`STORY_PROMPT`,
  and the state `complete` closes into via `STORY_DONE_STATE` (see `bin/story.sh`'s config
  block) for advanced/non-interactive callers. **The helper owns these — don't rewrite them
  here.**
