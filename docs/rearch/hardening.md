# W8 — crash, concurrency, and corruption hardening

> The last wave of the data-layer rearchitecture, and the only one whose output
> is mostly tests. Its question is not "does the store work" — W1 through W7
> answered that — but "what does it do when the machine, the process, or the file
> does not cooperate."
>
> Companion documents: [`STATE.md`](STATE.md) (the execution ledger),
> [`../spec/data-layer-rearchitecture.md`](../spec/data-layer-rearchitecture.md)
> (the design of record), [`baseline/`](baseline/) (the W0 *before* picture).

## What was found

Six defects, each fixed at its origin with a red→green regression test in its own
commit. Every one of them was found by writing a test that did the thing a user
does, rather than by reading code.

| what | where it surfaced | commit |
|---|---|---|
| a bare `cargo test` could write the developer's real store | the W7 junk project, `.tmpKGBY3a` | `1d3fcfa` |
| a damaged store reported the pragma that noticed it | `story init` on a truncated database | `912b205` |
| a malformed pointer file reported a `toml` error naming no file | a hand-edited `.storyhook.toml` | `e955611` |
| `story init` on a clone minted a *new* identity and left the committed pointer naming nothing | a second machine, which is what the uuid exists for | `20b9a8f` |
| the restore instructions were incomplete, and only fired on open | the restore drill | `6fef03e`, `9a25b2f` |
| **no call to a daemon had a timeout** | the soak, which hung a gate run for twelve minutes | `92c7b50` |

Plus one ruling that removes a feature rather than fixing it: `sync.mode = auto`
(the flip checklist's G4) is gone from the vocabulary. See below.

## The crash matrix

`tests/crash_matrix.rs`. Thirteen cases across all five fault-injection points,
every one of them a real process killed by a real `SIGKILL` at a named
instruction and a real database opened afterwards.

`STORYHOOK_FAULT=<point>` now delivers `SIGKILL` to the process rather than
calling `abort()`. For SQLite's durability the two are equivalent — neither
unwinds, neither flushes, and the database's state is in the write-ahead log and
the page cache rather than in the process's memory — but only one of them is
*unarguably* equivalent: `abort` raises a catchable signal and runs whatever
handler a linked library installed, and `SIGKILL` cannot be caught, blocked or
handled by anyone.

| point | operation | assertion |
|---|---|---|
| `before_commit` | `new` | nothing lands, and the story number goes back to the pool |
| `before_commit` | `move --if-state` | unmoved, and the guard still fires on the old value |
| `before_commit` | `relate` | neither end claims the edge |
| `after_commit_before_ack` | `new` | durable; no success claim; the next number is not reused |
| `after_commit_before_ack` | `move`, `relate` | durable, both ends |
| `after_commit_before_ack` | daemon dies | the client is told it cannot know |
| `after_commit_before_ack` | WAL replay | the `-wal` is non-empty; a fresh process recovers it |
| `mid_read_model_update` | `new --label` | no orphan label rows |
| `mid_read_model_update` | `relate` | no one-sided edge |
| `mid_migration` | opening a v1 store | version stays true; backup exists and verifies; resumable |
| `backup_verify` | opening a v1 store | nothing migrated; no unverified copy visible as a backup |
| `mid_migration` | 8 processes, one killed | applied once, recorded once, seven survivors succeed |
| every point | swept | `integrity_check` clean, and the store still answers |

Four assertions are the floor under every case: the child really died where it
was told to; `PRAGMA integrity_check` is `ok` on reopen; the operation is present
and complete or absent entirely; and `diff_read_model` — the oracle `story
doctor` runs — finds nothing.

### The client-side contract when the daemon dies post-commit

A crash between `COMMIT` returning and the caller being told is the one window
where "the write failed" and "the write happened" are both true from somebody's
point of view. storyhook's answer, pinned by
`sigkill_after_commit_before_ack_does_not_report_false_success` and
`a_daemon_killed_after_commit_tells_the_client_it_cannot_know`:

- **A `--local` client that dies makes no claim at all.** Killed by a signal, it
  has no exit code, let alone a zero one. The write is durable, because that is
  what `COMMIT` means.
- **A client whose *daemon* dies is told it cannot know**: *"the storyhook daemon
  stopped answering … This command may or may not have run — storyhook will not
  repeat it, because repeating a write it cannot prove failed is worse than
  reporting this. Check with `story show` or `story list`, then try again."*

The rule that follows, for anything scripting storyhook: **a command that died by
signal, or reported that the daemon stopped answering, must be *read* before it
is retried.** A blind retry is what turns this window into a duplicate story.
`HttpInvoker` will re-send only when the connection was *refused* — nothing was
delivered, so sending is a first attempt rather than a retry.

## The concurrency soaks

`tests/concurrency_soak.rs`. Four soaks, all of them with half the clients going
through the daemon while the other half write the database directly — which is
not an exotic configuration but a dashboard running while a git hook shells out
to `story --local`, which happens on this machine on every commit.

| soak | shape | asserts |
|---|---|---|
| mixed load | 8 clients × 3 rounds × 3 commands = 72 processes | exactly 24 stories, numbered 1..24 |
| relations | 8 concurrent `relate` calls | every edge symmetric, every mirror materialized |
| allocation burst | 16 simultaneous `story new` | 16 distinct contiguous numbers |
| read storm | 4 writers × 4 readers | no partial story ever observed |

Two assertions apply to every command in the file:

- **Nothing may exit 4, and nothing may say `locked`, `SQLITE_BUSY` or `timed out
  waiting`.** Contention is real under this load and absorbing it is the store's
  job — `busy_timeout` between processes, a write mutex within one — so a user
  seeing any of it is the regression.
- **Every child is waited on with a 30-second deadline.** A deadlock is reported
  as a deadlock naming the command, rather than hanging the suite until something
  several layers up kills it and takes the evidence with it.

Every soak ends with `diff_read_model` and a real `story doctor`.

## Every call to a daemon has a deadline now

The soak found this the hard way: a `make test` run stalled for twelve minutes,
with the test binary alive, one daemon alive, and no client processes — nothing
was waiting on a subprocess. What was waiting was a guard tearing itself down,
blocked in `lifecycle::request_shutdown`, which posted with no timeout of any
kind. All three daemon-facing calls were like that: `hello`, `request_shutdown`,
and the invoker's `send`.

The tempting assumption is that loopback either answers or refuses. A process
that accepts a connection and then never writes does neither, and every way that
can happen is reachable: a daemon wedged on a long operation, a daemon stuck in a
probe (W0 found `tailscale status` hanging for minutes and leaving servers bound
and silent), and — the case `hello`'s own docstring names — something that is not
storyhook holding the port, which could therefore hang the very check written to
detect it. In production the symptom is a `story daemon stop` that never returns
and never says why.

Two deadlines, and the split is the point:

- **`hello` and `request_shutdown`: a five-second global timeout.** They carry no
  work — an identity check and a shutdown request, both loopback round trips — so
  there is no legitimate slow case.
- **`HttpInvoker::send`: a five-second *connect* timeout and no global one.** That
  request carries the user's actual work, and abandoning a mutation is the
  expensive direction: the caller is then told it may or may not have run.
  Connecting is different — the peer is on loopback and either accepts at once or
  is not there.

`tests/daemon_timeouts.rs` sets a listener that accepts and never answers, and
makes each call on a worker thread with a channel deadline: a regression fails
the file in five seconds with a name, rather than stalling the suite, which is
precisely the failure mode being retired. A third case pins that a *refused*
connection still fails instantly, because storyhook relies on that distinction to
decide whether a request may be sent again.

## Corruption recovery

`tests/corruption_recovery.rs`. The rule the file enforces: **every failure names
what is wrong, where, and what to do next, and no raw `rusqlite`, `serde` or
`toml` message ever reaches a user.**

| case | behaviour |
|---|---|
| missing database | created; no diagnostic — this is every first run |
| zero-byte database | treated as fresh, by SQLite's own convention |
| truncated database | *"the storyhook store at … is damaged: database disk image is malformed. Nothing has been changed…"* |
| not a database at all | the same words |
| a directory in its place | the path that is in the way |
| schema from a newer storyhook | both version numbers, plus conditional release-update or source-build recovery guidance |
| damaged write-ahead log | discarded, not obeyed; the last checkpointed state survives |
| corrupt pointer file | the path of `.storyhook.toml`, the parser's account, and `git diff` |
| pointer missing its `uuid` | the same |
| pointer naming an unknown project | the uuid, and `story init` to adopt it here |

Three of those are cases where the *right* answer is to do nothing at all, and
they are tested for exactly that reason: a store that refused to open on a
zero-byte file, or on a write-ahead log it could not make sense of, would turn a
recoverable machine into an unusable one over a file that is by definition
regenerable.

## Backups: the restore drill

`tests/backup_restore.rs`. Run as a procedure rather than as a feature — the
question is whether a person whose tracker is damaged can get it back by
following what storyhook prints, using the `story` binary they have.

**The drill found that they could not.** The diagnostic said "copy the newest
snapshot over the database", and that is not a restore:

- the store is in write-ahead-logging mode, so `store.db` is not the whole
  database — copying a snapshot over it leaves the *old* database's `-wal` and
  `-shm` beside the *new* database's pages, and SQLite replays one into the
  other;
- and a running daemon holds the database open with its own page cache, so it
  serves the old data happily while the files are swapped underneath it.

The instructions now name the whole procedure:

```
run `story daemon stop`, delete store.db, store.db-wal and store.db-shm
from <data dir>, copy the newest snapshot there as store.db, then run
`story doctor`
```

`the_documented_restore_is_necessary_as_well_as_sufficient` pins both halves:
the naive restore does not produce the snapshot (either the old log is replayed
or the result is malformed), and the documented one does.

### Rotation and verification

- Seven snapshots are kept, and all seven open and pass `integrity_check` —
  asserted against twelve real `VACUUM INTO` copies of a changing database, not
  against twelve files named like one.
- **Pruning happens when a snapshot is *taken*.** Pre-migration backups share the
  directory and nothing prunes them in between, so the count is bounded at the
  daemon's next due run rather than continuously. On a machine that migrates
  often — a developer building storyhook — the directory can hold more than seven
  for a while. Bounded, not leaking; recorded here because the docstring's "seven
  are kept" reads stronger than what is promised.
- The pre-migration backup carries the version it was taken *from* in its name
  and in its `user_version`, which is what makes it useful after a bad upgrade.
- Backup age is reported by `story daemon status` (the W5 deviation), and the
  corruption diagnostic names the same directory the snapshots are really in —
  asserted, because a message pointing somewhere empty is worse than none.

## The rulings

### G4 — `sync.mode = auto`

**Removed from the vocabulary. SH-68 stays open for the redesign.**

Auto-sync fired from the tail of the pre-rearchitecture `app::run`, re-syncing
the affected story after every story-modifying command. `dispatch` was never
given an equivalent, so it has not run since W4's flip, and W6 deleted the code
with the rest of the legacy write path.

Reinstating it honestly means a network call to GitHub on the tail of every
mutating command — in the daemon as well as locally, since that is where dispatch
runs for a remote client — with a failure policy, a timeout, and a re-entrancy
story for the hooks that already shell out to `story`. That is a feature with a
design, not a switch to flip, and hardening is the wrong wave for it.

It could not simply stay, either: the first-run setup for `story github-sync` was
still offering *"Auto (sync on every story change)"* as menu option one, and
choosing it configured a project for nothing at all, silently. That is the defect
class W4 removed when `story web register --name` turned out to accept a name and
drop it.

So the menu offers `manual` and `off`; `SyncMode::Auto` stays *deserializable*,
because refusing to parse would make a migrated project unreadable over a setting
that is merely inert; and a project still carrying it is told so on every sync.

### SH-69 — the daemon leg's shape

**`make test-daemon` stays a separate target. `make gate` = `test` +
`test-daemon` is what a wave ends with and what a change to the tests should
run. `--test-threads=4` stays.**

The arithmetic: `make test` is 68s warm (114s when it has compiling to do),
`make test-daemon` is 51–60s, and the suite budget's hard ceiling is 180s with a
target of 120s. Folding the daemon leg in puts the per-commit gate at or past the
target on a warm machine and over the ceiling on a cold one — for a signal whose
cadence is "whenever a test changes" rather than "every commit".

The daemon leg is not redundant, and this wave is the evidence. The property that
the two modes *agree* is proved by the byte-comparison test, which is in `make
test`. What the leg finds is different, and it found **six** instances of it in
W8 alone: tests that are only correct when nothing else holds the store. A
running daemon keeps the database open with its own page cache and its own
write-ahead-log handle, so a fixture that assumes an empty backup directory, or
asks about bytes on disk through a client, quietly means something else. Every
one of those was a test defect — and a test that is wrong in one mode is a test
that can hide a product defect in both.

That hazard is introduced when a test is *written*, not when unrelated code
changes, which is what makes a per-wave gate the right cadence.

On `--test-threads=4`: it is a bound on how many daemons exist at once, adopted
at W5 when the leg stalled wide open. Re-measured here (2026-07-29): the leg now
passes at every parallelism tried — three consecutive green unbounded runs at
51s, 52s, 51s; `8` at 53s; `4` at 55s; `2` at 67s. The bound is kept anyway. It
costs four seconds on a 51-second leg; what it retires is a *stall*, which is the
worst thing a gate can do, because it produces no failing test name and no
signal; and the quantity it bounds grows with the suite — W8 alone added four
test files that take a `TestEnv::isolated()`, each of which is another daemon.

### The bare `cargo test` hazard

**Closed inside the binary, where `cargo test` cannot route around it.**

`make test` has always exported an isolated `STORYHOOK_DATA_DIR` and refused to
run without one, but a bare `cargo test` skips the Makefile entirely — and
roughly forty-five integration-test files build a fixture directory and run
`story` with the ambient environment inherited. W7 found the consequence in the
real store: a project named `.tmpKGBY3a` at a `$TMPDIR` path.

`Environment::from_process` now refuses to resolve a data home a test build was
not given, before anything is created. The sentinel is the `fault-injection`
feature, which is exact rather than approximate — `cargo build` and
`--release` do not enable it, `cargo test` does — so it is one thing to keep
true rather than two.

**One consequence to know about.** `cargo test` and `cargo build` write the same
`target/debug/story`, so the binary left there after a test run *is* a test build
and refuses to touch a real store. That is correct — it also carries live crash
points — but it is surprising the first time, so the message says so and names
`cargo build` as the way to get a usable one. `make test` ends with `cargo
build`, so a full gate leaves the ordinary binary behind.

## Performance against the W0 baseline

Regenerated with `scripts/capture-baseline.sh --out /private/tmp/w8-after
--census-runs 3`, on the machine the W0 capture used, warm both times. The
capture is not committed: `docs/rearch/baseline/` is documented as the *before*
picture, and overwriting it would destroy the thing being compared against.

**Caveat, stated first because it bounds everything below.** W0's gate figure is
a 10-run median and W8's is a 3-run median; the machine had a live daemon and a
browser on it for both; and one neighbouring project's build has already been
observed to cost this suite half its tests (STATE.md, W2b). These are honest
numbers on a working laptop, not benchmarks.

| | W0 | W8 |
|---|---|---|
| `make test`, warm, median | **36.375s** (10 runs) | **67.950s** (3 runs, 3/3 green) |
| Rust tests | 1171 | 1977 |
| test binaries | 52 | 86 |
| ignored | 2 | **0** |
| serial sum of per-binary medians | 19.08s | 43.95s |

### The gate is 87% slower and the suite is 69% bigger

Per test, that is 31.1ms → 34.4ms of gate: **+10%**. And the gate is not only
tests — it is `cargo fmt --check`, `clippy --workspace --all-targets` with
`-D warnings`, `cargo build`, and the bash suite, all of which grew with the
codebase and with 34 new test binaries to lint and link.

The per-binary sum tells the structural story better, because it excludes cargo,
clippy and the linker:

- as measured: 16.5ms/test → 22.4ms/test;
- **excluding the two binaries whose cost is deliberate waiting** —
  `daemon_invoke` (10.9s for 8 tests, each spawning a daemon) and
  `daemon_timeouts` (5.0s for 3, two of which wait out a five-second timeout on
  purpose), neither of which has a W0 equivalent — **14.5ms/test**, which is
  *faster* than the file-backed baseline.

That is the claim the rearchitecture can honestly make: **the work a test does
got cheaper; the suite got bigger and grew a category of test that waits on
processes by design.**

### Where it got faster, and why

Fifty binaries exist in both captures. The two largest movers are the two that
were doing the most filesystem work:

| binary | W0 | W8 | |
|---|---|---|---|
| `web_test` | 7.17s / 140 | 3.94s / 141 | **−45%** |
| `error_contract` | 5.22s / 3 | 2.51s / 3 | **−52%** |
| `move_if_state` | 0.23s / 9 | 0.11s / 9 | **−52%** |
| `golden_cli` | 1.64s / 27 | 1.72s / 27 | +5% |
| `story_sync_git` | 0.41s / 13 | 0.40s / 13 | −2% |

The small binaries mostly moved by a few tens of milliseconds in both
directions, which at that scale is process startup rather than storage.

Two rose enough to name, and both for the same reason — they gained tests rather
than got slower:

- `storyhook` (the lib's own unit tests) 0.17s → 0.86s, for 521 → 579 tests. The
  store's conformance and migration unit tests live here now, and they open real
  file-backed SQLite databases, which `:memory:` would have made meaningless.
- `tui_integration` 0.12s → 0.41s: the TUI's white-box tests were reconstructed
  through the `Invoker` seam in W2c, so each one now runs a real invocation
  instead of poking at a struct.

### No structural regression

Nothing in the comparison shows the store costing more per operation than the
tree it replaced. The gate remains inside the spec's budget: 68s against a 180s
hard ceiling and a 120s target. The census is 3/3 green with identical
pass/fail/ignored counts in every run.

## What W8 did not do

- **The junk project is still in the real store.** The deletion was prepared —
  a verified `VACUUM INTO` backup was taken first — and the permission classifier
  declined the write. The procedure is below, for a hand that is allowed to run
  it. It is one project with one story and three events, at a `$TMPDIR` path.

  ```sh
  sqlite3 ~/.local/share/storyhook/store.db <<'SQL'
  PRAGMA foreign_keys = ON;
  BEGIN IMMEDIATE;
  DROP TRIGGER events_reject_delete;
  DELETE FROM events   WHERE project_id = 1;
  DELETE FROM projects WHERE id = 1;
  CREATE TRIGGER events_reject_delete
  BEFORE DELETE ON events
  BEGIN
      SELECT RAISE(ABORT, 'events are append-only: DELETE is not permitted');
  END;
  COMMIT;
  SQL
  ```

  The triggers have to come off because the schema refuses to delete a project
  that has events *on purpose* — `0001_initial.sql` says so in a comment, and
  says that a future `delete_project` has to drop the guards inside its own
  migration and say so. Which is the other finding here: **there is no supported
  way to remove a project**, and the accident that creates one by mistake is
  exactly the accident this wave closed. Worth a story.

- **Seven of the nine ledger stories in W7's handoff are untouched** (SH-62,
  SH-63, SH-64, SH-65, SH-66, SH-67, SH-70). They are ordinary product defects
  rather than rearchitecture work — a nondeterministic comparator, positional
  verbs swallowing flags, a double-encoded `--json`, a dead error variant — and
  they belong to the backlog rather than to the last wave of a storage
  rearchitecture. They are filed, described, and reproducible.
