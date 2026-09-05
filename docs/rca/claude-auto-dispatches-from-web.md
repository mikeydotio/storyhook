# Managed Claude dispatch omitted the plugin that publishes its readiness witness

- **Date**: 2026-09-05
- **Severity/Impact**: Dashboard dispatch could not deliver work to Claude when StoryHook's
  Claude plugin was not registered globally. Every built-in attended, Auto, and Full Auto
  Claude launch in that configuration waited for readiness, returned `pane-not-ready` /
  `no-sentinel`, and withheld the prompt. The fail-closed path removed its owned Git resources
  and restored the story to `todo`, so no story data, code, or history was lost.
- **Status**: Fixed in `a01af0b2b`

## Summary

StoryHook could resolve its development dispatch helper without proving that Claude had loaded
the plugin containing that helper's SessionStart hook. The built-in Claude launch command did
not activate the plugin explicitly, so an environment without ambient registration had no
producer for the sentinel that StoryHook requires before prompt delivery. The fix derives the
helper's own plugin root and passes it to every managed Claude launch with `--plugin-dir`, while
leaving expert overrides and Codex unchanged. The repository-side chain is deterministic and
toggle-verified; confidence remains **MEDIUM** because no live Claude session exercised the
provider boundary and the original field dispatch record had aged out.

## Timeline

| When | What | Anchor |
|---|---|---|
| 2026-08-04 | Dashboard dispatch gained a resolver fallback from an absent installed Claude plugin to a protocol-compatible development helper. Helper availability and provider activation became independent facts | `eb61f4d1ad` |
| 2026-08-10 | Dispatch readiness changed from rendered-screen inspection to a sentinel published by StoryHook's plugin-owned SessionStart hook. Built-in Claude launch arguments still did not activate that plugin explicitly | `87aaf45db` |
| 2026-08-22 | The shared `plugins/story` tree began serving Claude and Codex, and the development fallback moved to its helper | `ef976a12b`, `5f67c4942d` |
| 2026-08-30 | Built-in Claude variants converged on `compose_claude_launch_tpl`; the centralized command still omitted plugin activation | `fb86420ac` |
| 2026-09-04 | Sentinel polling and diagnostics were hardened, but a hook that was never loaded still had no producer path | `22d5eba30` |
| 2026-09-05 | Missing ambient Claude registration exposed the latent dependency. The activation-sensitive regression reproduced `pane-not-ready` / `no-sentinel`, claim rollback, and no prompt delivery | SH-564 |
| 2026-09-05 | A one-variable activation experiment produced fail → pass → fail. Competing latency/write and process-identity explanations were refuted for the reproduced case | SH-564 RCA |
| 2026-09-05 | Managed Claude launch composition began binding the helper-owned plugin root explicitly; the new and directly impacted tests passed | `a01af0b2b` |

No culprit commit was mechanically established. Two historical bisect attempts were
inconclusive because the modern regression harness could not operate across the retired
repository and store mechanics. `87aaf45db` is therefore recorded as the adjacent observable
boundary that made the plugin artifact mandatory, not as a bisect-proven culprit.

## Root cause & trigger

The verified defect → infection → failure chain was:

1. **Defect — missing launch initialization.** The daemon could resolve a development helper
   after installed-provider lookup missed, while every built-in Claude mode converged on a
   launch composer that supplied no plugin path.
2. **Infection — the readiness producer was absent.** Without ambient registration, Claude
   could start without loading `plugins/story/hooks/hooks.json`. Its SessionStart command
   therefore could not publish `.claude/dispatch-sentinel.json` in the dispatch worktree.
3. **Failure — readiness correctly failed closed.** The live, correctly named pane remained
   `no-sentinel` until the poll budget expired. Dispatch returned `pane-not-ready`, removed its
   owned Git resources, rolled the claim back to `todo`, and never typed the prompt.

The causal link was tested at StoryHook's launch boundary by changing only exact plugin
activation: the baseline failed, an otherwise equivalent launch with the exact plugin root
passed, and removing the binding failed again. Pane identity, timing, helper, and readiness
mechanics remained fixed. Delayed-sentinel coverage passed separately, and the same process
model passed when activation changed, refuting latency/write failure and pane-process mismatch
as explanations for this reproduction. This models the provider boundary rather than launching
a live Claude model session.

**ODC classification:** **Assignment/Init / Missing**, triggered by **Configuration**. The
managed subprocess was initialized without a required provider argument; missing ambient
Claude registration exposed the omission.

## Contributing factors

- The resolver established that a protocol-compatible helper existed, not that the provider
  had activated the plugin owning the helper's required hooks.
- Ambient global registration historically supplied the missing dependency and concealed the
  launch contract gap.
- The shared tmux fake published a sentinel for any Claude-shaped launch, whether or not that
  launch activated StoryHook. Existing happy-path tests therefore encoded the symptom away.
- Exact launch-template tests pinned the old command without a plugin argument, reinforcing
  the ambient-registration dependency instead of detecting it.
- Provider-hook tests invoked manifest commands directly and resolver tests stopped after path
  selection. No test crossed helper resolution, provider activation, SessionStart, and
  sentinel readiness as one causal path.
- Hook latency and sentinel-write failures can produce the same `no-sentinel` result. Existing
  fail-closed behavior still handles those separate failure classes, but diagnostics alone
  could not identify missing activation.
- The original dispatch record aged out, and the investigation did not run a live Claude
  session. Those limits cap confidence at **MEDIUM** despite deterministic repository evidence
  and the installed CLI's documented `--plugin-dir` contract.

## The fix

Commit **`a01af0b2b`** applies a **SURGICAL** correction at the launch creator:

- `plugins/story/bin/story.sh` derives `STORY_PLUGIN_ROOT` from its already-absolute
  `SELF_PATH`, so later directory changes and ambient provider variables cannot redirect it.
- A focused quoting helper represents that root as one POSIX shell word, including paths with
  whitespace or apostrophes.
- `compose_claude_launch_tpl` adds `--plugin-dir <exact helper-owned root>` before the existing
  permission, model, effort, speed, and settings arguments. The shared composer covers built-in
  attended, Auto, Full Auto, selector, initial, and resume/respawn launch shapes.
- `STORY_LAUNCH_CMD` and `STORY_FULL_AUTO_LAUNCH_CMD` remain wholesale operator-owned commands.
  Codex behavior, readiness, daemon resolution, dispatch protocol, API, storage, deadlines,
  rollback, and sentinel production remain unchanged.

This addresses the origin rather than weakening the encounter point: the managed process now
receives the integration that produces its required readiness witness. The correction stays at
the single launch-composition convergence point, changes no cross-component interface, and is
locally reversible, which supports the **SURGICAL** verdict despite the helper's broader
hotspot history.

## Preventative action — killing the class

The fix lands an activation-sensitive guard and updates every directly affected launch
contract:

1. **`plugins/story/tests/test-dispatch-plugin-binding.sh`** models absent ambient
   registration. It requires the exact helper-owned root for success and proves a missing or
   wrong root produces `no-sentinel` with claim rollback.
2. **`plugins/story/tests/fakes/plugin-binding/tmux`** publishes the modeled SessionStart
   witness only for an exact binding. A Claude-shaped process alone can no longer satisfy the
   regression.
3. **`test-packaged-path-resolution.sh`** relocates the plugin beneath a version-like path with
   whitespace and an apostrophe, proving the copied helper binds its own copied root as one
   argument.
4. The launch matrix pins explicit binding for built-in Claude commands while preserving
   byte-for-byte expert overrides and unchanged Codex commands.

The new regression and directly impacted tests all passed:
`test-dispatch-plugin-binding.sh`, `test-dispatch-sentinel-readiness.sh`,
`test-dispatch-launch-template.sh`, `test-dispatch-launch-override.sh`,
`test-dispatch-auto.sh`, `test-dispatch-full-auto.sh`,
`test-packaged-path-resolution.sh`, and `test-dispatch-exec-launch.sh`.

The sibling sweep found no second Claude launch composer or bypass. Dashboard, helper, resume,
and other built-in paths all converge on or inherit the corrected composer. Codex has a
different activation and readiness model, so **SH-571** tracks that provider-specific question
instead of generalizing Claude's `--plugin-dir` solution without a Codex-specific reproduction.

## Lessons

- Resolving an integration's helper does not activate that integration in the process the
  helper launches. Availability and activation need separate, explicit contracts.
- A required readiness producer is a launch dependency. Managed launches must carry the
  provider integration that owns their readiness witness instead of relying on ambient global
  state.
- A happy-path fake must preserve the causal preconditions of the behavior it models. If a
  witness appears unconditionally, the test cannot detect a missing producer.
- A fail-closed consumer limited the impact but could not repair a missing producer. The
  correct fix supplied the dependency where the invalid command was created.
- Provider launch and readiness contracts are not interchangeable. A solution verified for
  Claude should not be copied to Codex without provider-specific evidence; SH-571 retains that
  work explicitly.
