# Web Dashboard Documentation

## Overview

The web dashboard is a read-only, live-updating browser interface for storyhook projects. It serves the same data as `story report --html` but refreshes automatically every 3 seconds, supports client-side filtering and sorting, and runs as a background daemon.

The dashboard is entirely local. The HTTP server binds to `127.0.0.1` only -- project data is never exposed on the network. There is no authentication because none is needed: only localhost can reach it.

Key properties:
- Read-only. No mutations through the web UI.
- Self-contained. Single embedded HTML file, no CDN, no framework, no build step.
- Live. Polls `/api/data` every 3 seconds; filter and sort state survives refreshes.
- Disposable. `story web start` / `story web stop`. Nothing to configure.

## Getting Started

```bash
# Initialize a storyhook project (if you haven't)
story init

# Start the dashboard (default port 3456)
story web start

# Open in browser
open http://127.0.0.1:3456

# Check status
story web status

# Stop when done
story web stop
```

To use a different port:

```bash
story web start --port 8080
```

The dashboard updates live as you create and modify stories in another terminal:

```bash
story new "Implement login"
story set SH-1 --priority high
story move SH-1 in-progress
# Dashboard reflects changes within 3-6 seconds
```

## Architecture

### HTTP Server

The server uses `tiny_http`, a synchronous, minimal HTTP library. It runs in a single-threaded request loop with no async runtime. The entire server is ~120 lines in `src/web.rs`.

Routes:
- `GET /` -- serves the embedded HTML dashboard
- `GET /api/data` -- returns project data as JSON
- All other paths -- 404
- All non-GET methods -- 405

The HTML file (`src/web_dashboard.html`) is compiled into the binary via `include_str!`. There are no external files to serve.

### Data Flow

```
Browser (3s poll)
    |
    v
GET /api/data
    |
    v
web::build_api_json(root)
    |
    v
app::build_report_data(root)     <-- reads .storyhook/events/ from disk
    |
    v
JSON response: { summary, stories, ready_ids, blocked_ids }
    |
    v
Browser JS: renderAll() with current filter/sort state
```

Each `/api/data` request reads the event log from disk and recomputes the full project state. This is intentionally simple -- there is no in-memory cache or file watcher. For typical project sizes (hundreds of stories), this adds negligible latency.

### Daemon Lifecycle

`story web start` does not run the server in the foreground. It spawns a detached child process:

1. Acquires an exclusive `fs4` file lock on `.storyhook/web.lock`
2. If the lock is held, reports "already running" with the existing PID and port
3. Spawns `story web --serve --port N --root /path/to/project` as a background process
4. Writes `{pid}\n{port}` to `.storyhook/web.pid`
5. Redirects stderr to `.storyhook/web.log`
6. If `web-serve` is in PATH (agentsmith environments), registers the port

`story web stop`:
1. Reads PID from `.storyhook/web.pid`
2. Verifies the PID belongs to a `story` process (via `/proc/{pid}/cmdline` on Linux)
3. Sends SIGTERM via `libc::kill`
4. Removes PID and lock files
5. If `web-serve` is in PATH, unregisters

`story web status`:
- Reports running/stopped based on PID file existence and process liveness
- Automatically cleans up stale PID files from crashed daemons

Files written to `.storyhook/`:

| File | Purpose |
|------|---------|
| `web.pid` | `{pid}\n{port}` -- tracks running daemon |
| `web.lock` | `fs4` exclusive lock -- prevents races |
| `web.log` | stderr from the daemon process |

### Security

- **Localhost binding**: Server binds `127.0.0.1:{port}`. Network access is blocked at the socket level.
- **Security headers**: All responses include `X-Content-Type-Options: nosniff`, `X-Frame-Options: DENY`, and `Content-Security-Policy: default-src 'self'; script-src 'unsafe-inline'; style-src 'unsafe-inline'`.
- **No authentication**: Unnecessary given localhost-only binding. The `web-serve` tool handles external access in agentsmith environments.
- **XSS prevention**: The dashboard uses DOM `textContent` assignment for all user-controlled data (story titles, labels, etc.), preventing script injection. The API returns data as JSON, which serde escapes correctly.
- **Strict routing**: Exact string match on URL paths. No path traversal possible.
- **Method whitelist**: Only GET is accepted. Everything else returns 405.

## CLI Reference

### `story web start [--port <PORT>]`

Start the dashboard as a background daemon.

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--port` | `u16` | `3456` | TCP port to bind. Must be 1-65535. |

Requires an initialized storyhook project. Returns an error if:
- Not in a storyhook project directory
- Port is invalid (0, negative, >65535, non-numeric)
- Port is already in use
- Dashboard is already running

Output: `Web UI started at http://<ip>:<port> (PID <pid>)`

The displayed IP is the Tailscale IP if available, then the first non-loopback LAN IP, then `127.0.0.1`.

### `story web stop`

Stop the running dashboard daemon.

No flags. Safe to run when nothing is running -- prints "Web UI is not running". Cleans up stale PID files from crashed daemons.

Output: `Web UI stopped (PID <pid>)`

### `story web status`

Check whether the dashboard is running.

Output (when running): `Web UI running at http://<ip>:<port> (PID <pid>)`
Output (when stopped): `Web UI is not running`

### `story help web`

Displays the web dashboard help topic with usage, examples, and how-it-works explanation.

## Dashboard Features

### Summary Cards

Five stat cards at the top: Total, Open, Closed, Blocked, Ready. Values update on every poll.

### State Distribution Bar

Horizontal stacked bar chart showing story counts per state (e.g., todo, in-progress, done). Color-coded with a legend below.

### Priority and Type Breakdowns

Badge-style displays. Priority uses severity colors: critical (red), high (orange), medium (yellow), low (green). Types use neutral styling.

### Story Table

Columns: ID, Title, State, Priority, Labels, Assignee, Updated.

- **Row highlighting**: Blocked stories have a red-tinted background with a red left border. Ready stories have a green-tinted background with a green left border.
- **Sorting**: Click any column header (except Labels) to sort. Click again to reverse. Sort direction shown with an arrow indicator. Sort state persists across data refreshes.
- **Filtering**: Text search across ID, title, and labels. Multi-select dropdowns for state and priority. Filter state persists across data refreshes.
- **Empty state**: Shows "No stories match the current filters" when all stories are filtered out.

### Auto-Refresh

The dashboard polls `GET /api/data` every 3 seconds via `setInterval`. The footer shows "Updated N seconds ago".

### Connection Indicator

Top-right corner. Green dot with "Live" when the last fetch succeeded within the past 10 seconds. Red dot with "Disconnected" when the server is unreachable (e.g., after `story web stop`).

### Dark Mode

Automatic via `@media (prefers-color-scheme: dark)`. Uses a slate color palette matching the static HTML report.

### Mobile Responsive

Below 768px viewport width:
- Stat cards stack vertically
- Filter controls stack vertically
- Story table scrolls horizontally
- Header elements stack

## API Reference

### `GET /`

Returns the dashboard HTML page.

| Header | Value |
|--------|-------|
| `Content-Type` | `text/html; charset=utf-8` |
| `Cache-Control` | `no-cache` |
| `X-Content-Type-Options` | `nosniff` |
| `X-Frame-Options` | `DENY` |
| `Content-Security-Policy` | `default-src 'self'; script-src 'unsafe-inline'; style-src 'unsafe-inline'` |

### `GET /api/data`

Returns the full project state as JSON.

| Header | Value |
|--------|-------|
| `Content-Type` | `application/json` |
| `Cache-Control` | `no-cache` |
| Security headers | Same as above |

Response schema:

```json
{
  "summary": {
    "total_open": 5,
    "total_closed": 3,
    "by_state": [["done", 3], ["in-progress", 2], ["todo", 3]],
    "by_priority": [["high", 2], ["medium", 1]],
    "by_type": [["Default", 6], ["epic", 2]],
    "blocked_count": 1,
    "flagged_count": 0,
    "ready_count": 4,
    "ready_stories": []
  },
  "stories": [
    {
      "story": {
        "id": "SH-1",
        "title": "Implement login",
        "state": "in-progress",
        "superstate": "open",
        "priority": "high",
        "story_type": null,
        "labels": ["backend", "auth"],
        "assignee": "alice",
        "awaiting": null,
        "updated_at": "2026-04-07T12:00:00Z",
        "closed_at": null,
        "comments": [],
        "relationships": []
      },
      "is_ready": true,
      "is_blocked": false,
      "derived_relationships": [],
      "warnings": [],
      "flagged_reasons": []
    }
  ],
  "ready_ids": ["SH-1", "SH-3"],
  "blocked_ids": ["SH-4"]
}
```

Key details:
- `by_state`, `by_priority`, `by_type` are arrays of `[name, count]` tuples
- `is_ready` and `is_blocked` are injected per-story by the web module (not part of the core `StoryView` struct)
- Empty projects return `total_open: 0`, `stories: []`, `ready_ids: []`, `blocked_ids: []`
- On internal error, returns 500 with `{"error": "<message>"}`

### Other Paths

- Any path not listed above returns `404 Not found` (plain text)
- Any non-GET method returns `405 Method not allowed` (plain text)
- Both include the same security headers as successful responses

## Configuration

| Setting | Source | Default | Description |
|---------|--------|---------|-------------|
| Port | `--port` flag | `3456` | TCP port for the HTTP server |
| Bind address | Hardcoded | `127.0.0.1` | Not configurable (security) |
| Poll interval | Hardcoded in JS | `3000` ms | Dashboard refresh interval |
| Connection timeout | Hardcoded in JS | `8000` ms | XHR timeout per request |
| Disconnect threshold | Hardcoded in JS | `10000` ms | Time before "Disconnected" indicator |

### web-serve Integration

In agentsmith environments where `web-serve` is available in PATH:
- `story web start` automatically runs `web-serve register <port>` after spawning the daemon
- `story web stop` automatically runs `web-serve unregister` after killing the daemon
- Registration failures are silent -- they do not prevent start/stop from succeeding

## Architecture Decision Records

### ADR-001: tiny_http Over Async Alternatives

**Status**: Accepted

**Context**: The web dashboard needs an HTTP server. Options considered: `tiny_http` (synchronous, minimal), `actix-web` (async, full-featured), `axum` (async, tower-based), `warp` (async, filter-based).

**Decision**: Use `tiny_http`. It has no async runtime dependency, compiles fast, and adds minimal binary size. The server handles one request at a time, which is sufficient for a local dashboard with a single user.

**Consequences**: No async code in the codebase. Thread pool handles concurrency transparently. If the server ever needs SSE or WebSocket, `tiny_http` would need to be replaced -- but that is an explicit non-goal for now.

### ADR-002: Client-Side Polling Over Server-Sent Events

**Status**: Accepted

**Context**: The dashboard needs live data. Options: SSE (server pushes), WebSocket (bidirectional), client-side polling (browser pulls).

**Decision**: Use 3-second client-side polling via `setInterval` + XHR. `tiny_http`'s thread pool model makes long-lived SSE connections impractical -- each connection would consume a thread indefinitely, eventually exhausting the pool.

**Consequences**: Slightly higher latency (up to 3 seconds) than push-based approaches. Slightly more load on disk (re-reads events on every poll). Both are negligible for local use. Polling is trivially debuggable -- open `/api/data` in a browser tab.

### ADR-003: Embedded HTML via include_str!

**Status**: Accepted

**Context**: The dashboard is a single HTML page with inline CSS and JS. Options: embed in binary via `include_str!`, serve from a file on disk, use a template engine.

**Decision**: Embed via `include_str!("web_dashboard.html")`. The dashboard ships as part of the binary with zero runtime file dependencies.

**Consequences**: Changing the dashboard requires recompiling. This is acceptable because the dashboard is tightly coupled to the JSON API contract. No risk of file-not-found errors at runtime.

### ADR-004: Localhost-Only Binding Without Authentication

**Status**: Accepted

**Context**: The dashboard exposes project data (story titles, assignees, comments). Options: bind all interfaces with auth, bind localhost only without auth, bind all interfaces without auth.

**Decision**: Bind `127.0.0.1` only. No authentication. In agentsmith environments, `web-serve register` handles external access through the host's reverse proxy, which has its own access controls.

**Consequences**: Dashboard is inaccessible from other machines unless `web-serve` or SSH tunneling is used. This is the correct default -- project data should not be network-accessible without explicit opt-in.

### ADR-005: fs4 File Locking for Daemon Lifecycle

**Status**: Accepted

**Context**: The daemon needs to prevent double-starts and detect stale PIDs. Options: PID file existence checks (simple, TOCTOU race), `fs4` advisory file locks (atomic, race-free), socket-based locking.

**Decision**: Use `fs4::FileExt::try_lock_exclusive` on `.storyhook/web.lock`. The lock is held by the running daemon. If `story web start` cannot acquire the lock, it knows another instance is running. The codebase already uses this pattern in `src/lock.rs`.

**Consequences**: Atomic. No TOCTOU race between checking and starting. If the daemon crashes, the OS releases the lock automatically. PID file is still used for port and PID information, but the lock file is the source of truth for "is it running?".

## Known Issues (Resolved)

Five issues were found during review and fixed before merge.

### 1. Server Bound to 0.0.0.0 Instead of 127.0.0.1

**Severity**: Critical. The server was listening on all network interfaces, exposing project data to the LAN and Tailscale mesh without authentication.

**Resolution**: Changed bind address from `"0.0.0.0:{port}"` to `"127.0.0.1:{port}"` in `src/web.rs`.

### 2. JSON Injection in Error Response

**Severity**: Critical. The `/api/data` error path used `format!()` to build JSON, so error messages containing double quotes produced malformed JSON.

**Resolution**: Replaced with `serde_json::json!({"error": e.to_string()}).to_string()`, which handles escaping correctly.

### 3. Lock File Used Existence Check Instead of fs4 Flock

**Severity**: Important. The daemon lifecycle used `lock_path.exists()` instead of `fs4::FileExt::try_lock_exclusive`, creating a TOCTOU race where two `story web start` commands could both think no server was running.

**Resolution**: Implemented `fs4` exclusive locking, matching the existing pattern in `src/lock.rs`.

### 4. Missing Security Headers

**Severity**: Important. HTTP responses lacked standard security headers, leaving the dashboard vulnerable to clickjacking and MIME-sniffing.

**Resolution**: Added `X-Content-Type-Options: nosniff`, `X-Frame-Options: DENY`, and `Content-Security-Policy` to all responses.

### 5. handle_stop Used Shell kill Instead of libc::kill

**Severity**: Important. The stop handler shelled out to `/usr/bin/kill` instead of using `libc::kill`, which is less robust and PATH-dependent.

**Resolution**: Changed to `unsafe { libc::kill(pid as i32, libc::SIGTERM) }` with `#[cfg(unix)]`, matching the plan specification.

## Deferred Items

Four items were identified during review and deferred as low-priority improvements.

### 1. Daemon Spawner Does Not Verify Server Started

`handle_start` returns success immediately after spawning the child process without verifying the server actually bound the port. If the child crashes immediately, the user sees "started" but the server is dead. Workaround: check `.storyhook/web.log` if the dashboard is unreachable.

### 2. build_api_json Uses Mutable JSON Instead of Typed Struct

The `build_api_json` function serializes `StoryView` to `serde_json::Value` then mutates the JSON object to inject `is_ready`/`is_blocked`. A typed `WebStoryView` wrapper would be cleaner. The current code works correctly.

### 3. is_process_alive Only Checks /proc on Linux

PID ownership verification reads `/proc/{pid}/cmdline`, which only works on Linux. The fallback (`kill -0`) checks process existence but not ownership, so PID reuse on macOS could theoretically target the wrong process. Per plan: "skip verification on other OS."

### 4. reachable_ip() Uses Linux-Only Commands

The `reachable_ip()` function shells out to `tailscale ip -4` and `hostname -I`, neither of which exist on macOS. Falls back to `127.0.0.1` safely. This is display-only and does not affect server behavior.

## Test Coverage

**706 tests total** (390 lib + 41 web integration), all passing. Zero mocks -- all tests use real filesystem, real HTTP server, and real CLI binary.

**24 tests added** during validation to close coverage gaps.

### Requirement Coverage Summary

| Area | Coverage | Notes |
|------|----------|-------|
| CLI parsing (all subcommands, flags, errors) | Full | 13 tests |
| HTTP routes (/, /api/data, 404, 405) | Full | 6 tests |
| JSON API contract (matches dashboard JS) | Full | 1 dedicated contract test |
| build_report_data correctness | Full | 5 tests (empty, mixed, blocked, priorities, types) |
| Special characters and unicode | Full | 2 tests (XSS payload, CJK) |
| Cache-Control headers | Full | 2 tests |
| Concurrent requests | Full | 1 test (10 parallel) |
| Port validation boundaries | Full | 6 tests (0, 1, 65535, 65536, negative, non-numeric) |
| Daemon lifecycle (start/stop/status) | Partial | "Not running" paths tested; full spawn/kill cycle is manual-test-only |
| Port-in-use error | Not tested | Race condition makes this flaky in CI |
| web-serve registration | Not tested | External tool dependency |

Accepted gaps are structural: daemon lifecycle testing requires building the binary and managing real processes, which is fragile in CI. The daemon code is straightforward and was thoroughly reviewed.
