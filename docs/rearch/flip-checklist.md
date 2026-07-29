# The W4 flip checklist

> **What this is:** a complete enumeration of the test-suite work the W4 flip
> (`feat!: move story data to a single global SQLite store`) must do, with file:line
> references. It is a *census*, not a plan — W4 owns the sequencing (see the internal commit
> order in [the spec](../spec/data-layer-rearchitecture.md#waves)).
>
> **Why it exists:** at the flip, `.storyhook/` stops being where story data lives. Every test
> that names a path inside it, fabricates state by writing into it, or calls a library function
> that W4 deletes, stops compiling or stops meaning what it says. Discovering that mid-flip —
> in the one wave budgeted as a single uninterrupted session with no safe internal handoff
> point — is the failure this file prevents.
>
> **Counted 2026-07-28 at `1a6eb8b`.** Every count below is reproducible with the command
> printed beside it; re-run them at the start of W4 rather than trusting these numbers, because
> W0b–W3 will move some.

## Summary

| Category | Count | Owner |
|---|---|---|
| A. `.storyhook` path references | **104 refs across 26 files** (99 in `tests/`, 5 in the support crate) | W4 |
| B. Raw-state fabricators needing `inject_events()` | **10 sites in 4 files**, covering 8 tests | W4 |
| C. White-box calls into APIs W4 deletes | ~~85 sites in 6 files~~ → **14 sites in 6 files** after W2c | W4 |
| D. `#[ignore]`d worktree-truth tests | **2** — un-ignoring them is W4's exit criterion | W4 |
| E. `TODO(rearch)` scratch_dir migrations outstanding | **45 files** | opportunistic, any wave |

The plan estimated "~85 refs across 18 files" for category A. The real figure is **104 across
26**; the two named hot spots were accurate (`init_command.rs` 20, `session_start.rs` 14). The
plan estimated "~8 tests" fabricating corruption via raw JSONL; that is right as a *test* count
(8) but they occupy 10 sites, and two of them corrupt by deleting or by writing TOML rather
than by writing JSONL.

---

## A. `.storyhook` path references

```sh
grep -rn '\.storyhook' tests/ crates/ | wc -l          # 104
grep -rn '\.storyhook' tests/ crates/ | cut -d: -f1 | sort -u | wc -l   # 26
```

Grouped by what the reference actually pins, because the remedy differs per group. The seven
groups partition all 104 refs: 15 + 22 + 23 + 10 + 11 + 18 + 3 = 102, plus the 2
`worktree_truth.rs` fixture lines covered in section D.

### A1. Repo-layout assertions — the tree stops existing (15 refs, 7 files)

These assert that `story init` creates the legacy directory tree. After the flip, `init` writes
one pointer file and a DB row (locked decision 7), so each becomes an assertion about the
pointer file plus a store query.

| Site | Asserts |
|---|---|
| `tests/init_command.rs:17` | `.storyhook/project.toml` exists |
| `tests/init_command.rs:18` | `.storyhook/states.toml` exists |
| `tests/init_command.rs:19` | `.storyhook/types.toml` exists |
| `tests/init_command.rs:20` | `.storyhook/open/stories` exists |
| `tests/init_command.rs:21` | `.storyhook/archive/archive.db` exists |
| `tests/web_test.rs:2404` | `.storyhook/project.toml` exists after a REST-driven init |
| `tests/registry_test.rs:220` | `.storyhook/project.toml` exists after registering a repo |
| `crates/storyhook-test-support/src/project.rs:296` | `ProjectBuilder`'s own postcondition — **fix this first**, it gates every fixture in the suite |
| `tests/story_state_archive.rs:33` | `.storyhook/archive/archive.db` exists |
| `tests/story_delete.rs:226`, `:362` | opens `archive/archive.db` with `rusqlite::Connection` and queries it |

`story_delete.rs` and `story_state_archive.rs` also assert the *absence* of the open JSONL after
archival (`story_delete.rs:222`, `story_state_archive.rs:36`, `cli_grammar.rs:102`, `:236`) —
four assertions of the open/archive split, which the `stories.archived` column deletes outright.
Their behavioral intent ("an archived story leaves the open set but stays readable") survives;
the mechanism does not.

### A2. Config-file paths — reads and writes of per-project config (22 refs, 7 files)

Per-project config becomes columns (`project_states`, `project_types`, `project_members`, …),
so these move to either a CLI round-trip or a store read.

- `tests/session_start.rs` — `.storyhook/plugin-config.toml` at `:67`, `:93`, `:712`, `:743`,
  `:775`, `:805`, `:828`, `:859`, `:892` (9 sites; the plugin-config family is the single
  largest cluster in the file)
- `tests/session_start_hook.rs:149` — same file, different binary
- `tests/tui_integration.rs` — writes `.storyhook/states.toml` at `:153`, `:527`, `:678`,
  `:835`, `:873`
- `tests/story_states.rs:22` — `states_toml()` helper, read by ~20 tests in that file
- `tests/event_hooks.rs` — writes `.storyhook/hooks.toml` at `:45`, `:84`, `:122`, `:151`
- `tests/error_contract.rs:178` — writes a malformed `.storyhook/github-sync.toml` to provoke
  `AppError::Config`
- `tests/member_add.rs:24` — reads `.storyhook/members.jsonl` and asserts the appended event's
  contents; becomes a `project_members` read

**Open question W4 must settle:** `plugin-config.toml` and `hooks.toml` are *user-authored*
config, not story data. Locked decision 7 says nothing but the pointer file is written by
storyhook — it does not say the user may not commit a config file. If they stay in the repo,
these 15 sites survive unchanged and only their parent directory moves; if they migrate to the
store, all 15 are rewrites. Decide before touching them.

### A3. Scaffolded `CLAUDE.md` (23 refs, 4 files)

`init` writes `.storyhook/CLAUDE.md` (agent instructions) and tests assert its contents:

- `tests/init_command.rs:34`, `:37`, `:41`, `:55`, `:58`, `:62`, `:76`, `:79`, `:83`, `:87`,
  `:91`, `:95`, `:109`, `:112`, `:116` (the other 15 of that file's 20)
- `tests/mcp_removal.rs:231`, `:234`, `:238`
- `tests/fix_cycle_5.rs:101`, `:104`, `:132`
- `tests/scaffold.rs:112` (asserts the *stdout* string `.storyhook/CLAUDE.md`), `:141`

**Open question W4 must settle, and it is user-visible:** a scaffolded CLAUDE.md is a repo
artifact by design — its whole purpose is to be read by an agent working in that repo. Either
the flip keeps writing it (and only its path changes, e.g. alongside the pointer file), or the
feature moves/dies. This is the one place where "zero repo writes during normal operation"
collides with a shipped feature; `init` is arguably not normal operation. Resolve it in the
spec, not in a test file.

### A4. Global-state paths — `~/.storyhook/` (10 refs, 4 files)

Absorbed into XDG data/state homes by locked decision 6.

- `tests/web_test.rs:94`, `:127`, `:257`, `:2424` (comments describing `~/.storyhook/web.pid`
  and `registry.toml` isolation), `:2517`, `:2554` (registry path construction)
- `tests/registry_test.rs:1`, `:332`; `crates/storyhook-test-support/src/env.rs:14`,
  `server.rs:125` (docs + path construction)

`tests/registry_test.rs` as a whole is not a rewrite target but a **deletion** target: `registry.rs`
is deleted in W4 and the file's subject goes with it. Its 8 white-box call sites are listed in
category C.

### A5. Corruption/absence fixtures — see category B (11 refs, 5 files)

`tests/doctor.rs:34`, `:76`, `:84`, `:131`; `tests/error_contract.rs:466` (the `story_log`
helper); `tests/session_start.rs:654`, `:680`, `:681`; `tests/tui_integration.rs:651`, `:797`;
`tests/move_if_state.rs:349` (reads the event log back to assert *exactly one* state-change
event was written — a double-write detector that must survive as an events-table count).

### A6. Prose only — no code change, but stale after the flip (18 refs, 10 files)

Doc comments and assertion messages that *describe* the legacy layout:
`tests/worktree_truth.rs:4`, `:8`, `:34`, `:56`, `:59`, `:88`, `:147`;
`tests/story_export.rs:212`; `tests/story_decompose.rs:275`; `tests/session_start.rs:38`, `:53`;
`tests/session_start_hook.rs:129`, `:135`; `tests/web_test.rs:793`; `tests/registry_test.rs:210`;
`tests/help_flag_sweep.rs:103`; `crates/storyhook-test-support/src/lib.rs:9`,
`project.rs:2`. Cheap to fix, and a lying comment in the file that *proves the flip worked*
(`worktree_truth.rs`) is worse than a broken assertion — at least the latter fails.

### A7. Two structural fixtures that need real thought (3 refs, 2 files)

- **`tests/help_flag_sweep.rs:135`** — `snapshot()` walks every file under `.storyhook` and
  `every_verb_answers_a_help_flag_with_help_and_changes_nothing` (`:190`) asserts the tree is
  byte-identical before and after 54 × `--help`. This is SH-52's regression test and its entire
  power comes from fingerprinting *all* mutable state. Post-flip the mutable state is the
  database: the fingerprint must become a store-level digest (event count + read-model hash, or
  `PRAGMA data_version`), or the test silently becomes a tautology over an empty directory.
  **A tautological green here re-opens SH-52.**
- **`tests/web_test.rs:2300`–`:2309`** — `remove_dir_all(".storyhook")` fabricates "a repo that
  was registered but whose tracker later vanished". Post-flip the equivalent is a pointer file
  naming a project uuid absent from the store, which is a *different* failure the dashboard must
  still survive. Rewrite as a real case, not a delete.

---

## B. Raw-state fabricators — need `store::test_support::inject_events()`

```sh
grep -rn 'fs::write\|fs::remove_file\|fs::remove_dir_all\|OpenOptions' tests/ | grep -i 'storyhook\|story_log\|stories_dir'
```

Eight tests fabricate states the public API refuses to produce. They must keep doing exactly
that — they are the only coverage of what happens when the store is already wrong — so W4 owes
them a validation-bypassing injection API rather than a rewrite that makes them well-behaved.

| # | Test | Site | Fabricates |
|---|---|---|---|
| 1 | `doctor.rs::doctor_reports_missing_inverse_edge` (fn `:10`) | `:33` | a `blocks` edge with no inverse |
| 2 | `doctor.rs::doctor_reports_parent_cycle_and_show_suppresses_virtual_relationships` (fn `:52`) | `:75`, `:83` | a parent↔child cycle (two files, mutually inconsistent) |
| 3 | `doctor.rs::doctor_flags_unknown_story_type` (fn `:113`) | `:130` | `StoryTypeSet` naming a type that does not exist |
| 4 | `error_contract.rs` Integrity row (`cases()` `:53`) | `:129` | an **empty** event log → folds to a story with no state |
| 5 | `error_contract.rs` Storage row | `:145` | unparseable bytes in the event log |
| 6 | `error_contract.rs` Config row | `:177` | malformed `github-sync.toml` (TOML, not JSONL) |
| 7 | `session_start.rs::session_start_corrupted_stories_dir_still_returns_json` (fn `:649`) | `:655`–`:656` | replaces the stories *directory* with a regular file |
| 8 | `session_start.rs::session_start_missing_project_toml_still_returns_json` (fn `:676`) | `:681` | deletes `project.toml`, keeps the rest |
| 9 | `tui_integration.rs::story_deleted_externally_closes_modal_with_notification` (fn `:638`) | `:652` | deletes a story's log mid-session (race) |
| 10 | `tui_integration.rs::incomplete_trailing_json_line_tolerated` (fn `:792`) | `:800` | a torn final line (concurrent write in flight) |

**What `inject_events()` must support**, derived from the ten rows above — the plan's one-line
description ("validation-bypassing") is not sufficient:

1. Write an event sequence for a story **bypassing service invariants** (rows 1–3).
2. Write an **empty** sequence, and write **bytes that are not valid events** (rows 4–5) — so
   the API must accept raw bytes, not just `Vec<StoryEvent>`; a typed-only API cannot express
   these two rows and they are two distinct `AppError` variants in the contract table.
3. **Delete** a story's events out from under a live reader (row 9).
4. Rows 6–8 are not event injection at all: they corrupt project-level config and structure.
   They need either a store-level equivalent (a project row with a NULL/absent required column)
   or an honest re-frame. Do not force them through `inject_events()`.

Row 10's "torn write" has no analogue under a transaction — SQLite cannot expose a half-written
row. That is a *win* (the failure mode is gone), but the test must be converted to something
that still proves tolerance, or deleted with an explicit note in the flip PR saying the class
was eliminated rather than the coverage dropped.

---

## C. White-box calls into APIs W4 deletes (85 sites, 6 files)

```sh
grep -rn 'storyhook::storage\|storyhook::lock\|storyhook::registry\|ProjectPaths' tests/ crates/ | wc -l   # 85
```

W4 deletes `lock.rs`, `registry.rs`, and the write half of `storage.rs`. Every site below stops
compiling — a **hard** break, unlike category A's silent-wrong-answer breaks.

| File | Sites | Owner | Note |
|---|---|---|---|
| ~~`tests/tui_integration.rs`~~ | ~~50~~ → **0** | done (W2c) | Reconstructed onto the Invoker seam. |
| ~~`tests/tui_undo.rs`~~ | ~~20~~ → **0** | done (W2c) | Same. Undo is now `Invocation::History::{Read,Restore}`; **`Restore` is still unported to the store**, so W4 (or W8) owes it the append-only design this row originally flagged. |
| `tests/registry_test.rs` | 3 | W4 | Deleted with `registry.rs`; the *behavior* (many checkouts of one project) is what `worktree_truth.rs` asserts instead. |
| `tests/web_test.rs` | 5 | W4 | Registry registration in fixtures; becomes a `projects` row. |
| `crates/storyhook-test-support/src/server.rs` | 1 | W4 | The harness's own registration helper — fix before the tests that call it. |
| `tests/error_contract.rs` | 1 | W4 | Holds the real project lock from the test process to provoke `LockTimeout`. With `lock.rs` gone this becomes "hold a write transaction"; the ~5s cost noted in STATE.md goes away if `busy_timeout` is configurable. |
| `tests/differential_support/mod.rs`, `tests/differential_config.rs` | 4 | W3/W4 | The differential harness reads a legacy project's catalog to seed the store leg. These *should* exist until the legacy leg is retired; they go when the harness does. |

**Consequence for scheduling, resolved:** the 70 TUI sites were W2c's, and W2c ported them.
`src/tui/` retains exactly one white-box reference — `event.rs`'s notify watcher, which W5
deletes — and `src/tui/`'s own `#[cfg(test)]` modules have none.

Also cleared by W2c: **`src/tui/data.rs` and `src/tui/app.rs` no longer build fixtures in
`$TMPDIR` through `storage::`**, though both still carry their `TODO(rearch)` marker for the
`tempfile::tempdir` calls themselves (category E).

---

## D. The exit criterion

```sh
cargo test --workspace --test worktree_truth -- --ignored    # currently 2 failed
```

| Test | Line | Ignore reason |
|---|---|---|
| `two_worktrees_of_one_repo_mint_colliding_ids` | `tests/worktree_truth.rs:118` (attr `:117`) | `SH-46: two checkouts are separate databases; goes green at the W4 flip` |
| `a_story_created_in_one_checkout_is_visible_from_the_other` | `tests/worktree_truth.rs:158` (attr `:157`) | same |

**Removing those two `#[ignore]` attributes, with both tests green in the normal `make test`
run, is W4's exit criterion.** They are the only two ignored tests in the workspace, and
`scripts/capture-baseline.sh` asserts that count against `docs/rearch/baseline/known-red.md` —
so an `#[ignore]` cannot be quietly added to keep the gate green.

Note the fixture: `two_checkouts_of_one_repo()` (`:64`) *commits* `.storyhook/` and
fast-forwards both worktrees onto that commit, because without it the worktrees resolve no
tracker at all (exit 3) — a different failure from the one under test, pinned by an assertion at
`:86`–`:88`. **At the flip that fixture must be rebuilt around the pointer file** while the two
assertions stay byte-identical. Changing an assertion in this file to make it pass is the one
edit that would make the whole program unfalsifiable.

---

## E. `TODO(rearch)` scratch_dir migration list

```sh
grep -rl 'TODO(rearch)' tests/ src/ crates/ | wc -l    # 45
```

**45 files**, each carrying `// TODO(rearch): migrate to storyhook_test_support::scratch_dir`
plus `#![allow(clippy::disallowed_methods)]` (the `clippy.toml` ban on raw `tempfile::tempdir`).
42 are under `tests/`; 3 are in `src/` (`storage.rs`, `tui/app.rs`, `tui/data.rs`).

This list is **not** W4 work. It exists so the count can only ever shrink: the wave that touches
a file's subject migrates it, and the grep is the live ledger. W4 will incidentally clear a
large share of it, because the files it must rewrite anyway are the same ones.

---

## F. Explicitly out of scope for W4

- **The bash plugin suite** — 33 `.storyhook` references across 15 files under `plugin/`
  (`story.sh` 9, `references/cli-reference.md` 4, `tests/test-dispatch-cwd.sh` 3,
  `tests/lib.sh` 2, `hooks/post-git.sh` 2, `hooks/stop-handoff.sh` 2, plus skills and docs).
  These belong to **W7** (`chore: migrate storyhook's own tracker and retire the .storyhook
  directory`). W0 already gave the suite XDG data-home isolation (`f4eefe1`), which is what
  keeps it from writing into the real store the moment the flip lands — that isolation is
  load-bearing between W4 and W7, not a nicety.
- **`src/` `.storyhook` literals** — the flip changes them by construction; they are not a
  checklist item.
- **The `#[cfg(test)]` modules inside `src/`** — they deliberately do not use the test-support
  crate (see STATE.md's note on linking two copies of `storyhook`), so nothing here applies to
  them.

---

## G. The store leg's exclusion list

```sh
make test-store        # STORYHOOK_INVOKER=local over the integration suite
```

**Added W2d 2026-07-28.** The store leg runs the same integration suite against
`STORYHOOK_INVOKER=local` — dispatch, the services, and a `SqliteStore` at the
XDG data home — instead of against `.storyhook/`. It is the strangler's proof
engine: every failure in it is a thing the flip would break, found while the
legacy path is still the default.

**Green as of W2d over 38 targets**, including `golden_cli` — all 27
byte-compatibility snapshots are identical on both legs — and `cli_grammar`,
`scaffold`, `story_states`, `story_delete`, `error_contract`,
`session_start_hook`, `move_if_state` and `story_flow`.

The list below must only ever shrink. Every entry names why it is out and the
wave that puts it back.

### G1. Not CLI-driven — permanently out, and not a burn-down item

`--exclude-prefix store_`, `service_`, `differential_`; `--exclude-file
invoker_seam`, `wire_envelope`. These call the library in-process, so
`STORYHOOK_INVOKER` has no effect on them at all; running them in the leg would
run them twice and prove nothing. They stay out after the flip.

### G2. Files excluded whole

| File | Failing | Reason | Burn-down |
|---|---|---|---|
| `web_test` | all | The dashboard's HTTP server reads the legacy registry and `.storyhook/` directly; it is not on the Invoker seam yet. | **W5** |
| `registry_test` | all | Tests `registry.rs`, which W4 deletes. Its subject goes with it. | **W4** (deletion) |
| `tui_integration` | most | Fabricates state by writing `.storyhook/states.toml` and deleting story logs (checklist A2/B). | **W4** |
| `tui_undo` | all | Undo is `History::Restore`, which the store cannot serve — see C. | **W4/W8** |
| `worktree_truth` | both | The `#[ignore]`d exit criterion. It goes green *at* the flip, not before. | **W4** |
| `doctor` | 3/4 | Fabricates dangling edges and parent cycles by writing raw JSONL (category B rows 1–3). | **W4** |
| `event_hooks` | 4/6 | Writes `.storyhook/hooks.toml`, which does not exist without the legacy tree (A2). | **W4** |
| `init_command` | 5/5 | Asserts the whole legacy directory tree exists (A1) and reads the scaffolded `.storyhook/CLAUDE.md` (A3). | **W4** |
| `member_add` | 1/1 | Reads `.storyhook/members.jsonl` and asserts the appended event (A2). | **W4** |
| `session_start` | 10/31 | Nine `plugin-config.toml` sites plus two corruption fixtures (A2, B rows 7–8). | **W4** |
| `help_flag_sweep` | 2/3 | `snapshot()` fingerprints the `.storyhook` tree; over an empty directory it is a tautology, and **a green tautology there re-opens SH-52** (A7). | **W4** |

### G3. Individual tests skipped, their file otherwise in the leg

This is the granularity that matters: eleven files stay in the leg because one
or two white-box assertions are skipped rather than the whole file dropped.

| Test | Reason |
|---|---|
| `init_creates_storyhook_claude_md_with_prefix` | reads `.storyhook/CLAUDE.md` (A3) |
| `sh35_init_claude_md_does_not_reference_graph_tree` | same |
| `sh35_init_claude_md_graph_section_only_has_valid_flags` | same |
| `init_generated_claude_md_does_not_mention_mcp` | same |
| `delete_archives_and_removes_open_jsonl` | asserts the open/archive file split (A1) |
| `doctor_fix_heals_stale_archived_snapshot_from_before_the_fix` | opens `archive.db` with rusqlite (A1) |
| `closed_state_moves_story_to_archive_db` | asserts `archive/archive.db` exists (A1) |
| `state_add_stores_description_and_role` + 4 siblings | read `.storyhook/states.toml` through `states_toml()` (A2) |
| `move_if_state_under_real_concurrency_yields_exactly_one_winner` | reads the event log to count state-change events (A5) |
| `hook_outputs_empty_json_when_plugin_disabled` | writes `.storyhook/plugin-config.toml` (A2) |
| `every_error_variant_holds_its_contract` | holds the real project lock via `storyhook::lock` (C) |

### G4. Open questions the leg surfaced, for W4 to settle

- **Auto-sync does not fire under the store leg.** `app::run` ends with
  `github::auto::maybe_auto_sync`, which re-syncs the affected story after every
  story-modifying command when `sync.mode = auto`. `dispatch` has no equivalent
  tail, so under `STORYHOOK_INVOKER=local` a `story comment` on a project in
  auto mode does not sync. Not covered by any test in the suite, which is why
  the leg did not catch it and a reading of `app.rs` did. **W4 or W5 owes it a
  home** — probably the invoker rather than the dispatcher, since it is a
  policy about a whole invocation rather than about one arm.
- **Root resolution now has three tiers, and W4 inherits them.** `StoreInvoker`
  answers project-*less* invocations before it looks for a project (`init`,
  `import-project`, the help family, `version`, `plugin`, `hooks` except
  `test`, and `decompose --dry-run`); resolves the project by pointer file then
  by path; and, failing that, still answers `session-start` with `{}` and
  `scaffold` with default values. Each tier reproduces something the legacy path
  did. Adding a project-less arm to `dispatch` without adding it to
  `is_project_less` makes that verb fail in an empty directory.
- **`web register` cannot find a deregistered checkout again** while the
  pointer file is off: the path was the only handle and forgetting it forgets
  the way back. Turning the pointer on at the flip fixes it, and
  `service_catalog.rs::a_deregistered_checkout_can_be_registered_again` pins the
  current behaviour so the change is deliberate.
