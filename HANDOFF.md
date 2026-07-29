# Handoff — the data-layer rearchitecture, after W3

**Read first:** `docs/rearch/STATE.md` (execution ledger — Key facts, then the
W3 step log), `docs/rearch/flip-checklist.md` (the enumerated W4 work, now with
sections D2 and G), `docs/spec/data-layer-rearchitecture.md` (the design of
record).

**Worktree:** `/Volumes/Code/mikeyward/storyhook/.claude/worktrees/rearch`.
Linked worktree: no version bumps, no deploys, no force-push, never touch main.

## Where the program is

W0 through W2d are merged. W3 is on `rearch/w3-importer` with a PR open.

**The port is complete and the door is now two-way.** All 49 `Invocation`
variants dispatch to the store-backed stack. `story migrate` moves a legacy
`.storyhook` tree into the store, and `tests/migrate_round_trip.rs` proves the
journey back: `store → story export → ProjectExport → story import-project →
legacy tree`, with the two read models compared story by story and field by
field.

One *action* is still owed a design rather than a port: `History::Restore`
replaces a story's history, which an append-only store cannot do. It refuses
loudly and names the checklist.

`src/app.rs` has exactly one arm from this wave — `story migrate`, which opens
the global store from the legacy leg, because otherwise nothing could be
migrated until after the flip. Nothing else routes production traffic through
the new stack: `STORYHOOK_INVOKER` still defaults to `legacy`.

## Next: W4, the flip

Everything it needs exists. Its budget, its file:line census and its two
`#[ignore]`d exit-criterion tests are the flip checklist. Three things to read
before starting:

1. **Section D2, the rollback procedure.** Paste it into the W4 PR. The revert
   policy is *conditional* on `cargo test --workspace --test migrate_round_trip`
   being 4/4 green; if it is red, the flip is a one-way door and must not merge.
2. **`.storyhook/` stays in the repository until W7**, and the reason is
   concrete rather than cautious: it is the only copy of `project.toml`'s
   `created_at` and `sync`/`doctor` settings and of `next-id`'s burned numbers,
   none of which an export document carries.
3. **The behaviour-change notes W4 owes its PR** are accumulated in STATE.md's
   Key facts: the burnt story number on a rejected `--state`, `doctor --fix`'s
   data loss, `import-project` refusing a non-empty project, the superstate
   re-fold, and now the SH-60 repairs — which change what this repository's own
   graph says (SH-40 loses five children it claimed alone; SH-31 gains a
   parent).

## The two gates — run both

```sh
make test           # ~1:33 warm, 1946 tests, 2 ignored (the W4 exit criterion)
make test-store     # ~6.5s, 40 targets
```

`make test-store` is the same integration suite under `STORYHOOK_INVOKER=local`.
**Standing rule from W2d onward:** run it after every commit alongside
`make test`, and record both times in STATE.md. Its exclusion list — file,
reason, burn-down wave — is flip-checklist section G, and **must only ever
shrink**.

## Traps that have already cost time

1. **`TestEnv` isolates child processes, not in-process library calls.** If a
   service reads a global path from the environment, an in-process test cannot
   redirect it — so make the path a parameter. Two W2d tests wrote into the
   developer's real home directory before this was understood.
2. **`make test` measurements are worthless on a loaded machine.** `web_test`'s
   readiness deadlines are wall-clock. Check `uptime` and
   `ps aux | sort -nrk 3` before hunting a regression — and
   `ls target/debug/deps | wc -l`, which W3 found stalls the machine's whole
   filesystem-event layer once it reaches six figures.
3. **A fixture at a fixed path under `/private/tmp/storyhook-tests` is a latent
   flake**, because this program runs its gate from a worktree while the main
   checkout may be running one too. W3 fixed the one instance of it.
4. **`git commit --amend` silently fails here.** Use reset --soft plus a fresh
   commit.
5. **Story IDs go in commit BODIES, never subjects** — the post-commit hook
   scans subjects and re-dirties the tree.

## Owed to the user, not yet done

- `rm -rf ~/.local/state/storyhook` — three fixture `.jsonl` files a test run
  wrote there before `StoreSyncStorage::backups_dir()` existed. The permission
  classifier declined the delete; it needs Mikey.
- The defects this program has found still need stories filed (task #15),
  including two more from W3: `TransferService::export` silently drops
  unknown-kind events, and this repository's fifteen live SH-60 violations are
  now fully characterised. Not from this worktree — minting ids here collides
  with ids minted in parallel worktrees.
