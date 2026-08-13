# Handoff — SH-255, mid-implementation, foundation landed

**Direction changed mid-story.** SH-255 was filed as a narrow keep/narrow/retire
decision on SH-250's loopback read exemption. Investigation found the story's own
option set was partly moot (retiring and narrowing are the same change, since
`GET /` is served credential-free regardless of the exemption). Mikey then
redirected the story: replace the whole credential model rather than adjudicate
one exception. Full record: two comments on SH-255 (direction-change + plan), plus
a third comment carrying the design council's verdict.

## The decided design

One credential concept — a named, persistent, cookie-carried token — replaces the
loopback exemption, `Authority::Public`/`Session`, and SH-254's session capability.
Full spec (6 questions, all resolved) is the council verdict comment on SH-255 and
`.council/sh-255-named-token-model/DECISION.md` in the **main repo** (not this
worktree — `.council/` is gitignored and shared). Highlights:

- Sidecar storage (`tokens.json`, 0600, in `daemon_state_dir()`), **not** SQLite —
  decided after the chair verified directly against the code that
  `nested_lane`/`is_nested_invoke` (the mechanism one proposal relied on to keep
  SQLite) cannot serve an ordinary accept-thread write without either reopening
  the exact back-pressure bypass its own doc comment forbids, or falling through
  to the bounded pool.
- Cookie: `storyhook_<StoreLocation::key()>`, HttpOnly, SameSite=Strict, Max-Age =
  remaining TTL, no `Domain`.
- Reads need `Sec-Fetch-Site: same-origin` (not just `SameSite=Strict` — every
  tailnet peer and every other loopback port is same-site). Mutations keep the
  existing `X-Storyhook`+`Host` guard, unchanged.
- Off-tailnet: bind-time refusal AND a pure accept-time `peer_admitted` check,
  composed — never a `tailscale` CLI call on the request path (preserves SH-186).
- Handoff coupon (`POST /handoff`) survives, now answering 204+`Set-Cookie` with
  no body, no XHR-to-redirect conversion.
- **Merge gate, not optional**: a hand-run verification in real Chrome and Safari
  that `SameSite=Strict` rides `story web open`'s navigation and that
  `Sec-Fetch-Site` is sent as expected — SH-251's standing rule against inferring
  browser behavior from a green suite. The Playwright MCP tools available in this
  session can likely stand in for this (real Chromium, not just persistence
  claims), but it hasn't been run yet.

## What's built (commit `25d3b67`)

`src/api/tokens.rs` — `TokenRegistry`: mint/list/revoke/validate, sidecar
persistence, monotonic-floor + persisted-high-water clock (revoke deletes the
record outright, so no clock manipulation resurrects one), `/api/v1/tokens`
wire gate. 21 tests, clippy clean, fmt clean. **Wired to nothing** — the old
exemption and SH-254's `session.rs` are both still fully intact and untouched, so
`make test` should still be green (not yet re-run after this commit landed).

`rand = "0.9"` added to `Cargo.toml` (already resolved in the lockfile
transitively, so this shouldn't perturb anything else).

## What's left, in dependency order

1. CLI: `story token new|list|revoke` (touches `cli.rs`, `web.rs` or a new
   handler module, `help_topics.rs`).
2. Cookie issuance: `Reply` has **no header-setting mechanism today** — needs a
   `set_cookie` field threaded through `finish()`. Wire `POST /handoff` to call
   `TokenRegistry::mint` (short TTL) and set the cookie.
3. `admission.rs`: accept a cookie/header-borne named token as an *additional*
   credential, additively — exemption stays for now, suite stays green.
4. `web_dashboard.html`: stop touching `sessionStorage` for credentials; the
   token modal posts a pasted token to be exchanged for a cookie; delete
   `?token=` from `connectEvents`. This is the highest-risk step — 283KB single
   file, hand-tuned JS, no build step to catch mistakes short of the browser
   itself.
5. Retire the exemption: delete `token_exempt` + its 14-test truth table, delete
   `session.rs` and its own suite, collapse `Authority` to two levels, invert
   `web_test.rs`'s three exemption tests + `proxy_trusted_hosts.rs` +
   `route_authority.rs`'s `public_means_exactly_what_the_loopback_read_exemption_admits`.
   Full characterization checklist (what's (a) deleted / (b) changes /
   (c) stays byte-identical) is in this session's transcript, not yet written to
   a file — worth extracting to a scratch note before starting this step.
6. Off-tailnet refusal (bind-time + `peer_admitted`).
7. e2e suite: `support.ts::seedToken` → cookie-based; `loopback-no-token.spec.ts`
   inverted/renamed; `dispatch.spec.ts` AC2 and `handoff.spec.ts`'s spent-coupon
   test updated. ~10 other specs need only the `seedToken` swap, no semantic
   changes.
8. Docs: `## As built — SH-255` appended to `docs/spec/dashboard-authorization.md`
   (repoint its own lines 338/506 hooks); README security + reverse-proxy
   sections; remove the startup banner's bare-nginx residual warning
   (`lifecycle.rs:704`) since the exemption it warns about is gone.
9. Full `make test` gate (historically ~480s wall clock, 3400+ Rust tests,
   ~94–98 e2e specs) + mutation checks on the new guards + the real-browser
   verification above.
10. PR referencing SH-255; comment the link back onto the story. **Stop there** —
    this is a linked worktree, no version bump, no deploy.

## Gate

`make test`, supervised in the background with log-growth as the heartbeat and a
stall bound, per this repo's standing rule. Never bump the version or deploy from
this worktree.
