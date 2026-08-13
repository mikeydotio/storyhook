# Handoff — SH-255, exemption retired and e2e green, off-tailnet refusal next

**Direction changed mid-story.** SH-255 was filed as a narrow keep/narrow/retire
decision on SH-250's loopback read exemption. Investigation found the story's own
option set was partly moot (retiring and narrowing are the same change, since
`GET /` is served credential-free regardless of the exemption). Mikey then
redirected the story: replace the whole credential model rather than adjudicate
one exception. Full record: comments on SH-255 (direction-change, plan, council
verdict, and three progress updates — most recently after this session).

## The decided design

One credential concept — a named, persistent, cookie-carried token — replaces the
loopback exemption, `Authority::Public`/`Session`, and SH-254's session capability.
Full spec (6 questions, all resolved) is the council verdict comment on SH-255 and
`.council/sh-255-named-token-model/DECISION.md` in the **main repo** (not this
worktree — `.council/` is gitignored and shared). Highlights:

- Sidecar storage (`tokens.json`, 0600, in `daemon_state_dir()`), **not** SQLite.
- Cookie: `storyhook_<StoreLocation::key()>`, HttpOnly, SameSite=Strict, Max-Age =
  remaining TTL, no `Domain`.
- Reads need `Sec-Fetch-Site: same-origin` (not just `SameSite=Strict` — every
  tailnet peer and every other loopback port is same-site). Mutations keep the
  existing `X-Storyhook`+`Host` guard, unchanged.
- **Off-tailnet: bind-time refusal AND a pure accept-time `peer_admitted` check,
  composed — never a `tailscale` CLI call on the request path (preserves
  SH-186). Not yet built — this is the next step.**
- Handoff coupon (`POST /handoff`) survives, answering 204+`Set-Cookie` with no
  body, no XHR-to-redirect conversion. Built and e2e-verified this session.
- **Merge gate, not optional**: a hand-run verification in real Chrome and Safari
  that `SameSite=Strict` rides `story web open`'s navigation and that
  `Sec-Fetch-Site` is sent as expected — SH-251's standing rule against inferring
  browser behavior from a green suite. Not yet run.

## What's built

Everything through "retire the exemption" and the e2e catch-up it required
(commits `25d3b67` through `359ce1a`, all on `worktree-SH-255`, rebased onto
current `main`):

- `src/api/tokens.rs` — `TokenRegistry`: mint/list/revoke/validate, sidecar
  persistence, monotonic-floor + persisted-high-water clock.
- `story token new|list|revoke`, wired to a live daemon route.
- Cookie issuance: `Reply::with_cookie`/`cookie()`; `POST /handoff` mints a
  `HANDOFF_TTL` (24h) named token and sets it as a cookie; `DELETE /handoff`
  clears it.
- `admission.rs`: one uniform gate for every `/api/**` route — master token,
  header-borne named token, or cookie-borne named token (reads additionally need
  `Sec-Fetch-Site: same-origin`). No per-route classification any more.
- `web_dashboard.html`: the token modal posts a pasted value to `/token` for a
  cookie exchange; `?token=` is gone from `connectEvents`; **this session** also
  removed the entire SH-254 session-capability apparatus that never got cleaned
  out (`SESSION_KEY`, `sessionOnly()`, `credentialVerdict`, `sessionEnded`,
  `attachCredentials`, the three UI controls they used to disable) and fixed a
  real bug — `redeemHandoff()` was still checking for the pre-SH-255 `200 +
  {session}` response shape, so a *successful* coupon redemption showed a "not
  accepted" toast (the cookie still got set; only the message was wrong).
- **Retired**: `token_exempt` and its truth table, `session.rs` and its suite,
  `Authority`/`authority()`/`project_authority()` in `src/api/routes.rs` (deleted
  outright, not collapsed to two levels — nothing in the runtime path classifies
  per-route authority any more), `story web revoke`/`story web status`'s
  session-counting meaning, the startup banner's stale "loopback reads need no
  token" message.
- e2e: `loopback-no-token.spec.ts` → `loopback-requires-a-token.spec.ts`
  (inverted); `handoff.spec.ts` (3 of 4 tests rewritten — the cookie is
  `HttpOnly` so tests can no longer read it back, only prove it authenticates);
  `dispatch.spec.ts`'s AC2 restructured (auth now happens on the first read, not
  the Dispatch click).
- README.md's Security and Reverse-proxying sections rewritten for the
  named-token model (a stale `story web revoke` reference was failing
  `tests/readme_command_reference.rs`, which is what surfaced this).

**Gate: full `make test` green** — fmt, clippy, 3494 Rust tests, the plugin bash
harness, all 111 e2e specs including the mobile suite. Verified twice: once via
`cargo test --workspace --no-fail-fast` standalone (surfaces every failure at
once rather than stopping at the first), once via the real `make test` target.

**Also done this session, not originally scoped as its own step**: rebased onto
`origin/main` (50 commits of drift, including SH-222's PR #335 touching 24 e2e
spec files' `beforeEach` hooks, flagged by name in an earlier SH-255 comment) —
no conflicts.

## What's left, in dependency order

1. **Off-tailnet refusal** — bind-time (`Listener::adopt` refuses a non-loopback,
   non-tailnet bind) and accept-time (`peer_admitted(bind_addr, peer_addr)`, pure,
   zero-syscall). See council decision 4 on SH-255 for the exact ranges and the
   `tailscale`-CLI-on-request-path prohibition (SH-186).
2. Docs: `## As built — SH-255` appended to `docs/spec/dashboard-authorization.md`
   (its own per-story convention; repoint its lines 338/506 hooks). README's
   security section is already done — this is the one doc still outstanding.
3. Mutation checks on the new guards (introduce a wildcard authority arm — though
   there's no `Authority` left to widen, so this may already be moot; a token
   that skips the hash comparison; a cookie without `SameSite`; confirm each
   turns a test red; revert) + the real-browser `SameSite`/`Sec-Fetch-Site`
   verification (Chrome and Safari, by hand or via this session's Playwright MCP
   tools).
4. Re-run full `make test` after step 1 lands.
5. PR referencing SH-255; comment the link back onto the story. **Stop there** —
   this is a linked worktree, no version bump, no deploy.

## Gate

`make test`, supervised in the background with log-growth as the heartbeat and a
stall bound, per this repo's standing rule. Never bump the version or deploy from
this worktree.
