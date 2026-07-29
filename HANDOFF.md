# Handoff — the data-layer rearchitecture, after W5 (the daemon)

**Read first:** `docs/rearch/STATE.md` — Key facts, then the W5 step log.
`docs/spec/data-layer-rearchitecture.md` is the design of record.

**Worktree:** `/Volumes/Code/mikeyward/storyhook/.claude/worktrees/rearch`.
Linked worktree: no version bumps, no deploys, no force-push, never touch main.

## Where the program is

W0 through W4 are merged. **W5 is on `rearch/w5-daemon` with a PR open.**

There is one storyhook process per machine now. It owns the store and serves
everything that talks to it: the dashboard's REST API and change feed on
loopback and the tailnet, and `POST /api/v1/invoke` on loopback only. Every
`story` command goes through it unless `--local` says otherwise.

`~/.storyhook/` is retired — read once, marked `MIGRATED.txt`, never written,
never deleted. `notify` is gone from `Cargo.lock` entirely.

## The gates

```sh
make test           # ~60s warm, 2084 Rust tests, 0 ignored, 17/17 bash
make test-daemon    # ~58s, the same suite over /api/v1/invoke
```

Three things about them that are load-bearing:

1. **`make test` sets `STORYHOOK_INVOKER=local`.** `make test-daemon` sets
   `daemon`. They are deliberately separate; the byte-comparison test is what
   proves the two modes agree.
2. **`--test-threads=4` in `test-daemon` is a bound on live daemons**, not
   tuning. One per test binary plus one per isolated environment, at the default
   fan-out, is dozens of SQLite processes at once. See STATE.md.
3. **`STORYHOOK_DAEMON_ADDR=127.0.0.1:0` and `STORYHOOK_PARENT_PID` are
   exported by four places** — `scripts/run-tests.sh`, `TestEnv`, and *both*
   `plugin/claude-code/tests/{lib.sh,run-tests.sh}`. Nothing may bind 3456 and
   nothing may outlive its run.

## What W5 did not finish, and exactly what is left

**The quarantine is still standing.** `app::run`, `lock.rs`, `registry.rs` and
`storage.rs`'s write half all survive, unused by anything a user can reach.
`invoker_seam.rs::the_legacy_path_is_reachable_only_from_the_web_dashboard`
still enforces it, so nothing new can start depending on them.

It is genuinely dead now — the dashboard was its last real caller and the
dashboard is on the services — so the deletion is mechanical. The blast radius,
measured:

| What holds it up | Where | What to do |
|---|---|---|
| `LegacyInvoker` | `src/invoke.rs` | delete; nothing in `src/` constructs one |
| `handle_register/deregister/list` | `src/web.rs` | delete with `app.rs`'s `Web` arm; `CatalogService` is the live path |
| The differential harness | `tests/differential_*.rs`, `tests/differential_support/` | delete — with no legacy leg there is nothing to differ against |
| `LegacyInvoker` equivalence tests | `tests/invoker_seam.rs` | delete those two tests; **keep** the resolution and project-less ones |
| `registry_test.rs` | whole file | delete with its subject |
| `tui_integration.rs` | drives a legacy tree via `LegacyInvoker` | move onto `StoreInvoker`, as `tui_undo.rs` already is |
| `ProjectBuilder::legacy` | `crates/storyhook-test-support/src/project.rs` | **the only real work.** It builds fixtures by running `app::run`, and `service_migrate.rs` needs legacy trees as input. It needs `init` and `new_story` only: write the tree directly (`project.toml`, `next-id`, `states.toml`, `types.toml`, `open/stories/SH-N.jsonl`) against `src/legacy/`'s layout, which is permanent and read-only. ~90 lines. |

Then flip the quarantine test to assert the doors are **gone** rather than
confined.

## Traps that have already cost time

1. **A guard in the file that is skipped is not a guard.** `lib.sh`'s isolation
   block runs only when `STORYHOOK_TEST_HOME` is unset, and
   `plugin/.../run-tests.sh` sets it. That is how the plugin suite spent a wave
   spawning real daemons.
2. **`DaemonInfo::is_this_binary` asks about the calling process.** Right in
   production, wrong in a test binary — compare against `story_binary()`.
3. **A hook's `story` must be an absolute path in a test**, or it resolves to
   the developer's installed build through `PATH`.
4. **`make test` measurements are worthless on a loaded machine.** Check
   `uptime`, `ps aux | sort -nrk 3`, `ls target/debug/deps | wc -l`.
5. **`git commit --amend` silently fails here.** Use reset --soft plus a fresh
   commit.
6. **Story IDs go in commit BODIES, never subjects** — the post-commit hook
   scans subjects and re-dirties the tree.

## Next: W6, the git features

`fix(git): scan full commit bodies for story references now that sync is
churn-free` — SH-56 and SH-58 (`%s` → `%B`), gated on W4 and now unblocked.
github-sync's state is already off `.storyhook` (`StoreSyncStorage` reads the
context's `Environment`), so what is left is the body scan, commit-sync
idempotency as a DB constraint (`StoryCommitLinked` + unique
`(project, story_no, sha)`), and SH-61's termination test.

## Owed to the user, not yet done

- `rm -rf ~/.local/state/storyhook` — three fixture `.jsonl` files a test run
  wrote there before `StoreSyncStorage` took its destination from the
  environment. The permission classifier declined the delete; it needs Mikey.
- The defects this program has found still need stories filed (task #15). W5
  adds seven, all **fixed in-wave**: a `GET` publishing a change event; the
  per-connection `PRAGMA data_version`; relative paths resolved in the daemon;
  stdin unable to cross the wire; `daemon stop` routing through the daemon;
  `daemon start` reusing another build's daemon; and the plugin suite spawning
  daemons on the production port. Not filed from this worktree — minting ids
  here collides with ids minted in parallel worktrees.
