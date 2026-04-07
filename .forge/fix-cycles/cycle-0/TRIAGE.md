# Triage Report

## Summary
- Total findings: 9 (review: 9, validate: 0 outstanding — all validate gaps were filled by tests)
- FIX: 5
- ESCALATE: 0
- Deferred: 4 (Useful severity, no action required this cycle)
- Yolo mode: false
- Fix cycle: 0 / 3

## FIX Items

### Server Binds to 0.0.0.0 Instead of 127.0.0.1 — FIX
- **Source**: REVIEW-REPORT
- **Severity**: Critical
- **Chosen Solution**: Change `"0.0.0.0:{port}"` to `"127.0.0.1:{port}"` at `src/web.rs:15`
- **Rationale**: FIX — single unambiguous solution. The plan explicitly specifies `127.0.0.1`. This is a one-line change. All 3 review agents flagged this. No trade-offs — `web-serve register` already handles the external access case.

### JSON Injection in /api/data Error Response — FIX
- **Source**: REVIEW-REPORT
- **Severity**: Critical
- **Chosen Solution**: Replace `format!("{{\"error\": \"{}\"}}", e)` with `serde_json::json!({"error": e.to_string()}).to_string()` at `src/web.rs:58`
- **Rationale**: FIX — single unambiguous solution. `serde_json` handles escaping correctly. One-line change. All 3 review agents flagged this.

### Lock File Uses Existence Check Instead of fs4 Flock — FIX
- **Source**: REVIEW-REPORT
- **Severity**: Important
- **Chosen Solution**: Use `fs4::FileExt::try_lock_exclusive` on the lock file, matching the `src/lock.rs` pattern. Acquire exclusive lock before checking/writing PID file.
- **Rationale**: FIX — the plan specifies `fs4`, the codebase already has the pattern in `lock.rs`, and `fs4` is already a dependency. Clear solution, eliminates TOCTOU race.

### Missing Security Headers on Responses — FIX
- **Source**: REVIEW-REPORT
- **Severity**: Important
- **Chosen Solution**: Add `X-Content-Type-Options: nosniff`, `X-Frame-Options: DENY`, `Content-Security-Policy: default-src 'self'; script-src 'unsafe-inline'; style-src 'unsafe-inline'` to all responses.
- **Rationale**: FIX — standard security practice, no design decision needed. Defense in depth. Clear implementation.

### handle_stop Uses Shell kill Instead of libc::kill — FIX
- **Source**: REVIEW-REPORT
- **Severity**: Important
- **Chosen Solution**: Use `unsafe { libc::kill(pid as i32, libc::SIGTERM) }` with `#[cfg(unix)]`. Check return value. Add `libc` to explicit dependencies.
- **Rationale**: FIX — plan specifies `libc::kill`. Clear solution. `libc` is likely already a transitive dependency via `fs4`.

## Deferred Items (Useful — no action this cycle)

### Daemon Spawner Does Not Verify Server Started
- **Source**: REVIEW-REPORT
- **Severity**: Useful
- **Rationale**: Deferred — adds 1-2s to startup path. The startup failure case is handled by log file. Low impact, clear fallback exists.

### build_api_json Uses Mutable JSON Instead of Typed Struct
- **Source**: REVIEW-REPORT
- **Severity**: Useful
- **Rationale**: Deferred — code works correctly. The `WebStoryView` wrapper would be cleaner but is a refactor, not a fix.

### is_process_alive Only Checks /proc on Linux
- **Source**: REVIEW-REPORT
- **Severity**: Useful
- **Rationale**: Deferred — plan explicitly says "skip verification on other OS". macOS support is out of current scope.

### reachable_ip() Uses Linux-Only Commands
- **Source**: REVIEW-REPORT
- **Severity**: Useful
- **Rationale**: Deferred — falls back to `127.0.0.1` safely. Display-only function. Becomes moot after bind address fix.

## ESCALATE Items

None. All findings have clear, unambiguous solutions that follow the plan spec, existing codebase patterns, or standard security practices. No user decision needed.
