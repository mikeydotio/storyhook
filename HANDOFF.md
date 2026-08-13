# Handoff — SH-255, implementation complete; the merge gate and the PR are what's left

**Direction changed mid-story.** SH-255 was filed as a narrow keep/narrow/retire
decision on SH-250's loopback read exemption. Investigation found the story's own
option set was partly moot (retiring and narrowing are the same change, since
`GET /` is served credential-free regardless of the exemption). Mikey then
redirected the story: replace the whole credential model rather than adjudicate
one exception. Full record: comments on SH-255 (direction-change, plan, council
verdict, and four progress updates — most recently after off-tailnet refusal
landed).

## The decided design — all six council questions now built

One credential concept — a named, persistent, cookie-carried token — replaces the
loopback exemption, `Authority::Public`/`Session`, and SH-254's session capability.
Full spec is the council verdict comment on SH-255 and
`.council/sh-255-named-token-model/DECISION.md` in the **main repo** (not this
worktree — `.council/` is gitignored and shared).

- Sidecar storage (`tokens.json`, 0600, in `daemon_state_dir()`), **not** SQLite. Built.
- Cookie: `storyhook_<StoreLocation::key()>`, HttpOnly, SameSite=Strict, Max-Age =
  remaining TTL, no `Domain`. Built.
- Reads need `Sec-Fetch-Site: same-origin`. Mutations keep the existing
  `X-Storyhook`+`Host` guard, unchanged. Built.
- Off-tailnet: bind-time refusal (`Listener::adopt`) AND a pure accept-time
  `peer_admitted` check, composed — never a `tailscale` CLI call on the request
  path (preserves SH-186). **Built this session** (commit `6a83f9b`).
- Handoff coupon (`POST /handoff`) survives, answering 204+`Set-Cookie` with no
  body, no XHR-to-redirect conversion. Built and e2e-verified.
- **Merge gate, not optional, and still outstanding**: a hand-run verification in
  real Chrome and Safari that `SameSite=Strict` rides `story web open`'s
  navigation and that `Sec-Fetch-Site` is sent as expected — SH-251's standing
  rule against inferring browser behavior from a green suite. **This is the one
  thing standing between here and a PR.**

## What's built

Everything (commits `25d3b67` through `6a83f9b`, all on `worktree-SH-255`, rebased
onto current `main`):

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
  cookie exchange; `?token=` is gone from `connectEvents`; the entire SH-254
  session-capability apparatus removed; `redeemHandoff()` fixed to match the
  actual `204` response (was still checking the pre-SH-255 `200 + {session}`
  shape, so a *successful* redemption showed a "not accepted" toast).
- **Off-tailnet refusal** (`src/daemon/serve.rs`, `src/daemon/http1/conn.rs`):
  `Listener::adopt(listener, Expected::Loopback | Expected::Tailnet(ip))` refuses
  a mismatched bind; `peer_admitted(bind_addr, peer_addr)` is a pure predicate
  (loopback listener admits only loopback; tailnet listener admits loopback, the
  bind address itself as a peer — same-machine connections don't always route
  through `lo` — Tailscale's CGNAT range, and its IPv6 ULA range), enforced in
  `serve_connections` on the accept thread before a thread is spawned or a
  `Request` built.
- **Retired**: `token_exempt` and its truth table, `session.rs` and its suite,
  `Authority`/`authority()`/`project_authority()` in `src/api/routes.rs` (deleted
  outright, not collapsed to two levels — nothing in the runtime path classifies
  per-route authority any more), `story web revoke`/`story web status`'s
  session-counting meaning, the startup banner's stale "loopback reads need no
  token" message.
- e2e: `loopback-no-token.spec.ts` → `loopback-requires-a-token.spec.ts`
  (inverted); `handoff.spec.ts` (3 of 4 tests rewritten); `dispatch.spec.ts`'s AC2
  restructured (auth now happens on the first read, not the Dispatch click).
- Docs: README's Security, Reverse-proxying, and Network exposure sections
  rewritten for the named-token model; `docs/spec/dashboard-authorization.md`'s
  new `## As built — SH-255` section is the story's full record.

**Gate: full `make test` green** — fmt, clippy, 3506 Rust tests, the plugin bash
harness, all 111 e2e specs including the mobile suite. Verified via
`cargo test --workspace --no-fail-fast` standalone and the real `make test`
target, both clean after off-tailnet refusal landed.

**Mutation-checked by hand**: widened the CGNAT range, disabled
`Listener::adopt`'s refusal, forced the accept-time `admit` closure to always
refuse — each turned a specific test red, reverted after confirming. (One
self-inflicted lesson from this pass, worth repeating: don't mutate a source
file for a manual mutation-check while a separate background full-suite compile
is still using that same file — it can build the test binary against the
mutated state and produce a wall of unrelated-looking failures. Revert fully,
or wait, before starting the next background compile.)

**Also done, not originally scoped as its own step**: rebased onto `origin/main`
(50 commits of drift, including SH-222's PR #335 touching 24 e2e spec files'
`beforeEach` hooks, flagged by name in an earlier SH-255 comment) — no conflicts.

## What's left

1. **The merge gate**: hand-run verification in real Chrome and Safari (isolated
   profile, matching SH-251's own experimental record) —
   - Set a `SameSite=Strict` cookie on `http://127.0.0.1:PORT`, `open(1)` it
     (i.e. what `story web open` actually does), confirm the cookie rides the
     resulting navigation.
   - Click a cross-origin link into the dashboard and confirm the *document*
     request lacks the cookie while the page's own same-origin XHRs still carry
     it.
   - Confirm `Sec-Fetch-Site: same-origin` arrives on the page's own reads.
   This session's Playwright MCP tools drive real Chromium and can likely stand
   in for the Chrome half; Safari needs an actual machine. Record the result as
   a comment on SH-255 either way — a failed verification is a finding, not a
   blocker to silently work around.
2. PR referencing SH-255; comment the link back onto the story. **Stop there** —
   this is a linked worktree, no version bump, no deploy.

## Gate

`make test`, supervised in the background with log-growth as the heartbeat and a
stall bound, per this repo's standing rule. Never bump the version or deploy from
this worktree.
