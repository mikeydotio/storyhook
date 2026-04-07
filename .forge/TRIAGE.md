# Triage Report

## Summary
- Total findings: 13 (deduplicated from REVIEW-REPORT + VALIDATE-REPORT)
- FIX: 0
- ESCALATE: 9
- Noted (no action): 4
- Yolo mode: false
- Fix cycle: 3 / 3 (max reached — all FIX items promoted to ESCALATE)

## Triage Team
- **Triager**: Primary deliberation
- **QA Engineer**: Risk assessment
- **Devil's Advocate (Skeptic)**: Challenge decisions, verify options

## Max Fix Cycles Reached

Fix cycle 3 of 3. Per policy, all remaining FIX items have been promoted to ESCALATE. The team unanimously voted FIX on 5 findings (UTF-8 panic, TOML parsing, --quiet/RawJson, HELP_TEXT, ghost --tree) — these are clear, low-risk fixes that were promoted only due to the cycle cap.

## ESCALATE Items

### UTF-8 Truncation Panic — ESCALATE (promoted from FIX)
- **Source**: REVIEW-REPORT + VALIDATE-REPORT
- **Severity**: Critical
- **Story**: SH-31 (priority: critical)
- **Description**: `msg.truncate(3900)` at `src/app.rs:2186` panics if the truncation point lands mid-codepoint in a multi-byte UTF-8 string. CJK/emoji story titles that push systemMessage past 3900 bytes crash the binary.
- **Options**:
  1. `msg.truncate(msg.floor_char_boundary(3900))` — one-line fix (Recommended)
  2. `char_indices` iteration — more verbose, unnecessary given MSRV 1.89
- **Recommendation**: Option 1 unanimously. Skeptic note: post-truncation appending adds 17 bytes; total may exceed 4000 with JSON envelope.
- **Rationale**: All 3 agents voted FIX. Promoted to ESCALATE solely due to max fix cycles.

### Fragile TOML Parsing in Plugin-Config — ESCALATE (promoted from FIX)
- **Source**: REVIEW-REPORT + VALIDATE-REPORT (flagged by all 5 agents)
- **Severity**: Important
- **Story**: SH-32 (priority: high)
- **Description**: `session_start()` at `src/app.rs:2127-2131` uses `contains("= false")` to check plugin-config.toml. Fails on valid TOML with extra whitespace. False positives from comments or other keys.
- **Options**:
  1. Parse with `toml` crate (already a dependency) — proper struct with `[plugin]` table (Recommended)
  2. Normalize whitespace before matching — still fragile
- **Recommendation**: Option 1. Skeptic flagged: struct must match `[plugin]` table schema. Bash hooks have same class of bug (separate concern).
- **Rationale**: All 3 agents voted FIX. Promoted to ESCALATE solely due to max fix cycles.

### --quiet Suppresses RawJson/session-start — ESCALATE (promoted from FIX)
- **Source**: REVIEW-REPORT
- **Severity**: Important
- **Story**: SH-33 (priority: high)
- **Description**: In `render_response()` at `src/output.rs:117-119`, the `quiet` check runs before the `RawJson` bypass. `story --quiet session-start` silently returns empty instead of JSON.
- **Options**:
  1. Move RawJson check before quiet check — 3-line swap (Recommended)
  2. Print directly to stdout in `session_start()` — breaks output separation
- **Recommendation**: Option 1 unanimously. Skeptic confirmed: RawJson only used by session_start, reordering is safe.
- **Rationale**: All 3 agents voted FIX. Promoted to ESCALATE solely due to max fix cycles.

### HELP_TEXT Missing --compact and --all Flags — ESCALATE (promoted from FIX)
- **Source**: REVIEW-REPORT
- **Severity**: Important
- **Story**: SH-34 (priority: medium)
- **Description**: `HELP_TEXT` at `src/cli.rs:78` shows `story help <command>` but omits `[--compact] [--all]`. Users cannot discover the new LLM-optimized output modes.
- **Options**:
  1. Update to `story help [<command>] [--compact] [--all]` (Recommended)
  2. Add note in footer
- **Recommendation**: Option 1. Single string change, zero risk.
- **Rationale**: Triager and skeptic voted FIX, QA voted ESCALATE (2:1 FIX). Promoted due to max fix cycles.

### Ghost Command --tree in Scaffold Template — ESCALATE (promoted from FIX)
- **Source**: REVIEW-REPORT
- **Severity**: Important
- **Story**: SH-35 (priority: medium)
- **Description**: `generate_claude_md()` at `src/storage.rs:262` references non-existent `story graph --tree`. Now more impactful as CLI docs are the sole integration surface.
- **Options**:
  1. Replace with `--blocked-by` — **SKEPTIC WARNING**: semantically wrong replacement (upstream blocks vs downstream children)
  2. Remove the line entirely (Skeptic Recommended)
  3. Replace with `--parallel-groups` — accurate, different purpose
  4. Implement `--tree` flag — YAGNI scope creep
- **Recommendation**: Team split. Skeptic strongly recommends Option 2 or 3 over Option 1. User decision required on replacement semantics.
- **Rationale**: All 3 voted FIX but disagreed on which option. Promoted due to max fix cycles.

### VERSION File vs Cargo.toml Drift — ESCALATE
- **Source**: REVIEW-REPORT
- **Severity**: Important
- **Story**: SH-36 (priority: medium)
- **Description**: VERSION=v0.12.0 but Cargo.toml=0.6.0. Pre-existing issue. Semver plugin bumps VERSION but Cargo.toml is not tracked.
- **Options**:
  1. Add Cargo.toml to `.semver/config.yaml` + sync now (Recommended)
  2. Manual sync only — will drift again
  3. Defer entirely — drift grows with each release
- **Recommendation**: Option 1. Two-minute fix that prevents recurring drift.
- **Rationale**: Two agents voted ESCALATE (out of scope for current work), one voted FIX-with-tight-scope.

### compact_reference() Drift Risk — ESCALATE
- **Source**: REVIEW-REPORT
- **Severity**: Important
- **Story**: SH-37 (priority: low)
- **Description**: Hand-curated `compact_reference()` has no test to detect when new commands are missing. 41 dispatch arms, some intentionally omitted.
- **Options**:
  1. Integration test with exclusion list — catches drift but exclusion list has same problem
  2. Comment-based manifest — self-documenting but can go stale
  3. Accept risk, no action (Skeptic Recommended) — existing tests provide partial protection
- **Recommendation**: Team split (2:1). Triager/QA favor Option 1 as future story. Skeptic argues the cure has the same disease.
- **Rationale**: Team split → when_in_doubt=escalate.

### No CHANGELOG Entry for MCP Removal — ESCALATE
- **Source**: REVIEW-REPORT (severity upgraded from Useful to Important by skeptic)
- **Severity**: Important
- **Story**: SH-38 (priority: medium)
- **Description**: MCP removal is a breaking change with no CHANGELOG entry or migration path documented.
- **Options**:
  1. Add CHANGELOG entry now (Recommended) — document removal, replacement (session hooks), setup path
  2. Bundle with next `semver bump`
  3. Add MIGRATION.md
- **Recommendation**: Option 1. Zero-risk text change, standard communication channel.
- **Rationale**: Two agents ESCALATE (content needs user decision), one voted FIX (text-only).

### Stale Skill Invocation in Plugin Install Message — ESCALATE
- **Source**: REVIEW-REPORT
- **Severity**: Useful
- **Story**: SH-39 (priority: low)
- **Description**: `plugin.rs:107` only mentions `/storyhook:context` skill, not the CLI equivalent. Inconsistent with CLI-first direction.
- **Options**:
  1. Add CLI alternative: mention both paths (Recommended)
  2. Replace with CLI-only reference
  3. Leave as-is — skill works fine
- **Recommendation**: Option 1. Additive, single string change.
- **Rationale**: Two agents ESCALATE, one DISMISS. Low priority.

## Noted Items (No Action Needed)

### Compact Reference Tight Size Margin — NOTED
- **Source**: REVIEW-REPORT + VALIDATE-REPORT
- **Severity**: Useful (advisory)
- **Description**: 2966/3000 bytes (1.1% headroom). Test `help_compact_output_under_3000_chars` guards against overflow.
- **Rationale**: All agents agree the test is the safety net. No action needed.

### python3 in Other Hooks — NOTED
- **Source**: REVIEW-REPORT
- **Severity**: Useful (advisory)
- **Description**: `post-git.sh` and `stop-handoff.sh` still use python3. Inconsistent with pure-bash session-start.sh.
- **Rationale**: All agents agree: intentional scope boundary. Other hooks do more complex JSON parsing. python3 is reasonable in Claude Code environments.

### sed-Based JSON Parsing Fragility — NOTED
- **Source**: REVIEW-REPORT
- **Severity**: Useful (advisory)
- **Description**: `session-start.sh:15` uses sed to extract `cwd` from JSON. Breaks on paths with double-quote characters.
- **Rationale**: All agents agree: vanishingly rare, not worth adding complexity for a theoretical edge case.

### Hook Script Line Count Ambiguity — NOTED
- **Source**: VALIDATE-REPORT
- **Severity**: Useful (advisory)
- **Description**: 26 total lines vs 16 functional. Criterion says "under 20 lines." Test asserts functional lines.
- **Rationale**: All agents agree: 16 functional lines meets the spirit. Test passes.

## Team Dynamics

**Strong consensus (3:0)**: UTF-8 panic, TOML parsing, --quiet/RawJson, all 4 noted items.

**Majority consensus (2:1)**: HELP_TEXT flags, CHANGELOG entry, stale skill invocation, compact_reference drift.

**Contested**: Ghost --tree replacement (all agreed on FIX, but disagreed on which option — skeptic strongly challenged Option 1 as semantically wrong).

**Severity adjustments**: Skeptic upgraded "No CHANGELOG" from Useful to Important. Triager downgraded TOML parsing from Critical to Important (functionality bug, not crash/data-loss).
