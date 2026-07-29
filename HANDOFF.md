# Handoff — the data-layer rearchitecture is complete

**Read first:** [`docs/rearch/STATE.md`](docs/rearch/STATE.md) — Key facts, then
the W8 step log. [`docs/rearch/hardening.md`](docs/rearch/hardening.md) is the
last wave's record. [`docs/spec/data-layer-rearchitecture.md`](docs/spec/data-layer-rearchitecture.md)
is the design of record.

**Worktree:** `/Volumes/Code/mikeyward/storyhook/.claude/worktrees/rearch`.
Linked worktree: no version bumps, no deploys, no force-push, never touch main.

## Where the program is

**All nine waves are done.** W0 through W6 are merged; W7 and W8 have PRs open,
and W8 is stacked on W7 — it was branched off `rearch/w7-cutover` because #71 was
still open when it started, so **it needs retargeting to `main` once #71 merges**.

Story data lives in one global SQLite store behind a local daemon. Every
repository carries a committed `.storyhook.toml` naming the project it belongs
to; no repository carries a `.storyhook/` directory. storyhook tracks itself that
way, with 85 stories in the real store.

## The gates

```sh
make test           # 1977 Rust tests, 0 ignored, 18/18 bash — 68s warm
make test-daemon    # the same suite over /api/v1/invoke — 60s
make gate           # both. What a wave ends with, and what a change to the tests should run.
```

Both green on the branch tip.

## What remains, and none of it happens from this worktree

Three things, all release-from-`main`:

1. **Reinstall the `story` binary.** `~/.local/bin/story` was built 2026-07-25
   and predates the flip: no `migrate` verb, and it still creates
   `.storyhook/lock` wherever it stands. The managed git hooks run `story` by
   *name*, so until it is replaced every commit in every repository leaves that
   residue and the hooks quietly do nothing. It happens on this repository's own
   commits today — `/.storyhook/` is in `.gitignore` because of it.
2. **`story web register` from the main checkout.** The store's recorded path for
   this project is this *worktree*, because W7's migration ran against a copy at
   a scratch path. Nothing breaks until then: resolution answers by the pointer
   file's uuid, not by path. Only the dashboard names a directory that is about
   to be deleted.
3. **A major version bump.** `feat!:` — story data moved, `.storyhook/` retired,
   `sync.mode = auto` removed. `/semver bump major` from `main`.

## Owed to Mikey

**One deletion the permission classifier declined**, with the backup already
taken and verified: the junk project `.tmpKGBY3a` — one story, three events, at a
`$TMPDIR` path — that a bare `cargo test` created in the real store on
2026-07-28. The exact `sqlite3` transaction is in
[`hardening.md`](docs/rearch/hardening.md#what-w8-did-not-do), ready to paste.
The hole it came through is closed: a test build now refuses to resolve a real
data home.

**A daemon from this worktree is registered as the machine's daemon.** `~/.local/
state/storyhook/daemon.json` names pid 71673 running
`…/worktrees/rearch/target/release/story daemon --serve --port 3456`, which fell
back to port 52950 because the pre-flip `story web --serve` (pid 8743, started
2026-07-26) still holds 3456. Two consequences: the worktree's binary will vanish
when the worktree does, and the thing on the bookmarked port is the *old*
dashboard, which reads a registry and per-repo directories that no longer exist.
Neither was touched by W8 — both are Mikey's to decide.

*(The W5 item about stray `.jsonl` files in `~/.local/state/storyhook` is
resolved: checked this wave, the directory holds only the daemon's runtime files
and the backups.)*

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
| SH-92 | **there is no supported way to delete a project.** The schema refuses it on purpose (`0001_initial.sql` says a `delete_project` must drop the append-only guards inside its own migration and say so), and the accident that creates one by mistake is what W8 closed at the other end |

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
