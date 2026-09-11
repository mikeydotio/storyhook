<!-- semver:start -->
## Semantic Versioning

This project uses semantic versioning managed by the `/semver` plugin.

### Version Awareness
- Read the `VERSION` file at the start of each conversation to know the current version.
- Read `.semver/config.yaml` to understand the versioning configuration.
- When discussing releases, deployments, or changes, reference the current version.

### Commit Discipline
- Write meaningful, descriptive commit messages. Each commit message may appear in an auto-generated changelog.
- Use conventional-commit-style prefixes when they fit naturally: `feat:`, `fix:`, `docs:`, `refactor:`, `test:`, `chore:`.
- The first line of the commit message should be a concise summary (under 72 characters). Add detail in the body if needed.

### Version Bump Guidance
When recommending or performing a version bump:
- **patch** (0.0.x): Bug fixes, documentation corrections, minor refactors with no behavior change.
- **minor** (0.x.0): New features, new capabilities, non-breaking additions to the public API or user-facing behavior.
- **major** (x.0.0): Breaking changes — removed features, changed interfaces, incompatible API modifications, behavior changes that require consumers to update.

When you notice the user has completed a logical unit of work, suggest running `/semver bump` with the appropriate level.

### Configuration
Versioning settings are in `.semver/config.yaml`. Do not modify this file unless the user explicitly asks to change semver settings.
<!-- semver:end -->

## Story priority rubric

**The rubric ships in the binary: run `story help priority-rubric`.** That is the
source — the damage ladder, the five levels, the ordered tiebreakers, the
detection-layer carve-out and the relationship rules. It is not restated here,
and `tests/priority_rubric.rs` fails if it starts being.

Settled 2026-08-16 by a three-seat panel — web/UX, architecture, QA — convened
over this backlog and applied to all 26 open stories. It lived in this file for
two days, which was long enough to prove the point SH-354 then fixed: nothing
that *sets* a priority could read it, so every level chosen by `story new`,
`/story new` or `story-triage` was chosen by vibe. Promoting it into
`story help priority-rubric` was a council decision — recorded on SH-354 — taken on
the grounds that the generic half describes storyhook's own model (a closed
five-level enum, a CHECK-constrained rank column, `domain::ready_order`,
`story next` skipping blocked stories) and so is the tool's to state, the same
way the required states and the scaffolded `AGENTS.md` workflow already are.

What stays below is what does **not** belong in a stranger's binary: this
project's own evidence for each rule. The rubric is the law; these are the cases
that made it.

### This project's precedents

- **A defect must never sit at `none`.** SH-283 is the case study: filed `none`,
  it described a live, silent, cross-story overwrite of the system of record, and
  sorted dead last of 26. `none` was also `story new`'s silent default until
  SH-354, so it claimed *deliberately parked* on behalf of everyone who never
  chose — the command warns now.
- **Price the class, not the sighting.** This project has paid the opposite four
  times: SH-136, SH-258, SH-198, SH-260/276.
- **The detection-layer carve-out.** SH-306 is the precedent — a gate that
  silently did not run shipped six unguarded pushes. Coverage appetite in general
  earns nothing; being the *missing detector for a named defect* earns this.
- **Recoverability rarely demotes here.** The append-only event log does not
  qualify: history is deliberately unreachable from the CLI, so nothing prompts
  you to look, and recoverable-but-undetectable is operationally identical to
  lost.
- **The blocker floor versus the carve-out.** They collide whenever a defect is
  `blocked-by` the very instrument that would observe it. Found the first time
  the rule was used in anger: SH-283 (critical) `blocked-by` SH-335 (high) — a
  pairing no longer live on either story, since SH-335 landed and SH-283's edge
  was demoted to `relates-to` when it closed. What the precedent preserves is the
  reasoning, not the relation; do not go looking for the edge. The
  carve-out wins, for two reasons worth keeping written down. It is the **more
  specific** rule — it speaks to this exact pairing, where the floor speaks to
  dependencies in general. And the floor's purpose is **anti-stall**, which a
  detector edge does not create: the detector already sorts above everything
  except the defect it is blocking, so the queue hands it out next by itself.
  Raising it would buy no scheduling and would erase the ordering the carve-out
  exists to state.

## Scope: adopt or file

**The rubric ships in the binary: run `story help scope-rubric`.** That is the
source — the default of adopting a mid-work discovery rather than filing it, the
test for whether it belongs to the story in progress, and what still gets filed.
It is not restated here, and `tests/scope_rubric.rs` fails if it starts being.
This project's own calibration: autonomous sessions run a 1M-token window, so
"at least half unused" is roughly 500k tokens used or fewer (SH-402).
