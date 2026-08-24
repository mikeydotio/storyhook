# Selective testing: `make test-changed`

Design of record for **SH-429**, bullet 2 ("Merge-gate tests should only be
the tests that cover code paths that have been touched since the last green
test"). Bullet 1 (push before test) and bullet 3 (release runs `make
test-full`, already true since SH-394) are covered by
`docs/spec/test-tiers.md`'s "The push gate narrowed to main/master" section.

## The problem, measured

`make test`'s Rust suite alone is 495-1041s on the machine that runs it
under its normal concurrent load (measured this story) — most of a
9-minute-nominal gate that this project has already paid to move off the
push-time critical path once (SH-306) and narrow the scope of once (SH-394,
the browser suite). What SH-394 did not touch is the 76% of the merge gate's
wall clock that IS the Rust suite itself: fmt (2s), clippy (44s), build
(19s) and the plugin harness (93s) are comparatively small; the rust-suite
leg (495s) dwarfs them.

Most of a session's edits touch a handful of files. Running all 187
integration-test binaries plus two lib crates' own unit tests for a
one-file change is correct — nothing here weakens that as the default —
but it is not the only sound choice, and a faster one exists if it can be
made trustworthy.

## The design

### No multi-hop chains

`scripts/select-tests.sh` always diffs the current tree against the
**nearest fully-certified** (`gate`/`full`) ancestor — never against a
previous `changed`-tier run. Concretely: it walks `git log --first-parent`
from `HEAD`, in the same idiom `scripts/browser-status.sh` already
established, and stops at the first commit whose tree carries a `gate` or
`full` receipt.

Two consequences follow directly from this rule, and both are the reason it
exists rather than a nicety:

- **There is never more than one hop of drift** between "what was fully
  tested" and "what a selective run trusts." The classic test-impact-
  analysis failure mode — change A alone is fine, change B alone is fine,
  A+B breaks a test neither one's own selective run picked — cannot
  accumulate across links, because there is only ever one link.
- **The selected set grows, never shrinks, the longer a branch goes without
  a full run.** Every commit since the baseline is in the diff `select-
  tests.sh` computes, so staleness costs wall-clock (more of the diff, more
  binaries selected, eventually the whole thing) — never soundness.

### Coverage only ever ADDS binaries to a run

`scripts/coverage-map.sh` builds the workspace with `-C instrument-coverage`,
runs every test binary (187 integration binaries plus the `storyhook` and
`storyhook_test_support` lib crates' own unit tests) against a green tree,
and records which tracked source files each one touched — a flat, sorted
`<binary>\t<repo-relative-file>` TSV keyed by that tree's oid, stored beside
`gate-receipt.sh`'s own receipts
(`$(git rev-parse --git-common-dir)/storyhook/coverage-maps/<tree>`).

**This map is never trusted to prove a binary unaffected.** LLVM line
coverage sees which lines of *compiled Rust* executed. It cannot see a test
that reads a tracked file directly (`CARGO_MANIFEST_DIR`-relative reads — 57
of this repo's test files, measured) or shells out to `git ls-files` (19
more) to scan the tree at runtime: the lines that execute there belong to
`std::fs::read`'s own generic body or to the `git` binary, not to whichever
*specific* file happened to be read. Three unconditional escape hatches sit
on top of the map for exactly this reason, checked by `select-tests.sh`
**before** the map is ever consulted:

1. **No map for the resolved baseline → run everything.** A map is only
   ever a strengthening signal on top of "run everything"; its absence is
   never treated as "nothing changed."
2. **Any changed path outside `src/**.rs`, `crates/**.rs`, `tests/*.rs` →
   run everything.** This is what covers `Makefile`, `scripts/`,
   `.githooks/`, `src/web_dashboard.html`, `plugin/`, `docs/`, `e2e/`, and
   every `include_str!`ed asset — none of which coverage instrumentation
   touches at all.
3. **The derived tree-scanning set, always applied.** Every `tests/*.rs`
   file whose own source names `git ls-files`, `CARGO_MANIFEST_DIR` or
   `include_str!` runs regardless of what changed — derived by scanning
   `tests/*.rs` at selection time (`git grep`), never a hand-kept list.
   CLAUDE.md's own SH-136/SH-198/SH-258/SH-260-276/SH-360 are five recorded
   costs of exactly that hand-kept shape in this project alone; this is that
   doctrine applied here rather than repeated a sixth time.

Only once all three are checked does `select-tests.sh` consult the map:
every binary the map names for a changed `src/**.rs`/`crates/**.rs` file,
plus the binary for any changed `tests/*.rs` file itself (a file the map has
never seen, because it is new), unioned and sorted.

### The tier, and where it does and does not gate

`gate-receipt.sh` gained a third tier, `changed < gate < full` (a strict
order — a weaker-tier run never overwrites a stronger receipt already on
file for the same tree, generalizing the existing full-cannot-be-downgraded-
by-gate rule). A `changed` receipt carries a fourth line, `base <tree>`,
naming the fully-certified tree it was diffed against — the fact that makes
the claim honest ("the selected tests passed, relative to a specific prior
green tree," never "the whole suite passed"). `postlude` refuses to write one
whose `base` does not itself carry a `gate`/`full` receipt.

**`.githooks/pre-push` accepts `changed` for a push** — SH-429's bullet 1
already narrowed that hook to reporting, not refusing, for any non-`main`
ref, so a `changed` receipt is simply one more tier it names on the way
through.

**`scripts/merge-preflight.sh` never accepts `changed` for a merge.** This
was a council decision (three seats: software-architect, qa-engineer,
skeptic; unanimous 3-0 after one round, full trail on story SH-429). The
reasoning: `merge-preflight.sh` exists because 14 of the last 30 merges
measured produced a tree matching neither parent — content no single
branch's own history-relative diff could ever have accounted for, since a
merge combines two independently-authored diffs. `select-tests.sh`'s
soundness argument (diff against one branch's own nearest green ancestor)
has no established argument for how it behaves across that combination, and
`merge-watch.sh` already runs the real `make test` against every
uncertified merge tree regardless — landing costs nothing extra by requiring
`gate`/`full` there. The practical shape: `test-changed` speeds up the
developer loop and what a push reports; it does not, by itself, speed up
what actually reaches `main`.

### `scripts/run-changed.sh`: the tier is honest, not aspirational

`select-tests.sh` can answer `ALL` for reasons that have nothing to do with
staleness (no baseline found at all, a changed path outside the three
covered globs) as well as for staleness itself. In every one of those cases
the whole suite just ran, so the receipt `test-changed` earns is `gate`,
never `changed` — a receipt claiming a narrower tier than what actually ran
would misrepresent, in the wrong direction, what future readers of the
receipt store can trust that tier to mean. Only a genuine subset run earns
`changed`, with the `base` tree `select-tests.sh` resolved.

This generalizes the tier-honesty rule a three-seat council settled for the
staleness case specifically (Q3 of the same verdict: a stale coverage map is
treated as "no map exists," `select-tests.sh` runs everything, and the
resulting receipt is stamped `gate`) to every escape hatch that runs
everything, not only the stale-map one.

### The detection layer: `coverage-watch.sh` / `coverage-status.sh`

The same shape as the browser tier's own detection layer (SH-418), and a
council verdict (Q2 of the same panel) chose it explicitly over the
alternative — piggybacking coverage capture onto every worktree's own
`gate-receipt.sh` postlude, so a map regenerates on every local green
`make test`. That alternative was rejected on the same grounds SH-418's own
council already used for the browser tier: it fires far more often than
needed while doing nothing to guarantee freshness relative to `main`'s
actual tip, which is the fact that matters for what a **merge** — and
therefore `select-tests.sh`'s own baseline resolution on the next branch cut
from `main` — will see.

`scripts/coverage-status.sh` reports how far `origin/main`'s tip is from the
last tree with a coverage map — `current` / `behind by N` / `never`, no
staleness threshold (a ceiling on "how stale is too stale" would be a bare
literal about one machine's cadence on one day, the same rule
`docs/spec/test-tiers.md` already states for wall-clock budgets).
`scripts/coverage-watch.sh` is the poller: one pass, its own persistent,
locked worktree (`coverage-watch-worktree`, kept separate from
`browser-watch-worktree` because an instrumented build lives in its own
`target-coverage/`, so sharing a worktree would mean the two pollers evict
each other's warm build on alternating runs), keyed to whether `origin/
main`'s tip already has a map. If the tip has no `gate`/`full` receipt yet
either (the ordinary case is that it does, since `merge-watch.sh` already
certified it on the way to landing), it runs `make test` there first.

Neither poller installs its own recurring timer — the same posture
`make browser-watch`, `make merge-watch` and `make e2e-install` already
take; bootstrapping the recurrence is a per-machine choice.

## What this does NOT change

- **`main`'s own protection.** `merge-preflight.sh` requires exactly what it
  always required: `gate` or `full`. `test-changed` is a developer-loop
  accelerant and a push-time reporting nuance, never a weaker merge gate.
- **Compilation.** `cargo clippy --workspace --all-targets -D warnings`
  still type-checks every test target unconditionally in `test-changed`'s
  own recipe — only test *execution* is selective. This is what keeps PR
  #484/SH-315's defect class (a variant added on one side, no arm added to
  an exhaustive match on the other) caught by the fast tier too.
- **Doctests.** `scripts/run-tests.sh --only <names>` always runs
  `cargo test --workspace --doc` unconditionally alongside whatever
  binaries were selected — they are not part of `coverage-map.sh`'s own
  enumeration (a `///` example is not a `tests/*.rs`/lib-`#[cfg(test)]`
  binary) and cost ~3s total (`docs/rearch/baseline/timings.md`), cheap
  enough that skipping them was never worth the soundness gap.

## Known limitation, stated rather than hidden

A `coverage-map.sh` capture takes real wall-clock (an instrumented rebuild
of the whole workspace plus 187+ individual binary runs, each incurring its
own daemon-startup cost) — measured in the tens of minutes, in the same
order of magnitude as the browser suite's own 24-minute leg. `select-tests.sh`
degrading to "run everything" whenever a map is missing or stale is
therefore not a rare edge case on a branch that outpaces `coverage-watch.sh`'s
own poll cadence; it is the expected behavior until the map catches up, and
it is the SAFE direction — the design's whole point is that staleness costs
time, never soundness.
