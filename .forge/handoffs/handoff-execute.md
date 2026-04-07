# Work Handoff

## Session Summary
- **Session**: session-resume-2026-04-07-s5
- **Stories completed**: 1
- **Stories attempted**: 1
- **Status**: Session limit reached (1/1 stories per session)

## What Happened
Executed SH-36 (Wave 3): Updated scaffold templates to remove all MCP references and add help command references. Modified 4 template functions: generate_storyhook_claude_md() in storage.rs (added decompose workflow, relationship types table, dependency graph section, help references), generate_agents_md() in app.rs (added `story help --compact` reference), generate_claude_md() in app.rs (added both help references), generate_cursor_rules() in app.rs (added `story help <command>` reference). Added 10 new test assertions across scaffold.rs and init_command.rs.

Also fixed stale storyhook states: SH-32 (verifying→done) and SH-34 (todo→done) — both had committed code but storyhook states weren't updated from prior session crashes.

## Stories Completed This Session
- SH-36: Update scaffold templates and CLAUDE.md — removed MCP references from all 4 scaffold template functions, added help references, made storyhook CLAUDE.md more comprehensive. 159 lines added across 4 files.

## Current Blockers
None.

## Working Context

### Patterns Established
- This is a Rust project using `cargo build` and `cargo test`
- Help flags parsed in parse_help() with --compact winning over --all when both given
- Flags checked before positional args (so `help --compact init` → compact, not topic)
- compact_reference() is a hand-curated &'static str (not generated from topic list)
- all_topics_text() iterates BTreeMap (alphabetical), skips alias topics
- Response::Message auto-wraps in JSON envelope when --json is active
- Response::RawJson outputs directly, bypassing JSON envelope regardless of --json flag
- Alias topics excluded from --all: awaits, context, is, link, priority, sync-git
- session-start uses early return in run() match arm to bypass auto-sync hook
- Plugin config checked via lowercase string matching for enabled = false / "false"
- serde_json::json! macro used for safe JSON construction in session-start
- generate_storyhook_claude_md() is in storage.rs (NOT app.rs) — it uses project prefix and done_state parameters

### Micro-Decisions
- Response::RawJson added to output.rs rather than special-casing in main.rs — cleaner separation
- session_start() uses early return from run() to avoid GitHub auto-sync after command
- Plugin config parsing is case-insensitive via to_lowercase()
- Truncation at 3900 chars (not 4000) to leave room for JSON wrapper and truncation message
- If .storyhook/ exists but stories can't be loaded, still outputs CLI reference with error note
- Priority sorting for "Next" story: by priority first, then created_at (matching story next behavior)
- generate_storyhook_claude_md() in storage.rs is the most comprehensive template — it's what `story init` creates in .storyhook/CLAUDE.md

### Code Landmarks
- `src/cli.rs` — Command parsing, HELP_TEXT, Invocation enum with SessionStart/HelpCompact/HelpAll
- `src/app.rs` — Command handlers; session_start() at ~line 2111; generate_agents_md() at ~line 2719; generate_claude_md() at ~line 2788; generate_cursor_rules() at ~line 2800
- `src/output.rs` — Response enum with RawJson variant; render_response intercepts RawJson at ~line 122
- `src/help_topics.rs` — Help topics BTreeMap; compact_reference() and all_topics_text() near bottom; session-start topic
- `src/storage.rs` — generate_storyhook_claude_md() (the init template) with relationship types, decompose, graph docs
- `src/main.rs` — Entry point, global flag parsing, 63 lines
- `src/lib.rs` — Module declarations, 18 lines
- `tests/session_start.rs` — 17 integration tests for session-start command
- `tests/help_new_flags.rs` — 12 acceptance tests for --compact and --all flags
- `tests/scaffold.rs` — 11 tests including MCP absence and help reference checks
- `tests/init_command.rs` — 5 tests including MCP absence, help references, relationship types, decompose+graph
- `tests/session_start_hook.rs` — Hook tests (1 pre-existing failure: hook_system_message_contains_cli_reference)

### Test State
- 390 unit tests: all pass
- Integration tests: all pass except tests/session_start_hook.rs (1 failure — pre-existing, testing SH-37 feature)
- tests/scaffold.rs: 11 tests, all pass
- tests/init_command.rs: 5 tests, all pass
- Clippy: 1 pre-existing warning (collapsible_if in cli.rs), no errors
- Run command: `cargo test`
- No special environment setup needed

## What's Next
- SH-37 (Wave 4): Rewrite session-start hook — replace python3-based hook with story session-start call. Now unblocked (SH-35 and SH-36 both done).
- After SH-37: All stories complete → transition to review+validate
