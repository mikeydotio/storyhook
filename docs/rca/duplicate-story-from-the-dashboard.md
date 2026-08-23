# A slow-but-successful dashboard create was reported as a failure, and the user's own retry duplicated it

- **Date**: 2026-08-15 (failure 2026-08-14)
- **Severity/Impact**: One duplicate story filed from the dashboard (SH-310/SH-311), 24
  seconds apart, byte-identical. Data-only: no lost work, cleaned up with one `story delete`.
  This is the **fourth** duplicate-creation pair in this tracker's own history (after
  SH-227/SH-228, SH-251/SH-252, SH-271/SH-272), and the first traceable to the web dashboard
  rather than a scripted CLI caller.
- **Status**: Fixed on `fix/sh-312-duplicate-story-from-the-dashboard`. Council verdict on the
  one open design question: recorded on SH-312, and restated in full below.

## Summary

The dashboard's "New story" modal had no guard against submitting twice, and its `api()`
helper gave every mutation a flat 10-second timeout against a daemon whose event hooks are
sanctioned to run synchronously, *after* the commit, for up to 60 seconds
(`event_hooks::HOOK_TIMEOUT_CEILING_SECS`). A create that took longer than 10 seconds — but
still succeeded — was reported to the user as `"request timed out"`, a message that reads as
"nothing happened" but proves nothing of the kind. The user's own record shows this exactly:
`SH-312`'s description names SH-110/SH-111 as the duplicate pair, but the store's event log —
consulted because provenance columns exist for this purpose — shows those two are unrelated
stories filed ten hours apart. **The real incident is SH-310/SH-311**, filed from the
dashboard 24 seconds apart with identical payloads, three minutes before SH-312 itself was
filed. The fix disables the submit path while a request is outstanding, tells the truth about
an outcome the client cannot prove, and raises the mutation deadline so a genuine timeout
becomes the rare case hardening.md's doctrine was written to govern, rather than the routine
one this defect made it.

**The lesson: a client that cannot prove a write failed must not say it did, and a deadline
shorter than the server's own documented worst case is not a timeout — it is a coin flip the
client loses on every slow write.**

## Timeline

| When | What | Anchor |
|---|---|---|
| 2026-08-02 | `docs/rearch/hardening.md` records the doctrine this defect violates: "a command that died by signal, or reported that the daemon stopped answering, must be *read* before it is retried. A blind retry is what turns this window into a duplicate story." | `docs/rearch/hardening.md:81-85` |
| 2026-08-10 23:59:47Z | First duplicate pair in this tracker (SH-227/SH-228), 4s apart — CLI-originated, `story new` typed twice by an agent, no retry mechanism involved | — |
| 2026-08-12 16:28:04Z | Second pair (SH-251/SH-252), 0s apart — CLI-originated | — |
| 2026-08-13 15:38:34Z | Third pair (SH-271/SH-272), 5s apart — CLI-originated, `command='new'` recorded on both by SH-246's then-new provenance columns | — |
| 2026-08-14 22:57:04Z / 22:57:28Z | SH-310/SH-311 filed from the dashboard, 24s apart, identical `labels:["web"]`, `type:normal`, no description — the shape only `route_create_story` produces (labels/type set *in* the creation batch; the CLI skill files bare and enriches after) | store forensics |
| 2026-08-14 23:00:02Z | SH-312 filed, misnaming the pair as SH-110/SH-111 | — |
| 2026-08-15 | RCA: store forensics (a read-only snapshot of `store.db`), three independent code explorations, council vote on the one open design question, fix | this doc |

## Root cause & trigger

**Defect** — `src/web_dashboard.html`'s `submitCreate()` had no in-flight guard, and `api()`'s
mutation timeout (10s, hardcoded) was shorter than the daemon's own documented worst case for
a write. Two independent mechanisms, either one sufficient on its own to file a duplicate:

1. **No in-flight guard.** `#create-submit` was never disabled during a request. Two
   independent entry points reached `submitCreate()` — the button's `click` and Enter in
   `#create-title`, with no `e.repeat` guard on the latter — and the modal stayed open, fully
   populated, with a live button until the request settled.
2. **A manufactured ambiguity window.** `api()`'s `xhr.timeout = 10000` gave up long before the
   daemon necessarily would: a request queues behind a zero-capacity dispatcher rendezvous
   (`daemon/serve.rs`), waits on a process-wide write lock, and — only *after* the commit —
   may run synchronous event hooks for up to `HOOK_TIMEOUT_CEILING_SECS` (60s). `ontimeout`
   rejected with the string `"request timed out"`, which the `.catch` printed verbatim into
   `#create-error` as if it were a definite, provable outcome. It was not: the write may
   already have committed. The user read a failure, and the modal — form intact, button live —
   invited exactly the retry that filed the duplicate.

**Infection** — the dashboard believed, and told the user, that a write of unknown outcome had
definitely failed.

**Failure** — the user's own natural next action (click Create again) filed a second,
byte-identical story against a create that had already succeeded.

Both mechanisms are independently sufficient — a double-click needs no timeout at all, and a
hand retry after a real timeout needs no double-click — so this is an ODC **OR**, not an AND;
fixing either alone would have narrowed the window, not closed it. Both were fixed.

**ODC**: Function/Class, **Missing** (no in-flight guard existed to be wrong) and
**Interface/Timing** (the client's and server's notions of "this failed" disagreed, and
nothing reconciled them). Triggered on **an ordinary write under transient load** — no
configuration or environment precondition, unlike SH-226's launchd/`PATH` trigger; this fires
on any create whose round trip exceeds 10 seconds, which the daemon's own architecture
explicitly permits.

**Why it fired then, from the dashboard specifically, for the first time.** Every prior
duplicate pair in this tracker was CLI-originated, and the CLI has never had this defect:
`HttpInvoker` (`src/invoke.rs`) already treats an unproven outcome as unproven — it retries
*only* a refused connection (nothing delivered), and reports every other failure, including a
timeout, as "may or may not have run," explicitly declining to repeat it. The dashboard is a
second client of the same daemon that never learned that rule. It was only a matter of time
before a dashboard create's round trip — under whatever load — crossed the fixed 10s line the
CLI never draws.

## Contributing factors

- **The two claims already on record for three of the four prior pairs are false, and are
  corrected here.** `plugins/story/bin/story.sh` has never retried a failed `story new`.
  Its own comment says so — *"Deliberately NOT retried by the caller: a repeated create files a
  duplicate story"* — and that comment, byte-identical, predates SH-227 in plugin versions
  0.4.0 through HEAD. SH-252 was deleted with the reason "a `--json` parse fallback in the
  filing command ran after the story had already been created," and SH-272 with "a scripted
  `story new` whose `--json` parse failed silently" — neither mechanism exists in this
  codebase, at any version checked. The true cause of those three pairs was not independently
  re-diagnosed as part of this investigation (out of scope: they are closed, and their
  forensic trail — `command='new'` on both halves of SH-271/SH-272 — shows a CLI door, not the
  one this story investigates); what is corrected here is only that the recorded reasons
  attribute a mechanism that provably does not exist.
- **Provenance existed but was not wired to this door.** SH-246 added `command`/`actor`
  columns to `events` specifically so "which door wrote this" would never again require a
  store dump — but `src/api/rest.rs`'s per-project `Ctx` was built with no `.with_provenance`
  at all, so every dashboard write folded to `Provenance::unrecorded`, indistinguishable from
  a pre-SH-246 row or a test fixture. This is what made confirming SH-310/SH-311 came through
  the dashboard (rather than, say, a hand-built request) require a raw event-stream read
  instead of a `story log`.
- **No per-request daemon log.** The daemon writes no access log; the in-flight record
  (`daemon.current.json`) deletes itself the instant nothing is outstanding. The only surviving
  record of the incident was the store's own append-only event log — which was sufficient, but
  only because SH-246's provenance columns and the store's own immutability held.
- **The codebase already had the doctrine this violated, in writing, one door over.**
  `docs/rearch/hardening.md` and `HttpInvoker`'s own doc comments state the exact rule the
  dashboard broke. The dashboard was never audited against it because it is a different client
  of the same daemon, in a different language, and the doctrine's own text is CLI-scoped.

## The fix

On `fix/sh-312-duplicate-story-from-the-dashboard`, each commit green:

- **In-flight guard.** `submitCreate`, `saveDraft`, and `discardDraft` share one flag
  (`createModalInFlight`) that disables all three of the modal's mutating buttons for the
  duration of any one outstanding request — the same pattern `dispatchButtons()` already uses
  for a running dispatch, applied to the one surface that had never adopted it.
- **Honest ambiguity.** A mutation's `.catch` now distinguishes a *definite* server answer
  (400/403/422/500/…, kept as-is — the daemon answered, so it is not ambiguous) from `status:0`
  (network error or client timeout — the daemon may or may not have acted). The latter now
  reports *"storyhook could not confirm whether this reached the daemon — it may or may not
  have gone through. Check the board, then try again if it didn't,"* mirroring `HttpInvoker`'s
  own wording for the identical ambiguity, and refetches the board in the background so the
  truth is visible before the user decides whether to retry.
- **A derived, not hand-copied, deadline.** `api()`'s mutation timeout is now
  `MUTATION_TIMEOUT_MS` — `HOOK_TIMEOUT_CEILING_SECS` (60s) plus a stated 15s margin for the
  dispatcher rendezvous and write lock, *not* a second, independently-chosen constant.
  `tests/dashboard_mutation_deadline.rs` reads both the Rust constant and the JS declaration
  and fails if they drift apart — the exact failure mode that has already cost this project
  three counts of hand-maintained numbers going stale unnoticed (SH-136, and the scans
  `tests/store_isolation.rs` and `tests/release_targets.rs` exist because of).
- **REST-door provenance.** `route_provenance()` derives a `web:<verb>` command label from the
  matched route and attaches it at `Ctx` construction — the same point `invoke.rs` attaches it
  for the CLI door — so a dashboard write is attributable in `story log` rather than
  `(unrecorded)`.

**Why this is the origin and not the encounter point.** The timeout itself was the
manufactured defect (CLAUDE.md: fix at the origin, not the encounter point) — a client deadline
shorter than the server's own sanctioned worst case is not a "genuine" ambiguous outcome, it is
a self-inflicted one. Raising it, derived from the server's real ceiling, is what makes a
timeout the rare anomaly the existing doctrine already knows how to handle, rather than routine
noise a slow write triggers on every over-10s round trip.

**Council verdict on the one open design question** (does `POST .../story` also gain a
server-side idempotency key?): **no**, unanimous after deliberation (verdict on
SH-312). A caller-minted key does not
deliver the same unrepresentability guarantee an intrinsic key (a commit sha, a story id)
does — a reload or a re-typed form mints a fresh key and files the duplicate regardless — and
a DB-backed key table would be the first row in this store needing time-based garbage
collection outside the event-log fold. The residual risk this leaves open — a user reads the
honest ambiguous message, checks nothing, and retries by hand into a write that had in fact
already landed — is accepted as representable by design: it is visible on the board and
reversible with one `story delete`, which is judged the right price for not adding mutable
daemon state, a migration, and a new API contract to close a window this fix already narrows
by orders of magnitude. Two explicit triggers are recorded for revisiting this: a second
occurrence of this defect class after this fix ships, or the first non-interactive consumer of
`POST .../story` with no human present to read the ambiguity. If triggered, the required shape
is on record in the same decision document.

## Preventative action — killing the class

1. **`tests/dashboard_mutation_deadline.rs`** — kills the *drift* sub-class: raising the
   server's hook ceiling without raising the dashboard's deadline to match is now a build
   failure, not a silent reopening of this race.
2. **`e2e/specs/duplicate-create.spec.ts`** — two browser-level regression tests. One holds a
   real create response open and drives both of `submitCreate()`'s entry points while it is
   outstanding, asserting exactly one request reaches the network. The other delays a real
   reply past a shrunk client deadline and asserts the message is honest and the board reflects
   the truth, rather than asserting a fabricated failure.
3. **`tests/web_test.rs::web_create_story_is_attributable_to_the_web_door`** — pins that a
   web-originated create's `StoryCreated` event carries `Provenance::command("web:new")`, not
   `unrecorded`, so this class of RCA never again needs a store dump to answer "which door
   wrote this."
4. **`tests/web_test.rs::web_create_story_twice_with_identical_bodies_files_two_stories`** —
   pins the council's decision as current, intentional behavior at the HTTP layer: two
   independent creates with identical bodies are two distinct stories. If either flip trigger
   above is ever pulled, this is the test that must change alongside the redesign.

## Lessons

- **A deadline shorter than the thing it is timing is not a timeout — it is a guess dressed as
  a fact.** The client's 10s bound had no relationship to the server's actual worst case; it
  was chosen once, for nothing in particular, and outlived being correct the day event hooks
  were allowed to run for up to 60s after commit.
- **"Request timed out" is not a neutral phrase — it is a claim of failure, and claims of
  failure must be provable.** The codebase already had the correct wording, one door over
  (`HttpInvoker`'s "may or may not have run"). The gap was that a second client, in a second
  language, restated the same problem without inheriting the same answer.
- **A tracker's own record can misname its own incident.** SH-312's description names SH-110/
  SH-111; the real pair is SH-310/SH-311. Provenance and an append-only event log are what made
  it possible to establish this from evidence rather than from what the report said — the same
  reason SH-246 was filed in the first place.
- **A door that skips a cross-cutting invariant will eventually prove it wasn't cross-cutting
  after all.** Provenance (SH-246) and the retry doctrine (`hardening.md`) were both written
  with the CLI in view; the REST door existed the whole time and inherited neither until this
  incident forced the audit.
