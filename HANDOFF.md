# Handoff — SH-116, C4: Selection

*(Supersedes the SH-114 handoff. SH-114 is closed; so are SH-115, SH-94, SH-110.)*

The run itself is described by
[`HARDENING_PROGRESS.md`](HARDENING_PROGRESS.md) — read its **START HERE**
section first. That is the process; this file is only what the next story needs
on top of it.

**SH-116 is the next story, and it is ready.** All three of its blockers
(SH-114, SH-115, SH-62) have landed. It is the second link of the critical path
`SH-114 → SH-116 → SH-119 → SH-121`, and no council has been convened for it —
so unlike the last four stories, this one starts by *reading* rather than by
implementing a verdict.

## What SH-114 changed under it, and it matters more than it looks

SH-116 was written while `--local` existed. Three of its own paragraphs now read
differently:

1. **"Silence, at three layers" is already half-built.** Its third bullet —
   *"all of the above must also be silent when the daemon is unreachable"* — is
   done for `git commit`: `tests/hook_silence.rs` pins it, in both directions,
   with a control that proves the hook is not merely switched off. What is *not*
   pinned is `story session-start` printing nothing with the daemon stopped.
   That is one of SH-116's five acceptance criteria and the cheapest one left.
2. **"Watch out" is spent.** SH-62 landed: a flag-shaped token no verb declares
   is refused ahead of every parser, fail-closed, so adding `--project` to every
   verb cannot reintroduce the swallowing it warns about. `tests/unknown_flag_sweep.rs`
   is where a new global flag gets its entry in the declared-flags table.
3. **There is one transport.** A refusal that names "both ways out" is now
   composed in the daemon and rendered by the client, so its text crosses the
   wire as a `WireError` — check `tests/wire_envelope.rs` if the variant is new.

## Ground worth measuring before designing

- `src/env.rs` already resolves `--store-path`/`$STORYHOOK_STORE_PATH` and
  publishes the result to every child (`main.rs::publish_store_path`).
  `--project`/`$STORYHOOK_PROJECT` is the same shape and probably wants the same
  treatment; whether it does is a real question, because a *project* is not a
  process-wide fact the way a store is.
- SH-115 landed the remotes schema and one URL normalizer, which is what step 3
  of the resolution order ("origin normalizes to a registered URL") is built on.
  `src/domain/remote.rs` is the normalizer; the store side is migration 6.
- The current resolution walk is what SH-119 deletes. Do not delete it here —
  SH-116 adds the new order beside it, SH-119 removes the old one, and mixing
  those is how a bisect stops being able to attribute a regression.

## Two things that bit during SH-114

- **`kill(getpid(), SIGKILL)` does not stop the calling thread.** It posts a
  signal. In a multi-threaded process the next instruction can still run, and it
  did: six of the crash matrix's thirteen cases died of `SIGABRT` and reported
  the fault as never having fired. Fixed in `src/store/fault.rs`; pinned by
  `tests/fault_injection.rs`. Worth knowing generally — the same shape is
  available anywhere a test kills a process it also inspects.
- **A daemon comes back on the next command.** Standing one down at the top of a
  test buys nothing: `TestEnv::stop_daemon()` belongs immediately before the
  bytes are touched. Every test that asks about a file now does this, and
  `crates/storyhook-test-support/src/crash.rs` enforces it for the ones that kill
  the writer.

## Gate

`make test` is now the whole gate — `make test-daemon` and `make gate` are gone
with the second transport. It runs the entire suite over `/api/v1/invoke` at
`--test-threads=4`. Numbers are in `HARDENING_PROGRESS.md`'s SH-114 entry.
