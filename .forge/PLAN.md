# Implementation Plan: Storyhook Web Dashboard

## Overview

Add a live-updating web dashboard to storyhook, launched via `story web start|stop`. Mirrors the HTML report with client-side filtering/sorting, 3-second auto-refresh, mobile-responsive design, and coderig-aware server registration.

## Architecture

- **HTTP server**: `tiny_http` crate (minimal, synchronous, no async runtime)
- **Live updates**: Client-side polling every 3 seconds (no SSE — avoids `tiny_http` thread pool exhaustion)
- **Frontend**: Single HTML file embedded via `include_str!` — vanilla JS, no framework, no build step, no CDN
- **Daemon mode**: Background process via `Command::new(current_exe())` with explicit root path arg; PID file + `fs4` flock for lifecycle management
- **Bind address**: `127.0.0.1` by default (security: don't expose project data on network)
- **Default port**: 3456 (configurable via `--port`)
- **Coderig**: Auto-detect `web-serve` in PATH, register/unregister on start/stop

## Task Breakdown

### Wave 1 (no dependencies — all tasks parallel)

- [ ] Task 1.1: Add `tiny_http` dependency and create web module scaffold
  - Acceptance:
    - `tiny_http = "0.12"` (or latest) is in `[dependencies]` in Cargo.toml
    - `src/web.rs` exists with `pub fn start_server(root: &Path, port: u16) -> Result<(), AppError>` stub returning `Ok(())`
    - `pub mod web;` added to `src/lib.rs`
    - `WebAction` enum in `cli.rs`: `Start { port: u16 }`, `Stop`, `Status`, `Serve { port: u16, root: PathBuf }` (Serve is internal/hidden)
    - `Invocation::Web { action: WebAction }` variant added
    - `parse_web(args)` handles: `story web start [--port N]` (default 3456), `story web stop`, `story web status`
    - `"web" => parse_web(args)` dispatch added in `parse_invocation`
    - `HELP_TEXT` includes `story web start [--port <PORT>]` and `story web stop`
    - Invalid port (negative, >65535, non-numeric) returns `AppError::Usage`
    - `story web` with no subcommand returns `AppError::Usage` with help
    - `cargo check` passes
  - Files: Cargo.toml, src/lib.rs, src/web.rs, src/cli.rs

- [ ] Task 1.2: Extract reusable report data builder from app.rs
  - Acceptance:
    - New `ReportData` struct in `output.rs`: `{ summary: SummaryView, stories: Vec<StoryView>, ready_ids: Vec<String>, blocked_ids: Vec<String> }` with `#[derive(Clone, Debug, Serialize)]`
    - New `pub(crate) fn build_report_data(root: &Path) -> Result<ReportData, AppError>` in `app.rs` that encapsulates the summary-building logic from the Report handler (lines 283-425)
    - Both `Invocation::Report { html: true }` and `Invocation::Report { html: false }` handlers refactored to call `build_report_data()` instead of duplicating logic
    - `build_story_views` remains private — `build_report_data` is the public interface
    - `ReportData` serializes to valid JSON via `serde_json::to_string`
    - `cargo test` passes with no regressions
    - `story report` and `story report --html` produce identical output to before
  - Files: src/app.rs, src/output.rs

### Wave 2 (depends on Wave 1 — tasks sequential)

- [ ] Task 2.1: Implement HTTP server with routing and JSON API
  - Depends on: Task 1.1, Task 1.2
  - Acceptance:
    - `web::start_server(root: &Path, port: u16)` binds `127.0.0.1:{port}` using `tiny_http::Server::http`
    - Server logs `"Storyhook dashboard: http://127.0.0.1:{port}"` to stderr on startup
    - `GET /` returns 200, `Content-Type: text/html; charset=utf-8`, `Cache-Control: no-cache`, body = embedded dashboard HTML (placeholder OK for this task — will be replaced in 2.2)
    - `GET /api/data` returns 200, `Content-Type: application/json`, `Cache-Control: no-cache`, body = `build_report_data(root)` serialized as JSON
    - Each story in the JSON response includes `is_ready: bool` and `is_blocked: bool` fields
    - `GET /api/data` on empty project returns `{ "summary": { "total_open": 0, ... }, "stories": [], "ready_ids": [], "blocked_ids": [] }`
    - Unknown routes return 404 with plain text body
    - Non-GET methods return 405
    - Server handles concurrent requests without hanging
    - Server handles stories with special characters in titles — JSON properly escapes via serde
    - On bind failure (port in use), returns `AppError` with message: `"Port {port} already in use. Try a different port with --port."`
    - `cargo check` passes
  - Files: src/web.rs

- [ ] Task 2.2: Build the dashboard frontend (HTML/CSS/JS)
  - Depends on: Task 2.1
  - Acceptance:
    - File `src/web_dashboard.html` exists, valid HTML5 (`<!DOCTYPE html>`, `<html lang="en">`)
    - Embedded in web.rs via `const DASHBOARD_HTML: &str = include_str!("web_dashboard.html");`
    - **Summary section**: Stat cards for Total, Open, Closed, Blocked, Ready — values populated from `/api/data` JSON
    - **State distribution**: Horizontal bar chart with colored segments and legend (matching HTML report style)
    - **Priority breakdown**: Styled badges (critical=red, high=orange, medium=yellow, low=green, none=gray)
    - **Type breakdown**: Styled badges
    - **Story table**: Columns — ID, Title, State, Priority, Labels, Assignee, Updated
    - **Row highlighting**: Blocked rows have red-tinted background, ready rows have green-tinted background
    - **Filtering**: Text search input (filters across ID, title, labels), state multi-select, priority multi-select — filter state stored in JS variables, NOT reset on data refresh
    - **Sorting**: Click column header to sort asc/desc; visual sort indicator (arrow); repeated click toggles direction; sort state stored in JS variable, NOT reset on data refresh
    - **Polling**: `setInterval(fetchAndRender, 3000)` fetches `/api/data` and re-renders using current filter/sort state; initial fetch on page load
    - **Connection indicator**: Green dot "Live" when last fetch succeeded within 10s; red dot "Disconnected" when fetch fails
    - **Last updated**: Footer shows "Updated N seconds ago"
    - **Mobile-responsive**: Below 768px — stat cards stack vertically, table scrolls horizontally, filter controls stack
    - **Dark mode**: `@media (prefers-color-scheme: dark)` with color scheme matching existing HTML report
    - **No external deps**: Zero CDN links, zero framework imports
    - Page renders without JS errors in browser console
  - Files: src/web_dashboard.html, src/web.rs (update include)

### Wave 3 (depends on Wave 2 — tasks parallel)

- [ ] Task 3.1: CLI command handlers and daemon lifecycle
  - Depends on: Task 2.2
  - Acceptance:
    - `main.rs` dispatches `Invocation::Web` before `app::run()` (same pattern as `tui` dispatch at main.rs:11)
    - Internal `story web --serve --port N --root /path` runs `web::start_server()` in foreground (hidden flag, not in help text)
    - **`story web start`**:
      - Checks if `.storyhook/web.lock` can be exclusively locked (via `fs4::fs_std::FileExt::try_lock_exclusive`); if lock fails, reads PID from `.storyhook/web.pid` and reports `"Web UI already running (PID {pid} on port {port}). Run 'story web stop' first."`
      - Spawns `Command::new(current_exe())` with args `["web", "--serve", "--port", port, "--root", root_absolute_path]`, stdin/stdout/stderr redirected (stderr to `.storyhook/web.log`)
      - Writes child PID and port to `.storyhook/web.pid` as `{pid}\n{port}`
      - Prints `"Web UI started at http://127.0.0.1:{port} (PID {pid})"`
      - If `web-serve` exists in PATH, runs `web-serve register {port}` (failure logged to stderr, does not block start)
    - **`story web stop`**:
      - Reads PID from `.storyhook/web.pid`; if file missing, prints `"Web UI is not running"` and returns Ok
      - Verifies PID belongs to a `story` process (reads `/proc/{pid}/cmdline` on Linux; skip verification on other OS)
      - Sends SIGTERM via `libc::kill(pid, libc::SIGTERM)` (Unix)
      - Removes `.storyhook/web.pid`
      - If `web-serve` exists in PATH, runs `web-serve unregister` (failure logged, does not block stop)
      - Prints `"Web UI stopped (PID {pid})"`
      - If PID file exists but process is dead (stale), cleans up PID file and prints `"Cleaned up stale PID file"`
    - **`story web status`**:
      - If no PID file, prints `"Web UI is not running"`
      - If PID file exists and process alive, prints `"Web UI running at http://127.0.0.1:{port} (PID {pid})"`
      - If PID file exists but process dead, prints `"Web UI is not running (stale PID file cleaned up)"` and removes PID file
    - `story web start` from a non-storyhook directory returns `"not a storyhook project"` error
    - `cargo check` passes
  - Files: src/main.rs, src/app.rs, src/cli.rs, src/web.rs

- [ ] Task 3.2: Help topic for web command
  - Depends on: Task 3.1
  - Acceptance:
    - `story help web` returns a detailed help topic explaining: purpose, start/stop/status commands, --port flag, default port 3456, coderig/web-serve integration, how to access from browser
    - Help topic registered in `help_topics.rs` following the existing `register_topics()` pattern
    - `cargo check` passes
  - Files: src/help_topics.rs

### Wave 4 (depends on Wave 3)

- [ ] Task 4.1: Integration tests
  - Depends on: Task 3.1, Task 3.2
  - Acceptance:
    - New test file `tests/web_test.rs`
    - Test: init project in tempdir, start server in background thread via `web::start_server`, GET `/` returns 200 with Content-Type text/html and body contains "Storyhook"
    - Test: GET `/api/data` returns 200, Content-Type application/json, body parses as valid JSON with `summary` and `stories` keys
    - Test: create story via `app::run`, GET `/api/data`, verify story appears in response
    - Test: GET `/nonexistent` returns 404
    - Test: `parse_web` correctly parses all subcommands (start, stop, status, --port)
    - Test: `parse_web` rejects invalid ports (negative, >65535, non-numeric, missing value)
    - Test: `build_report_data` on empty project returns zero counts and empty stories
    - Test: `build_report_data` with mixed states returns correct summary counts
    - Test: `ReportData` serializes to valid JSON and round-trips through serde
    - Tests use random ports (bind to port 0 or pick from high range) to avoid conflicts
    - `cargo test` passes with all new tests and no regressions
  - Files: tests/web_test.rs

## Test Strategy

### Unit Tests (in respective modules)
- `parse_web()` parsing: all subcommands, flags, invalid inputs, edge cases
- `build_report_data()`: empty project, single story, mixed states, blocked stories, priority/type counts
- `ReportData` serialization round-trip

### Integration Tests (tests/web_test.rs)
- Server start and route responses (/, /api/data, 404, 405)
- API data accuracy matches expected story data
- Story creation reflected in API response

### Manual Testing Checklist
- [ ] Open dashboard in browser, verify layout matches HTML report
- [ ] Create/modify stories while dashboard is open, verify updates within 3-6 seconds
- [ ] Apply filters, verify data refresh doesn't reset them
- [ ] Sort by different columns, verify sort persists across refreshes
- [ ] Test mobile viewport (Chrome DevTools, 375px)
- [ ] Test dark mode (toggle OS preference)
- [ ] `story web start` → close terminal → `story web status` in new terminal shows "running"
- [ ] `story web stop` while browser open — page shows "Disconnected" indicator
- [ ] Start in coderig, verify web-serve registration
- [ ] Port conflict: verify clear error message

## Resumption Points

- **After Wave 1**: Project compiles, CLI parses web commands (no-op handlers), report data extractable as JSON. Can pause.
- **After Wave 2**: Server runs in foreground, serves live dashboard. Testable with `cargo run -- web --serve --port 3456 --root .`. Visually verifiable.
- **After Wave 3**: Full `story web start|stop|status` lifecycle works. Feature is complete.
- **After Wave 4**: Tests provide regression safety. Ready for version bump.

## Risk Register

| Risk | Impact | Likelihood | Mitigation |
|------|--------|------------|------------|
| `build_report_data` refactor breaks existing report output | High | Low | Run `cargo test` after. Manually verify `story report --html` is identical. |
| Port 3456 conflicts with user's other services | Medium | Medium | `--port` flag, clear error message suggesting `--port`. |
| PID recycling: `story web stop` kills wrong process | High | Low | Verify PID ownership via `/proc/{pid}/cmdline` before killing. |
| Daemon dies without PID cleanup | Medium | Medium | Status/start detect stale PID (process not alive) and auto-cleanup. |
| Frontend grows unwieldy in vanilla JS | Low | Low | ~300 lines of JS. Can extract to separate file if needed. |

## Scope Boundaries

**IN scope:**
- HTTP server with `/`, `/api/data` routes
- Dashboard with summary, table, filtering, sorting, 3s polling
- Daemon start/stop/status with PID + lockfile lifecycle
- `web-serve` auto-registration in coderig
- Integration tests

**OUT of scope:**
- Story mutation from web UI (read-only dashboard)
- Authentication (localhost-only binding)
- HTTPS/TLS
- SSE/WebSocket (polling sufficient; additive later)
- Persistent filter/sort across browser sessions
- Story detail drill-down pages
