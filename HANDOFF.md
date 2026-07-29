# Handoff — the data-layer rearchitecture, after W2d

**Read first:** `docs/rearch/STATE.md` (execution ledger — Key facts, then the
W2d step log), `docs/rearch/flip-checklist.md` (the enumerated W4 work, now with
section G), `docs/spec/data-layer-rearchitecture.md` (the design of record).

**Worktree:** `/Volumes/Code/mikeyward/storyhook/.claude/worktrees/rearch`.
Linked worktree: no version bumps, no deploys, no force-push, never touch main.

## Where the program is

W0, W0b, W1, W2a, W2b, W2c are merged. W2d is on `rearch/w2d-git` with a PR open.

**The port is complete.** All 48 `Invocation` variants dispatch to the
store-backed stack; `invoke::dispatch`'s match is exhaustive without a
catch-all, so a new variant stops the file compiling until someone decides what
it does. `unported_probes()` in `tests/differential_lifecycle.rs` is empty.

One *action* is owed a design rather than a port: `History::Restore` replaces a
story's history, which an append-only store cannot do. It refuses loudly and
names the checklist.

`src/app.rs` has not changed since W2c. Nothing routes production traffic
through the new stack yet — `STORYHOOK_INVOKER` defaults to `legacy`.

## The store test leg — use it

```sh
make test-store     # 38 targets, ~6.4s, green
```

The same integration suite against `STORYHOOK_INVOKER=local`: the real `story`
binary, dispatch, the services, a `SqliteStore` at an isolated data home.
`golden_cli` is in it and green — all 27 byte-compatibility snapshots identical
on both legs.

**Standing rule from W2d onward:** run it after every commit alongside
`make test`, and record both times in STATE.md. It is not part of `make test`
(that would double a gate every wave pays on every commit).

Its exclusion list — file, reason, burn-down wave — is flip-checklist section G,
and **must only ever shrink**. Most entries are W4's: they fail because a
fixture writes into `.storyhook/`, which is precisely what the flip deletes.

## Next: W3, the legacy importer (`story migrate`)

Everything it needs exists:

- `service::transfer::import_project(&store, root, &Clock, &ProjectExport)`
  materializes a project from an export document, ids and histories included.
- `WriteOps::append_raw_events` + `RawEvent` preserve event kinds this binary
  does not understand, byte for byte.
- `WriteOps::reserve_story_no` puts the counter above the imported ids.
- `tests/story_export.rs::export_import_export_is_byte_identical` and
  `service_transfer.rs::a_project_round_trips_through_export_and_import_byte_for_byte`
  are the oracle, on the legacy and store legs respectively.
- Two fixtures are needed, not one: `docs/rearch/baseline/golden-export.json`
  (61 stories, default catalog, no members) and `story_export.rs`'s synthetic
  project (custom states, custom types, a member). Neither alone is sufficient.

`import_project` refuses a project that already holds stories. W3 has to decide
whether `story migrate` re-runs are idempotent by refusing, or by comparing.

## Traps that have already cost time

1. **`TestEnv` isolates child processes, not in-process library calls.** Two
   tests in this wave wrote into the developer's real home directory before this
   was understood. If a service reads a global path from the environment, an
   in-process test cannot redirect it — so make the path a parameter. See Key
   facts.
2. **`make test` measurements are worthless on a loaded machine.** `web_test`'s
   readiness deadlines are wall-clock; a neighbouring project's test suite has
   twice made it look like a regression. Check `uptime` and
   `ps aux | sort -nrk 3` before hunting one.
3. **`git commit --amend` silently fails here.** Use reset --soft plus a fresh
   commit.
4. **Story IDs go in commit BODIES, never subjects** — the post-commit hook
   scans subjects and re-dirties the tree.

## Owed to the user, not yet done

- `rm -rf ~/.local/state/storyhook` — three fixture `.jsonl` files a test run
  wrote there before `StoreSyncStorage::backups_dir()` existed. The permission
  classifier declined the delete; it needs Mikey.
- The defects this program has found still need stories filed (task #15). Not
  from this worktree — minting ids here collides with ids minted in parallel
  worktrees.
