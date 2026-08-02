# Handoff — SH-114, part 2: the removal

*(Supersedes the SH-115 handoff. SH-115, SH-94 and SH-110 are all closed.)*

The run itself is described by
[`HARDENING_PROGRESS.md`](HARDENING_PROGRESS.md) — read its **START HERE**
section first. That is the process; this file is only what the next story needs
on top of it.

**SH-114 is open and in-progress.** Part 1 landed: the two diagnostic fixes the
council ruled had to come *first*, because you cannot remove the escape hatch
until the failure it was hiding is fixed. Part 2 is the removal itself.

Read the council verdict on the story — `story show SH-114`, the comment dated
2026-08-02. **Do not re-run that vote.** It was unanimous 3–0 in round one and
its eight clauses (D1–D8) are the specification. Audit trail:
`.council/sh114-daemon-only-shape/`.

## What landed

| commit | what |
|---|---|
| `fix(cli): a command that needs no store must not open one` | D2. `invoke::needs_no_store` + `dispatch_without_store`, called before `main` resolves a store. **Closes SH-149.** |
| `fix(daemon): the client reports why the daemon could not start` | D1. `spawn_child` returns the `Child`; `await_healthy` calls `try_wait`; the `--serve` process records a `WireError` beside its portfile and the client reconstructs it. Adds `AppError::with_context`. |

Measured on a 66-byte `store.db`: **5000ms → 71ms**, and the message went from
*"did not start within 5s"* — recommending the flag this story deletes — to the
store named, the damage named, the backups directory and the restore procedure.

## What remains — D3 to D8

1. **`crash_matrix.rs` (10 cases).** In daemon-only the process owning the write
   transaction *is* the daemon, so the daemon is what must die. The shape is
   already proven at `tests/crash_matrix.rs:413`
   (`a_daemon_killed_after_commit_tells_the_client_it_cannot_know`): spawn
   `story daemon --serve --port N` with `STORYHOOK_FAULT` armed, hold the
   `Child`, send one client command, assert `SIGKILL` on the child you armed.
   Lift it into **one** fixture rather than copying it ten times.
   - **Two pass-shaped hazards** the council requires assertions against: a
     daemon auto-spawned *before* the fault-armed one (the corpse is then the
     wrong process — exactly why that test fails in the target-state run), and
     one auto-spawned before the inspection (the answer then comes from a page
     cache). Assert the pid that died is the pid armed, and that no daemon is
     live at inspection.
   - The two `Driver::Upgrading` points (`MidMigration`, `BackupVerify`) are
     *easier*: the fault fires during store open, so a hand-spawned daemon dies
     at startup and needs no client command at all.
   - `MIGRATING_COMMAND` already exists in that file and already explains itself.
2. **`backup_restore.rs` (2 cases).** Fixtures stop the daemon after building
   rather than never starting one — CLAUDE.md's bytes-on-disk rule.
   `the_documented_restore_is_necessary_as_well_as_sufficient` needs a corpse
   holding a hot WAL, so it needs the crash fixture above.
3. **`corruption_recovery.rs`.** Drop `--local` from `story_in` and from
   `local_project`; every assertion stays. Its four diagnostics already pass over
   the daemon since part 1 — that was the point of doing part 1 first.
4. **`daemon_invoke.rs` (7).** Delete the three whose subject is the flag
   (`:213, :229, :250`); hand the four comparison tests' coverage to
   `tests/golden_cli.rs` under `INSTA_UPDATE=no`.
5. **The deletion.** `GlobalFlags::local` and its parse arm (`src/cli.rs:630,
   692`), `run_locally` and `refuse_unknown_backend` (`src/main.rs`),
   `ProjectBuilder::local` / `Project::local`, and the `STORYHOOK_INVOKER`
   exports in `scripts/run-tests.sh:44`, `scripts/capture-baseline.sh:168`,
   `plugin/claude-code/tests/lib.sh:60`, `plugin/claude-code/tests/run-tests.sh:39`.
   **Keep** the `STORYHOOK_DAEMON_ADDR`/`STORYHOOK_PARENT_PID` pair in all five
   places — they are set unconditionally and are what stops orphan daemons, so
   SH-136's list stays at five, not three.
6. **One leg.** `make test` gains `-- --test-threads=4`; `test-daemon` and `gate`
   are deleted, with CLAUDE.md's three references. The bound stays: what it
   bounds — concurrent daemons — strictly grew. **Nobody has timed the merged
   leg.** If it exceeds the 120s target the lever is threads, never scope.
7. **launchd (D7).** Keep `RunAtLoad`, keep `KeepAlive` **absent**, and pin the
   decision with a unit test on `agent_plist` in SH-131's idiom. The strongest
   new reason: `KeepAlive{SuccessfulExit:false}` would turn part 1's exact
   scenario — a daemon that cannot open a damaged store — into a respawn loop on
   launchd's 10s throttle.
8. **Silence (D8).** A test that `git commit` prints nothing with the daemon
   stopped and unable to start. Already true at the script layer
   (`src/hooks.rs`: `2>/dev/null` plus `|| true`); nothing pins it.
9. **Docs.** `src/tui/event.rs:54-58` calls the TUI "a `--local` client by
   construction"; `docs/spec/data-layer-rearchitecture.md:275-279` calls
   `--local` a "documented first-class mode" — reverse that in the document's own
   *As built* section. The TUI itself is left alone on purpose, and is SH-150.
10. **Say what is lost, in the commit** (D5): *coverage of a bare,
    directly-invoked process holding the write transaction and dying — that
    process shape becomes **unbuildable**, not merely untested.* Plus the
    two-transport agreement property, and `concurrency_soak`'s premise that two
    supported modes write one store at once (`tests/concurrency_soak.rs:177`).

## The measurement to start from

The whole suite was run in the target state (`.local()` neutralized, every
`STORYHOOK_INVOKER=local` site forced to `daemon`, then `STORYHOOK_INVOKER=daemon
… --test-threads=4`): **102 green blocks, exactly 16 failing tests**, confined to
`crash_matrix` (10), `corruption_recovery` (4 — fixed by part 1) and
`backup_restore` (2). `daemon_invoke.rs` was excluded from that patch, so 16 is a
**floor**; add its 7. Everything else in the suite — `store_isolation`,
`concurrency_soak`, `illegal_state_pair`, `story_purge`, `temp_project_refusal`,
`test_build_guard`, `project_path_hygiene` — passes with no local transport at
all, and several carry doc comments claiming they cannot.

## Two things that bit during part 1

- **A `store.db` overwritten with garbage is not corrupt while `store.db-wal` is
  still beside it** — SQLite rebuilds the database from the log, so the fixture
  proves nothing. Delete the `-wal` and `-shm` too. This produced one wrong
  conclusion before it was caught.
- **The gate's preflight refuses to start if an earlier run left daemons**, and
  the target-state experiment left 22 of them. `bash
  scripts/check-no-orphan-servers.sh preflight` lists them.
