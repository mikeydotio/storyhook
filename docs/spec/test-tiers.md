# Test tiers: the CLI gates merges, the browser gates releases

Design of record for **SH-394**.

## The problem

This tracker's own CLAUDE.md already recorded the symptom before it recorded
the cause: `make test` described as "nine minutes nominal, routinely longer
under the three-to-four concurrent worktree suites this machine actually
runs" (SH-306). That description drove a 900-second `PreToolUse` hook past its
own ceiling six times in three days — the defect SH-306 fixed by moving the
gate to git's own `pre-push`, which has no deadline. SH-394 asked the question
SH-306 didn't: *why* is the suite that shape, and does every part of it need
to run on every push?

Two measurements answered it.

**Where the wall clock goes.** `docs/rearch/baseline/timings.md` put the whole
Rust suite (`cargo test --workspace`, ~2,200+ `#[test]` functions across 173
files) at a **36-second median** over ten runs, serial per-binary sum 19s.
`e2e/playwright.config.ts`'s own SH-222 measurement put the Playwright leg —
52 desktop specs run **twice** (`chromium` and `webkit`), plus a 22-case
mobile project — at **2.9 minutes idle, 4.4 minutes at load 44, 6.4 minutes at
load 100, per desktop project**. The browser suite is not a large fraction of
gate wall-clock; it is nearly all of it. Tiering the 173 Rust test files by
speed or risk — the shape the story text originally suggested — would have
been churn with no payoff; the one leg worth moving was always the browser
suite.

**Where the flakes actually are.** The harness's own bounded waits
(`crates/storyhook-test-support/src/server.rs`'s `ACCEPT_DEADLINE`,
`PORTFILE_DEADLINE`) are already measured and argued in their own doc
comments — ~141 calls per run landing at 0-4ms, and a panic message that
names the two things that have ever caused it (SH-110's `wait_for_server`
race; a `target/debug/deps` pile driving `FSEventStreamStart` past a fixed
budget). Raising those would be churn against recorded evidence. The
fragile class was different: **assertions that put a sub-5-second absolute
ceiling on a quantity that includes a process spawn**, on a machine where
`tests/daemon_lifecycle.rs`'s own comment measured a Mach-O first-exec cost
of 32.5s against a warm 5.1s.

## The decision

**The entire Playwright leg moves out of the merge gate and into the release
gate.** Decided directly (not by council — the tradeoff was legible enough for
a single call once the two measurements above were in hand): the dashboard is
a feature of storyhook; the CLI is the tool. A merge gate that proves the CLI
still works is the right floor for `main`; a release is the right floor for
"and the dashboard still works too."

No new escape hatch on `scripts/release.sh`. The story text suggested a
`--no-tests` flag; that was considered and declined in favor of the script's
existing doctrine — `--skip-gate` stays hard-refused outside `--local-only`,
and the gate step now runs the stronger battery rather than gaining a way to
skip it.

## The mechanism

| | `make test` | `make test-full` |
|---|---|---|
| fmt, clippy, the Rust suite, `cargo build`, the plugin bash harness | ✓ | ✓ |
| `scripts/run-e2e.sh` (the browser suite) | — (named deferral) | ✓ |
| Gates | every push (`.githooks/pre-push`) | `scripts/release.sh`'s public path |
| Receipt tier | `gate` | `full` |

`test-full: E2E=1` is a **target-specific variable** (not recursive `make`):
`test-full` depends on `test`, and `E2E` is visible to every prerequisite
`test` reaches — including the `$(if $(E2E),…)` conditional around the e2e
leg and the tier argument to `gate-receipt.sh postlude`. The two tiers can
never drift apart on anything but that one leg and that one argument, because
they are one recipe, not two. `scripts/leg.sh` wraps every leg to report its
own wall clock (the evidence base for ever moving another one) and to print a
named deferral — `leg e2e: SKIPPED — not part of this tier. Run make
test-full to include it.` — when `test` runs without `E2E` set, so the
reduced gate can never silently read as full coverage (the SH-306 shape one
layer up: a green run answering a question nobody asked).

`gate-receipt.sh`'s receipt gained a third line, `tier gate|full`. A `full`
receipt is a strictly stronger claim about the same tree than a `gate` one,
so `postlude` refuses to let a later `gate`-tier run downgrade an existing
`full` receipt — re-running the cheap tier after the expensive one on
unchanged content must not erase the stronger claim. `.githooks/pre-push`
still accepts either tier (the reduced gate protecting `main` is the entire
point of the split) but now names which one it found, so a push that skipped
the browser suite is never indistinguishable from one that didn't.

`tests/gate_tiers.rs` fences the split **behaviourally**, through
`make -n --no-print-directory <target>` dry runs against the tracked
Makefile — never a text scan, which would drift the moment a line moved.
It pins: `test-full` reaches `run-e2e.sh` and `test` does not (with the named
deferral in its place); each tier certifies with its own receipt tier and
never the other's; every other leg is byte-for-byte identical between the two
dry runs (mutation-checked — deleting the `E2E` prerequisite variable fails
this); and `scripts/release.sh`'s gate step names `make test-full` exactly
once. A positive control (`make_dash_n_fails_on_an_unknown_target`) proves a
successful dry run means something, per SH-364's lesson that an oracle
nobody has tested is blind.

## Merge commits reach the gate a different way (SH-396)

Everything above assumes the gate is reached by a **push**: `.githooks/pre-
push` checks the tree being pushed against the receipt store, and `gate-
receipt.sh` writes a receipt as the last line of `make test`/`make test-
full`. A PR merged with `gh pr merge --merge` is a **server-side merge** —
GitHub computes the merge commit and updates `main` without a push ever
reaching this machine, so the push gate never fires. This project also runs
no test CI in GitHub Actions by policy (Actions are deploy-only). Put
together: **every merge commit on `main` is an ungated tree by
construction**, regardless of how green its two parents were.

This is not hypothetical. PR #484 (SH-315, "story attachments") merged
cleanly — zero textual conflict — into a tree that failed to compile:
`tests/unassessed_priority_paths.rs`'s exhaustive `Invocation` match had no
arm for the `Attachment` variant SH-315 added on the other side. Both
branches were independently certified; their union was never tested by
anyone, because nothing could have been. `main` was red for 73 minutes.
Measured over the 30 merges preceding the fix: **14 produced a tree matching
neither parent** — content no receipt could possibly have covered.

**The fix asks the same question a push does, computed instead of pushed.**
`git merge-tree --write-tree origin/main <pr-head>` computes the tree a merge
would produce without touching the working directory or creating a commit,
and is byte-identical to what a real `git merge` of the same two parents
produces — pinned by `tests/merge_gate.rs::
the_predicted_tree_matches_a_real_merges_tree_exactly`, and confirmed against
the actual incident (`git merge-tree --write-tree <main-before-484>
<SH-315-branch-tip>` reproduces the broken merge's tree byte-for-byte).
`scripts/merge-preflight.sh` checks that tree against the **same** tree-oid-
keyed receipt store `.githooks/pre-push` already reads — a merge tree and a
pushed tree are the same kind of claim (content that passed `make test`), so
one store serves both rather than teaching every reader about a second one.

**Reached by polling, not a hook, because nothing local fires for a server-
side merge.** `scripts/merge-watch.sh` (`make merge-watch`) is one reconcile
pass over every open PR: for each, it asks `merge-preflight.sh`, and for any
merge tree with no receipt yet, checks it out in a persistent worktree (kept
so `target/` stays warm — a fresh `git worktree add` per pass would mean a
cold compile every 1-3 minutes, and sharing `CARGO_TARGET_DIR` across
worktrees is already ruled out below) and runs `make test` against it for
real — `make test` is 36.4s median warm on this machine
(`docs/rearch/baseline/timings.md`), so this is never a compile-only proxy.
A green run certifies the tree through the same `gate-receipt.sh` postlude
every ordinary push uses. Meant to be re-run every 1-3 minutes by something
that already exists on the machine (`/loop`, a launchd job); installing that
recurrence is a per-machine bootstrap step this spec documents rather than
performs, the same posture `make e2e-install` already takes for the browser
suite's toolchain. Polling reaches what a hook structurally cannot: a merge
made from the GitHub web UI, another machine, or a session that never
enrolled this clone.

**Status is reported on the PR itself, upserted rather than posted fresh.**
A comment sits in the maintainer's path at the exact moment of the risky
action — clicking merge — which a status file nothing reads yet, or a story
filed per red poll, do not. But posting a fresh comment every pass would mean
a new notification every 1-3 minutes for a PR that is simply still broken —
the self-noise shape this project has already paid for three times (SH-306,
SH-345, SH-263: a gate or fixture that fires repeatedly for one unchanged
fact, rather than once for the fact itself). So the comment is found by a
fixed marker and edited in place; GitHub does not notify on an edit the way
it does on a new comment, so a still-red PR produces one notification total,
not one per poll. The comment always carries a last-checked timestamp, for
the same reason SH-306 named one layer down: a gate that goes silent (the
poller dies, `gh` auth expires) must read as stale, not as a quiet all-clear.

**Why `merge-watch.sh` itself carries no automated test.**
`scripts/merge-preflight.sh` — the part that decides correctness — has the
exhaustive treatment: `tests/merge_gate.rs` drives it against real git the
way `tests/push_gate.rs` drives the push gate, with receipts written by the
production `gate-receipt.sh`, never hand-forged, and mutation-checked
(SH-295). `merge-watch.sh` is thin orchestration on top of that already-
tested primitive plus real `gh` API calls; this project's testing tenets are
explicit that mocking *behaviour* validates the mock rather than the
integration, and SH-263 and SH-345 are the recorded cost of exactly that gap.
Verified by hand against this repo's own live PRs instead, each time the
script changes.

## The timing-ceiling rule

A wall-clock ceiling states that some deadline *D* was not spent. It is only
honest if there is room between the machine's real cost and *D*. SH-394 swept
every test comparing a measured duration against a bare `Duration::from_*`
literal and re-expressed each so the ceiling derives from the deadline it
proves did not fire:

- **Where the deadline is large relative to the measurement** (a hook's
  declared `timeout_seconds`, `PEER_IO_TIMEOUT`, `SPAWN_DEADLINE`,
  `SPAWN_LOCK_DEADLINE`, `CONTROL_DEADLINE`, `FORCE_GRACE`), the ceiling is
  now derived from that constant — half of it, twice it, or the constant plus
  a stated margin, whichever the doc comment for that site can defend.
  Several of those production deadlines (`SPAWN_DEADLINE`,
  `http1::PEER_IO_TIMEOUT`) were made `pub` for exactly this, joining
  `CONTROL_DEADLINE`, `SPAWN_LOCK_DEADLINE` and `FORCE_GRACE`, which already
  were.
- **Where a fixture had a fixed cost too close to its own ceiling to widen**
  (`tests/daemon_concurrency.rs`'s hook-queuing proof), the fixture's own
  delay was widened instead of the assertion — the property under test is a
  binary "queued or concurrent" distinction, not a speed claim, so lengthening
  the wedge is what keeps the two cases unmistakable under load, at the cost
  of real wall-clock in that one test.
- **Where a self-calibrating ratio already existed** but a concurrent
  measurement pays more contention than the single measurement it is
  compared against (`tests/daemon_lifecycle.rs`'s four-clients-share-one-
  attempt proof), the ratio's own margin was widened (2x → 3x), still nowhere
  near the ~4x signature of the regression it exists to catch.
- **Where neither move was safe** — `tests/tailnet_startup.rs`, whose own
  header rejects a generous bound by name, having already proven one vacuous
  against the exact regression this file exists to catch (SH-186's original
  ~20s bound, ~6x `TAILNET_PROBE_TIMEOUT`, stayed green with the defect
  present) — the site went to a 3-member council. Two of three seats failed
  to deliver a proposal after two dispatch attempts each; the chair adopted
  the one substantive proposal received, documented as a chair decision
  under an aborted quorum (see `story show SH-394` for the full trail — this
  project's own council directories are gitignored and per-worktree, so the
  verdict is restated here rather than pointed at alone, per SH-363).
  **Verdict: drop the wall clock entirely.** No fixed ceiling can be both
  tight enough to reject a
  reintroduced synchronous probe and loose enough to survive this machine's
  load, because that is a structural property of comparing two unrelated
  quantities (probe latency and OS scheduling noise), not a threshold-tuning
  problem. Three tests now use `HeldTailscale` — a shim that cannot proceed
  past its own first instruction until the test releases it — so a
  client-observable outcome arriving at all while it is held proves nothing
  on the path that produced it was waiting on tailscale, a structural fact
  (event order) rather than a timing measurement (event duration). This
  refines the council's literal "touch a marker, check it later" proposal:
  `src/daemon/serve.rs`'s `tailnet_reprobe` calls `probe_and_bind_tailnet` on
  its first loop iteration with no initial delay (the backoff schedule only
  applies after a *failed* attempt), so a marker-existence check would race
  the background probe thread rather than observe it cleanly — a hold has no
  such race, because the shim cannot make any progress, not even exit, until
  released. The anti-hang backstop these tests still need comes from
  existing, already-audited production bounds (`port_of`'s own
  `PORTFILE_DEADLINE`, `lifecycle::ensure`'s own `SPAWN_DEADLINE`) rather
  than a new bespoke deadline — nothing new to fence.

`tests/timing_assertions.rs` fences the shape mechanically going forward:
derived over `git ls-files 'tests/*.rs'`, it flags any comparison whose
entire compared quantity is a bare `Duration::from_*(<digit>…)` call —
in either direction, qualified or not — while explicitly allowing a
**definition** (`const CEILING: Duration = Duration::from_secs(5);`, which
*is* the fix) and a literal folded into a larger expression as a margin on an
already-calibrated quantity (`baseline * 4 + Duration::from_millis(500)`,
`tests/daemon_concurrency.rs`'s own line, pinned directly). It cannot judge
whether a margin is wide enough — that took reading each site's production
deadline, which is what this story actually did — but it kills the shape
that hides the question a reviewer would otherwise ask.

## The orphan bracket: refuse before the run, reap after it (SH-412)

`scripts/check-no-orphan-servers.sh` brackets `make test` the way
`gate-receipt.sh` does (`Makefile:144,152-153`), fails when a server this
worktree's own suite starts is still running (SH-51: a leaked test daemon
holds a port, and a later run handed that port talks to a stranger's
registry — 78 of 139 tests down, all spurious 404s), but its two phases
answer different questions and used to give them the same verdict.

**Preflight has no grace period, and none is correct.** A match here is a
process that existed *before this run started* — it may be a daemon a
developer started on purpose, and it makes this run's own verification a lie
regardless. Refuse, name it, and never kill it: that decision belongs to
whoever started it.

**A postlude match is a different fact, and used to get the preflight's
verdict anyway.** On 2026-08-17, every leg of a real `make test` passed
(rust-suite 873s) and the postlude then refused, naming two daemons with
etimes 2m16s and 7m39s — both gone within a minute. `make` fail-fasts, so no
receipt was minted for a suite that had already gone green; the only
sanctioned path was a 22-minute re-run. That is precisely the pressure SH-306
was filed for: a gate whose cost is disproportionate to what it proves trains
the operator toward `SKIP_PREPUSH_TESTS=1`, and this incident used it, twice,
in the same session that filed SH-412.

The postlude already waited before refusing — a 10-second grace loop, added
2026-07-29 alongside `STORYHOOK_PARENT_PID` containment, on the theory that a
daemon "exits when its parent does, but learns by polling, so failing
immediately would be failing on the defence working." That theory is correct
but incomplete: `SHUTDOWN_CHECK` (`src/daemon/serve.rs`) — the parent-watch
poll tick, 250ms — is the *only* bound left in play by the time the postlude
runs, because every test binary this run started has already exited, and
`DaemonGuard`'s `STOP_DEADLINE` (15s) and `lifecycle::stop`'s `FORCE_GRACE`
(2s) are both spent *inside* a test binary's own teardown, before it exits.
Ten seconds is already 40x `SHUTDOWN_CHECK`. So a survivor of the grace
period was never "still winding down" — waiting longer cannot fix a case the
sanctioned shutdown paths already exclude, which is why the fix widens what
the postlude *does* with a survivor rather than how long it waits for one,
and why the grace period's value does not move (`ORPHAN_GRACE_SECS`,
`tests/orphan_check.rs::the_grace_period_is_derived_from_the_parent_watch_tick_it_disproves`
pins the ratio so it cannot silently drift).

**The postlude now reaps a survivor and certifies the tree, rather than
refusing.** This is sound specifically *because* the preflight is a hard
prerequisite of `test` and fails closed on any match — a postlude survivor
was therefore provably spawned by *this* run, and this run is entitled to
collect it. SIGTERM, a bounded wait, then SIGKILL, then verify; the postlude
fails only if something survives SIGKILL, which is a genuinely different
fact — a process this run cannot even end, not one it merely forgot to. A
process that exits on its own during the grace window is never reported at
all: only a process that actually needed a signal is a leak.

**Forensics survive past the run that found them.** The whole reason this
class was hard to pin down is structural: `scripts/run-tests.sh` deletes its
isolated data root on `EXIT`, taking a survivor's own portfile and daemon log
with it, and one macOS process cannot read another's environment — so the
two specific processes SH-412 measured left no recoverable evidence of what
they were. The reap now writes what it can see (`ps`'s pid/ppid/pgid/state/
etime/command) to stderr *and* appends it to a durable, per-day log under
`$(git rev-parse --git-common-dir)/storyhook/orphan-reports` — the same
shared, per-clone, never-committed, never-cloned directory `gate-receipt.sh`
already keeps its receipts in.

**`$1` is now a validated phase, not a dual-purpose free-text label.**
`preflight` and `postlude` were phases; anything else used to fall through to
the strict, ungraced check as a free-text label —
`scripts/capture-baseline.sh` relied on exactly that, passing a descriptive
string as its own phase argument. A typo'd phase therefore silently became
the strict check: the wrong verdict for the word actually typed, and this
story's own failure shape (SH-357's class — an argument that lands nowhere
must be refused, not misread). The script now takes `preflight | postlude |
check [label]`; anything else is refused with a usage message, and
`capture-baseline.sh` calls the explicit `check` form.

Proven the way SH-306 requires a gate to be proven — by provoking it.
`tests/orphan_check.rs` is the test this script never had: real processes,
the tracked script reached by symlink from a disposable, git-initialized
fixture root (never this checkout, never a sibling worktree — `pgrep -f` is
global on a machine that routinely runs 3-4 concurrent worktree suites at
once), spawned with the *exact* argv shape `spawn_child`
(`src/daemon/lifecycle.rs`) builds for a real daemon. That argv choice makes
every case double as the positive control the SH-113 hazard calls for: a
pattern that stops matching production's actual shape would mean the
expected refusal or reap never fires, and the test asserting it would fail
loudly rather than the suite passing vacuously.

## Out of scope, named rather than silently dropped

- **Sharing `CARGO_TARGET_DIR` across worktrees.** ~185GB duplicated across
  17 linked worktrees on a volume at 88% capacity — a real win, and a
  separate one: it would break `check-no-orphan-servers.sh`'s per-worktree
  process scoping and `non_temporary_dir`'s "beside the running test binary"
  rung (SH-258).
- **Env-overridable production spawn/control deadlines.** Declined —
  SH-174/SH-182 deleted exactly that shape (`STORYHOOK_EXCHANGE_DEADLINE_SECS`)
  after an exported value silently abandoned every write on a machine.
- `scripts/capture-baseline.sh` still calls `make test` for its flake census;
  that measurement is about the Rust suite's own determinism, unaffected by
  which tier a developer's ordinary push runs.
- **A `make land` wrapper that performs the merge (SH-396).** Considered and
  declined: the poller makes it unnecessary, and a wrapper only protects a
  session that remembers to use it — an advisory step this project's own
  history (SH-136, SH-198, SH-258, SH-260/276, SH-360) says drifts.
- **GitHub rulesets or an Actions-based required check (SH-396).** Out of
  this repo, and an Actions job that ran tests would collide with the
  standing "no test CI in Actions" policy this doc's own mechanism already
  depends on.
- **Installing `merge-watch.sh`'s own recurring timer (SH-396).** Documented
  as a per-machine bootstrap step (`/loop`, launchd), not performed by any
  target here — the same posture `make e2e-install` takes for the browser
  suite's toolchain, and for the same reason: a background job on the
  machine is something to opt into, not something a merge should install.
- **Retro-certifying the 13 other merge trees the 14-of-30 measurement
  found untested (SH-396).** `main`'s current tip is what matters, and the
  first `merge-watch.sh` pass after this lands covers every PR open at the
  time; historical merges that already landed are not re-examined.
