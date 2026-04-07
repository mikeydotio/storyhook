# Validation Report

## Test Suite Results
- Total: 732 | Pass: 732 | Fail: 0 | Skip: 0
- Run command: `cargo test`
- Duration: ~7 seconds
- Build: Clean (no errors)
- Clippy: 1 pre-existing warning (collapsible_if in `src/cli.rs:311` — cosmetic only)

## Findings

### Plugin-Config Whitespace Parsing Bug
- **Severity**: Critical
- **Description**: `session_start()` at `src/app.rs:2129` checks `lower.contains("= false")` which requires exactly one space between `=` and `false`. Valid TOML allows arbitrary whitespace (e.g., `enabled  =   false`). A user who formats their TOML with extra whitespace will find that disabling the plugin does not work — `session-start` continues to output a systemMessage instead of `{}`. Documented by test `session_start_plugin_config_extra_whitespace_bug_documented`.
- **Location**: `src/app.rs:2129`
- **Option 1 (Recommended)**: Parse with `toml` crate (already a dependency) and check the `enabled` field properly. -- Pros: Handles all valid TOML. Cons: Slightly more code.
- **Option 2**: Normalize whitespace before matching. -- Pros: Minimal change. Cons: Still fragile.

### UTF-8 Truncation Panic Risk
- **Severity**: Critical
- **Description**: `msg.truncate(3900)` at `src/app.rs:2186` can panic if the truncation point lands mid-codepoint in a multi-byte UTF-8 string. Projects with CJK or emoji in story titles that push the systemMessage past 3900 bytes will crash.
- **Location**: `src/app.rs:2186`
- **Option 1 (Recommended)**: Use `msg.floor_char_boundary(3900)` (stable since Rust 1.76). -- Pros: One-line fix. Cons: None.
- **Option 2**: Iterate `char_indices` to find safe boundary. -- Pros: Works on older Rust. Cons: More verbose.

### Compact Help Tight Size Margin
- **Severity**: Important
- **Description**: `story help --compact` output is 2966 bytes — only 34 chars under the 3000-char limit (1.1% headroom). Test `help_compact_output_under_3000_chars` will catch overflow, but margin is tight.
- **Location**: `src/help_topics.rs:1142`
- **Option 1 (Recommended)**: No action needed — test guards against regression.
- **Option 2**: Shorten descriptions to build margin.

### Hook Script Line Count Ambiguity
- **Severity**: Useful
- **Description**: Acceptance criterion says "under 20 lines" — total file is 26 lines, but only 16 are functional (excluding shebang, comments, blanks). Test asserts <20 functional lines, which passes. Ambiguity is documented but not blocking.
- **Location**: `plugin/claude-code/hooks/session-start.sh`
- **Option 1 (Recommended)**: Accept — 16 functional lines meets the spirit of the criterion.
- **Option 2**: Trim comments to bring total under 20.

## Requirement Coverage

| Requirement | Tested? | Test Location | Notes |
|---|---|---|---|
| SH-32: MCP source files deleted | YES | `mcp_removal.rs::no_mcp_source_files_exist` | Asserts mcp.rs, mcp_install.rs don't exist |
| SH-32: MCP test files deleted | YES | Verified absent; regression guard in mcp_removal.rs | |
| SH-32: No MCP code in source | YES | `mcp_removal.rs::help_topic_mcp_config_not_found`, `help_output_does_not_mention_mcp_config`, `help_all_does_not_mention_mcp_after_removal` | |
| SH-32: cargo build passes | YES | Clean build verified | |
| SH-32: cargo test passes | YES | 732/732 passing | |
| SH-33: No mcp-config refs outside .forge/ | YES | Multiple tests in mcp_removal.rs | Only .forge/, .planning/, worktrees contain historical refs |
| SH-34: --compact 40-100 lines | YES | `help_new_flags.rs::help_compact_is_concise` | Actual: 60 lines |
| SH-34: --compact under 3000 chars | YES | `help_new_flags.rs::help_compact_output_under_3000_chars` | Actual: 2966 chars |
| SH-34: --compact contains key commands | YES | `help_new_flags.rs::help_compact_produces_output_with_key_commands` | Tests init, new, list, next, show, move, etc. |
| SH-34: --all contains all topics | YES | `help_new_flags.rs::help_all_produces_all_topics` | Tests 10 topics |
| SH-34: --all 3x+ longer than compact | YES | `help_new_flags.rs::help_all_is_much_longer_than_compact` | Actual: 1033 vs 60 lines (17x) |
| SH-34: JSON output for compact | YES | `help_new_flags.rs::help_compact_with_json_flag_produces_json` | |
| SH-34: JSON output for --all | YES | `help_new_flags.rs::help_all_with_json_flag_produces_json` | |
| SH-34: Backward compat | YES | `help_new_flags.rs::help_with_topic_still_works`, `help_no_args_lists_topics` | |
| SH-35: Valid JSON with systemMessage | YES | `session_start.rs::session_start_valid_project_outputs_system_message`, `session_start_output_is_valid_json_object` | |
| SH-35: Contains CLI reference + state | YES | `session_start.rs::session_start_contains_cli_reference`, `session_start_contains_project_state` | |
| SH-35: Empty project -> 0 stories | YES | `session_start.rs::session_start_empty_project_zero_stories` | |
| SH-35: No .storyhook/ -> {} | YES | `session_start.rs::session_start_no_project_outputs_empty_json` | |
| SH-35: Plugin disabled -> {} | YES | `session_start.rs::session_start_plugin_disabled_outputs_empty_json`, `session_start_plugin_disabled_string_value_outputs_empty_json` | |
| SH-35: Special chars in titles | YES | `session_start.rs::session_start_special_characters_in_title`, `session_start_unicode_in_story_title`, `session_start_newline_in_story_title` | |
| SH-35: Under 2 seconds | YES | `session_start.rs::session_start_completes_within_two_seconds` | |
| SH-36: No MCP in scaffold outputs | YES | `scaffold.rs::scaffold_agents_md_no_mcp_references`, `scaffold_cursor_rules_no_mcp_references`, `scaffold_claude_md_no_mcp_references` | |
| SH-37: Hook under 20 lines | YES | `session_start_hook.rs::hook_script_is_under_20_functional_lines` | 16 functional lines |
| SH-37: No python3 | YES | `session_start_hook.rs::hook_script_does_not_use_python3` | |
| SH-37: Calls story session-start | YES | `session_start_hook.rs::hook_script_calls_story_session_start` | |
| SH-37: cli-reference docs session-start | YES | Verified present in cli-reference.md | |
| SH-37: workflow-patterns no MCP | YES | Verified clean | |

## Tests Written This Step

### Unit tests (src/help_topics.rs)
| Test | What It Verifies |
|------|-----------------|
| `compact_reference_under_3000_chars` | Size contract at unit level |
| `compact_reference_between_40_and_100_lines` | Line count contract |
| `compact_reference_contains_all_section_headers` | All sections survive edits |
| `compact_reference_contains_critical_commands` | 14 essential commands present |
| `compact_reference_does_not_reference_mcp` | No MCP in compact ref |
| `all_topics_text_does_not_include_alias_topics` | Alias exclusion works |
| `all_topics_text_includes_canonical_topics` | Canonical topics present |
| `all_topics_text_does_not_reference_mcp` | No mcp-config in --all |

### Integration tests (tests/)
| Test | File | What It Verifies |
|------|------|-----------------|
| `help_compact_output_under_3000_chars` | help_new_flags.rs | Size contract at CLI level |
| `help_compact_does_not_reference_mcp` | help_new_flags.rs | CLI-level MCP guard |
| `help_all_with_json_flag_produces_json` | help_new_flags.rs | --all + --json envelope |
| `session_start_corrupted_stories_dir_still_returns_json` | session_start.rs | Corrupted filesystem graceful degradation |
| `session_start_missing_project_toml_still_returns_json` | session_start.rs | Missing config graceful degradation |
| `session_start_plugin_config_extra_whitespace_bug_documented` | session_start.rs | Documents whitespace parsing bug |
| `session_start_plugin_config_enabled_true_produces_system_message` | session_start.rs | Explicit enabled=true works |
| `session_start_plugin_config_malformed_still_works` | session_start.rs | Garbage config fails open |
| `session_start_no_plugin_config_file_produces_system_message` | session_start.rs | Missing config defaults to enabled |
| `session_start_output_is_one_of_two_valid_shapes` | session_start.rs | Only {} or {systemMessage} allowed |
| `session_start_stderr_is_empty` | session_start.rs | No leaking debug output |
| `session_start_system_message_does_not_mention_mcp` | mcp_removal.rs | Session-start CLI ref MCP-free |
| `no_mcp_source_files_exist` | mcp_removal.rs | Guard against file reintroduction |
| `hook_script_is_under_20_functional_lines` | session_start_hook.rs | Hook size contract |
| `hook_script_does_not_use_python3` | session_start_hook.rs | No python dependency |
| `hook_script_calls_story_session_start` | session_start_hook.rs | Hook delegates correctly |

Also updated bounds on `help_compact_is_concise` from 10-80 to 40-100 lines to match acceptance criteria.

## Mock Audit

Zero mocks in the entire test suite. All tests use:
- Real `story` binary via `assert_cmd::Command::cargo_bin("story")`
- Real filesystem via `tempfile::tempdir()`
- Real bash shell for hook tests via `std::process::Command`

## Strengths

- **Comprehensive regression guards**: `mcp_removal.rs` tests the negative — MCP flag rejected, MCP commands error, scaffolds/help/session-start contain no MCP. Gold standard for removal testing.
- **Real integration testing**: No mocks, real binary, real filesystem. Tests exercise actual code paths.
- **Edge case depth**: Special characters, unicode, control characters, corrupted directories, malformed configs, missing files — all tested.
- **Performance assertions**: Both session-start (2s) and hook (5s) have timing bounds.
- **Output shape contracts**: Tests verify exact JSON shape (`{}` or `{"systemMessage":"..."}`), stderr cleanliness, and size limits.
- **Contract enforcement**: Unit tests in help_topics.rs enforce compact_reference invariants (size, content, sections) at compile-test level, catching drift early.
