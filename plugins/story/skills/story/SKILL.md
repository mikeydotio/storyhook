---
name: story
description: "Use for the complete Storyhook lifecycle: list or view stories, file a new story, start ready work, complete work safely, inspect context, plan, triage, sync, hand off, install, update, or configure Storyhook. Routes deterministic operations through the packaged helper and delegates focused workflows to sibling Storyhook skills."
---

# Storyhook lifecycle router

Infer the requested verb and arguments from the user's request or direct skill invocation. Do
not assume the host supplies a separate argument variable.

## Resolve packaged files and helper

Take the absolute directory containing this loaded `SKILL.md` as `<skill-dir>` and resolve
`<plugin-root>` as the normalized absolute path `<skill-dir>/../..`. Load
`<plugin-root>/references/helper-command.md` and follow it to resolve `<story-helper>`.
Substitute absolute paths for every placeholder below and shell-quote them. Never resolve
packaged files from the user's current working directory.

Deterministic work lives in `bash "<story-helper>" <subcommand> …`. **Route and render —
never call `story`, `git`, or `tmux` yourself for helper-backed verbs.** Every helper run
returns exactly one JSON object. If `ok` is `false`, show `display` and stop. On `ok:true`,
show `display` plus any `warning` or `pane_tail`.

When this router delegates, load the sibling file by its absolute installed path under
`<plugin-root>/skills/<skill-name>/SKILL.md` and follow it directly. Hosts may display an
installed name with a namespace such as `story:story-context`; the packaged file path is the
authority, so do not guess a displayed name or re-derive the workflow from memory.

## Routes

| User intent or verb | Action |
|---|---|
| No operation supplied | Run **List → Pick** below. |
| A story id such as `SH-45` | Run **View + Offer** below. A bare token is an id only if it matches `^[A-Za-z0-9]+-[0-9]+$`. |
| `do <id> [--auto] [--force] [--agent=claude|codex]` | Run **Provider dispatch** below. `--force` is only for a named story already in the project's active-role state (`in-progress` unless the project moved the role); it reuses that claim without writing another transition. |
| `view <id>` | Run `bash "<story-helper>" view <id>`, show `display`, stop. |
| `new <description>` | Load `<plugin-root>/references/story-new.md` and follow it. |
| `complete <id>` | Load `<plugin-root>/references/story-complete.md` and follow it. |
| `capture <id>` | Run **Provider dispatch** below in capture mode. |
| `doctor` | Run **Provider dispatch** below in doctor mode. |
| `claim <id>` or `claim --next` | Run **Claim** below. One of the two is required; a bare `claim` is refused rather than resolved to `--next`. |
| `context [--full]` | Load `<plugin-root>/skills/story-context/SKILL.md` and pass the flag through. |
| `setup` | Load `<plugin-root>/skills/story-setup/SKILL.md`. |
| `sync [--since <duration>]` | Load `<plugin-root>/skills/story-sync/SKILL.md` and pass the flag through. |
| `handoff [--since <duration>]` | Load `<plugin-root>/skills/story-handoff/SKILL.md` and pass the flag through. |
| `triage` | Load `<plugin-root>/skills/story-triage/SKILL.md`. |
| `update` | Load `<plugin-root>/skills/story-update/SKILL.md`. |
| `plan <file-or-description>` | Load `<plugin-root>/skills/story-plan/SKILL.md` and pass the input through. |
| `install` | Load `<plugin-root>/skills/story-install/SKILL.md`. |
| Anything else | Show a one-line usage summary of the supported routes, then stop. |

## List → Pick

1. Run `bash "<story-helper>" list`. If `ok` is `false`, show `display` and stop.
2. If `count` is `0`, show `display` and stop without asking a question.
3. Otherwise show the ready stories in their returned order and ask exactly one concise
   question for the story to inspect. Use the host's structured question mechanism when
   available. For a long list, keep all ids visible in plain text even if the structured UI
   presents only the first few as shortcuts.
4. Map the answer back to an id, using a returned `stories[]` entry when possible. If a
   free-form answer is not a story id, repeat this flow rather than guessing.
5. Run **View + Offer** on that id.

## View + Offer

1. Run `bash "<story-helper>" view <id>`. If `ok` is `false`, show `display` and stop.
2. Show `display`.
3. Ask exactly one concise question: whether to work on that story now. Use the host's
   structured question mechanism when available.
4. If the answer is no, stop. If yes, run **Provider dispatch** for the id.

## Claim

Claiming is the one operation this router runs against the `story` CLI directly rather than
through the helper: `story claim` is a single atomic invocation with nothing to orchestrate,
so wrapping it would only add a second JSON shape to keep in step. This is the same reason
`story-triage`'s resolution commands are direct CLI calls.

1. Load `<plugin-root>/references/ensure-cli.md` and follow it. Do not continue until it passes.
2. Run `story claim <id>` for a named story, or `story claim --next` to take whichever story
   `story next` would answer with. Exactly one of the two — never both, and never neither.
   Pass through `--phase <N>` (with `--next` only), `--comment <text>` or `--no-comment`, and
   `--dry-run` when the user asked for them.
3. A conflict — another session claimed the story first — is reported as `result:"conflict"`
   with `.actual` naming the state found, and exit code 9. Show it and stop; it is not a
   transient error to retry.
4. `--next` with nothing ready answers `no ready stories`. That is an answer, not a failure:
   show it, suggest `triage`, and stop.
5. On success the command renders the claimed story in full — the state it came out of, then
   title, priority, labels, state, comments, and relationships.

### Present working context

Show that rendering. Then summarize what the story is about and what needs to be done. If the
story has child stories, list them. If it has dependencies that are already done, note what was
completed. Proceed with the implementation work from there.

This synthesis is yours to write. The claim answers with facts; what they *mean* is judgment,
and no command can assert it for you.

## Provider dispatch

Dispatch, capture, and doctor depend on terminal behavior that differs by agent host. Load
the matching file from `<plugin-root>/adapters/` and follow it. For dispatch, an explicit
`--agent=claude|codex` selects that provider even when it differs from the active host; without
one, the adapter supplies its own host as the default. If no adapter exists for the
active host, explain that the provider-specific operation is unavailable and suggest `claim <id>` for
safe in-session work. Never invoke a different provider's adapter or report dispatch success
without one.

## Shared notes

- Every helper-backed verb requires the `story` CLI on `PATH`; provider dispatch may also
  require tmux.
- The CLI decides which project a verb acts on. It resolves an explicit `--project`, then
  `STORYHOOK_PROJECT`, then the nearest committed `.storyhook.toml`, then the repository's
  registered origin. Do not infer project identity in prose.
- `STORY_DRY_RUN=1` previews side-effecting helper verbs for tests and advanced callers.
- `do <id> --force` bypasses only the already-claimed refusal. It does not bypass
  worktree, branch, tmux, provider-readiness, or prompt-delivery safety checks, and it cannot
  be combined with the helper-only `dispatch --next` mode.
- Storyhook stories are not GitHub issues. Do not invent issue-label or `Closes #N`
  conventions; use story ids and Storyhook relationships.
