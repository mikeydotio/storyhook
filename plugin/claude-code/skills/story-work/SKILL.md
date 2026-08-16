---
name: story-work
description: "Use when starting work on a specific story or picking the next available task. Transitions the story to in-progress, shows full context, and sets up the working session. Call with a story ID to work on a specific story, or with no arguments to auto-pick the next ready story."
user-invocable: true
allowed-tools: Bash(story *), Bash(command -v *), Bash(bash ${CLAUDE_PLUGIN_ROOT}/bin/story.sh *), AskUserQuestion
argument-hint: "[STORY-ID]"
---

# Storyhook Work

You are a **thin router**. Deterministic work — story selection, `[plugin].tracking`
resolution, the active-role state transition, and the start comment — lives in
`bash ${CLAUDE_PLUGIN_ROOT}/bin/story.sh work [story-id]`. **Route and render — never call
`story` yourself for the steps below.**

## Steps

### 0. Ensure the storyhook CLI is available

Follow `${CLAUDE_PLUGIN_ROOT}/references/ensure-cli.md`. Do not continue until it passes.

### 1. Select and start the story

Run `bash ${CLAUDE_PLUGIN_ROOT}/bin/story.sh work`, passing a story ID as the sole argument
if one was given (e.g., `/story-work SH-3`). `ok:false` → show `display`, stop. If
`reason` is `"claim-conflict"`, another session claimed the story first (or, with no id
given, kept losing the race to every ready story the helper tried) — this is not a
transient error to retry, just show `display` and stop.

`ok:true` with `picked:false` means nothing is ready — show `display` (it already suggests
`/story-triage`) and stop.

`ok:true` with `picked:true` means the helper has already: resolved the story (the one
named, or the highest-priority ready one), read the `[plugin].tracking` setting from the
committed `.storyhook.toml`, moved the story into whichever state carries the project's
`active` role (`in-progress` unless the project defines a different one), and — unless
tracking is `quiet` — posted a "Starting work on this story" comment. `moved`/`commented`
report what actually happened; if `moved` is `false`, a `note` on `display` explains why
(the state transition itself failed) — surface it, the story was NOT claimed.

### 2. Present working context

Show `display` — it is the story's own full rendering (title, priority, labels, state,
comments, relationships). Then summarize what the story is about and what needs to be
done. If the story has child stories, list them. If it has dependencies that are already
done, note what was completed. The agent should now proceed with the actual implementation
work.

This synthesis is yours to write — the facts it is drawn from all came from step 1.
