# Handoff — `story project init|deinit|list`

**Branch:** `feat/project-lifecycle-verbs`, ten commits off `main` (v1.0.0).
**Stories:** SH-97 (umbrella) → SH-98…SH-107, all closed. Closes **SH-92**.
**Plan of record:** `~/.claude/plans/now-that-storyhook-manages-kind-sunrise.md`.
The rearchitecture's own record is still
[`docs/rearch/STATE.md`](docs/rearch/STATE.md); all nine waves are merged.

## What changed and why

Registration was vestigial. `story web register` existed because the dashboard
used to read `~/.storyhook/registry.toml`, a second file naming the repos it
should show. In the store there is no second file, and `init` has recorded the
checkout itself since the flip — so `web register` re-did what `init` had already
done, and "registering with the web UI" named a concept with nothing behind it.

Separately, the dashboard **hid** projects: `CatalogService::list` filtered out
any project with no checkout row and every per-project route 404'd the rest. A
project whose directory was deleted and then forgotten by `story doctor --fix`
had all of its stories in the database the daemon had open, and no surface at all
from which to reach them.

- `story project init [PATH] [--prefix P] [--name N] [--no-agents-md]`
- `story project deinit [PATH|SLUG] [--force]` — **hard delete**, always confirmed
- `story project list` — every project, checkout or not
- `story init` and `story web register|deregister|list` are **gone** (breaking)
- The dashboard serves every project, read-only when this machine has no checkout

## Four things to know before touching it

1. **Schema 3 narrows the append-only guard rather than lifting it.**
   `events_reject_delete` now abstains only for an event whose project no longer
   exists — a state nothing but `delete_project`'s own second-to-last statement
   can produce. `events_reject_update` is untouched. Verified against the bundled
   SQLite 3.46 rather than assumed: a cascade *does* fire the child's BEFORE
   DELETE trigger, so `ON DELETE CASCADE` would have tripped the very guard it
   was meant to route around.
2. **Deleting `projects` before `events` relies on `defer_foreign_keys`**, already
   set in `SqliteWriteTx::begin`. If that ever goes, the first statement fails
   loudly and nothing is written — `store_rebuild.rs` pins both halves.
3. **Deinit's confirmation renders in the client, not the service.** An unforced
   deinit returns `Response::ConfirmationRequired(DeinitPlan)` having written
   nothing; `main.rs` prompts and re-sends with `force`. That is what makes the
   daemon transport work at all, and the dashboard's modal renders the *same*
   `DeinitPlan`, so the two front-ends cannot grow two different warnings.
4. **`AGENTS.md` is removed only on an exact byte match** with what the current
   template would generate. When the template changes, older files stop matching
   and are kept — the safe direction, and `templates.rs` says so. Do not loosen
   it to a fuzzy match.

## The gates

```sh
make test           # in-process, plus 18/18 bash
make test-daemon    # the identical suite over /api/v1/invoke
make gate           # both
```

Both green on the branch tip.

## What remains — from `main`, after merge

1. **Reinstall the `story` binary.** `~/.local/bin/story` predates both the flip
   and this rename, so `story init` still works there and `story project init`
   does not. It is also why `plugin/claude-code/tests/run-tests.sh` fails when run
   standalone but passes under `make test`, which puts `target/debug` on `PATH`.
2. **`/semver bump major`** — v1.0.0 → v2.0.0. Two verb families removed. Never
   from a worktree.

*(The old item "`story web register` from the main checkout" is obsolete: that
verb is gone, and the dashboard now shows every project the store knows
regardless of which path is recorded.)*

## Known flakes, filed not fixed

- **SH-108** — `concurrency_soak::mixed_local_and_daemon_clients_under_load_lose_nothing`
  times out on its first command under full-suite load. Ruled out as caused by
  migration 3: the soak's store is fully migrated before any client thread spawns.
- **SH-110** — `web_start_status_address_advertise_magic_dns_fqdn_when_available`
  fails when the daemon's best-effort tailnet bind loses under `--test-threads=4`.

Both pass in isolation and on an immediate re-run. Neither is caused by this
branch.

## Deviations from the plan, recorded rather than edited away

- **`src/storage.rs` still says `story init`.** It is the rollback path
  `migrate_round_trip` exercises, and `invoker_seam.rs`'s
  `the_legacy_write_path_is_gone` fails if a `src/` file so much as names
  `crate::storage`.
- **`available: false` gained companions.** The plan said pathless projects would
  report `available: false`; in the dashboard that value already meant
  *unclickable*, so the response carries `available` + `read_only` + `reason`.
- **Deinit clears every recorded checkout**, not only the directory the caller
  named. Found by the web tests: deleting by slug left a `.storyhook.toml` naming
  a project that no longer existed, which the next `init` would have silently
  resurrected as an empty project. The plan lists every file first; that listing
  is the consent.
- **`story help --compact` went over its 3000-char budget** when the new verbs
  were added, and was trimmed back to 2991 rather than the cap being raised.

## The backlog the rearchitecture leaves behind

Seven ledger stories, all ordinary product defects rather than rearchitecture
work. They are filed, described and reproducible; W8 deliberately did not take
them, because a storage rearchitecture's last wave is the wrong place for a
nondeterministic sort comparator.

| Story | What |
|---|---|
| SH-63 | `story next` is nondeterministic — the comparator has no total order. Fixing it deletes `golden_cli.rs`'s `>= 1s` sleep |
| SH-62 | positional verbs swallow unknown `--flags` as data |
| SH-67 | `export` silently drops unknown event kinds — the round-trip guarantee is conditional on this |
| SH-66 | `context --format json --json` double-encodes |
| SH-70 | `import-project` of a pre-#18 export does not project its `[git]` link comments |
| SH-65 | `AppError::SyncConflict` is a dead variant — give it a caller or delete it |
| SH-64 | id ordering is numeric in some commands and lexicographic in others |

Two more:

| Story | What |
|---|---|
| SH-68 | `sync.mode = auto` — **now a design question, not a regression.** Removed from the vocabulary; reinstating it means a GitHub call on the tail of every mutating command, in the daemon as well as locally. W8's ruling is a comment on the story |
| ~~SH-92~~ | ~~there is no supported way to delete a project~~ — **closed by this branch.** Migration 3 drops the guard inside its own migration and says so; `story project deinit` is the sanctioned operation |

W8 also filed **SH-86 … SH-91**, closed, as records of the six defects it found
and fixed; and closed **SH-69** with the daemon-leg ruling. `story doctor` is
clean against the real store.

## Traps that have already cost time

The W5, W6 and W7 lists still apply. Three to add, all from W8:

- **A test that asks about bytes on disk must not have a daemon.** A daemon holds
  the store open with its own page cache and log handle: it answers from memory,
  keeps alive a write-ahead log that would otherwise be discarded, and does not
  notice the file being replaced. Use `ProjectBuilder::local()`. The same fact
  reaches users — a restore has to stop the daemon first, and the diagnostic
  says so.
- **The binary left by `cargo test` refuses to touch a real store**, because it
  carries the `fault-injection` feature and that is now the test-build sentinel.
  `cargo build` — which `make test` ends with — produces a usable one.
- **`mv file.bak file` gives the file its *old* mtime, so cargo does not
  rebuild.** A reverted `sed -i.bak` experiment silently ran for three test runs
  afterwards. `touch` it, or use `git checkout --`.
