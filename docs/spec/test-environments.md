# Test environments: one definition, four renderings

Design of record for SH-531.

## The problem

Story data lives in **one SQLite store per machine**, served by **one daemon per
store**. So "which store am I talking to" is machine-wide state, and every test,
every harness script, and every command typed in a checkout is a potential
writer to the tracker this project uses to track itself.

The defences existed, and each was added where its incident happened:

| Defence | Where | What it covers |
|---|---|---|
| an isolated `STORYHOOK_DATA_DIR` per run | six shell harnesses | whatever that harness runs |
| `TestEnv` | `crates/storyhook-test-support/src/env.rs` | the test files that use it |
| `is_test_build()`'s refusal | `src/env/mod.rs` | a bare `cargo test` with **nothing** naming a store |
| `migration_guard`, `install_guard` | `src/migration_guard.rs`, `src/daemon/install_guard.rs` | a schema advance, a launchd enthronement |
| derived scans | `tests/store_isolation.rs` | drift in three named variables |

What did **not** exist was a statement, anywhere, of what a storyhook test
environment *is*. Six shell harnesses hand-copied the same block, and they had
already drifted apart from each other and from the Rust one:

| | run-tests | run-e2e | capture-baseline | coverage-map | plugin run-tests | plugin lib | `TestEnv` |
|---|---|---|---|---|---|---|---|
| disposable-root refusal | ✅ | ✅ | ❌ | ❌ | ✅ | ❌ | n/a |
| `HOME` | ❌ | ❌ | ❌ | ❌ | ✅ | ✅ | ✅ |
| `XDG_DATA_HOME`, `XDG_CONFIG_HOME` | ❌ | ❌ | ❌ | ❌ | ✅ | ✅ | ✅ |
| clears `STORYHOOK_GITHUB_TOKEN` | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ✅ |
| `STORYHOOK_PARENT_PID` | `$$` | `$$` | `$$` | sentinel | `$$` | `$$` | own pid |

`plugins/story/tests/run-tests.sh` documented its duplication as deliberate, and
its reason was real: `lib.sh`'s block is skipped when `run-tests.sh` has already
set `$STORYHOOK_TEST_HOME`, so a block written in only one of the two left the
whole-suite run with no isolation at all — which is how a set of leaked daemons
was found. That reason is answered by two call sites calling one function, not
by two copies of twenty lines.

And there was **no entry point at all** for a person. `./target/debug/story
list`, typed in a worktree, resolves the real store and the real daemon on port
3456: `is_test_build`'s sentinel is the `fault-injection` feature, which `cargo
test` sets and `cargo build` does not, and this repository carries a committed
`.storyhook.toml` naming the project storyhook tracks itself with. The only way
to exercise a change by hand was `make install`, which replaces the binary
everything else on the machine runs.

## The definition

`src/env/test_environment.rs`. Each parameter carries a **disposition** (a path
beneath the environment root, a literal, the isolating process's own pid, or
removed), a **scope**, and the reason it exists — written for a stranger,
because it is rendered into shipped help.

```
                    ┌──────────────────────────────────────┐
                    │ storyhook::env::test_environment     │
                    │   TEST_ENVIRONMENT: &[Parameter]     │  ← THE definition
                    │   resolve(root, pid, scope)          │
                    └───────────────┬──────────────────────┘
      ┌──────────────┬──────────────┼───────────────┬───────────────────┐
      ▼              ▼              ▼               ▼                   ▼
 TestEnv        test-env.sh    story help      scratch-env.sh      this document
 (Rust tests)   (six shell     test-           (make scratch)
                 harnesses)     environment
      ▲              ▲
      └─ proven equal by tests/test_environment.rs, behaviourally ─┘
```

It lives in the **library**, not in the test-support crate, for the reason
`story help priority-rubric` lives there: a suite driving `story` from another
repository needs it, and must be able to ask the tool rather than read
storyhook's own documentation. That is what makes the strategy checkout-robust
rather than machine-specific.

### Why `HOME` carries a scope

This is the distinction a flat list could not express, and the reason the
`scripts/` harnesses looked incomplete for as long as they did.

`CARGO_HOME` and `RUSTUP_HOME` are unset on an ordinary machine. A harness that
exports a fake `HOME` around `cargo test` therefore costs cargo its registry and
its build cache; one that does it around the browser suite costs playwright its
downloaded browsers. Both silently, both as a large slowdown rather than an
error.

`TestEnv` may redirect `HOME` regardless, because it applies isolation to each
`story` **child** rather than to its own process. A harness that runs nothing
but `story` and `git` — the plugin suite — may too, and passes `--home`. A shell
wrapper around `cargo` may not. So the answer is a property of the parameter
*and* of the caller: `Scope` on the parameter, an argument at the call site.

### Why `STORYHOOK_STORE_PATH` is set rather than unset

It outranks `STORYHOOK_DATA_DIR`. A developer with one exported — exactly what
somebody debugging a second store has — would otherwise send a whole run into
it, while every guard that would complain inspects the variable that lost.
Setting it makes what a child sees **asserted** rather than assumed.

### What is deliberately out of the table

A parameter earns its place by answering one question: *what of the developer's
own does a storyhook process reach when this is left alone?* Two things every
harness also sets are not answers to it, and folding them in would make the
table mean two things at once.

- `INSTA_UPDATE` is a snapshot-tool setting. It says nothing about which store a
  process reaches.
- `STORYHOOK_GATE_PROGRESS` and `STORYHOOK_GATE_PROGRESS_PATH` are gate
  orchestration: `run-tests.sh` and `run-e2e.sh` deliberately **set** them and
  strip them per-child with `env -u`. A table of things every harness must
  neutralize cannot also contain a thing a harness must set —
  `tests/store_isolation.rs` fences those two on their own terms.

## How the renderings are kept in agreement

**Behaviourally, never structurally.** The SH-357 doctrine, one language over: a
scan comparing shapes passes while two implementations mean different things.

`tests/test_environment.rs::the_shell_rendering_and_the_library_isolate_identically`
poisons every parameter with a decoy value in the parent, sources
`scripts/test-env.sh`, calls `storyhook_isolate`, `exec`s `env(1)`, and compares
the child's environment to `resolve()`. Both scopes, both directions — a
parameter the shell forgets fails, and a variable the shell sets that the table
does not name fails too.

**Poisoning the parent is what makes the first direction observable at all.**
With no decoy, a forgotten `export` and a correct `unset` produce an identical
child: nothing there, and the table says nothing should be.

Mutation-checked in both directions when it was written: removing
`XDG_CONFIG_HOME` from the shell table fails naming `XDG_CONFIG_HOME`; adding a
parameter the shell does not render fails naming it, in two tests.

### The harness scans, and where the threshold comes from

Two rules, derived over `git ls-files`, no list:

- a tracked `*.sh` that sets **two or more** parameters must call
  `storyhook_isolate`;
- one that calls it must set **none** by hand.

Two, not one, and the reason is the distinction between the two things a script
can be doing. Setting a single parameter is *pointing* a run somewhere —
`scripts/merge-watch.sh` names a store for a real, deliberate, non-isolated run.
Setting two or more is *constructing an environment*, and constructing one by
hand is what produced six copies that had already drifted.

## The scratch environment

`scripts/scratch-env.sh`, reached as `make scratch`. This checkout's binary, a
throwaway store under `/private/tmp/storyhook-scratch/<name>`, and a daemon that
dies with the shell it drops you into.

**The isolation is the test suite's own** — the same `storyhook_isolate` every
harness calls. That is the design, not an implementation convenience: a person
exercising a change by hand should not be running under a weaker contract than
the gate, and `story help test-environment` then documents both at once.

Three decisions worth keeping written down:

- **`$HOME` stays the caller's unless `--isolate-home`.** The store, the daemon,
  its port, its logs and its backups are all keyed off the other parameters, so
  the real `$HOME` is reached for exactly one thing storyhook does — `story
  daemon install`, which writes a launchd agent — and keeping it buys the caller
  their shell's rc file, their git identity and their ssh keys.
- **The root persists**, unlike every other harness root here, which traps
  itself away on `EXIT`. Coming back to a store you set up is the point.
- **The binary is built before the environment is applied**, because cargo wants
  the real `$HOME` for its registry, and under `--isolate-home` it would not
  have one.

`--test-build` compiles with `fault-injection`, which is the feature
`storyhook-test-support` turns on as a dev-dependency and therefore what makes
`cargo test`'s binary a test build. It answers SH-531's own sentence — "run a
test build with a test store" — and is the only build whose store crash points
can be armed by hand. It overwrites `target/debug/story`; the next `cargo build`
(and `make test`, which runs one) puts the ordinary binary back.

## The fixture migration

43 test files reached the binary directly — `assert_cmd::Command::cargo_bin
("story")`, 140 call sites, no `.env()` at all — so which store they wrote to
was decided by whichever wrapper script happened to invoke them.

Two things kept that survivable and **neither is a guarantee**:
`scripts/run-tests.sh` exported an isolated `STORYHOOK_DATA_DIR`, and
`is_test_build` refuses to guess a store when *nothing* names one. Neither
covers the case that matters: a developer with `$STORYHOOK_STORE_PATH` exported
bypasses the wrapper, and the refusal does not fire because something *did* name
a store.

### The acceptance check

`is_test_build`'s refusal is what makes migration verifiable rather than merely
plausible:

```
env -u STORYHOOK_DATA_DIR -u STORYHOOK_STORE_PATH -u XDG_DATA_HOME \
  cargo test --test <name>
```

passes if and only if every `story` in that binary was handed an environment by
the harness. **None of the 43 could pass it before; all of them pass it now.**

### The hazards that needed judgement rather than a pass

- **A commit count.** `story_sync_git.rs` asserts `"scanned 3 commits"`, and
  `ProjectBuilder::git()` writes an initial commit — which would make it four,
  too recent for `--since 1h` to filter out. It keeps its own `init_git`,
  converted to the shared `git` helper rather than replaced by the builder.
- **A hook that resolves `story` itself.** `session_start_hook.rs` spawns
  `bash <hook>`, and the hook resolves `story` from `$PATH` and runs it.
  Isolating only this file's own `story()` would have left the hook reading the
  ambient store: every context assertion degrades to `{}`, and the failure reads
  as a defect in the hook.
- **A held spawn lock.** `session_start.rs`'s `degrades_under_contention` holds
  an exclusive flock on the daemon spawn lock. From a *shared* environment that
  blocks every other test in the binary behind `SPAWN_LOCK_DEADLINE`, so those
  two keep `TestEnv::isolated()`.
- **The battery classifier.** Eight invocations pass no `current_dir` at all —
  `--help`, `--version`, `-V` — and `TestEnv::story()` requires one.
  `env!("CARGO_MANIFEST_DIR")` is the obvious directory and the wrong one:
  `scripts/rust-test-targets.sh` classifies a file into the core or the
  checkout-contract battery by scanning for exactly that marker, so reaching for
  it would silently move five files between batteries. The shared environment's
  own home is used instead, and the contracts battery's membership was diffed
  against `origin/main` to prove nothing moved.
- **Two files were already half-migrated.** `event_hooks.rs` and
  `session_start.rs` each had one test on `TestEnv` and the rest inheriting the
  ambient store. Migrating half a file again would recreate exactly that.

### The fence

`tests/fixture_isolation.rs`: no tracked test file may contain
`cargo_bin("story")`. One marker, derived over `git ls-files`, and **no
allowlist** — verified rather than hoped, because every legitimate raw
invocation already goes through `storyhook_test_support::story_binary()`, which
resolves this build's own binary and refuses an installed one. That is the
correct door rather than a tolerated one, and the rule points offenders at it.

Its marker is assembled at run time so the file is not its own first violation,
and it carries two positive controls, because the scan can go silent in two
different ways: an empty `git ls-files`, and a marker that no longer matches how
the call is spelled.

## Two live defects this closed

- **Only the Rust harness cleared `STORYHOOK_GITHUB_TOKEN`.** SH-153 was fixed
  where it was found and nowhere else, so the bash plugin suite and the browser
  suite handed every fixture child and every test daemon the developer's real
  PAT — meaning what those suites did depended on whose shell ran them. Three
  more variables had the same shape and had never been considered:
  `STORYHOOK_PROJECT` (a selection made outside the run), `STORYHOOK_ACTOR` (the
  identity writes are attributed to), and the three `STORYHOOK_ALLOW_*`
  overrides, which **disarm the very guards a run may be testing** — a suite
  that inherits one cannot observe the refusal it is asserting, and passes.
- **The plugin suite ran the installed binary.** It resolves `story` by name;
  `make test` prepends `target/debug` and a standalone run — which is what a
  person or an agent types — did not. Nothing was damaged, because the store was
  isolated either way; the wrong binary was simply exercised. Found by hitting
  it: two tests failed against an installed v2.2.0 whose `default_states()`
  predates the `verifying` state, reporting `error: state `verifying` not
  found` — an error naming a state, a project and a store, and not the one thing
  that was actually wrong. This is the SH-226 doctrine one layer over: ask what
  a process **is**, not what a name happens to resolve to. `lib.sh` prepends the
  checkout's own `target/debug` and **refuses** when that binary is absent,
  because a fallback here is precisely the silent substitution being fixed.

## Deliberately out of scope

Named rather than silently dropped:

- **`is_test_build`'s remaining gap.** A developer's exported
  `$STORYHOOK_STORE_PATH` still defeats the refusal, because the guard asks
  whether anything *named* a store rather than which one was reached — a
  deliberate choice recorded at `src/env/mod.rs` (a `is_default()` predicate was
  tried and reverted, since `TestEnv` deliberately builds a fake `HOME` whose
  store sits at the default-shaped path). The migration removes the exposure for
  every test file; the guard itself is unchanged.
- **`make install` has no worktree or dirty-tree check**, and
  `migration_guard`'s own refusal message recommends it as the way through.
- **`real_store.rs`'s last-rung fallback** into `$HOME/.cache` when a checkout
  is itself temp-rooted.
