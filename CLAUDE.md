<!-- semver:start -->
## Semantic Versioning

This project uses semantic versioning managed by the `/semver` plugin.

### Version Awareness
- Read the `VERSION` file at the start of each conversation to know the current version.
- Read `.semver/config.yaml` to understand the versioning configuration.
- When discussing releases, deployments, or changes, reference the current version.

### Commit Discipline
- Write meaningful, descriptive commit messages. Each commit message may appear in an auto-generated changelog.
- Use conventional-commit-style prefixes when they fit naturally: `feat:`, `fix:`, `docs:`, `refactor:`, `test:`, `chore:`.
- The first line of the commit message should be a concise summary (under 72 characters). Add detail in the body if needed.

### Version Bump Guidance
When recommending or performing a version bump:
- **patch** (0.0.x): Bug fixes, documentation corrections, minor refactors with no behavior change.
- **minor** (0.x.0): New features, new capabilities, non-breaking additions to the public API or user-facing behavior.
- **major** (x.0.0): Breaking changes — removed features, changed interfaces, incompatible API modifications, behavior changes that require consumers to update.

When you notice the user has completed a logical unit of work, suggest running `/semver bump` with the appropriate level.

### Configuration
Versioning settings are in `.semver/config.yaml`. Do not modify this file unless the user explicitly asks to change semver settings.
<!-- semver:end -->

## Story priority rubric

**The rubric ships in the binary: run `story help priority-rubric`.** That is the
source — the damage ladder, the five levels, the ordered tiebreakers, the
detection-layer carve-out and the relationship rules. It is not restated here,
and `tests/priority_rubric.rs` fails if it starts being.

Settled 2026-08-16 by a three-seat panel — web/UX, architecture, QA — convened
over this backlog and applied to all 26 open stories. It lived in this file for
two days, which was long enough to prove the point SH-354 then fixed: nothing
that *sets* a priority could read it, so every level chosen by `story new`,
`/story new` or `story-triage` was chosen by vibe. Promoting it into
`story help priority-rubric` was a council decision — recorded on SH-354 — taken on
the grounds that the generic half describes storyhook's own model (a closed
five-level enum, a CHECK-constrained rank column, `domain::ready_order`,
`story next` skipping blocked stories) and so is the tool's to state, the same
way the required states and the scaffolded `AGENTS.md` workflow already are.

What stays below is what does **not** belong in a stranger's binary: this
project's own evidence for each rule. The rubric is the law; these are the cases
that made it.

### This project's precedents

- **A defect must never sit at `none`.** SH-283 is the case study: filed `none`,
  it described a live, silent, cross-story overwrite of the system of record, and
  sorted dead last of 26. `none` was also `story new`'s silent default until
  SH-354, so it claimed *deliberately parked* on behalf of everyone who never
  chose — the command warns now.
- **Price the class, not the sighting.** This project has paid the opposite four
  times: SH-136, SH-258, SH-198, SH-260/276.
- **The detection-layer carve-out.** SH-306 is the precedent — a gate that
  silently did not run shipped six unguarded pushes. Coverage appetite in general
  earns nothing; being the *missing detector for a named defect* earns this.
- **Recoverability rarely demotes here.** The append-only event log does not
  qualify: history is deliberately unreachable from the CLI, so nothing prompts
  you to look, and recoverable-but-undetectable is operationally identical to
  lost.
- **The blocker floor versus the carve-out.** They collide whenever a defect is
  `blocked-by` the very instrument that would observe it. Found the first time
  the rule was used in anger: SH-283 (critical) `blocked-by` SH-335 (high) — a
  pairing no longer live on either story, since SH-335 landed and SH-283's edge
  was demoted to `relates-to` when it closed. What the precedent preserves is the
  reasoning, not the relation; do not go looking for the edge. The
  carve-out wins, for two reasons worth keeping written down. It is the **more
  specific** rule — it speaks to this exact pairing, where the floor speaks to
  dependencies in general. And the floor's purpose is **anti-stall**, which a
  detector edge does not create: the detector already sorts above everything
  except the defect it is blocking, so the queue hands it out next by itself.
  Raising it would buy no scheduling and would erase the ordering the carve-out
  exists to state.

### One standing rule of this project's own

Not in the shipped rubric, because it is about *this* tracker rather than about
priorities in general: **a defect in the work-allocation path — `story next` or
claiming — that hands out work which is nonexistent, already taken, or blocked
is `critical` by standing rule**, and a race there whose collision shows only
after work is duplicated is `high`. The autonomous loop in
`HARDENING_PROGRESS.md` finds its work through that path and inherits any
defect in it silently, which is the whole reason for the exception.

## Scope: adopt or file

**The rubric ships in the binary: run `story help scope-rubric`.** That is the
source — the default of adopting a mid-work discovery rather than filing it,
the test for whether it belongs to the story already in progress, when to fix
it now versus widen scope and leave the story open, and what still gets
filed. It is not restated here, and `tests/scope_rubric.rs` fails if it starts
being.

Settled 2026-08-17 on SH-402. This project's own evidence for the rule: the
backlog this doctrine was written against sat at 43 open against 358 closed
and climbing, and every prior autonomous cycle paid a full dispatch — plan,
worktree, PR, merge, close, reap — for problems a session already understood
before it ever filed them. The mechanism for landing more than one fix per
story needed no new plumbing: the autonomous charter has always read "every
pull request you open" and "once every merge lands", already plural. Only the
permission to use it, and the doctrine that names when to, were missing.

This widens the operator's own "except trivial same-session finds" — it does
not repeal "defects become stories before they become fixes" for anything
genuinely separate, too large for one session, or blocked on something
unreachable from where you are.

**This project's own numeric calibration**, which the shipped charter
deliberately does not carry (a raw token count there would be an opinion
about one operator's context window, not a fact about every install): this
project's autonomous sessions run a 1M-token context window, so "at least
half unused" is roughly 500k tokens used or fewer — the figure SH-402 itself
named. Recorded here, next to the window it derives from, per the project's
own "a ceiling derives from the deadline it disproves, never a bare literal"
rule.

## Rearchitecture roadmap

Story data has moved out of per-repo `.storyhook/` directories into one global SQLite store
behind a local daemon. Design of record: [`docs/spec/data-layer-rearchitecture.md`](docs/spec/data-layer-rearchitecture.md).
Execution state — wave status, step log, discovered defects — lives in
[`docs/rearch/STATE.md`](docs/rearch/STATE.md); read it before resuming this program.

| Wave | Scope | Status |
|---|---|---|
| W0 | quality-gate repair, shared test harness, baseline capture | **complete — merged** |
| W0b | wire-serializable envelope + the `Invoker` seam | **complete — merged** |
| W1 | `Store` trait, SQLite engine, migrations, rebuild-diff | **complete — merged** |
| W2a | story lifecycle + relation services, differential harness | **complete — merged** |
| W2b | project, config, system and grouping services; 27 of 46 arms ported | **complete — merged** |
| W2c | query + integrity services; TUI onto the seam; 38 of 48 arms ported | **complete — merged** |
| W2d | git/GitHub/transfer services; **all 48 arms ported**; the store test leg | **complete — merged** |
| W3 | `src/legacy/` reader, `story migrate`, the round-trip rollback gate | **complete — merged** |
| W4 | **the flip**: the global store is the default; `worktree_truth` green | **complete — merged**. Revert only while `migrate_round_trip` is 4/4 green — procedure in the flip checklist's section D2 |
| W5 | daemon promotion + `/api/v1/invoke` transport | **complete — merged**, except the quarantine deletion, which W6 carried out |
| W6 | the quarantine deleted (10,849 lines); full commit-body scanning; link idempotency as a DB constraint | **complete — merged** |
| W7 | this repo's own tracker migrated; `.storyhook/` retired; the defect ledger filed | **complete — merged** |
| W8 | crash, concurrency, and corruption hardening — **the last wave** | **complete — merged** |

**The program is complete and merged.** Follow-on work now lands as ordinary
branches; the rules below still apply to anything touching the store or the seam.

**The server-owned epic (SH-112) is complete** — all fourteen children merged.
The daemon is required, project selection is explicit, and git is an optional
convenience layer that refuses rather than guessing. Design of record:
[`docs/spec/server-owned.md`](docs/spec/server-owned.md), whose "As built"
section records the three deviations from the epic as filed — chiefly that the
committed `.storyhook.toml` pointer survives as an *identity*: a URL belongs to
at most one project, so a registered origin cannot answer for the second project
in one repository, nor for a fresh clone on a machine that has registered nothing.

**Store isolation landed** in v2.0.0 (SH-113, the first child of the
server-owned epic SH-112, and the origin-fix for SH-123). Daemon runtime state
is derived from the **canonical store path**, so one store has exactly one
daemon by construction; `--store-path`/`$STORYHOOK_STORE_PATH` name a store on
every command, and `story store new` creates one. Design of record:
[`docs/spec/store-isolation.md`](docs/spec/store-isolation.md), whose "As built"
section records the six decisions taken during implementation.

Its load-bearing invariants are pinned by tests rather than by this file, and
that "As built" section names the test guarding each (SH-131). A change that
breaks one fails the suite; one that keeps the promise by other means does not.

Standing rules for every wave:

- Every commit passes `make test`; history stays bisectable and two-hats clean.
- **`make test` is the merge gate; `make test-full` is the release gate** (SH-394). Both run
  the whole Rust suite over `/api/v1/invoke`, because since SH-114 that is the only way a
  `story` command reaches the store — `make test-daemon` and `make gate` are gone with the
  second transport, and what the daemon leg caught (a fixture only correct when nothing else
  holds the store) the single run catches now. `--test-threads=4` is a **bound on how many
  daemons exist at once**, kept as a measured decision; the arithmetic is on the target.
  `make test-full` additionally runs `scripts/run-e2e.sh`, the dashboard's browser suite:
  `e2e/playwright.config.ts`'s own SH-222 measurement put that leg at 2.9-6.4 minutes *per
  desktop project*, against a 36s median for the entire Rust suite
  (`docs/rearch/baseline/timings.md`) — nearly all of what made "nine minutes nominal,
  routinely longer" true, and the dashboard is a feature of storyhook, not the tool itself.
  `.githooks/pre-push` accepts a receipt from either tier and names which one it found;
  `scripts/release.sh` requires `test-full` and refuses `--skip-gate` outside
  `--local-only`. Design of record: [`docs/spec/test-tiers.md`](docs/spec/test-tiers.md).
- **A wall-clock ceiling on a test must derive from the deadline it disproves, never a bare
  literal** (SH-394). `assert!(elapsed < Duration::from_secs(2), ...)` states two things at
  once — that some production deadline was not spent, and an opinion about how fast that
  should look on this machine, today — and the second claim is the one that flakes on a
  machine running three-to-four concurrent worktree suites at once. Derive the ceiling from
  the constant it is meant to prove was not reached (half of it, twice it, or the constant
  plus a stated margin), widen a fixture's own fixed cost rather than tightening the
  assertion when the two sit too close together to separate any other way, or widen an
  already self-calibrated ratio when concurrent measurement pays more contention than the
  baseline it is compared against. `tests/timing_assertions.rs` fences the bare-literal shape
  mechanically, in either comparison direction and whether or not the type is qualified;
  it cannot judge whether a margin is wide enough, only that the question was asked by name.
- **`make test` must keep its isolated `STORYHOOK_DATA_DIR`** (`scripts/run-tests.sh`).
  ~45 test files still build fixtures with `tempfile::tempdir()` and inherit the process
  environment; without the override a test run writes into the developer's real store.
- **`app::run`, `lock.rs` and `registry.rs` are gone** (W6, 10,849 lines). `storage.rs`
  survives on purpose as the **rollback path** — `store -> export -> a legacy tree`, which
  `tests/migrate_round_trip.rs` runs end to end and the W4 revert policy is conditional on.
  Nothing under `src/` may call it: `invoker_seam.rs::the_legacy_write_path_is_gone` fails if
  a `src/` file so much as names `crate::storage`.
- **A test that asks about bytes on disk must stand the daemon down first** —
  `TestEnv::stop_daemon()`, immediately before the file is touched rather than once at the
  top, because the next `story` command starts another one. A daemon holds the store open
  with its own page cache and log handle, so it answers reads from memory and does not
  notice the file being replaced. `crates/storyhook-test-support/src/crash.rs` is where the
  same rule is enforced for a test that kills the writer.
- **A test build refuses to resolve a real data home** (`storyhook::env::is_test_build`), which
  is what makes a bare `cargo test` safe. Consequence: the binary `cargo test` leaves in
  `target/debug` will not touch a real store; `cargo build` produces one that will.
- **Nothing in any test may bind port 3456 or outlive its run.** Which tracked shell
  scripts must export `STORYHOOK_DAEMON_ADDR=127.0.0.1:0` and `STORYHOOK_PARENT_PID` is
  derived, not enumerated here (SH-136) — a hand-maintained count drifted three times
  before it stopped being trusted. `tests/store_isolation.rs::every_harness_that_
  isolates_the_data_dir_also_contains_its_daemon` pins it to whichever scripts export
  `STORYHOOK_DATA_DIR`, the same set `…_neutralizes_the_store_path` derives beside it;
  today that's `scripts/run-tests.sh`, `scripts/capture-baseline.sh`, `scripts/run-e2e.sh`,
  and *both* `plugin/claude-code/tests/{lib.sh,run-tests.sh}` — the last two because
  `run-tests.sh` sets `STORYHOOK_TEST_HOME`, which makes `lib.sh` skip its block. `TestEnv`
  can't drift the same way: it pins both variables once, in
  `storyhook_test_support::daemon_containment()`. Four Rust test files `env_clear()` on
  purpose and call `daemon_containment()` afterward to reinstate containment, rather than
  hand-copying its two literals a fifth, sixth, seventh and eighth time.
- **Rendered output is never evidence that a process is running — confirm the process.**
  Dispatch's readiness gate inferred "Claude is ready" from a frame rule and a prompt glyph,
  which a shell prompt supplies for free, and typed the autonomous charter into zsh; the
  shell executed it and closed four stories (SH-226,
  `docs/rca/dispatch-types-the-agent-charter.md`). `wait_ready` now also requires
  `#{pane_current_command}` to match `READY_PROCESS_PATTERN`, and an unconfirmed pane is
  refused rather than warned about. The daemon side already had this right —
  `await_healthy` wants a portfile, an identity check and a `hello` round trip. Screen-scraping
  a TUI footer is a heuristic; treat it as one.
  **But a process's NAME is not its identity** (SH-239): tmux reports
  `#{pane_current_command}` as the basename of the *resolved* executable, and Claude Code's
  native installer points `~/.local/bin/claude` at `~/.local/share/claude/versions/<version>`
  — so the occupant is called `2.1.228` and every dispatch was refused. `pane_runs` now also
  accepts the launch command's own resolved binary, and a retained sibling version beside it
  for update skew. Widening the *pattern* would have been the wrong repair twice over: the
  version changes under running sessions, and `.` re-opens SH-226. Ask what a process **is**,
  not what it is spelled.
- **A test that needs a store the guards call "real" asks the harness for one, never the
  checkout** (SH-258). `storyhook_test_support::non_temporary_dir` resolves a fixture root
  `service::project::is_under_temp` will reject, independently of where the checkout sits —
  `$STORYHOOK_TEST_REAL_ROOT`, then a directory beside the running test binary, then
  `$HOME/.cache/storyhook/test-real-fixtures`, verified at each rung with the production
  predicate. Four sites used to infer "not temporary" from `CARGO_MANIFEST_DIR` or
  `CARGO_TARGET_TMPDIR` instead — the checkout's own `target/` — which is silently false from
  a checkout that is itself temp-rooted: a fresh disposable clone, the correct tool for
  cutting a release or reproducing against pristine `origin/main`. From there every one of
  those guards read both sides as throwaway, went correctly inert, and the tests expecting a
  refusal failed with messages naming neither the store nor the cause. Only one test read as
  broken (`left: 201, right: 201`) because `cargo test` fail-fasts at the first failing
  target; the true extent was three guards across four files, fourteen tests, found by
  reproducing with `CARGO_TARGET_DIR` pointed under `/private/tmp` before writing the fix.
  `tests/store_isolation.rs::nothing_outside_real_store_rs_re_infers_a_real_store_from_the_
  checkout` fences the pattern the same way the daemon-containment scan does — derived over
  `git ls-files`, not a hand-maintained list of the sites that do this.
- **A fake that keeps state on disk names its directory or refuses to run** (SH-263).
  `plugin/claude-code/tests/fakes/tmux` is re-exec'd per call, so its whole model lives in
  files under `$FAKE_TMUX_STATE`; that variable used to default to a fixed `/tmp` path, and
  five test files took the default — sharing one directory with each other, with every
  concurrent run, and with the `issue` plugin's fake of the same name. Two users in one
  directory corrupt each other: one's `new-window` clears the other's `launched` and `input`,
  whose next Enter is read as a launch of nothing and writes the fallback shell name over the
  first's occupant — so a dispatch refused a pane it had itself launched `claude` into,
  reporting `zsh`. The gate was right; the fixture lied to it. `lib.sh` now mints one per test
  and the fake refuses both an unset variable and a directory it would have to create.
  `test-fake-tmux-state.sh` pins it. Note what did **not** reproduce it: stale state seeded
  before a run, which `new-window` resets. It took a concurrent second writer, which is what a
  fixed shared path is *for*.
- **A fixture may not doctor a file a live daemon owns** (SH-345). A test rewrote a running
  daemon's portfile — `stale.exe_mtime -= 1`, leaving the process itself alone — expecting the
  doctoring to hold until the next `daemon start` read it. It didn't always: since SH-186 a
  daemon's background tailnet probe (`serve::tailnet_reprobe`) rewrites that same file with its
  own correct `exe_mtime` the instant it binds, and on a tailnet-equipped machine that happens
  shortly after nearly every daemon starts. Under load, that rewrite can land between the
  doctored write and the next command's read of it, silently restoring the correct build and
  making the version-skew check pass right over the case it exists to catch — reported as
  identical old/new pids, `assertion left != right failed`. Confirmed by toggle, not by
  argument: with a 150ms gap inserted before the confirming read and `tailscale` left reachable,
  8/8 runs failed with the exact reported signature; with the same gap and `tailscale` denied,
  8/8 passed. `tests/web_test.rs`'s `web_open_falls_back_to_the_bare_url_when_arming_fails` had
  already hit and fixed the identical hazard once, against a different doctored field, with its
  own local workaround; SH-345 promoted it to `storyhook_test_support::path_without_tailscale`
  and moved both fixtures onto it. **The general form:** any fixture that mutates a file a
  running process also writes is racing that process, whether or not the mutation looks like it
  targets a static one. `tests/portfile_fixture_hygiene.rs` fences the specific class — a
  portfile write that follows a daemon start, in the same function, must carry the guard — over
  a coarser file-level design rejected by unanimous council vote (`.council/
  sh-345-portfile-fixture-hygiene-fence/`, gitignored) because the two files that have ever
  doctored a portfile are already permanently "compliant" by that coarser measure, so it could
  only ever have caught a hypothetical third file.
- **A `pub` item with no caller is invisible to `dead_code`, so a test has to find it**
  (SH-198). `get_timeline` sat on `GithubClient` with zero call sites anywhere in `src/`
  or `tests/`, and the compiler never warned: `dead_code` only fires for an item
  unreachable from a crate root, and `pub` makes everything reachable — correct for a
  published library, wrong here, since this crate is published nowhere
  (`.github/workflows/release.yml` ships binaries on version tags, never `cargo
  publish`) and nothing outside `src/` and `tests/` can ever call one. Nine more had
  already accumulated the same way, two of them actively misleading rather than merely
  idle — `is_executable`'s doc promised an execute-bit check its body never performed,
  and `ArchiveRepairReport` documented a function (`repair_archived_snapshots`) that a
  prior deletion had already removed. `tests/dead_public_surface.rs` fences the class:
  derived over `git ls-files`, the same style the store-isolation scans use, rather than
  a hand-maintained list — which is exactly what let ten of these go uncounted.
- **Platform-specific code needs a platform in the release matrix, or it needs to not
  exist** (SH-260, SH-276). `cfg(target_os = "windows")` accumulated in two shapes with
  no Windows target anywhere in `.github/workflows/release.yml`'s matrix and no signal,
  on any platform, that either compiled: a Cargo dependency table (SH-260 —
  `windows-native-keyring-store`, activated by default, deleted) and, once that scan
  couldn't see them, two bare source arms with no dependency behind them at all
  (SH-276 — `src/clipboard.rs`, `src/web.rs`, deleted). `tests/release_targets.rs` pins
  both shapes now — `every_platform_gated_dependency_table_targets_a_built_platform` for
  manifests, `every_platform_gated_source_arm_targets_a_built_platform` for `cfg` arms in
  tracked `.rs` files — and both are matrix-derived, not Windows-specific: a `target_os`
  this project has not been taught (`matrix_substring_for`) panics rather than passing,
  and a target_os the matrix *does* build passes with no edit. Adding a platform for
  real means adding it to the matrix and proving the code by building it there; the test
  passing is the matrix's decision, not this file's.
- **An argument that lands nowhere must be refused, not dropped** (SH-357). `story daemon
  token new psamathe` printed the daemon's **master** bearer token and exited 0: `parse_daemon`'s
  `"token"` arm was a bare unit variant that never read `args[2..]`, so a request to mint a
  scoped, revocable token was answered with the credential that rotates on every daemon
  restart — and the output was byte-identical to the one a correct invocation produces, which
  is what carried a wrong hypothesis into SH-319. This is the SH-52/SH-62 doctrine
  (`reject_unknown_flags`, one guard ahead of every verb) arriving for *positional* arguments,
  which had no equivalent. Every complete arm now ends in `expect_no_more`, naming the
  offending word above that arm's own usage string. **The filed extent was wrong in both
  directions and only execution settled it** — nine candidates found by scanning for the shape
  the bug happened to have; three of those (`github-auth login|status|logout`) already
  length-checked, and the real count was **25**, across ten parse functions and the top-level
  `dispatch` table itself (`summary`, `export`, `session-start`). `tests/trailing_arguments.rs`
  pins the class, and pins it **behaviourally**: it derives only the *vocabulary* from
  `src/cli.rs` — every quoted word left of a `=>` — and then asks the parser whether
  `parse_invocation(P + [junk])` returns the **same `Invocation`** as `parse_invocation(P)`.
  Equality rather than acceptance is the entire distinction, and a shape scan cannot make it:
  `story import` reads stdin where `story import <file>` reads a file, and both are correct
  because the word landed in a field. A word that leaves the invocation unchanged landed
  nowhere. Because the property is about what the parser *does*, an arm written in a shape
  nobody has thought of yet is still caught — verified by mutation, adding a fresh
  `"dump-everything"` arm and watching the test name it with no edit of its own. The scan is
  cheap and safe to run this exhaustively only because `parse_invocation` is pure, which is
  also why it can provoke `story daemon install` without installing anything.
- **A hook that did not refuse is not evidence that a hook ran** (SH-306). A Claude Code
  `PreToolUse` hook **fails open at its timeout**: the harness SIGTERMs it and then lets the
  tool call proceed, silently. `~/.claude/hooks/pre-push-tests.sh` runs `make test` under a
  900-second ceiling that this suite — nine minutes nominal, longer whenever two of the
  three-to-four concurrent worktree sessions overlap — routinely exceeds. Six of the eight
  hook logs left on this machine ended at exactly 900s across three days; each was a push
  that shipped with no gate and no message saying so. The `pre-push-tests: running…` line is
  therefore a statement of intent, never of completion, and its *absence* means nothing at
  all (a backgrounded tool call never shows it even when the hook ran the full 900s). This is
  the SH-226 doctrine one layer up: rendered output is not evidence a process ran, and now,
  neither is a gate's silence. **The gate that counts is git's own** — `.githooks/pre-push`,
  which has no deadline and no opinion about how the push was invoked. It verifies a receipt
  naming the tree that went green rather than re-running the suite, because a hook that
  re-ran it would inherit the same deadline and would start a second nine-minute suite while
  the first was still running — already recorded here as the cause of a false red.
  `scripts/gate-receipt.sh preflight` (the first line of `make test`) **enrols the clone**, so
  running the gate is what installs the gate; the `postlude` phase is its **last** line, so
  "no receipt unless every leg passed" holds by make's own fail-fast semantics. Two limits
  are deliberate and neither is a bug: the receipt attests **the tip tree of each pushed
  ref**, so unreceipted commits in a range are *named, not blocked* (a receipt per commit
  would mean a nine-minute suite per commit, which is the pressure that caused SH-306); and
  **forgery is not the threat model**, since anyone who can hand-write a receipt already has
  `--no-verify`. `tests/push_gate.rs` provokes the bypass rather than asserting the hook
  exists, and is mutation-checked in both directions.
- **A timeout is not a rollback; a client that gave up is not a server that didn't** (SH-312).
  `src/web_dashboard.html`'s `api()` gave every mutation a flat 10s before reporting
  `"request timed out"` — a phrase read as "nothing happened" — against a daemon whose event
  hooks are sanctioned to run synchronously, *after* the commit, for up to 60s
  (`event_hooks::HOOK_TIMEOUT_CEILING_SECS`). A slow-but-successful create was reported as a
  definite failure with the form still live, and the user's own retry filed a duplicate story
  (SH-310/SH-311, 24s apart — the fourth such pair in this tracker's history, and the first
  from the dashboard rather than a scripted CLI caller). `HttpInvoker` (`src/invoke.rs`) had
  already drawn the correct line for the CLI door — it retries *only* a refused connection
  (nothing delivered) and reports every other failure, including a timeout, as "may or may not
  have run" — but the dashboard is a second client of the same daemon, in a second language,
  and never inherited that doctrine (see `docs/rearch/hardening.md`'s "read before retry" rule,
  written with the CLI in view). The fix: an in-flight guard on the create modal's mutating
  actions (the class had none — every other surface, `dispatchButtons()` included, already
  guards); a mutation's `.catch` now distinguishes a *definite* server answer from `status:0`
  (network error or client timeout, provably unprovable) and reports the latter honestly,
  refetching in the background rather than leaving a lie beside a primed resubmit button; and
  the mutation deadline itself is raised, **derived from `HOOK_TIMEOUT_CEILING_SECS`** rather
  than hand-copied a second time — `tests/dashboard_mutation_deadline.rs` fails if the two
  numbers drift apart, the same class of failure SH-136 already cost this project three times.
  Full RCA: `docs/rca/duplicate-story-from-the-dashboard.md`. **The rule for any client this
  daemon has, present or future:** an ambiguous outcome (timeout, connection reset after the
  request left the process, a daemon that stopped answering) is reported as ambiguous, never as
  failure — a comforting, false "it didn't work" is worse than an honest "I don't know," because
  the reader acts on the lie.
- **A timestamp is not an ordering key** (SH-336). Every storyhook timestamp is RFC3339 at
  one-second precision (`service::Clock::System`), and this tracker's normal workload is
  agents writing in bursts, so a comparator whose only key is `updated_at`/`created_at` is
  blind inside a second — it was, on five surfaces at once (SQL, the board's "Modified" and
  "Added", the dashboard List view, and the TUI's recent-activity panel), found by SH-329's
  own flake and a sibling sweep after. `stories.head_global_seq` is the exact tiebreak: the
  change-feed position of the event a row was folded from, allocated inside the write
  transaction and therefore total — the same doctrine `domain::ready_order` already stated
  for SH-63 (order on a key that structurally cannot tie, never a timestamp). It reaches the
  browser on `StoryView.head_global_seq` and the TUI on `ProjectSnapshotView.head_global_seqs`;
  `StorySnapshot` deliberately does **not** carry it, because that document is the verbatim
  fold of a story's events and `story doctor`'s `diff_rebuilt` compares it against a fresh
  fold — a non-fold field there reports every story as divergent. A recency order that does
  not end in `head_global_seq` is not a total order. `docs/spec/recency-ordering.md` names
  the comparator and the test on each surface.
- **A fixture derives the vocabulary it writes into a column production writes** (SH-364).
  `seed_a_v14_project` seeded `events.kind` values `'story_created'` and
  `'story_priority_set'` for fourteen migrations — snake_case spellings no writer emits
  (`domain::event_kind` returns PascalCase, `write.rs` inserts it verbatim, and
  `is_known_event_kind` is case-sensitive on purpose). Harmless only because migration 15's
  backfill joins on `seq = head_seq` and never reads `kind`. SH-359's migration made `kind`
  the **entire** predicate, and a seat on its council read that fixture as the nearest
  precedent — correctly, it was — and proposed the same snake_case spelling for the
  migration. A test that seeds a spelling and a migration that matches the same spelling
  agree with each other, pass, and match **zero rows** in every real store, silently
  backfilling every story to "never assessed". Review caught it; the suite structurally
  could not. This is SH-263 one layer over: *the gate was right; the fixture lied to it.*
  **The hazard is confined to this one column by construction, and that is why it needs a
  test rather than a constraint** — `superstate`, `priority` and `priority_rank` all carry
  schema `CHECK`s, so a fixture cannot lie there at all, while `events.kind` deliberately
  cannot have one, because SH-54 requires a store written by a *newer* storyhook to stay
  readable by this one. `tests/event_kind_vocabulary.rs` is the CHECK that column cannot
  have: derived over `git ls-files` in the SH-198/SH-260 style, covering production SQL as
  well as fixtures (migration 9 hand-writes a kind, and a wrong one there outlives every
  test), and carrying a positive control so a parser that stopped recognising statements
  fails instead of reporting a clean tree. It reads SQL only, so the **typed** door —
  `RawEvent { kind: "…" }` through `inject_raw_events`/`append_raw_events` — is fenced
  differently on purpose: no static rule can judge it, since a fixture there legitimately
  wants a known kind (*damage*) or an unknown one (*data from the future*), and that choice
  is the entire subject of the tests using it. Each spelling is a named constant with a
  test asserting which of the two it is. Measured rather than assumed: misspelling
  `store_inject.rs`'s constant leaves all eight of that file's other tests green.
- **A council's verdict goes on its story, and no tracked file names the council's own
  directory** (SH-363). The `/council-vote` skill writes its trail to a slug directory under
  a gitignored plugin-state folder, relative to whatever directory the agent was standing
  in — so a council convened inside a per-story worktree is deleted at teardown while the
  code citing it survives. Measured when this was filed: 82 such citations across 51 tracked
  files, of which 16 distinct slugs were already gone. The **inverse** pin is the only one
  implementable, and `tests/council_citations.rs` is it: a fresh clone has none of that
  directory, so "every cited slug resolves" would fail everywhere but one machine or go
  vacuously quiet — instead, a slug may not follow the slash. A **bare** mention is fine and
  needs no exemption, which is why that test needs none for itself: it assembles its own
  fixtures at run time so the marker never sits adjacent to a slug in its source. Cite
  `story show SH-N`; where the trail is already gone, **state the verdict inline** rather
  than pointing at a story that carries nothing — a pointer that fails silently is worse
  than the dead path it replaced, which at least failed loudly. Two councils settled this,
  both unanimous, and the second only because the first's salvage step needed a release that
  does not exist yet: `story comment` on a *closed* story was fixed by SH-261 and is in no
  release (SH-369), which is what SH-370 waits on. Both councils rejected reopen/re-close —
  it retracts and re-stamps `closed_at` and fires live `StateChange` hooks, falsifying an
  audit trail in order to salvage one — and rejected running an unreleased daemon against
  the live store, which would migrate it past what the installed binary can read.
- **A column the oracle forgot is a column nothing watches — so the compiler checks the
  struct and a damage test checks the comparison** (SH-365). `diff_rebuilt` is `story
  doctor`'s only oracle over the `stories` read model, and it reached the columns through
  `row.field` accessors: its list was hand-written with nothing checking it, so a column
  added to `StoryRow` and forgotten there was simply never compared, forever and silently.
  SH-211 had already added the two lines `hidden_at` and `draft` were missing; it added
  nothing preventing the third. Two mechanisms now, because neither is sufficient alone.
  `column_comparisons` destructures `StoryRow` with **no** `..`, so a new field is
  `error[E0027]` on a bare `cargo check`, and a bound-but-unused one fails clippy's
  `-D warnings`. `tests/read_model_column_coverage.rs` damages each column underneath the
  store through a second connection and demands the oracle name it, its case list derived
  from `StoryRow`'s own definition so it cannot drift, with a positive control on the
  parser and a clean-fixture assertion so no case can pass vacuously. **Why the compiler
  is not enough is measured rather than argued:** `sqlite::read::hydrate` is *already* a
  rest-free struct literal over this exact type and still cannot see that
  `raw_story_from_row` fills it by **position**, `row.get(N)`, across three
  `Option<String>`s, three `bool`s and four `String`s — swap an adjacent pair and every
  field is named and every field is wrong. That decides the damage suite's *form*: it goes
  through SQLite, never against a hand-built row, or it would sail past exactly that
  fault. It also closes the `field: _` discard, which is one character and otherwise
  reviewable only by eye (SH-306). Both mutations were run in both directions: deleting
  `report("draft", …)` fails the damage test naming `draft`, and adding a column fails
  `cargo check` here where before the refactor it compiled the whole library clean. Two
  limits are stated rather than claimed away — a `stories` column the read model never
  exposes is outside both controls (`priority_rank`, guarded by its own CHECK), and a
  *symmetric* mis-comparison still passes, because both sides move together. Settled by a
  council whose verdict is on the story (`story show SH-365`): a 1-1-1 self-vote split in
  round one, unanimous after one deliberation round, with the QA seat ranking **its own**
  proposal last once its alternative — deriving coverage from `diff_rebuilt`'s own
  `report("…")` literals — was shown to be self-referential, able to prove only that
  comparisons which already exist are tested and blind by construction to a column that
  was never reported at all.
- **The e2e suite runs on WebKit, not just Chromium — one project per seed, never a shared
  one** (SH-335, SH-348). `e2e/playwright.config.ts` names two engine pairs: `chromium`/`webkit`
  (desktop) and `mobile-chromium`/`mobile-webkit` (mobile). Each pair is keyed off the identical
  spec selector — one `MOBILE_SPECS` constant, used four times, excluded by the desktop pair and
  matched by the mobile pair — so nothing is hand-listed onto one engine —
  `tests/e2e_browser_coverage.rs` fences both pairs' identity (and that the two pairs share the
  constant under opposite selector kinds), plus that every engine the config names is one
  `make e2e-install` actually installs. `scripts/run-e2e.sh` runs each project against its OWN
  daemon, seed and `FAKE_TMUX_STATE`, never a shared one: `e2e/specs/dispatch.spec.ts` claims a
  seeded story for real (a CAS-guarded `story move ... in-progress`) and creates a real `git
  worktree`, so a second engine's pass against a daemon the first engine's pass already
  dispatched through would fail on an already-claimed fixture for a reason that has nothing to
  do with either engine — SH-335 is where that shape was ruled out. A bare
  `bash scripts/run-e2e.sh` therefore loops once per project *derived from the config*, not a
  list here; `--project=NAME` still runs one project alone. Separately: WebKit's Tab order skips
  buttons and links unless this machine has `AppleKeyboardUIMode >= 2` (macOS's Full Keyboard
  Access, off by default — real Safari's own behavior, not a Playwright quirk). The harness
  measures it and exports `E2E_FULL_KEYBOARD_ACCESS` rather than silently flipping a System
  Setting (the SH-306 shape: a gate whose verdict depends on state it never checked or reported)
  or permanently quarantining the affected assertions (undated debt on the exact
  keyboard-reachability class this suite exists to strengthen); `e2e/specs/support.ts`'s
  `fullKeyboardAccess()` is the one place a spec reads it back to gate the handful of assertions
  that need real Tab traversal, unconditionally on `chromium`. A handful more tests
  (`board-readiness.spec.ts`, `duplicate-create.spec.ts`, `drawer-field-mutation-timeout.spec.ts`)
  are quarantined under `webkit` unconditionally, for an unrelated, not-yet-root-caused gap
  where WebKit doesn't reliably surface a held or delayed `page.route()` to the page's own XHR
  handlers within this suite's timeouts (SH-347). `mobile-webkit`'s own first run surfaced a
  fourth, since fixed: WebKit ignores `min-height` on a default-appearance `<select>`, so every
  select in the dashboard measured ~23px against the intended 44px coarse-pointer minimum on
  that engine (SH-377). Fixed by giving every `select` an explicit `height` instead of a second
  `min-height` rule WebKit would keep ignoring — `docs/spec/responsive-dashboard.md`'s own "Tap
  targets (D3)" section has the rule and why `height` was chosen over `appearance: none` plus a
  replacement caret — so `responsive.mobile.spec.ts`'s select-half tap
  target test is fully load-bearing again, on every project, with no quarantine left to name.
  The button/link half of that sweep stays a separate test regardless, so a *future* select
  regression still can't take button/link coverage down with it by sharing one selector. Every
  remaining WebKit quarantine, either
  shape, is a `test.skip(...)` naming its story in the reason string —
  `tests/e2e_browser_coverage.rs::every_webkit_quarantine_names_a_story` fails the build on one
  that doesn't.
- **A hook that annotates must never decide** (SH-355). `githooks(5)`: git ignores a nonzero
  `post-commit`/`post-merge`, but a nonzero `prepare-commit-msg` **aborts the commit** — the
  only one of the three managed hooks whose exit status git actually obeys.
  `PREPARE_COMMIT_MSG_HOOK` (`src/hooks.rs`) used to end on `[ -n "$STORY_ID" ] && { ... }`
  with nothing after it, so an empty backlog — `story next` answers that with
  `{"result":"ok","message":"no ready stories"}` at **exit 0**, a real answer, not a
  refusal — left that conditional's own status as the script's last command, and every
  editor-opening commit or `--amend` against an empty backlog was silently refused, git 2.50.1
  measured, since the hook was written. Every exit path in a managed hook now ends in an
  explicit `exit 0` for exactly this reason, `post-commit`/`post-merge` included even though
  git ignores theirs — one invariant across all three that a test can check mechanically
  (`tests/hooks.rs::no_managed_hook_lets_its_own_last_statement_decide_gits_verdict`, reading
  the installed files back) rather than a comment a future edit could silently violate.
- **A declared focus rule with nothing measuring it fails the build, derived on both sides**
  (SH-360). SH-338 built `measureFocusIndicator` (`e2e/specs/support.ts`) — real Tab presses,
  `getComputedStyle`, the backdrop composited from the live ancestor chain, all four theme
  resolutions — and pointed it at exactly two of the sheet's nine `:focus`/`:focus-visible`
  selectors; the other seven sat unmeasured until SH-360 closed the gap. What stops it
  reopening is `tests/dashboard_focus_coverage.rs`, which names no selector itself: DECLARED
  comes from scanning `web_dashboard.html`'s one `<style>` block for every focus pseudo-class
  (comments stripped first — CSS's `/* */` blocks routinely sit directly above a rule with no
  intervening brace, which swallowed two real selectors into their own doc-comment prose the
  first time this ran), MEASURED comes from every string-literal argument
  `measureFocusIndicator(` is called with across `e2e/specs/*.spec.ts`, and both reduce to a
  base selector (stripping the trailing pseudo-class) before a plain set-equality check in both
  directions — an unmeasured declared rule, or a measured selector the sheet no longer
  declares. A hand-maintained coverage list was rejected outright: it is the exact shape that
  let the original two-control gap sit unnoticed, and this project has paid for that shape
  before (SH-136, SH-198, SH-258, SH-260/276, SH-364). Rust over an in-page CSSOM scan for the
  same reason `tests/dead_public_surface.rs`/`tests/store_isolation.rs` are Rust: a focus rule
  inside `@media` needs identical treatment to one outside it, so no brace-depth tracking is
  needed either way, and CSSOM can only ever see the declared half — checking the measured half
  from inside a browser spec would report a repo-hygiene finding as a two-engine failure in a
  ~9-minute leg instead of a sub-second `cargo test`. It is a **wiring** fence, not a behaviour
  one: it proves a call site exists, never that it reaches the right pixel, and it structurally
  cannot find SH-338's *own* defect class — a focusable control with no author-declared rule at
  all, which `.btn`/`.ctxmenu-item` still are today, deliberately, and so never enter the
  declared set this fence reads from.
- **An absent field in a message written by an older version is not a stated value**
  (SH-372). GitHub-sync's issue-body block omitted its `priority:` key whenever a story's
  priority was `Priority::None` — collapsing *deliberately parked* (`story prioritize <id>
  none`, a decision) and *never assessed* (SH-359's `priority_assessed = false`, silence)
  into the identical wire shape. A push read only `story.priority`, never
  `priority_assessed`; a pull that found no key wrote no `StoryPrioritySet` event at all, so
  a parked story pulled back down — on a second clone, or an "Import all" re-run — folded
  unassessed, and SH-358's own new warning then told the operator a falsehood about a story
  they had personally assessed. The fix is a decoded three-state read
  (`github::field_map::RemotePriority::{Unknown, Assessed}`) rather than a bare `Priority`,
  with `Unknown` — no key, no block, or a value `Priority::parse` rejects — resolved against
  the sync's own merge base instead of ever being read as "unassessed" on its own; only an
  explicit `Assessed(p)` can move either side of a priority merge. That resolution is also
  what keeps a merge base written before `priority_assessed` existed (every `github_bases`
  row from any release before this fix has no such key in its stored JSON, and deserializes
  it to `false` even for a story assessed at a real level) from reading as a live remote
  change — normalized once, on load, against `fold_story`'s own invariant
  (`priority == None || priority_assessed`), rather than trusted as stored. Council verdict
  on the sibling question — whether the wire format should carry a version marker so an
  already-synced, already-quiet story could be force-repushed onto the corrected shape —
  was no, unanimous by round 2 after one deliberation round: `story show SH-372` (SH-363 —
  cite the story, not the council's own directory, which resolves on no fresh clone). The
  general form travels beyond this one field: a message format that predates a distinction cannot
  retroactively express it, and the fix is for *absence* to decode as "states nothing,"
  resolved against whatever the reader already believes, never silently promoted to a
  negative answer.
- **A blocker that is a story is recorded as a story, not as prose about one** (SH-398).
  `story block <id> "<reason>"` could only ever write a free-text `awaiting` field —
  `is_ready`'s three block signals (the reserved `blocked` state, an open `blocked-by`/
  `obviated-by` edge, and `awaiting`) already agreed that only the middle one is a fact
  the store can act on: it clears itself the instant the blocker closes, is visible from
  both stories, and `story doctor` can audit it. Prose is inert. An autonomous session
  blocked SH-394 *on* SH-397 with nothing but a paragraph naming it; there was no edge to
  clear when SH-397 closed, and the dashboard drawer's banner — gated on `awaiting` alone
  and rendering `linkifyStoryIds()`'s own mixed array of nodes straight into a
  `display: flex; align-items: center` container with no wrapping body — rendered that
  paragraph as a run of narrow, unreadable columns rather than wrapped prose. Fixed on
  three layers: `story block <id> --on <blocker>...` records the edge and the reason in
  one transaction (`RelationService::block_on`, `relate()` generalised from one target to
  N); a nudge (`src/block_notice.rs`) warns — never refuses, since `story block` runs
  non-interactively — when a written reason names an open story with no edge behind it,
  wired at every dispatch arm that can set `awaiting` and fenced by a derived door list
  (`tests/block_notice_paths.rs`) rather than a hand-kept one, the shape SH-136/SH-198/
  SH-258/SH-260/276 already cost this project; and `story doctor` sweeps the existing
  backlog for the same gap, since the nudge only fires at authoring time. The dashboard's
  card badge and drawer banner now derive their blocker/obviator lists from one shared
  function (`blockCauses`, `src/web_dashboard.html`) rather than each filtering
  `st.relationships` on its own — the drawer used to be blind to a relation-only block
  entirely, showing the "add a reason" form beneath a card that already read `● blocked
  (SH-397)`. Design of record: `docs/spec/blocked-causes.md`.
- **`.githooks/pre-push` cannot see a merge commit, so nothing certified it — a poller has
  to** (SH-396). `gh pr merge --merge` is a server-side merge: no push happens, so the push
  gate never fires, and this project runs no test CI in GitHub Actions by policy. PR #484
  (SH-315) landed exactly that gap: two independently green branches — an exhaustive
  `Invocation` match in `tests/unassessed_priority_paths.rs`, and SH-315's new `Attachment`
  variant with no arm added for it — with zero textual conflict, merged into a tree that
  failed to compile. `main` was red for 73 minutes before anyone noticed. Measured over the
  30 merges preceding the fix: **14 produced a tree matching neither parent** — content no
  receipt could possibly have covered; one of the 14 didn't compile. `scripts/merge-
  preflight.sh` asks the exact question before a merge happens rather than a proxy for it:
  `git merge-tree --write-tree origin/main <pr-head>` computes the tree the merge WOULD
  produce (verified byte-identical to a real merge of the same two parents,
  `tests/merge_gate.rs::the_predicted_tree_matches_a_real_merges_tree_exactly`), and checks
  it against the same tree-oid-keyed receipt store `.githooks/pre-push` already reads — a
  merge tree and a pushed tree are the same kind of claim, so they share the one store
  rather than needing a second one taught to every reader of the first. `make test` is
  36.4s median warm on this machine (`docs/rearch/baseline/timings.md`), so the gate runs
  as `scripts/merge-watch.sh`, a poller over every open PR (`make merge-watch`, meant to be
  re-run every 1-3 minutes by something that already exists on the machine — installing
  that timer is a bootstrap step this repo documents rather than performs) rather than a
  merge-time-only check that depends on an agent remembering to invoke it: it reaches a
  merge made from the GitHub UI, another machine, or a session that never read this file,
  none of which a local hook can. A green run certifies the tree for real through the same
  `gate-receipt.sh` postlude every ordinary push uses, so a PR that has gone green here
  needs no further work once actually merged. Status is reported by upserting one PR
  comment per PR, found by a fixed marker and edited in place rather than posted fresh —
  GitHub does not notify on an edit the way it does on a new comment, so a PR that is still
  red after the next poll produces one notification total, not one per pass, which is the
  self-noise shape this project has already paid for three times over (SH-306, SH-345,
  SH-263: a gate or fixture that fires repeatedly for one unchanged fact). The comment
  always carries a last-checked timestamp for exactly the reason SH-306 named one layer
  down: a gate that goes silent (the poller dies, `gh` auth expires) must read as stale, not
  as a quiet all-clear. `tests/merge_gate.rs` exhaustively covers the primitive
  (`merge-preflight.sh`) the way `tests/push_gate.rs` covers the push gate — real git, real
  receipts from the production writer, mutation-checked; `merge-watch.sh`'s own `gh`
  orchestration is deliberately outside that suite, for the same reason SH-263 and SH-345
  are the precedent rather than the counter-example: mocking `gh`'s behaviour would validate
  the mock, not the integration, so it is verified by hand against this repo's own live PRs
  instead.
- **The forward-compat gate needed a write-side twin, or a newer binary could break an older
  one's store on the way in** (SH-404). SH-54 refuses an older binary that opens a *newer*
  store — the read side. Nothing stopped the opposite: a `cargo build` binary (debug or
  release, in any worktree) resolves the real data home and applies every pending migration
  on first open, one-way, with no prompt. `storyhook::env::is_test_build` fences `cargo test`
  builds out of the real data home; a `cargo build` binary carried no fence at all. On
  2026-08-17 a worktree's debug binary carrying a migration merged to `main` but shipped in
  no release moved the real store's schema from 16 to 17; the installed v2.1.1 binary
  understood only 16, so every `story` command failed at daemon start until a build from
  `main` was installed by hand. No data was damaged — only the schema stamp moved — but the
  tracker was down. `storyhook::migration_guard` (`src/migration_guard.rs`) is the write-side
  twin: it refuses to advance the **default** store's schema when the running binary is not
  the `story` its own `$PATH` would resolve, and a store already at a non-zero version has
  something pending. Two choices were deliberate rather than obvious. The predicate is PATH
  identity, not a build-provenance sentinel in the `is_test_build` mould — a sentinel would
  also be absent from every `cargo test` binary, so the guard would fire across the whole
  suite and need its own test-build exemption, making it impossible to prove end-to-end, and
  it would need `build.rs` machinery this crate has none of. It is also not a per-migration
  "released in" marker, which would refuse the very recovery this incident used — `make
  install` from `main`, carrying an unreleased migration on purpose. The scope is
  `StoreLocation::is_default()`, not `StoreOrigin`: the daemon always spawns its serving
  child with `--store-path` on its own argv, so inside the one process that ever migrates an
  existing store, origin is always `Flag`, never `XdgDefault` — an origin-keyed guard would
  never fire in production. `is_default()` is nearly inert *inside this repository's own test
  suite* for the opposite reason — `storyhook_test_support::TestEnv` mirrors the default
  layout under a fake `HOME`, the same gap `is_test_build`'s own refusal and
  `service::project::refuse_temp_project_in_real_store` already record — so what actually
  keeps ~70 fixture sites green is that they all open a **fresh** store (`from_version == 0`,
  which the guard always permits), not `is_default()` reading false. The one place in this
  tree where `is_default()` is true, a fixture plants a non-zero schema, and `$PATH` is
  deliberately shadowed is `plugin/claude-code/tests/run-tests.sh`'s decoy-`story` fixtures —
  safe only because that harness always creates its store fresh in the same run, which is why
  the fresh-store exemption is load-bearing for `make test` itself, not merely a convenience.
  Fail-open is deliberate and measured, not assumed: `$PATH` naming no `story` at all (a
  launchd-started daemon's plist carries no `PATH`) permits, and `tests/migration_guard.rs`
  pins that case rather than leaving it implicit. The refusal message deliberately does not
  point at `story update` — an unreleased migration has no release to update *to*, and that
  dead end is SH-405's own defect; repeating it here would ship it twice.
- Story IDs belong in commit **bodies**, never subjects — a subject reference makes the
  post-commit hook re-dirty the tree.
- Land your own work: merge commit, verify it landed, delete the branch. No direct pushes
  to `main`, no force-pushes, and no version bumps or deploys from a linked worktree.
- Deviations from the spec get recorded in the spec's own "As built" section — one
  document to open rather than two. (During the rearchitecture they went to
  `docs/rearch/STATE.md`, which stays the record for those nine waves.)
