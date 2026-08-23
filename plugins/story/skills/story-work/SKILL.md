---
name: story-work
description: "Use when starting work on a specific story or picking the next available task. Transitions the story to in-progress, shows full context, and sets up the working session. Call with a story ID to work on a specific story, or with no arguments to auto-pick the next ready story."
---

# Storyhook Work

You are a **thin router**. Deterministic work — story selection, `[plugin].tracking`
resolution, the active-role state transition, and the start comment — lives in
`bash "<story-helper>" work [story-id]`. **Route and render — never call
`story` yourself for the steps below.**

## Resolve packaged files

Take the absolute directory containing this loaded `SKILL.md` as `<skill-dir>`. Resolve
`<plugin-root>` as the normalized absolute path `<skill-dir>/../..`, and resolve
`<story-helper>` as `<plugin-root>/bin/story.sh`. Substitute the absolute path for every
placeholder below and shell-quote it. Never resolve packaged files from the user's current
working directory.

## Steps

### 0. Ensure the storyhook CLI is available

Load `<plugin-root>/references/ensure-cli.md` and follow it. Do not continue until it passes.

### 1. Select and start the story

Run `bash "<story-helper>" work`, passing a story ID as the sole argument
if one was given. `ok:false` → show `display`, stop. If
`reason` is `"claim-conflict"`, another session claimed the story first (or, with no id
given, kept losing the race to every ready story the helper tried) — this is not a
transient error to retry, just show `display` and stop.

`ok:true` with `picked:false` means nothing is ready — show `display` (it already suggests
triage) and stop.

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
