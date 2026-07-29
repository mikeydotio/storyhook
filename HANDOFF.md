# Handoff — the data-layer rearchitecture, after W4 (the flip)

**Read first:** `docs/rearch/STATE.md` (execution ledger — Key facts, then the
W4 step log), `docs/spec/data-layer-rearchitecture.md` (the design of record).
`docs/rearch/flip-checklist.md` is now closed; its header records what the wave
actually found against what it was planned against.

**Worktree:** `/Volumes/Code/mikeyward/storyhook/.claude/worktrees/rearch`.
Linked worktree: no version bumps, no deploys, no force-push, never touch main.

## Where the program is

W0 through W3 are merged. **W4 is on `rearch/w4-flip` with a PR open, and the
flip has landed on that branch.**

`story` reads and writes one SQLite database per machine. `.storyhook/` is not
created, not read by any command, and not deleted — it stays in this repository
until W7, because it is the only copy of `project.toml`'s `created_at`, its
`sync`/`doctor` settings, and `next-id`'s burned numbers, none of which an
export document carries.

**The program's exit criterion is met.** Both `worktree_truth` tests are
un-ignored and green, with assertions byte-identical to the ones written against
the failing behaviour, verified 20/20 over consecutive runs. The workspace has
zero ignored tests.

## The gate

```sh
make test           # ~50s warm, 2003 Rust tests, 0 ignored, 17/17 bash
```

`make test-store` is gone: the default suite *is* the store now.

Two things about it that are load-bearing rather than tidy, both in
`scripts/run-tests.sh`:

1. **It sets an isolated `STORYHOOK_DATA_DIR` and refuses to run without one.**
   ~45 test files still build fixtures with `tempfile::tempdir()` and inherit
   the process environment; without the override a test run writes into the
   developer's real store.
2. `INSTA_UPDATE=no` keeps the golden corpus a real gate.

## Next: W5, the daemon

W5 is where the quarantine comes down. `app::run`, `storage.rs`'s write half,
`lock.rs` and `registry.rs` all survive unused, reachable only from
`src/web.rs`, because the dashboard still reads `.storyhook/` directly.
`invoker_seam.rs::the_legacy_path_is_reachable_only_from_the_web_dashboard`
fails if anything else reaches them — so the deletion is mechanical once the
dashboard is on the store.

Four things W5 inherits, all recorded in STATE.md's Key facts:

1. **Auto-sync has no home.** `app::run` ended with
   `github::auto::maybe_auto_sync`; `dispatch` has no equivalent tail, so a
   project in `sync.mode = auto` no longer re-syncs after a story-modifying
   command. It is a policy about a whole invocation, so it belongs on the
   invoker rather than in a dispatch arm.
2. **`registry.toml` is adopted, never retired.**
   `service::adopt_legacy_registry` runs on every store open, is idempotent, and
   neither writes nor deletes the file — the dashboard still reads it. The
   `MIGRATED.txt` marker and the file's retirement are W5's.
3. **`error_contract`'s LockTimeout row costs ~5s** and is now SQLite's
   `busy_timeout` rather than `src/lock.rs`'s deadline. If W5 makes it
   configurable, the row gets cheap.
4. **`src/tui/event.rs`'s notify watcher** is the TUI's last white-box
   reference, waiting on the daemon's change feed.

## The rollback, while it is still available

`docs/rearch/flip-checklist.md` §D2 is the procedure, and it is pasted into the
W4 PR body. **The revert policy is conditional on
`cargo test --workspace --test migrate_round_trip` being 4/4 green** — it is, at
this commit. If it ever goes red, the flip is a one-way door.

One narrowing this wave introduced, and it is in the PR body's table: undo now
writes `StoryCommentRetracted`/`StoryAssigneeCleared`, which an older storyhook
cannot decode. A project whose undo has been used will fail `import-project` on
a reverted binary — *loudly*, with serde's unknown-variant error, not silently.

## Traps that have already cost time

1. **`is_project_less` has sprung three times.** Adding an arm to
   `dispatch_unscoped` without adding it there makes the verb fail in an empty
   directory. It is now a test
   (`the_project_less_verbs_all_answer_outside_a_project`), not a warning.
2. **`TestEnv` isolates child processes, not in-process library calls.** If a
   service reads a global path from the environment, an in-process test cannot
   redirect it — make the path a parameter.
3. **`make test` measurements are worthless on a loaded machine.** `web_test`'s
   readiness deadlines are wall-clock. Check `uptime`,
   `ps aux | sort -nrk 3`, and `ls target/debug/deps | wc -l`.
4. **`git commit --amend` silently fails here.** Use reset --soft plus a fresh
   commit.
5. **Story IDs go in commit BODIES, never subjects** — the post-commit hook
   scans subjects and re-dirties the tree.

## Owed to the user, not yet done

- `rm -rf ~/.local/state/storyhook` — three fixture `.jsonl` files a test run
  wrote there before `StoreSyncStorage::backups_dir()` existed. The permission
  classifier declined the delete; it needs Mikey.
- The defects this program has found still need stories filed (task #15). W4
  adds two more to the list, both **fixed in-wave** but both worth a record:
  `story web register --name` was silently dropping the name (the class is "a
  flag accepted and discarded"), and `differential_git`'s empty-window row was a
  latent clock-boundary flake. Not filed from this worktree — minting ids here
  collides with ids minted in parallel worktrees.
