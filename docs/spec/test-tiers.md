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
| fmt, clippy, core Rust, checkout-contract Rust, `cargo build`, plugin bash | ✓ | ✓ |
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

### A failed leg invalidates only itself

Every executable gate leg now runs through `scripts/leg.sh --reuse`. A green
leg records an atomic, machine-local receipt under the shared git directory,
keyed by the leg's command line and the content of its relevant tracked input
space. A failed leg records nothing. Retrying the same gate therefore reuses
every still-valid green result and executes the failed leg again; a browser
failure cannot cause formatting, clippy, Rust, build, or plugin work to repeat
merely because the aggregate `full` receipt was never reached.

The fingerprints are dependency scopes, not a whole-tree timestamp. Rust
formatting follows Rust sources and rustfmt/workspace configuration; clippy
follows compilable Rust targets; build follows production Rust inputs; the
plugin and browser legs add their own payloads and harnesses. Rust execution
is split into two disjoint batteries. `rust-suite` owns ordinary integration
tests, workspace library tests, and doctests. `rust-contracts` owns the
integration targets whose source reads `CARGO_MANIFEST_DIR`, shells out to
`git ls-files`, or embeds checkout content with `include_str!`; its honest
input is the whole tracked tree because those tests inspect scripts,
documentation, plugin files, and e2e specs at runtime. Thus a browser-only
edit reruns the browser and the related checkout contracts, never the core
Rust battery. `scripts/rust-test-targets.sh` derives the disjoint target sets
from Cargo metadata and test source rather than maintaining a list.

In all cases, an edit inside the relevant space yields a different fingerprint
and forces only that battery to run again. The fingerprint contract and
`Makefile` are inputs to every leg, so changing the cache mechanism itself
cannot reuse evidence produced under its old rules.

These per-leg receipts are evidence used only while assembling a gate run;
they never certify a tree by themselves. `gate-receipt.sh postlude` remains
the sole writer of `gate`/`full` receipts and still refuses mid-run tracked
content drift. A leg also fingerprints before and after execution and records
nothing if its own inputs changed while it ran.

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

## The push gate narrowed to `main`/`master`, so work ships before it is tested (SH-429)

Once the SH-396 section above was true — `merge-preflight.sh`/`merge-watch.sh`
are what actually decide whether content reaches `main`, unconditionally,
regardless of what any push carried — a second fact followed that this
project had not yet acted on: `.githooks/pre-push` refusing an *ordinary
feature-branch push* for lacking a receipt was gating content that was never,
on its own, the thing landing on `main`. It cost real wall-clock all the same:
`make test` runs 495s+ on the machine that actually runs it (measured this
story, three-to-four concurrent worktree suites), so every session paid that
cost **before its work ever left the machine**, on every branch, whether or
not that branch was ever going near `main` directly.

**The decision:** `.githooks/pre-push` now refuses only a direct push to
`main` or `master` with no receipt — defence in depth behind the org's
`protect-main` ruleset, which already blocks direct pushes there by policy.
Every other ref is *reported*, never refused: which tier's receipt the tree
carries, or that it carries none, and that `scripts/merge-preflight.sh` is
what actually decides whether this content may land. The autonomous dispatch
charter (`plugins/story/bin/story.sh`'s `PROMPT_TPL`/`AUTO_PROMPT_TAIL`)
changed to match: commit, push, and open the PR *before* running the test
suite, so work is preserved on the remote even if testing turns something up
— then run `make test` and merge only once it passes, since the merge gate
still requires it.

**Why this is sound and not merely convenient.** Nothing about `main`'s actual
protection moved: `merge-preflight.sh` still refuses a merge tree with no
`gate`/`full` receipt exactly as it always has. What moved is *when* a
session pays the suite's wall-clock cost relative to
when its work becomes durable: pushing first means a crash, a context
compaction, or a session hitting a hard stop mid-test no longer risks losing
commits that were never given a chance to leave the machine. A push that is
merely *reported* as unreceipted is not a weaker claim about `main`'s safety
than a push that was *refused* for the same reason — both leave the tree
exactly as untested as it was; only the party who finds out, and when, has
changed.

**What stays a hard refusal, and why the line is drawn there.** A direct push
to `main`/`master` is categorically different: unlike a feature branch, its
content does not pass through `merge-preflight.sh` on the way in (there is no
merge — it *is* `main` already). The org ruleset already blocks this by
policy, so in the ordinary case this refusal never fires; it exists for the
case where policy is misconfigured, bypassed, or the ruleset is not the layer
actually enforcing it (e.g. a fork, a mirror, or a future repo that adopts
this hook without the ruleset). A tool-level check that assumes the platform
check is always present is exactly the single point of failure SH-306's
family of stories keeps finding.

**`tests/push_gate.rs`** provokes the split directly rather than inspecting
it: `push_branch` provokes the new non-`main` report-not-refuse path (a
fresh, receipt-less branch push must still succeed and must still move the
remote ref — SH-306's own doctrine that the remote ref, not the exit code, is
the load-bearing assertion), while the renamed
`a_push_to_main_with_no_receipt_is_refused_and_the_remote_does_not_move`
keeps pinning the surviving refusal. Mutation-checked in both directions:
forcing every ref to be treated as protected turns exactly the one new
report-path test red; making the `main`/`master` case arm unreachable (so
nothing is ever refused) turns every test whose assertion depends on that
refusal red. Full counts and case names are in the test file's own module
doc, updated with this change rather than left to drift (the SH-136/SH-198/
SH-258/SH-260-276/SH-360 doctrine applied to a doc comment, not just code).

## Something has to run the release tier between releases (SH-418)

Everything above tells you what the two tiers *are*. It does not say what runs
the expensive one. Until SH-418 the answer was: a human who chose to, and
`scripts/release.sh`. Nothing else, ever. So a dashboard change could
invalidate a browser spec, merge behind a correctly-green `make test`, and sit
red indefinitely — with the first person to find out being whoever tried to cut
a release, at which point it blocked one.

**SH-416 is the measured case**, not a hypothesis. SH-398 restructured the
drawer's blocked banner and left `blocked-drop-reason.spec.ts` asserting the
shape it replaced. That merged. It was found by SH-368 running the browser
suite as an incidental regression check on unrelated work. Between those two
points every `make test` was green and every push and merge was correctly
certified — the gates were all working, and none of them was looking.

**And the state of the machine says it plainly.** When SH-418 was filed there
were **109 receipts in this clone's store and zero carrying `tier full`**: 33
said `tier gate`, 76 predated the tier line. SH-394 built that line in order to
distinguish the tiers, and for the whole life of the split nothing had ever
asked the stronger question.

This is SH-306's rule one tier up. The coverage existed and was correct — the
spec caught the drift the instant it ran. A check whose verdict nobody ever
collects is operationally identical to a check that did not run.

### The trigger is the tree's own certification state, never a path diff

The story proposed running the browser tier for a merge tree whose diff touches
`src/web_dashboard.html` or `e2e/`. **A three-seat council rejected that
unanimously on the first ballot** (devops, architecture, QA — the verdict is on
the story: `story show SH-418`, per SH-363, never a council directory that
resolves on no fresh clone). Two reasons, and both are general:

- **A path predicate under-triggers by construction.** A change to `src/web.rs`
  or `src/daemon/**` can break a browser spec with neither of those paths in its
  diff — the dashboard's JavaScript talks to a Rust HTTP surface, and
  `run-e2e.sh` seeds its fixtures with the real `story` binary. A trigger that
  is wrong in the *quiet* direction is the defect this story is about.
- **It is a hand-kept list.** SH-136, SH-198, SH-258, SH-260/276 and SH-360 are
  five recorded cases of exactly that shape drifting.

The trigger is instead: **does `origin/main`'s tip tree already carry a `tier
full` receipt?** Derived from content, so it re-arms on every merge whatever
that merge touched; free to evaluate (a file stat); and self-coalescing — a
burst of merges collapses into one run against the newest tip rather than one
run each. It also *upgrades* the `gate` receipt `merge-watch.sh` already wrote
for that same tree, which `gate-receipt.sh postlude` explicitly permits.

### Its own worktree and its own lock, not `merge-watch.sh`'s pass

Also the council's, and decided on two measurements rather than taste:

| Measured | Value |
|---|---|
| Merges into `main` per day (2026-08-15 … 08-18) | 11, 24, 16, 9 |
| The browser leg, four Playwright projects, this machine under concurrent load | **1454s (24.2 min)** |
| The same run's merge-gate legs (fmt 2s, clippy 44s, rust-suite 495s, build 19s, plugin 93s) | 653s total |

Folding a 24-minute browser leg into `merge-watch.sh`'s 1-3 minute reconcile
pass would starve SH-396's merge gate for hours a day at that merge rate. So
`scripts/browser-watch.sh` owns `$(git rev-parse
--git-common-dir)/storyhook/browser-watch-worktree` and takes a lock before it
runs. The lock is a directory (`mkdir(2)` is the atomic primitive available
everywhere this runs; macOS ships no `flock(1)`), and its staleness is decided
by a **fact** — whether the recorded pid is still alive — never by a timeout,
which would be a bare literal about how long a browser suite is allowed to take
(the rule this document already states one section down).

### The verdict is collected from the store that already carries it

No second notion of "certified" was introduced. A green pass writes an ordinary
`tier full` receipt through the same `gate-receipt.sh postlude` every push uses;
`.githooks/pre-push` and `scripts/merge-preflight.sh` keep accepting either tier
with **no change to how they read it**. What is new is a reader that
discriminates.

`scripts/browser-status.sh` walks `main` back — `--first-parent`, because a
commit a merge brought in was never `main`'s own content — to the nearest tree
carrying a `full` receipt, and reports **commits-behind and age, or `never`**.

**Distance, computed per read, and deliberately not a cached marker.** A
marker file recording the last outcome was proposed (it answers the real
ambiguity: a receipt's absence alone cannot distinguish "never tried" from
"tried and failed") and was declined, because it is a second store to be wrong
about the world, for a fact the tree-keyed store can already state. Distance
answers the same question from the store itself, and answers it on **one scale
that only grows**: a poller that has died, a `main` that has been red all day,
and a machine that has never run the suite are three readings of the same
number. Silence is never a pass — it is `never`, which is the largest reading
there is.

**No staleness threshold.** The reader reports; it does not judge. "How far
behind is too far" would be a bare literal about one machine's cadence on one
day, which this document already refuses for test budgets.

Four places collect it:

| Where | What it says |
|---|---|
| **every `make test`**, on the e2e deferral line | how long the browser tier has gone unrun — see below |
| `make browser-status` | the full reading, exit 0 current / 1 behind / 2 never / 3 unresolvable ref |
| every `merge-watch.sh` PR comment | one line — the tier that gate does **not** cover, in front of whoever is about to click merge |
| `$(git rev-parse --git-common-dir)/storyhook/browser-watch-reports/<date>.log` | one line per pass, forensics only |

**The first of those is the one that needs no bootstrap, and it is where
SH-394's own anti-silence line had the same gap one tier up.** `scripts/leg.sh
--skipped e2e` existed so the reduced gate "can never silently read as full
coverage" — but it said only that *this run* skipped the browser suite, and
nothing about whether any run ever hadn't. `make test` now follows it with
`browser-status.sh`:

```
leg e2e: SKIPPED — not part of this tier. Run `make test-full` to include it.
browser-status: never — no tree in origin/main's 642-commit first-parent
  history has ever passed the browser suite. Run 'make browser-watch'.
```

Three properties of that line are deliberate and all three are pinned by
`tests/gate_tiers.rs::the_merge_gates_deferral_reports_how_stale_the_browser_
tier_is` (mutation-checked in both directions). It **never gates** — `|| true`,
because a merge gate that failed on the release tier's staleness would undo the
split SH-394 measured. It is **absent from `test-full`**, where the suite is
about to run and a stale reading would be noise measured moments before it
stops being true. And it is on the path **every session already takes**: `make
browser-watch` and `make merge-watch` both want a per-machine timer, and on the
machine that filed this story neither had one.

That last one is **not** the marker the council declined, and the distinction is
load-bearing: nothing reads it to decide whether a tree is certified and no gate
consults it. It exists because a pass that found a red suite should leave
evidence after the scrollback is gone — the same shape, and the same directory
family, as the orphan reap's per-day log (SH-412).

### What is fenced, and what is deliberately not

`tests/browser_gate.rs` provokes the reader against **real git**, with every
receipt written by the production `gate-receipt.sh` and never hand-forged — the
control `tests/merge_gate.rs` and `tests/push_gate.rs` already require, because
a hand-written receipt proves the reader's file format rather than the
producer's behaviour. Ten cases, mutation-checked in both directions:

- the tier comparison loosened from `= "full"` to "a receipt exists" → **3 of 10
  red**;
- `--first-parent` dropped from the walk → **1 of 10 red**;
- `browser-watch.sh`'s command array changed to `make test` → **1 of 10 red**.

The positive control is
`a_full_tier_receipt_on_the_tip_reads_as_current`: without it, every `never`
assertion could pass for the wrong reason if the fixture's certification path
silently stopped working (SH-364).

**The decision lives entirely in the reader, on purpose.**
`scripts/browser-watch.sh` asks `browser-status.sh` and obeys it. That is the
lesson taken from `merge-watch.sh`, which carries no automated test because
mocking `gh` would validate the mock (SH-263, SH-345): the answer was not to
accept a second untested decision but to move the decision somewhere testable.
What is left in `browser-watch.sh` is fetch, lock, checkout, exec. The one fact
about it a test *can* pin without mocking `make` is pinned —
`browser-watch.sh --plan` prints the very command array the run path executes,
so `the_pass_a_poller_would_run_is_the_full_tier_not_the_merge_gate` cannot
drift from what runs.

`tests/gate_tiers.rs::the_browser_tier_detection_targets_reach_their_scripts` is
a **wiring** fence and claims nothing more: a `make` target that stopped
invoking its script would restore the exact silence this story ended, without
failing anything.

### Bootstrap, per machine, as ever

Two steps this document describes rather than performs, the same posture `make
e2e-install` and `make merge-watch` already take:

1. **A timer.** `make browser-watch` runs one pass and exits. It wants a
   *coarse* recurrence — the leg is 24 minutes — not `merge-watch`'s 1-3
   minutes; the lock is what makes an over-eager timer harmless rather than
   catastrophic.
2. **`make e2e-install` inside the poller worktree**, once. `e2e/node_modules`
   is gitignored, so a fresh worktree has none. `browser-watch.sh` refuses with
   that exact command rather than running it (a network fetch writing a shared
   browser cache outside the repo is a thing to opt into), and refuses **before**
   spending the Rust legs — `make test-full` runs the browser leg last, so an
   unprovisioned worktree would otherwise fail at the final line after ten
   minutes of work.

### As built: the first real reading, and what it found

Recorded because it is the honest state of the thing and a later reader will
otherwise assume better:

- The first `browser-status.sh` run over this repo answered **`never` across
  641 first-parent commits**. The story's claim was not an estimate.
- The first `make test-full` run in SH-418's own worktree was **RED**: the merge
  gate's legs all passed and the browser leg failed with **7 failures — 1
  `chromium`, 6 `webkit`** (`mobile-chromium` and `mobile-webkit` passed). Every
  one maps onto an **already-open** load-sensitivity story — SH-401, SH-419,
  SH-349, SH-375, SH-378 — rather than an unfiled regression; one failure was a
  `page.goto` timing out at 26661ms, i.e. after load grace had already widened
  it.

The consequence is worth stating plainly rather than discovering later: **on
this machine, under the concurrent load it normally carries, a `full` receipt is
currently unobtainable.** That is a fact about the suite, not about the
detector — and it is the same fact that would block the next release, which is
exactly what SH-418 said would happen. A poller will report a growing distance
until those stories land. That reading is correct, and it is the first time the
number has ever been visible.

### As built, second reading: "load-sensitivity" was the wrong name (SH-496, SH-501)

The reading above attributed all seven of its failures to the already-open
load-sensitivity stories. A release attempt on 2026-08-27 cutting v2.3.0 refused
with **13** failures (SH-496), and working that ledger measured the class instead
of inferring it. Those are two different runs, so nothing below re-diagnoses
SH-418's own seven — what it retires is the *framing* they were filed under, which
outlived them and which the measurement does not support. The correction belongs
here rather than only in a story, because this section is what a later reader
opens.

**Most of it was not load, and most of it was not even flaky.** Six of the
thirteen were deterministic assertions that had simply never been run, because
since SH-394 nothing on a merge path executes this tier and, per the reading
above, no tree had ever carried a `full` receipt. Four specs were asserting
behaviour three deliberate changes had superseded — SH-446, SH-449 and SH-481 —
which is `5cb6c94`'s four-file diff, and two of those specs failed on both
engines, which is how four spec files account for six of the thirteen rows.
Their own teardown then found a seventh thing, store-side: SH-497, an epic whose
children are all deleted stays permanently unmovable, because deletion is soft
and `has_children` only tests for the edge. All of it is fixed. On tip
`fa81744`, **`chromium` is 366/366, `mobile-chromium` 38/38 and `mobile-webkit`
38/38** — three of the four projects fully green, twice over. The one
`mobile-chromium` row now passes too — in the one full four-project run since,
which is the only run that exercised that project — with no change attributable
to it: recorded as unexplained rather than as fixed.

**What is left is one webkit class, and the daemon is not in it.** Four failures
per full webkit run, roaming from run to run, all one shape: a fresh context's
first `page.goto("/")` never completing. While it stalled, an external prober
polled the same URL on the same daemon every two seconds — **445 samples, HTTP
200, slowest 28 ms** — and a second prober counted **at most 10 established
sockets** against the 128 of `MAX_CONNECTIONS`. The stalled page's own Playwright
trace holds **no network record at all**, not even for the navigation, and its
screenshot is a blank `about:blank`: the request never left the browser. One
stall ran a full 60 s at a measured `contention=0.80` — an idle machine by the
harness's own definition, where load grace's multiplier is exactly 1.

So the honest statement is narrower and more useful than the one above: a `full`
receipt is unobtainable on this machine **because of one webkit navigation
defect** (SH-501), not because the suite in general is too load-sensitive to
pass. Load grace is working as designed and cannot reach this; the stories the
first reading pointed at name individual sightings, where SH-501 names the class.
The distance `browser-status.sh` reports will keep growing until SH-501 lands,
and that reading is still correct.

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

## Load grace in the browser suite (SH-347)

A binding user determination, 2026-08-17, verbatim: *"relax the timeouts
when machine is under load. If timeout fires, examine load, and if high
contention, reset timeout timer (up to a maximum of 15 minutes) rather than
ending the test."* This is a standing capability of the browser suite, not a
per-spec workaround, and it sits beside the timing-ceiling rule above as its
qualified sibling: that rule is about *not widening a bare number*; this one
is about *not ending a test on a machine that is doing more than it usually
does*. Neither licenses the other — a ceiling still has to derive from the
deadline it disproves, and grace still has to be reported, never silent.

**Why "reset the timer" has to mean "extend it pre-emptively."** Playwright
enforces a test's timeout in the runner itself; once it fires, the test is
torn down and there is no hook that runs after that point. The only lever is
`testInfo.setTimeout()`, and it only works *before* the deadline arrives. So
"reset the timer rather than end the test" is implemented as: sample
contention continuously, and grant more time before the clock can run out —
never in response to a timeout that has already happened, because by then
there is nothing left to reset.

**Two layers**, both in `e2e/load-grace.ts` and consumers of it:

1. **Config-evaluation scaling** (`e2e/playwright.config.ts`) grades the two
   SH-222 budgets — `timeout: 15_000` and `expect: { timeout: 5_000 }` —
   once per project run (`scripts/run-e2e.sh` invokes Playwright once per
   project). This is the *only* way to grace `expect.timeout` at all; it
   cannot be retuned mid-run once a project starts.
2. **A per-test watchdog** (the `loadGrace` auto-use fixture in
   `e2e/specs/support.ts`) samples every 500ms during a running test and
   calls `testInfo.setTimeout()` before its own deadline, monotonically —
   a grant is never retracted, because a test already mid-flight has no way
   to know it no longer needs the room. This is the layer that reacts to
   contention arriving *mid-test*, which config-evaluation-time scaling
   cannot see. It adopts a spec's own `test.setTimeout()` (e.g.
   `dispatch.spec.ts`'s multiple of `DISPATCH_COMPLETION_TIMEOUT`) as its
   base the moment one appears, so it only ever *adds* grace on top of a
   budget a spec already asked for — and it clamps to the user's ceiling as
   an **absolute** bound on the result, not a multiplier applied to
   whatever base happened to be in effect, so a spec with an already-large
   custom budget cannot be stretched past 15 minutes just because grace was
   computed as a multiplier of it.

**The contention signal**: `os.loadavg()[0] / cores()` — processor sharing.
A ratio of 1.0 means exactly as many runnable threads as logical cores, the
point past which a thread that owned a whole core at idle now shares it, and
wall clock stretches roughly in proportion. Below 1.0, nothing is contending
and the multiplier is exactly 1 — every budget is bit-identical to SH-222's
own numbers, so **a real defect still surfaces exactly as fast as it does
today on an idle machine.** Above 1.0, the multiplier tracks the ratio
directly (`Math.max(1, ratio)`) rather than a curve fitted to the three
Chromium-only data points already in the config's own comment — derived, not
tuned, the same doctrine the timing-ceiling rule above already states for a
production deadline. Known imprecision, stated rather than hidden: a
one-minute load average under- and over-reacts at the edges of a load burst.
Both directions are the safe direction here — grace only ever grants
patience, never withholds it.

**The ceiling's own provenance**: `MAX_TEST_TIMEOUT_MS = 15 * 60_000` is a
**recorded human tolerance, not a measurement** — the user's own words,
above, expressed as the arithmetic in that sentence rather than a raw
millisecond literal with none. `tests/e2e_load_grace.rs` pins the exact
expression so a future edit that changes it has to change the words in this
section too.

**An assertion that proves a bound states its own timeout; the config
default is patience, not proof.** Grading the harness's own defaults never
weakens a single assertion that exists to *prove* a deadline — every one of
those already states its own `{ timeout: N }` (`DATA_DELAY_MS * 3`,
`GONE_TIMEOUT`, `DISPATCH_COMPLETION_TIMEOUT`), untouched by either layer.
The two graced numbers only ever govern how long the harness is willing to
*wait* for something it expects to become true.

**Reported, never silent** — the SH-306 shape one layer up, a gate whose
verdict depends on state it never reported: one line at config-evaluation
time naming the measured load, ratio, and chosen budgets; one line on stderr
per watchdog extension, naming the test, the sampled ratio, and the new
budget; one `testInfo.annotations` entry per extension, landing in the
JSON/HTML reporters. `tests/e2e_load_grace.rs::an_extension_is_never_silent`
fences the watchdog's own call site for both.

**The cost, admitted rather than hidden**: at SH-222's own mid measurement
(load ≈44 on 10 cores, ratio ≈4.4) the multiplier is 4.4× — a 66-second test
budget against a *measured* 8.6-second worst case. That is deliberately more
patience than the measurement says is needed; the ceiling, not the
multiplier, is what bounds it. A genuinely wedged test can now burn up to 15
minutes instead of 15 seconds before it is reported. `E2E_LOAD_GRACE=0` is
the kill switch, for a run that specifically wants today's bare SH-222
numbers regardless of load.

**What this buys, and what it does not.** Grace is patience, not a fix — it
lets a test that would have passed on more time actually get that time,
which is what the user's determination asks for. It cannot make an
assertion that is structurally wrong become right, and does not substitute
for the mechanism work below.

## A held request races its own client-side deadline (SH-347)

Six e2e tests were quarantined under the `webkit` Playwright project on an
unconfirmed hypothesis: that WebKit does not reliably surface a
held-then-aborted or held-then-delayed `page.route()` interception to the
page's own XHR handlers. `e2e/specs/interception-contract.spec.ts` measured
the actual engine contract directly, on both `chromium` and `webkit`, rather
than continuing to guess from the symptom:

- **A route held indefinitely**: Chromium's `xhr.timeout` fires `ontimeout`
  on schedule. WebKit's never fires at all — the client-side deadline is
  simply never observed while a request sits in Playwright's interception
  layer.
- **A route held then `route.abort()`ed**: both engines surface `onerror` to
  the page within single-digit milliseconds of the abort. WebKit surfaces an
  abort just as promptly as Chromium — the original hypothesis, as stated,
  is refuted.
- **A route held then delivered late, past the client's own shrunk
  deadline**: Chromium's `ontimeout` fires as configured. WebKit's never
  fires — the client silently receives the late reply as an ordinary
  successful response instead.

**The real mechanism for the three tests that shrink `mutationTimeoutMs` and
race a delayed `route.fulfill()`** (`duplicate-create.spec.ts`,
`drawer-field-mutation-timeout.spec.ts` ×2) is now confirmed to the byte,
deterministically — not load-sensitive, reproduces every time at idle:
**WebKit does not enforce `XMLHttpRequest.timeout` on a request Playwright's
route-interception layer is holding.** The entire premise these three tests
were built on — "the client gives up first, so the write's outcome is
genuinely ambiguous to it" — cannot occur on WebKit, because the client
never gives up; it just receives the late reply as a normal success.

**The rule that falls out, stated plainly**: a spec that holds a page-issued
request across its own assertions is racing that request's own client-side
deadline, and must set that deadline explicitly past anything the harness
can grant a test — never leave it as whatever hardcoded literal the
production code happens to carry. `src/web_dashboard.html`'s five
`xhr.timeout` assignments are all named constants now
(`tests/dashboard_deadline_knobs.rs` fences the bare-literal shape), and the
three a browser spec actually holds past are `intFromQuery`-backed knobs
(`boardFetchTimeoutMs`, `catalogFetchTimeoutMs`, `apiGetTimeoutMs`) a spec
sets to `heldReadDeadlineMs()` (`e2e/specs/support.ts`) — twice the running
test's own timeout, so no page clock this harness can ever grant a test can
be the thing that ends an assertion the test means to own.

**`board-readiness.spec.ts`'s three tests are a separate case, deliberately
NOT resolved the same way**: their held route is aborted, not delayed, and
the page's own clock has since been removed from the race entirely — yet a
real full-suite WebKit run (a council-decided plan, `interception-
contract.spec.ts`'s Probe D) measured a single held route's abort *not*
reflected on screen within 12 seconds, while the two-route shape saw it in
~3ms in the identical run. Abort delivery is real but not reliably prompt
under contention, and one green sample for the two-route shape against a
same-run sibling failure does not meet the bar to lift either. All three
stay quarantined, with the measured (not hypothesized) reason in each
`test.skip`'s own string — the general lesson is that even a *structurally
sound* interception mechanism (abort, proven prompt at idle) can still be
unreliable once the machine is doing enough else at once, which is exactly
the condition load-grace exists to give a test room to survive, not to paper
over a mechanism that is not actually reliable.

## The audit (a floor, not a ceiling, per the story's own words)

| Site | Held request | Client-side clock raced | Verdict |
|---|---|---|---|
| `board-readiness.spec.ts` ×3 (`holdDataFor`/abort) | `/data` | none (WebKit never arms it while held) | quarantined — abort itself unreliable under real load (Probe D) |
| `catalog-readiness.spec.ts` ×3 (`holdUntilRefused`) | `/api/repos`, `/states` | `fetchReposOnce`/`api()` GET defaults | knobbed |
| `deep-link.spec.ts` ×2, `drawer-open-race.spec.ts` (`holdFetch`) | `/data` | `fetchData` | knobbed (not on the story's own list) |
| `board-readiness.spec.ts:736`, `dispatch-log.spec.ts:252` | aborted immediately, never held | none | clear |
| `duplicate-create.spec.ts:58`, `story-context-menu-priority.spec.ts:291` | held to the *test's* own release | `MUTATION_TIMEOUT_MS` (75s ≫ any test budget) | clear |
| `modal-enter-autorepeat.spec.ts:158` | held to the test's own release | same | clear |

## One suite at a time on this machine (SH-457)

`scripts/run-tests.sh` runs under the machine-wide `gate` lock
(`scripts/machine-lock.sh`, SH-456). Every caller therefore queues: both Rust
batteries, `scripts/run-changed.sh`, and a bare `bash scripts/run-tests.sh`
typed by hand.

**Why.** This suite is 36.375s median warm and idle
(`docs/rearch/baseline/timings.md`) and has been measured at **873s** under the
three-to-four concurrent worktree suites this machine routinely runs — the
contention documented as the cause of an open class of load-sensitive failures
(SH-347, SH-349, SH-375, SH-378, SH-401, SH-419), and the reason `make test`'s
own e2e leg carries `load-grace.ts` at all. Full Auto (SH-452) runs N agent
lanes at once and multiplies exactly that, which is decision D4 of
`docs/spec/full-auto-engine.md`.

**Interactive runs are not exempt, and that is deliberate.** A human's suite
contends identically with a lane's. Exempting it would leave the hole open in
precisely the case where somebody is present to be confused by the result.

**Why inside the script rather than around the `make test` recipe.** A rule in
the Makefile covers the one door and none of the others. Sited here, the queue
is a property of running the suite, not of the way it was invoked — the same
reason `gate-receipt.sh preflight` enrols the clone from inside `make test`
rather than asking anyone to remember a step.

### The escape hatch is reported, always

`STORYHOOK_GATE_LOCK=0` skips the lock and prints a line on **stderr** naming
the variable that caused it. A bypass nobody can see is the SH-306 shape, and
this project has already paid for it once with `SKIP_PREPUSH_TESTS`: six pushes
in three days shipped with no gate and no message saying so. The message is
asserted by name in `tests/gate_lock.rs`, not merely the behaviour — a working
bypass that says nothing passes every behavioural assertion there is.

### What this does not cover, named rather than glossed

* **`make test` reaches `run-tests.sh` twice**, once per disjoint Rust battery,
  so two concurrent runs *interleave at the battery boundary*. Only one
  `cargo test` ever exists on this machine, but the runs are not whole-run
  exclusive and their fmt/clippy/build/plugin legs still overlap. Whole-run
  exclusivity needs nothing new — `scripts/machine-lock.sh gate -- make test`,
  which is the case that wrapper's reentrancy branch was built for and which
  the engine's lanes use.
* **`scripts/run-e2e.sh` does not take this lock.** The browser leg is outside
  D4's wording; `browser-watch.sh` has its own lock for its own reason.
* **The orphan bracket is entirely outside the lock** and its verdict is
  unchanged. `check-no-orphan-servers.sh` runs as a prerequisite of the `make`
  target, before any leg reaches `run-tests.sh`, so a second run in the *same*
  checkout still refuses at preflight rather than queueing — which is correct:
  a pre-existing daemon makes this run's verification a lie whether or not it
  ever gets the lock.
* **Receipt semantics are untouched.** `gate-receipt.sh preflight` is still the
  first recipe line and `postlude` still the last;
  `tests/push_gate.rs::the_makefile_enrolls_first_and_certifies_last` and
  `tests/selective_gate.rs::test_changed_enrolls_first_and_certifies_last` are
  what hold that, and neither needed a change.

### The handshake, and the fork bomb behind it

The take is a re-exec: `run-tests.sh` execs
`machine-lock.sh gate -- bash <self> "$@"` and marks the child so it does not
wrap again. Reentrancy in general is left to `machine-lock.sh`, which already
has that branch and reports taking it; reading `STORYHOOK_MACHINE_LOCKS` in
this script too would put that format in a second place (SH-136), and was
measured to change no observable when it was there.

What the marker prevents is not a hang. `machine-lock.sh` runs its command in a
**background** child — it has to, or a signal could not reach a fifteen-minute
suite — so a re-exec that keeps coming back leaves a live process waiting on
the next one, measured at roughly two hundred processes a second. The `else`
branch therefore carries a depth guard that refuses by name rather than
looping. The two halves sit at two sites so that breaking either is caught by
the other, which is SH-365's two-mechanism shape; `tests/gate_lock.rs`'s own
header records that before the guard existed this exact mutation was not red at
all.

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

### The second class: a daemon whose store is gone (SH-493)

Everything above scopes on the binary's path — `${repo_root}/target/debug/…` —
which is what makes a match safe to act on, and is also why for as long as this
bracket has existed it could not see the population SH-493 counted. **672
leaked daemons were alive on this machine across three days, spanning three
calendar dates, and both ends of the bracket reported a clean tree the whole
time.** That is SH-306's shape again, one layer down from where SH-412 found
it: a gate whose silence reads as an all-clear while the exact thing it exists
to catch accumulates into the hundreds.

**The leak was one Rust test file, not the bash harness the story was filed
against.** `tests/plugin_install.rs::Harness::run` calls `env_clear()` —
correctly, so a provider CLI is found on the fixture's own `PATH` and nowhere
else — and reinstates `HOME`, `PATH`, `TMPDIR`, the three `XDG_*` homes and
`STORYHOOK_DATA_DIR`, but never the containment `scripts/run-tests.sh` exports
for the whole run. Since SH-114 every `story` starts a daemon, so every one
that file ran asked for port 3456 and had no parent to die with. Confirmed from
a survivor's own environment with `ps eww`, which showed exactly that variable
list and no `STORYHOOK_PARENT_PID`. The fix is one `.envs(daemon_containment())`
per command; the fence is
`tests/store_isolation.rs::every_rust_harness_that_clears_the_environment_reinstates_daemon_containment`,
the Rust twin of SH-136's shell-only scan, which is why nothing caught this.

**Why only part of the population accumulated, and why that hid it.** The file
builds 16 harnesses from a *packaged copy* of the binary at
`<fixture>/package/story` (`Harness::new(true)`, proving path resolution from
an installed layout) and 4 from `target/debug/story`. One run leaks 20 daemons,
one per harness. The bracket collected the 4 every run and could not see the 16
— so the visible half looked like an ordinary, handled trickle while the
invisible half grew without bound. The four survivors a real `make test`
postlude reaped on 2026-08-26 were exactly those 4.

**A looser regex is not available.** `pgrep -f` is global, this machine
routinely runs three or four concurrent worktree suites, and every one of them
builds fixtures under one shared root (`storyhook_test_support::scratch_root`,
`/private/tmp/storyhook-tests`). A checkout-agnostic *pattern* would refuse
this run over a stranger's **live** suite at the preflight and murder one at
the postlude — the same false-red this bracket's own SH-412 fix exists to
prevent.

**So the key is what the process IS, not where its binary sits** — SH-239 one
layer over. A daemon whose `--store-path` names a file that no longer exists is
serving nobody, whoever started it: daemon runtime state is keyed off the
canonical store path (SH-113), so its portfile went with the fixture directory,
and a client resolving that path would find no database and start its own. It
cannot be a daemon a developer is using, because theirs exists. Measured before
the rule was written rather than argued: on this machine it separated **718
abandoned daemons from exactly two live ones** — the developer's own dashboard,
and a concurrent gate run's. Run for real afterwards it collected 728 and left
the real daemon untouched.

**Collected in every phase, and never refused over.** This is not the
postlude's argument borrowed early. It is a different and simpler one that
holds in both phases at once: a provably-abandoned daemon is making no run's
verification a lie, and is nobody's to have started on purpose, so there is
nothing for the preflight's "that decision belongs to whoever started it" to
protect. Refusing would block this run over another worktree's mess — hundreds
of them at a time — which is the SH-306 pressure exactly. It is reported every
time it fires, to stderr and to the same durable per-day log, because a
detector whose whole subject is a population that accumulated unnoticed does
not get to be the next quiet thing.

**The age floor is the one thing a bare existence check gets wrong.** Between
`spawn_child` and the store being created there is a real window in which a
perfectly healthy daemon has no store file; reaping there would make this
script the cause of the failure it exists to report.
`ABANDONED_STORE_MIN_AGE_SECS` is derived from `SPAWN_DEADLINE`
(`src/daemon/lifecycle.rs`) rather than picked (SH-394) — how long a client
waits for a daemon it just spawned to answer, so past it the only process that
was waiting has already given up — at twice it for margin on a loaded machine,
pinned by
`tests/orphan_check.rs::the_abandoned_age_floor_is_derived_from_the_spawn_deadline_it_disproves`.
`ps` has no `etimes` keyword on macOS, so the `[[dd-]hh:]mm:ss` form is parsed
rather than read.

**The store path is delimited by the verb that follows it, never taken as the
next whitespace field.** macOS hands out home directories like `/Users/Ada
Lovelace` without comment, and the real store lives under one — so a
field-split read yields `/Users/Ada`, which does not exist, which classifies
the developer's own running daemon as abandoned and kills it. That is the worst
thing this rule could do, and it is invisible on any machine whose own paths
happen to have no spaces, which is why
`a_store_path_containing_a_space_is_read_whole_and_its_daemon_left_alone`
**constructs** the input rather than waiting for a machine that has one — the
same posture SH-420's tap-target straddle takes, one subsystem over.

**The tests for this class are global, and say so.** They cannot be fixtured
under a checkout the way every case above is — being visible from outside one
checkout is the entire property. So they assert on **the script's own report
naming the pid**, never on the process merely being gone afterwards, which a
concurrent sibling's own orphan check could satisfy for us and turn the case
vacuous. The two negative cases are safe by construction rather than by luck:
the live-store one holds a store that exists, which is the whole rule, and the
too-young one is protected for exactly as long as it is too young, and asserts
immediately.

**Two limits, stated rather than claimed away.** A daemon leaked while its
fixture directory still exists is outside this class until that directory goes
— soundness was chosen over completeness, because the incomplete half is
collected by the containment contract the fence above now enforces, while an
unsound reap false-reds a suite that did nothing wrong. And a test that
deliberately deleted a store and kept a daemon serving it would be
misclassified; this project's own standing rule already forbids that shape (a
test that asks about bytes on disk stands the daemon down first), and
`tests/corruption_recovery.rs` — the one place that removes a store file — runs
`story daemon stop` before it does.

**A defect in the tests themselves, found by mutation rather than by review.**
Every shim in `tests/orphan_check.rs` is a direct child of the test process, so
a shim that is killed becomes a **zombie** until the test process waits on it —
and `kill -0` succeeds on a zombie. An "it was killed" assertion written against
`pid_alive` therefore passes whether or not the kill worked, which zeroing the
age floor is what exposed. `pid_running` reads the process state instead.
Asserting a shim is *alive* is safe either way, which is why the cases that
predate this one never had to know.

**And one fixture fragility this made load-bearing.** The SIGKILL-survivor
supervisor spawned a worker every 0.05s that self-expired after 0.6s. Once this
file went from 7 tests to 12 that failed about one run in three, with an empty
stderr: under load the supervisor's spawn loop can stall longer than a worker
lives, the population reaches zero, and the postlude's grace loop exits 0 on the
first empty poll it sees, having proved nothing. A worker now lives one whole
`ORPHAN_KILL_GRACE_SECS` and the cadence is 0.2s, so the population cannot gap
and the churn drops from twenty process spawns a second to five. The
supervisor's own deadline is derived from the script's two constants rather
than the literal `18` that did not notice when the worst case moved.

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
