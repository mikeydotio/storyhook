# Handoff — SH-117 PR 2: the sweep, `project delete`, and the retirement

*(Supersedes the part-2 handoff. **Part 2's PR 1 is merged as #103** and the
story is still open and still `in-progress`.)*

The run itself is described by
[`HARDENING_PROGRESS.md`](HARDENING_PROGRESS.md) — read its **START HERE**
section first. That is the process; this file is only what remains.

## Do not re-run the council

`.council/sh117-project-verb-surface/DECISION.md` is the implementable
document: 22 numbered decisions, a commit plan, a test plan and a disarm
matrix. The verdict is also a comment on SH-117. Build it as written; deviate
only with a recorded reason, as PR 1 did twice.

## What is left: PR2/c11 through c14, in the council's own order

### c11 — `refactor(test): sweep project init to project new --prefix SH`

**No `src/` change may appear in this commit's diff.** That is the check D22
makes the reviewer run, and it is why the sweep is separate: a 46-file text
substitution and a behaviour change must not share a commit, or a bisect over
the retirement cannot distinguish "the rename is wrong" from "the new semantics
are wrong".

Three ordered `re.sub` substitutions over `tests/*.rs`, and the order matters —
each later one must not re-match what an earlier one produced:

```python
# 1. a prefix is already there: only the verb changes.
re.sub(r'("project",\s*)"init"(\s*,\s*"--prefix")', r'\1"new"\2', text)
# 2. a bare word after the verb is the old positional path. Five sites.
re.sub(r'"project",(\s*)"init",(\s*)("(?!--)[^"]*")',
       r'"project",\1"new",\2"--prefix",\2"SH",\2"--attach",\2\3', text)
# 3. everything else gains the now-required prefix.
re.sub(r'"project",(\s*)"init"', r'"project",\1"new",\1"--prefix",\1"SH"', text)
```

Plus `crates/storyhook-test-support/src/project.rs:159`, where `ProjectBuilder`
builds `vec!["project", "init"]` and must become `new` with `self.prefix` or
`SH` — the prefix is no longer optional. Plus
`plugin/claude-code/tests/lib.sh:92`.

**Measured surface:** 285 Rust occurrences of the two-word form, 240 of them the
bare `.args(["project", "init"])`.

**Excluded by name**, because their assertions invert and they land in c12/c14:
`tests/init_command.rs` (whole file, renamed), `tests/project_command.rs`,
`tests/project_deinit.rs`, the three `relink_*` tests in
`tests/project_path_hygiene.rs`, one needle in `tests/project_selection.rs:118`,
`compact_reference_contains_critical_commands`, and two entries in
`src/cli.rs`'s `a_declared_flag_is_left_alone`. `tests/project_new.rs` and
`tests/temp_project_refusal.rs` are already on the new verb.

**After the commit:** `grep -rn 'project", *"init' tests crates plugin` returns
zero, and `git show --stat` contains no `src/` path.

### c12 — `feat(cli)!: project delete replaces deinit and touches no filesystem`

D6. `story project delete [--force]`, **no positional** — the target is named by
the ordinary SH-116 selector and, when none answers, refused through
`no_project_refusal`. It reads and writes no filesystem, which deletes
`repository_roots` (private, one caller at `src/service/project.rs:857`, called
at `:739`), `agents_md_is_pristine`, and `DeinitPlan::{files, kept}` — `-D
warnings` will not let them survive losing their only caller.

`forced()`'s inner match is already exhaustive, so a missing `Delete` arm is a
compile error rather than an infinite confirmation loop. Keep the two-step round
trip and the typed-slug token verbatim.

D20's coverage must exist before it ships: (b) the checkout's `.storyhook.toml`
and `AGENTS.md` survive a delete; (c) after a delete `project_by_remote` answers
`None` and a second project may claim the same origin — **the only test in the
suite that would notice `ON DELETE CASCADE` on `project_remotes` regressing**;
(d) deleting by selector from an unrelated directory leaves that checkout's
files intact; (e) `checkout_path` is cleared with the row.

### c13 — `refactor: rename DeinitPlan to DeletePlan and friends`

Pure rename, no behaviour: `ConfirmationPlan::Deinit` → `Delete`,
`DeinitOutcome` → `DeleteOutcome`. **`ConfirmationPlan` is internally tagged on
purpose** — the dashboard reads `err.body.plan.slug` out of a 409 — so
`a_deinit_confirmation_keeps_the_flat_shape_the_dashboard_reads` is the test
that notices if the rename changes the wire shape.

### c14 — `feat(cli)!: retire init, deinit and relink behind redirects`

D10, D11, D14, D15, D16. `init` and `deinit` become redirect `AppError::Usage`
errors naming their replacement, listed nowhere, removed at 3.0.0. `relink` —
the verb, `Invocation::Relink`, `parse_relink` and `CatalogService::relink`
including its pointer read — is **deleted**, with a redirect naming
`story project link checkout`; its three tests become capability tests against
`link checkout` rather than being removed.

`POST /api/repos` becomes `ProjectAction::New`, and **`prefix` becomes
required** — absent is a 400. `buildInitForm()` grows a required prefix input
with a client-side derivation; `domain::prefix::validate` runs server-side, which
is what makes the untestable JS safe to ship.

D15's string sweep is an **enumerated list, not grep-and-hope**:
`no_project_refusal`, the empty-catalog message, the unmigrated-clone message,
`orphan_advice`, `deregistered_message`, `src/api/rest.rs:310`, `PROJECT_USAGE`,
`HELP_TEXT`, four help topics, `compact_reference`, `README.md`, the CLI
reference, `SKILL.md` (**which must gain `--prefix`, because an agent has no
terminal**) and `plugin/claude-code/tests/lib.sh:92`.

## Measurements you do not have to retake

- **`compact_reference()`'s budget is fine.** D14's exact replacement is **six
  characters shorter** than the two lines it replaces, so the 3000-char headroom
  goes from 14 to 20. The sibling needle test moves from `story project init` to
  `story project new`.
- **The ~130 golden snapshots are not exposed**: no `.snap` names `project
  init`, `deinit` or `relink`.
- **No `project list` parsing helper breaks** when every fixture project gains a
  `checkout` line: all three helpers do `lines().find(|l| l.contains(path))` and
  the project row precedes its own checkout line, so `find` still returns the
  row.
- `story link` / `story unlink` are **top-level aliases for `relate` /
  `unrelate`** and have nothing to do with projects. D16 requires a
  `Not to be confused with:` block in `story help project`, `story help relate`
  and the CLI reference.

## Three things that bit during PR 1

- **`git checkout <path>` is destructive against uncommitted work.** It cost
  forty lines of `main.rs` that had not been committed yet. Use `git stash`, or
  reverse the edit.
- **`cargo fmt` after committing leaves earlier commits failing the gate.** Run
  `cargo fmt --all` *before* each commit, or rebuild the commits — a fix-up
  commit leaves bisect broken.
- **The gate is the thing that finds the defect.** PR 1's first attempt was red
  at one test, and the test was right: `doctor --fix` was leaking a stale
  `checkout_path`.

## Gate

`make test`, supervised in the background with **log growth as the heartbeat**
and a 120-second stall bound. Eleventh consecutive story with no wedge; the
streak is worth keeping. Budget ~10 minutes per run and expect to need two.
