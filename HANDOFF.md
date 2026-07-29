# Handoff — the data-layer rearchitecture, after W7 (the repo cutover)

**Read first:** `docs/rearch/STATE.md` — Key facts, then the W7 step log.
`docs/spec/data-layer-rearchitecture.md` is the design of record.

**Worktree:** `/Volumes/Code/mikeyward/storyhook/.claude/worktrees/rearch`.
Linked worktree: no version bumps, no deploys, no force-push, never touch main.

## Where the program is

W0 through W6 are merged. **W7 has a PR open. W8 is the last wave.**

storyhook now tracks itself the way it asks everyone else to: 61 stories in the
store, one committed `.storyhook.toml`, no `.storyhook/` directory. `story
doctor` exits 0 against the real store.

The backlog is reconciled. Twelve stories closed with acceptance evidence,
twenty-four filed from the rearchitecture's defect ledger (SH-62 … SH-85), five
pre-W0 blocks cleared.

## The gates

```sh
make test           # 1931 Rust tests, 0 ignored, 18/18 bash
make test-daemon    # the same suite over /api/v1/invoke
```

Both green on the branch tip. Load-bearing details unchanged from W6's handoff.

## W8's inbox

Nine open ledger stories, in the order a hardening wave probably wants them:

| Story | What |
|---|---|
| SH-68 | `sync.mode = auto` has no implementation — **decide**: reinstate it on the invoker, or drop `auto` from the vocabulary and say so |
| SH-69 | the daemon leg's `--test-threads=4` is a bound on live daemons; give it a permanent shape |
| SH-63 | `story next` is nondeterministic — the comparator has no total order. Fixing it deletes `golden_cli.rs`'s `>= 1s` sleep |
| SH-62 | positional verbs swallow unknown `--flags` as data |
| SH-67 | `export` silently drops unknown event kinds — the round-trip guarantee is conditional on this |
| SH-66 | `context --format json --json` double-encodes |
| SH-70 | `import-project` of a pre-#18 export does not project its `[git]` link comments |
| SH-65 | `AppError::SyncConflict` is a dead variant — give it a caller or delete it |
| SH-64 | id ordering is numeric in some commands and lexicographic in others |

Two more W8 items that are not stories because they are about this machine and
this harness rather than the product:

- **A bare `cargo test` still writes into the real store.** `make test` is safe
  — `scripts/run-tests.sh` exports an isolated `STORYHOOK_DATA_DIR` and refuses
  to run if it is not under `/private/tmp` — but nothing stops someone running
  `cargo test` directly, and one already did: the real store held a junk project
  named `.tmpKGBY3a` before this wave started. The refusal belongs somewhere
  `cargo test` cannot route around.
- **`~/.local/state/storyhook` had three stray fixture `.jsonl` files** from an
  old run. Still owed to Mikey; the permission classifier declined the delete.
  (Unchanged since W5. The directory now legitimately holds the daemon's
  runtime files, so this needs a careful hand rather than an `rm -rf`.)

## Owed to Mikey, and it is not optional

**Reinstall the `story` binary once this ships.** `~/.local/bin/story` was built
2026-07-25 and predates the flip: it has no `migrate` verb and it still creates
`.storyhook/lock` wherever it stands. The managed git hooks run `story` by
*name*, so until it is replaced, every commit in every one of his repositories
leaves that residue and the hooks quietly do nothing. It was observed on this
wave's own first commit, which is why `/.storyhook/` is now in `.gitignore`.

**One command after merging this PR:** `story web register` from the main
checkout. The store's recorded path for this project is currently *this
worktree*, because the migration ran against a copy at a scratch path and the
worktree is what could be registered from here. Nothing breaks until then —
resolution answers by the pointer file's uuid, not by path — but the dashboard
will name a directory that is about to be deleted.

## Traps that have already cost time

The W5 and W6 lists still apply. One to add:

**A `story` binary from before the flip is a live hazard, not a historical
note.** W6 fixed it inside the harness by putting the binary under test at the
front of `PATH`. Outside the harness there is no such fix, and the symptom — a
stray `.storyhook/` appearing in a clean tree — reads like a storyhook bug when
it is a stale install. Check `story --version` and `story help | grep migrate`
before believing anything else.
