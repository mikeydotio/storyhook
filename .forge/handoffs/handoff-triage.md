# Handoff: Triage → Plan (Fix Cycle 1)

## Step Completed
triage

## Artifacts Produced
- `.forge/TRIAGE.md` — 5 FIX, 0 ESCALATE, 4 deferred

## Key Decisions
- All 5 fixable findings have unambiguous solutions — no ESCALATE needed
- 2 Critical (security): bind address, JSON injection — both one-line fixes
- 3 Important: fs4 flock, security headers, libc::kill — all follow plan/patterns
- 4 Useful items deferred (daemon verification, typed struct, macOS proc, Linux-only IP)

## Context for Next Step

### FIX Items for Plan
1. **src/web.rs:15**: `"0.0.0.0:{port}"` → `"127.0.0.1:{port}"`
2. **src/web.rs:58**: `format!` → `serde_json::json!({"error": e.to_string()}).to_string()`
3. **src/web.rs:167-183**: Replace file existence check with `fs4::FileExt::try_lock_exclusive`
4. **src/web.rs:46-84**: Add X-Content-Type-Options, X-Frame-Options, CSP headers to all responses
5. **src/web.rs:251-253**: Replace `Command::new("kill")` with `libc::kill()` under `#[cfg(unix)]`

### Complexity Assessment
- Items 1-2: trivial (one-line each)
- Item 3: small (follow existing `lock.rs` pattern)
- Item 4: small (add header to response builder)
- Item 5: small (replace shell-out with libc call)

All 5 can go in a single wave. Estimated: 1 story, ~30 lines changed.

## Pipeline State
- Fix cycle: 0 → entering cycle 1
- Yolo mode: false
- ESCALATE stories: 0
