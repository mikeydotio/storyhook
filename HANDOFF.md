# Handoff — the data-layer rearchitecture, after W6 (the git features)

**Read first:** `docs/rearch/STATE.md` — Key facts, then the W6 step log.
`docs/spec/data-layer-rearchitecture.md` is the design of record.

**Worktree:** `/Volumes/Code/mikeyward/storyhook/.claude/worktrees/rearch`.
Linked worktree: no version bumps, no deploys, no force-push, never touch main.

## Where the program is

W0 through W4 are merged. **W5 and W6 have PRs open.**

The legacy write path is **gone**. `app.rs`, `lock.rs`, `registry.rs` and
`github/auto.rs` are deleted; the differential harness is deleted with the leg
it compared against. Nothing under `src/` may name `crate::storage`, and
`invoker_seam.rs::the_legacy_write_path_is_gone` fails if anything starts.

`commit-sync` and the post-merge hook read **whole commit messages** now, and a
commit link is event kind #18 with a primary key behind it rather than a string
scan.

## The gates

```sh
make test           # 111.7s warm, 1931 Rust tests, 0 ignored, 17/17 bash
make test-daemon    # 53.2s, the same suite over /api/v1/invoke
```

Load-bearing details are unchanged from W5's handoff (`STORYHOOK_INVOKER`,
`--test-threads=4` as a bound on live daemons, the four places that export
`STORYHOOK_DAEMON_ADDR`). One is new:

**`TestEnv` now sets `PATH`.** A managed git hook runs `story` by *name*, so
without it every fixture that fires a hook exercised the developer's installed
build. That is no longer a trap to remember; it is a property of the harness.

## What W6 left standing, deliberately

**`src/storage.rs` survives, pruned to the rollback writer.** It is the far side
of the two-way door: the rollback procedure is `store → story export →
ProjectExport → a legacy tree`, `storage::import_project` materializes that last
step, and `tests/migrate_round_trip.rs` runs the loop for two fixtures. The W4
revert policy is conditional on it. It also builds `story migrate`'s fixtures,
archived stories included — those live in SQLite and cannot be written by hand.

Twenty-six functions went: everything the CLI used to call and the round trip
does not need.

## Next: W7, the repo cutover

`chore: migrate storyhook's own tracker and retire the .storyhook directory`.

Two things W6 did that W7 should know about:

1. **A migrated project's `[git]` comments arrive as link records.** `story
   migrate` goes through `append_raw_events`, which projects them, so the first
   `commit-sync` after the cutover will *not* re-link this repository's whole
   log. Pinned by `service_migrate.rs::a_migrated_projects_git_comments_arrive_as_link_records`.
2. **The churn loop is dead and proven dead.** `tests/commit_sync_termination.rs`
   asserts a byte-clean `git status` through five passes. After the cutover this
   repository stops writing `.storyhook/` on every commit, which is the whole
   point of the loop having no fuel.

The bash plugin suite's 33 `.storyhook` references are W7's, per the flip
checklist's section F. `src/github/`'s are already at zero and there is a test
holding them there.

## Owed to the user, not yet done

- `rm -rf ~/.local/state/storyhook` — three fixture `.jsonl` files an old test
  run wrote there. The permission classifier declined the delete; it needs
  Mikey. (Unchanged from W5.)
- **The defect ledger** (task #15). W6 adds four, all fixed in-wave: `TestEnv`
  not isolating `PATH`; the colliding pre-migration backup filename; the
  legacy-comment projection that would have re-opened the impersonation hole;
  and `[git] rebase:`-shaped prose being read as a hash by the SQL backfill.
  Still not filed from this worktree — minting ids here collides with ids minted
  in parallel worktrees.
- **`sync.mode = auto` has no implementation.** The flip checklist's G4 flagged
  that auto-sync fired only from `app::run`'s tail and left it for W4 or W5;
  neither settled it, and W6 deleted the code with the rest of that file.
  Nothing regressed — it has not run since the flip — but a user with `auto` in
  their configuration now gets manual behaviour silently. **W8 owes a decision:**
  reinstate it on the invoker, or drop `auto` from the vocabulary and say so.

## Traps that have already cost time

The W5 list still applies. Two to add:

1. **A rusqlite error string may belong to an earlier statement.** The backup
   collision reported `table schema_migrations already exists` — a different
   statement's message, surfaced through a stale `sqlite3_errmsg`. Trust the
   call that failed, not the text it came back with.
2. **`tests/fixtures/schema/v1.db` must never be regenerated.** It exists so a
   future migration has an *old* database to migrate; `regenerate.sh` against
   the current list destroys exactly that. The comparison is pinned to
   `MIGRATIONS[..1]`, and `the_committed_fixture_migrates_forward` is what
   proves it still does its job.
