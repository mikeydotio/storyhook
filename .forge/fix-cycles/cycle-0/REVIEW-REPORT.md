# Review Report

## Summary

The web dashboard implementation is solid: clean module boundaries, proper XSS prevention, consistent codebase patterns, and reasonable test coverage. However, there is one **critical security defect** — the HTTP server binds to `0.0.0.0` instead of `127.0.0.1`, exposing project data to the network without authentication. There is also a JSON injection bug in the error response path. The daemon lifecycle uses file-existence checks instead of the planned `fs4` file locking, creating a TOCTOU race condition.

## Findings

### Server Binds to 0.0.0.0 Instead of 127.0.0.1
- **Severity**: Critical
- **Description**: The server binds to `0.0.0.0:{port}` (all interfaces) instead of `127.0.0.1:{port}` (localhost only). The plan explicitly states: "Bind address: 127.0.0.1 by default (security: don't expose project data on network)". With `0.0.0.0`, anyone on the LAN, Tailscale mesh, or Docker bridge network can read all project data (story titles, assignees, comments, priorities) at `/api/data` without any authentication. The stderr message even prints `127.0.0.1` giving a false impression of localhost binding.
- **Location**: `src/web.rs:15`
- **Flagged by**: reviewer, software-architect, security-researcher (3/3 agents)
- **Option 1 (Recommended)**: Change `"0.0.0.0:{port}"` to `"127.0.0.1:{port}"` — Pros: One-line fix, matches plan, secure by default. Cons: None; `web-serve register` already handles the external access case.
- **Option 2**: Add `--bind` flag defaulting to `127.0.0.1`, requiring explicit opt-in for network exposure — Pros: Flexible. Cons: More work, YAGNI.

### JSON Injection in /api/data Error Response
- **Severity**: Critical
- **Description**: The error path constructs JSON via `format!("{{\"error\": \"{}\"}}", e)`. If the `AppError` message contains double quotes (e.g., from file paths), the resulting JSON is malformed. OS error messages can contain arbitrary characters.
- **Location**: `src/web.rs:58`
- **Flagged by**: reviewer, software-architect, security-researcher (3/3 agents)
- **Option 1 (Recommended)**: Use `serde_json::json!({"error": e.to_string()}).to_string()` — Pros: One-line fix, proper escaping. Cons: None.
- **Option 2**: Use `serde_json::to_string` on a struct — Pros: Consistent with other paths. Cons: Heavier for a simple error.

### Lock File Uses Existence Check Instead of fs4 Flock
- **Severity**: Important
- **Description**: Plan specifies `fs4::FileExt::try_lock_exclusive` for daemon lifecycle. Implementation uses `lock_path.exists()` + PID verification — non-atomic, has a TOCTOU race window. The existing `src/lock.rs` already demonstrates the correct `fs4` pattern.
- **Location**: `src/web.rs:167-183`
- **Flagged by**: reviewer, software-architect, security-researcher (3/3 agents)
- **Option 1 (Recommended)**: Use `fs4::FileExt::try_lock_exclusive` matching the `lock.rs` pattern — Pros: Race-free, consistent with codebase. Cons: Slightly more code.
- **Option 2**: Keep current approach, document the race — Pros: Simple. Cons: Technical debt.

### Missing Security Headers on Responses
- **Severity**: Important
- **Description**: HTTP responses lack `X-Content-Type-Options: nosniff`, `X-Frame-Options: DENY`, and `Content-Security-Policy`. Without these, the dashboard is vulnerable to clickjacking (iframe embedding) and MIME-sniffing.
- **Location**: `src/web.rs:46-84`
- **Flagged by**: security-researcher
- **Option 1 (Recommended)**: Add `X-Content-Type-Options: nosniff`, `X-Frame-Options: DENY`, `Content-Security-Policy: default-src 'self'; script-src 'unsafe-inline'; style-src 'unsafe-inline'` to all responses — Pros: Defense in depth. Cons: Slightly more verbose.
- **Option 2**: Add headers only to the HTML response — Pros: Simpler. Cons: Inconsistent.

### handle_stop Uses Shell `kill` Instead of libc::kill
- **Severity**: Important
- **Description**: Plan specifies `libc::kill(pid, libc::SIGTERM)`. Implementation shells out to `Command::new("kill")` and discards failures with `let _ =`. Less robust and less portable.
- **Location**: `src/web.rs:251-253`
- **Flagged by**: reviewer, software-architect
- **Option 1 (Recommended)**: Use `unsafe { libc::kill(pid as i32, libc::SIGTERM) }` with `#[cfg(unix)]` — Pros: No fork, no PATH dependency. Cons: Requires `unsafe` block.
- **Option 2**: Keep shell-out but check exit status — Pros: No `unsafe`. Cons: Still PATH-dependent.

### Daemon Spawner Does Not Verify Server Actually Started
- **Severity**: Useful
- **Description**: `handle_start` spawns the child process and immediately returns success without verifying the server bound the port. If the child crashes immediately, the user sees "started" but the server is dead.
- **Location**: `src/web.rs:198-229`
- **Flagged by**: software-architect
- **Option 1 (Recommended)**: Poll `TcpStream::connect` with 1-2s timeout after spawn (same pattern as test `wait_for_server`) — Pros: Immediate feedback on failures. Cons: Adds 1-2s to startup.
- **Option 2**: Add note in output: "Check .storyhook/web.log if dashboard unreachable" — Pros: No delay. Cons: Users discover failure late.

### build_api_json Augments StoryView via Mutable JSON
- **Severity**: Useful
- **Description**: `build_api_json` serializes `StoryView` to `serde_json::Value`, then mutates the JSON object to inject `is_ready`/`is_blocked`. This is fragile and relies on untyped territory.
- **Location**: `src/web.rs:288-319`
- **Flagged by**: software-architect
- **Option 1 (Recommended)**: Create a `WebStoryView` wrapper with `#[serde(flatten)]` — Pros: Type-safe. Cons: One more struct.
- **Option 2**: Add fields to `StoryView` with `skip_serializing_if` — Pros: No new struct. Cons: Web concerns in shared type.

### is_process_alive Only Checks /proc on Linux
- **Severity**: Useful
- **Description**: `/proc/{pid}/cmdline` check works on Linux but not macOS. The fallback (`kill -0`) only checks process existence, not ownership, so PID reuse could kill an unrelated process on macOS.
- **Location**: `src/web.rs:114-126`
- **Flagged by**: software-architect, security-researcher
- **Option 1 (Recommended)**: On macOS, use `ps -p {pid} -o comm=` and check for "story" — Pros: Cross-platform PID ownership. Cons: More platform code.
- **Option 2**: Accept current behavior per plan ("skip verification on other OS") — Pros: Less code. Cons: PID reuse risk on macOS.

### reachable_ip() Reveals Network Topology and Uses Linux-Only Commands
- **Severity**: Useful
- **Description**: Shells out to `tailscale ip -4` and `hostname -I` (Linux-only). Combined with `0.0.0.0` binding, actively encourages users to share network-exposed URLs. Once binding is fixed to `127.0.0.1`, this becomes moot for the URL display.
- **Location**: `src/web.rs:129-153`
- **Flagged by**: security-researcher
- **Option 1 (Recommended)**: After fixing bind to `127.0.0.1`, always display `127.0.0.1` in URL. Only show network IP if explicit `--bind` flag used — Pros: Consistent with security posture. Cons: Requires coordinating with bind fix.

## Design Alignment

**MINOR DRIFT** — The implementation follows the planned architecture closely (tiny_http, embedded HTML, 3s polling, PID file, web-serve integration, CLI structure, build_report_data extraction). Three specific deviations:

1. **0.0.0.0 vs 127.0.0.1 binding** (Critical — contradicts security decision)
2. **File existence vs fs4 flock** (Important — loses atomicity)
3. **Shell kill vs libc::kill** (Minor — same behavior, different mechanism)

## Dependency Audit

| Dependency | Advisory | Risk | Action |
|-----------|----------|------|--------|
| tiny_http 0.12 | None | Low | Current |
| serde_yml 0.0.12 | RUSTSEC-2025-0068 (unsound) | Medium | Replace with serde_yaml (separate concern) |
| libyml 0.0.5 | RUSTSEC-2025-0067 (unsound) | Medium | Transitive via serde_yml |

## Strengths

- **XSS prevention**: Dashboard uses `textContent`-based `esc()` function for all user-controlled content — sound pattern
- **Clean module boundaries**: `web.rs` depends only on `app::build_report_data`, `storage::ensure_project`, and `error::AppError`
- **Filter/sort state preservation**: Dashboard preserves user's filter and sort state across polling refreshes
- **Stale PID detection**: Thorough handling in start/stop/status with automatic cleanup
- **Strict URL routing**: Exact-match `match url.as_str()` completely eliminates path traversal
- **Zero external frontend dependencies**: Self-contained HTML, no CDN or framework
- **Method whitelisting**: Only GET accepted, all other methods get 405
- **Consistent codebase patterns**: Follows `tui`/`--mcp` dispatch pattern in main.rs
