# Validation Report

## Test Suite Results
- **Before validation**: 390 lib + 17 web integration = 682 total | 682 pass | 0 fail
- **After validation**: 390 lib + 41 web integration = 706 total | 706 pass | 0 fail
- **Tests added**: 24 new tests in `tests/web_test.rs`
- **Run command**: `cargo test`
- **Duration**: ~3s full suite

## Findings

### JSON API Contract Between Server and Dashboard Not Validated
- **Severity**: Critical
- **Description**: The dashboard JavaScript reads specific field paths (`summary.total_open`, `summary.blocked_count`, `summary.ready_count`, `summary.by_state` as `[name, count]` tuples, story fields). Existing tests only checked `summary.total_open` and story existence. If a field name changed, the dashboard would silently break.
- **Action**: Added `web_serve_api_json_structure_matches_dashboard` test validating every field the dashboard reads.
- **Option 1 (Recommended)**: Keep the new contract test — Pros: Catches dashboard/API divergence. Cons: None.

### 405 Method Not Allowed Had No Test
- **Severity**: Important
- **Description**: Plan requires "Non-GET methods return 405" (Task 2.1). The implementation handles this correctly but it was untested.
- **Action**: Added `web_serve_post_returns_405` test.
- **Option 1 (Recommended)**: Test covers the gap — Pros: Directly matches plan requirement. Cons: None.

### Special Characters in Story Titles Untested Through API
- **Severity**: Important
- **Description**: Plan requires "Server handles stories with special characters in titles — JSON properly escapes via serde." No test verified this through the HTTP endpoint.
- **Action**: Added `web_serve_api_data_special_chars_in_title` (XSS payload) and `web_serve_api_data_unicode_title` (CJK characters) tests.
- **Option 1 (Recommended)**: Tests cover the gap — Pros: Verifies full pipeline. Cons: None.

### is_ready/is_blocked Values Never Asserted
- **Severity**: Important
- **Description**: Existing test only checked `is_ready` field existed (`.is_some()`) but never verified the actual boolean values were correct for ready vs blocked stories.
- **Action**: Added `web_serve_api_data_ready_and_blocked_flags_correct` with blocked-by relationship, asserting correct values.
- **Option 1 (Recommended)**: Test covers the gap — Pros: Verifies correctness, not just existence. Cons: None.

### No Concurrent Request Test
- **Severity**: Important
- **Description**: Plan requires "Server handles concurrent requests without hanging." No test exercised this.
- **Action**: Added `web_serve_handles_concurrent_requests` test (10 parallel requests).
- **Option 1 (Recommended)**: Test covers the gap — Pros: Verifies thread pool handles concurrency. Cons: None.

### CLI parse_web Had No Unit Tests
- **Severity**: Important
- **Description**: The `cli::tests` module had zero web-related unit tests. All testing was through subprocess invocations, which are slower and less precise.
- **Action**: Added 7 unit-level tests for `parse_web` covering start/stop/status/serve/defaults/errors.
- **Option 1 (Recommended)**: Tests cover the gap — Pros: Fast, precise, follows existing patterns. Cons: None.

### build_report_data Only Tested Empty and Simple Cases
- **Severity**: Useful
- **Description**: Blocked stories, priority counts, type counts, and non-project errors were never tested.
- **Action**: Added `build_report_data_with_blocked_story`, `build_report_data_counts_priorities`, `build_report_data_counts_types`, `build_report_data_non_project_errors`.
- **Option 1 (Recommended)**: Tests cover the gap — Pros: Better coverage of data layer. Cons: None.

### Cache-Control Headers Untested
- **Severity**: Useful
- **Description**: Plan specifies `Cache-Control: no-cache` on both `/` and `/api/data`. No test verified these headers.
- **Action**: Added `web_serve_root_has_no_cache_header` and `web_serve_api_data_has_no_cache_header`.
- **Option 1 (Recommended)**: Tests cover the gap — Pros: Catches header regressions. Cons: None.

### Daemon Lifecycle Not Integration Tested (Accepted Gap)
- **Severity**: Important
- **Description**: `handle_start`/`handle_stop`/`handle_status` spawn real child processes. The full start→status→stop→status cycle is untested. Testing this requires spawning/killing processes, which is fragile in CI since `env::current_exe()` returns the test binary, not `story`.
- **Option 1 (Recommended)**: Accept as manual-test-only. The "not running" paths are tested; the daemon lifecycle is structural and well-reviewed — Pros: Avoids flaky tests. Cons: No automated coverage.
- **Option 2**: Write a test that builds the binary first, then exercises the lifecycle — Pros: Full coverage. Cons: Complex setup, slow, CI-fragile.

### Port Binding Failure Not Testable Without Race (Accepted Gap)
- **Severity**: Useful
- **Description**: Testing "port in use" requires binding a port then racing to start the server. The error path is simple string matching on `tiny_http` error messages.
- **Option 1 (Recommended)**: Accept as manual-test-only — Pros: Avoids flaky test. Cons: Error path untested.

## Requirement Coverage

| Requirement (PLAN.md) | Tested? | Test Location | Notes |
|---|---|---|---|
| tiny_http dependency + web module | YES | `web_serve_and_query_root` | Compile + server start |
| WebAction enum (Start/Stop/Status/Serve) | YES | `web_parse_*` (4 tests) | All variants |
| parse_web all subcommands | YES | `web_parse_*` + CLI tests | Unit + integration |
| Invalid port returns AppError | YES | 6 tests | 0, negative, non-numeric, >65535, boundaries |
| Default port 3456 | YES | `web_start_default_port_is_3456` | Added this step |
| HELP_TEXT includes web commands | YES | `help_text_includes_web_commands` | Added this step |
| ReportData with Serialize | YES | `report_data_serializes_to_json` | Round-trip |
| build_report_data correctness | YES | 5 tests | Empty, mixed, blocked, priorities, types |
| GET / returns HTML | YES | `web_serve_and_query_root` | |
| GET /api/data returns JSON | YES | 5 tests | Empty, stories, special chars, unicode, structure |
| is_ready/is_blocked per story | YES | `web_serve_api_data_ready_and_blocked_flags_correct` | Values verified |
| Empty project returns zeros | YES | `web_serve_api_data_empty_project` | |
| Unknown routes return 404 | YES | `web_serve_404_unknown_route` | |
| Non-GET returns 405 | YES | `web_serve_post_returns_405` | Added this step |
| Concurrent requests | YES | `web_serve_handles_concurrent_requests` | 10 parallel |
| Special chars JSON-escaped | YES | `web_serve_api_data_special_chars_in_title` | XSS payload |
| Cache-Control: no-cache | YES | 2 header tests | Added this step |
| Port-in-use error message | NO | — | Accepted gap (race) |
| Dashboard embedded HTML | YES | `web_serve_and_query_root` | Body check |
| story web start spawns daemon | NO | — | Accepted gap (process lifecycle) |
| story web stop kills daemon | PARTIAL | `web_stop_when_not_running` | Not-running path only |
| story web status | PARTIAL | `web_status_not_running` | Not-running path only |
| web-serve registration | NO | — | Accepted gap (external tool) |
| story help web topic | YES | `help_web_topic_exists` | Added this step |
| Non-project directory error | YES | `web_start_requires_project` | |
| JSON contract matches dashboard | YES | `web_serve_api_json_structure_matches_dashboard` | Added this step |

## Tests Written This Step

| Test | What it verifies |
|------|-----------------|
| `web_serve_post_returns_405` | Non-GET method returns 405 |
| `web_serve_api_data_special_chars_in_title` | XSS payload properly JSON-escaped |
| `web_serve_api_data_unicode_title` | CJK characters survive round-trip |
| `web_serve_root_has_no_cache_header` | GET / has Cache-Control: no-cache |
| `web_serve_api_data_has_no_cache_header` | GET /api/data has Cache-Control: no-cache |
| `web_serve_api_json_structure_matches_dashboard` | Full JSON shape matches dashboard JS |
| `web_serve_handles_concurrent_requests` | 10 parallel requests all succeed |
| `web_serve_api_data_ready_and_blocked_flags_correct` | is_ready/is_blocked boolean values correct |
| `build_report_data_with_blocked_story` | blocked_ids populated correctly |
| `build_report_data_counts_priorities` | by_priority counts non-none priorities |
| `build_report_data_counts_types` | by_type counts types correctly |
| `build_report_data_non_project_errors` | Non-project dir returns error |
| `help_text_includes_web_commands` | HELP_TEXT contains web start/stop |
| `web_start_default_port_is_3456` | parse_web defaults to port 3456 |
| `web_parse_start_with_custom_port` | --port 8080 parsed correctly |
| `web_parse_stop` | stop subcommand parsed |
| `web_parse_status` | status subcommand parsed |
| `web_parse_serve_internal` | Internal --serve --port --root parsed |
| `web_parse_serve_missing_root_errors` | --serve without --root fails |
| `web_start_extra_unknown_flag_errors` | Unknown flag rejected |
| `web_start_port_one_is_valid` | Port 1 boundary accepted |
| `web_start_port_65535_is_valid` | Port 65535 boundary accepted |
| `web_start_port_65536_is_invalid` | Port 65536 boundary rejected |
| `web_start_port_negative_is_invalid` | Negative port rejected |

## Mock Audit

Zero mocks. All tests use:
- Real filesystem via `tempfile::tempdir()`
- Real HTTP server via `storyhook::web::start_server()`
- Real CLI binary via `assert_cmd::Command::cargo_bin("story")`
- Real `build_report_data()` with actual storyhook projects

## Strengths

- **Zero mocks**: Entire test suite uses real implementations
- **Real HTTP server tests**: Tests actually start servers, make requests, verify responses
- **Isolation**: Each test has its own tempdir and port — no cross-contamination
- **Good boundary testing**: Port validation covers 0, 1, 65535, 65536, negative, non-numeric
- **XSS resilience**: Special character and unicode tests verify safe serialization through HTTP
- **Consistent patterns**: All tests follow setup→init→act→assert pattern
