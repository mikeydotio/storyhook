# Dogfooding at the release level

The design of record for how storyhook tooling reaches a machine, and why every
part of it arrives together or not at all. Settled on SH-530.

Before this, storyhook's "local installation" was not one thing. It was five,
each with its own independent path from a working tree to production:

| Component | How it arrived | What it tracked |
|---|---|---|
| the `story` CLI | `make install` | whatever tree was checked out |
| the daemon | the same binary | already in lockstep — `DaemonInfo::is_this_binary` |
| the Claude/Codex plugin | `claude plugin marketplace add "$repo_root"` | **the live checkout directory** |
| the store schema | any binary that opened the default store | whichever binary ran last |
| git hooks, the launchd plist | whichever binary ran `story hooks install` | ditto |

## The incident this is written against

Measured on the filing machine, 2026-09-01:

- `story --version` reported `story 2.2.0 (build 52dd2acb2502)`. That build id
  is the tracked tree of commit `45fde9bd` — 332 commits past the `v2.2.0` tag.
  The binary called itself 2.2.0 and was not v2.2.0. Only SH-406's build stamp
  made that discoverable; the semver alone said nothing.
- `v2.2.0` supports schema 18. The store was at 21. `origin/main` carried 26.
  **The newest published release could not open the machine's store**, so the
  tracker was openable by exactly one binary in existence — an unreleased local
  build — and `story update` would have taken it down in order to fix it.
- The moment was still on disk: a pre-migration backup named `…-v18.db`, at
  `user_version` 18, dated 2026-08-28, with the next snapshot at 21. One
  `make install` carried the production store from the release level to a level
  no release supports, silently and one-way.
- `~/.codex/config.toml` pointed the storyhook marketplace at
  `source = "/Volumes/Code/mikeyward/storyhook"`. The Codex plugin was a live
  view of the checkout: every merge, every checkout, every uncommitted edit.
- The three plugin manifests declared `0.6.0+codex.20260823221659` against
  `Cargo.toml`'s `2.2.0`, and nothing required them to move together.

Under `story help priority-rubric`, "leaves the data unopenable — AND does not
say so at the time" is the definition of `critical`.

## Why `migration_guard` did not catch it

SH-404 built the write-side guard for exactly this shape and it could not fire.
`decide` permits when the running executable **is** the `story` that `$PATH`
resolves — which is precisely what `make install` arranges. The guard was built
for a worktree's debug binary; the incident came from the installed one.

## Read-only degradation (the cure, not the fence)

An incompatible store now opens **read-only** rather than not at all.

The asymmetry that makes this possible: a newer schema's *additions* do not stop
an older build reading the columns it already knows, but a newer schema's
*invariants* are ones that build has never heard of and cannot maintain. Reads
degrade; writes refuse with `AppError::ReadOnlyStore`, exit 11.

The probe is a **capability** test, never a version distance. "More than N
versions newer is too far" would be an unfounded constant of exactly the kind
this project forbids elsewhere; the question that actually matters is whether
this build can still read the store, and that is answerable directly by
preparing a `SELECT` over `read::STORY_COLUMNS` — production's own column list,
so there is no second copy to drift. A newer storyhook that only added to the
schema degrades; one that restructured what this build reads still earns the
honest `SchemaTooNew` refusal.

**The degrade is only defensible because it is loud.** A newer migration can
change the *meaning* of an existing column rather than only adding one —
SH-372's `priority_assessed` and SH-359's `kind` predicate are both precedents
in this repository — so a degraded read can be wrong rather than merely
incomplete. Under this project's damage axis that is a wrong answer someone acts
on (rung 3) replacing a loud refusal (rung 4), and the trade is only worth
making if the reader is told. Degraded-and-silent would be a regression.

### Delivering the warning, and two things that had to be measured

`open_store` runs inside the **daemon**, so its stderr is the daemon log rather
than the terminal the command was typed into — the SH-306 shape exactly. The
notice therefore travels back over `/api/v1/invoke`. Two mechanisms were tried
and refuted by running them:

- A notice recorded where the condition is detected fires for **no request at
  all**: the daemon opens its store once, at startup. `rpc::degraded_notice`
  asks the store that actually served the request instead.
- A thread-local buffer on the client is written on a thread `main` never reads,
  because `HttpInvoker::exchange` runs the exchange on its own thread. The
  buffer is process-global, which makes no claim about threads that a future
  refactor can quietly falsify.

Placement follows the two contracts already in force: the notice goes **after**
the error on plain-text stderr, so stderr still begins `error: `, and under
`--json` it rides the success envelope, because stderr must stay empty there
(SH-59). The `--json` *error* envelope is deliberately untouched — its key set
is a pinned contract, and an error there already carries its own explanation.

## What is settled, and what is filed

SH-530 lands the part that cures the acute damage and puts the guardrails in.
The release-channel rework is filed as children rather than adopted, on this
project's own scope rubric — "too large to land in one story even with the room
to try … work that needs its own design review":

| Landed on SH-530 | Filed |
|---|---|
| an incompatible store degrades to read-only, loudly | a prerelease (`vX.Y.Z-beta.N`) channel, and `story update --channel` |
| one version across the binary and every plugin manifest | `release.sh` installing the CI-built asset instead of rebuilding locally |
| a `PreToolUse` hook refusing edits to the *installed* copy | the plugin payload travelling inside the binary |
| `story doctor install` — the installed set, and what is pending | tightening `migration_guard` once a beta channel exists to recover through |

## Rules this establishes

- **A refusal that leaves a tracker unopenable is a last resort, not a default.**
  Prefer degrading to a narrower capability and saying so. `SchemaTooNew`
  survives for the case where reading itself is guesswork.
- **A warning about the store must reach the person, not the daemon log.**
  Since SH-114 the store is only reachable through the daemon, so anything
  detected there needs a route back over the wire. Printing where you detect is
  the SH-306 shape.
- **A change to a release artifact belongs in the checkout, never in the
  installed copy.** The installed copy is overwritten by the next
  `story plugin install`, so an edit there is lost as well as unversioned —
  which is why the hook that refuses it names the checkout file rather than
  merely saying no.
- **`make install` stays ungated.** `StoreError::SchemaTooNew`'s own message
  prescribes building from source as the recovery, and `tests/store_migrations.rs`
  and `tests/corruption_recovery.rs` assert that it does. A guard there would
  make the store's own advice a dead end — the trap SH-404's module doc
  documented and SH-405 was filed for.
