# Handoff — SH-119: C7, the subtraction

**SH-117 is done and merged.** The run itself is described by
[`HARDENING_PROGRESS.md`](HARDENING_PROGRESS.md) — read its **START HERE**
section first. That is the process; this file is only what the next story
inherits.

## Pick up SH-119, and confirm it is ready

`story next` should lead with **SH-119** now that SH-117 has closed. It is the
large, load-bearing subtraction: `project_paths`, the resolution walk, and
identity out of `.storyhook.toml`.

## Four things SH-119's description asks for are already gone

SH-117 removed them because `-D warnings` would not let them survive losing
their callers. Do not go looking:

| SH-119 says delete | actually removed in |
|---|---|
| `story relink` — the verb, `Invocation::Relink`, `parse_relink`, `CatalogService::relink` | SH-117 PR 2, c14 (a redirect naming `story project link checkout` is what is left) |
| `deinit`'s repository-file cleanup — `ProjectService::repository_roots`, `agents_md_is_pristine` | SH-117 PR 2, c12 |
| `DeinitPlan::{files, kept}` | SH-117 PR 2, c12 |
| `src/cli.rs:133,338,705,1376` line references for `relink` | stale — the file has moved a long way since |

`projects.checkout_path` — the column SH-119's description says replaces
`project_paths` — **already exists** (migration 0007, SH-117 PR 1) and already
has a production reader (`story project list` prints it). What is left for
SH-119 is deleting the table and the walk, not creating the column.

## What SH-117 deliberately did **not** touch, and why it matters to you

**D21: SH-117 changed nothing about project resolution.** `resolve_project`,
`ancestors`, `resolve_at`, `pointer_at_or_above` and every `project_paths` read
work exactly as they did. `checkout_path` is written and read and *never*
consulted for resolution. Two tests hold that line, and both go red if
`link checkout` is ever re-implemented as a `project_paths` write:
`tests/project_link.rs::linking_a_checkout_records_it_without_making_it_resolve`
(a linked directory still refuses to resolve) and
`tests/project_path_hygiene.rs::link_checkout_does_not_read_the_pointer_file_it_is_pointed_at`
(a directory carrying another project's pointer still answers for *that*
project after being linked to this one).

That rule exists so SH-121's fixture audit stays out of SH-117. It is now
**your** charter: SH-119 is the story that moves resolution, and SH-121 is
blocked on you.

**One conformance arm is a trap for you specifically.** Two distinct projects
may hold the same `checkout_path`, and an arm in `src/store/conformance.rs`
asserts it. That is deliberate — a cross-project uniqueness constraint
forecloses the two-projects-in-one-repository case SH-151 exists to resolve. If
collapsing `project_paths` into the column tempts you to re-add the unique index
`project_paths` had, that arm is the argument against it.

## Three things that bit during SH-117

- **A background `make test` reported "exit code 0" while `make` exited 2.**
  The command was `(make test > log; echo "MAKE_TEST_EXIT=$?" >> log)`, so the
  subshell's status — and therefore the harness's completion notification — was
  the *echo's*. The `MAKE_TEST_EXIT=` line in the log is what caught it. Read
  the log, never the notification. This is the third appearance of this trap in
  this run.
- **`cargo fmt` after committing leaves earlier commits failing the gate.** Run
  `cargo fmt --all` *before* each commit.
- **`AppError::Validation` is HTTP 422, not 400.** `Usage` is 400. A REST test
  asserting the wrong one fails at the gate rather than at design time.

## Gate

`make test`, supervised in the background with **log growth as the heartbeat**
and a 120-second stall bound. Twelfth consecutive story with no wedge; the
streak is worth keeping. Budget ~10 minutes per run and expect to need two.
