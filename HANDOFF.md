# Handoff — SH-151: two projects in one repository share an origin

**SH-152 is done and merged.** The run itself is described by
[`HARDENING_PROGRESS.md`](HARDENING_PROGRESS.md) — read its **START HERE**
section first. That is the process; this file is only what comes next.

## Take SH-151, not what `story next` prints

`story next` leads with **SH-63**, and that is SH-63 itself talking: the ready
comparator ties on second-precision timestamps and falls back to read order, so
among equal-priority stories the order is age, not importance. Order among ready
stories of one priority is the orchestrator's call (Phase 1 recorded the same
deviation for SH-129).

Take **SH-151**. It is `high`, it **blocks SH-119**, and SH-119 is the next link
in the server-owned epic's critical path. Everything else at `high` is
independent and will still be there.

## What SH-151 is, in one paragraph

`git config --get remote.origin.url` **walks up** the directory tree, so every
storyhook project inside one git repository reports the same origin. Today that
is benign — `init` skips registering an origin another project already holds,
and the others still resolve by pointer file or recorded path. **SH-119 deletes
that walk**, and at that moment a second project in the same repository has no
automatic route at all: every command in it would need `--project` or
`$STORYHOOK_PROJECT`. A monorepo with a project per service is a supported
layout that would silently stop working.

The story's own comment is complete: it names the file (`service/project.rs`,
`origin_of`), the two call sites, a repro, and the candidate fix — use SH-119's
`projects.checkout_path` as the tiebreak, making resolution "origin, then which
subtree of it". Migration 0007 already added that column (`b8fc36d`). Two SH-116
deviations rest on this story and are recorded there; check whether the fix lets
either be withdrawn.

## What SH-152 leaves you

- **`AppError::SyncConflict` is live.** `story github-sync` exits 8 when a
  conflict is left undecided. `tests/error_contract.rs` calls it `UNPROVOKABLE`
  rather than `UNREACHABLE` now — reachable, but not reproducible offline.
- **The prompt allowlist in `tests/invoker_seam.rs` is 4, not 5.** It lost
  `src/github/conflict.rs`. The count is asserted, so a change there is
  deliberate. `src/github/initial.rs` (SH-153) and `src/service/story.rs`
  (SH-154) are the two defects still on it.
- **Two new stories.** SH-158 (no `GithubApi` trait seam, so `run_sync_with` has
  no test at all — the council's own deferral) and SH-159 (the sibling sweep:
  per-story sync *errors* are still reported inside a success at exit 0).
- **SH-65 is closed as obviated**, not deleted: the dead variant it was filed
  about now has a caller.

## Three things that bit during SH-152

- **A test asserting on help text broke on a line wrap.** `merge base` was split
  across two lines of the topic, so `contains("merge base")` failed while the
  words were both there. Reflow the prose rather than weaken the assertion.
- **A stub is a better red than a missing symbol.** `base_after_sync` was landed
  first as `synced.clone()` — the exact old behaviour — so the failing test
  reported *the defect* (0 conflicts on the second sync where 1 is required)
  rather than a compile error. Worth repeating.
- **`--test-threads=4` and a `TestEnv::shared()`** are what a new integration
  test file should use; `TestEnv::new()` does not exist (it is `shared()` or
  `isolated()`).

## Gate

`make test`, supervised in the background with **log growth as the heartbeat**
and a 120-second stall bound. Thirteenth consecutive story with no wedge; the
streak is worth keeping. Budget ~10 minutes per run. Do **not** push with
`SKIP_PREPUSH_TESTS=1` — SH-117's log records why that was the wrong call even
with a green gate.
