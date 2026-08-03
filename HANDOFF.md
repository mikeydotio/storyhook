# Handoff — SH-119: C7, the subtraction, now carrying four blocking criteria

**SH-151 is done and merged.** The run itself is described by
[`HARDENING_PROGRESS.md`](HARDENING_PROGRESS.md) — read its **START HERE**
section first. That is the process; this file is only what comes next.

## Take SH-119, and read R1–R4 on it before planning anything

SH-119 is `critical`, it is the next link in the server-owned epic's critical
path, and SH-151 has just unblocked it. `story next` will lead with something
else — that is SH-63 talking, the ready comparator tying on second-precision
timestamps — and order among ready stories of one priority is the orchestrator's
call.

**SH-119 is no longer the story its description says it is.** SH-151's council
recorded four blocking acceptance criteria on it as story comments, and one of
them contradicts SH-119's own written AC. Read them first; they are the plan.

- **R1 — `uuid` and `prefix` survive in `.storyhook.toml`.** SH-119's text
  deletes identity from that file. It cannot: under SH-151's rule a project that
  does not own its repository's origin may never be associated with it, so the
  committed pointer is the *only* thing that can tell two projects in one
  repository apart. `project_paths`, `PathKind`, `touch_project_path`,
  `project_by_path` and the **path half** of `resolve_at` still go. Bound the
  pointer climb with a stat for `.git`, not a subprocess.
- **R2 — the rejected alternatives**, so they are not re-proposed: an
  `(origin, subpath)` key, and `projects.checkout_path` as a resolution index.
  Both are argued out on the story.
- **R3 — the pin.** `invoker_seam.rs::the_nearest_project_wins_over_an_outer_one`
  already asserts nested-project resolution today, and
  `origin_ownership.rs::a_second_project_in_one_repository_resolves_by_its_pointer`
  now sits beside it. Deleting the pointer step deletes a behaviour with two
  named passing tests.
- **R4 — back-fill origins in the same wave.** Measured 2026-08-03: the live
  store has **13 projects, zero registered remotes, zero `checkout_path`s**.
  Deleting the walk without a backfill leaves every one of them unresolvable.
  Use SH-151's ownership constructor against each project's recorded checkout,
  and **report** the projects that own no origin rather than guessing.

If the pointer step is deleted anyway, the failure is not a refusal: a
sub-project on a fresh clone silently answers as the repository-root project.

## What SH-151 leaves you

- **`service::project::origin_at(cwd) -> RepoOrigin`** is the ownership check:
  `Owned` / `Inherited { owner }` / `Unknown(reason)` / `Absent`. Only `Owned`
  yields the `OwnedOrigin` that `register_origin` — the sole `link_remote`
  caller in `src/`, pinned by a source grep — accepts.
- **The predicate**, if you need it again: `canonical(cwd) == canonical(rev-parse
  --show-toplevel)` **and** `--git-dir == --git-common-dir`, from one
  invocation. Not `git worktree list --porcelain`, whose head is the *gitdir*
  inside a submodule.
- **`git_output` scrubs nine `GIT_*` variables and deliberately keeps
  `GIT_CONFIG_*`** — that is how the suite isolates git config.
- **D3's refusal half is deliberately unbuilt** and belongs to you: refusing a
  run that would leave a project identified by neither an owned origin nor a
  pointer only becomes possible once the path row is gone.
- **Two new stories.** SH-160 (the daemon inherits its first client's git
  environment — it breaks today's resolution, not only ownership) and SH-161
  (`doctor`'s pointer-vs-origin advisory, now un-blocked).

## Three things that bit during SH-151

- **A fast non-zero exit from `make test` is usually `cargo fmt --check`**, which
  runs before the tests. 30 seconds and exit 2 is formatting, not a failure.
- **A test asserting on a refusal's wording broke on a design widening.** The
  SH-151 guard in `project_link.rs` asserted "top level"; ownership is wider than
  that now. Rewrite the assertion to the promise, not the sentence.
- **Building a rule whose premise is false costs more than deferring it.** D3's
  refusal took down two legitimate fixtures on its first run, because
  `project_paths` still resolves what it claimed was unreachable.

## Gate

`make test`, supervised in the background with **log growth as the heartbeat**
and a 120-second stall bound. Fourteenth consecutive story with no wedge; the
streak is worth keeping. Budget ~4–10 minutes per run. Do **not** push with
`SKIP_PREPUSH_TESTS=1` — SH-117's log records why that was the wrong call even
with a green gate.
