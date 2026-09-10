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

## As built: the plugin is part of the binary release

SH-538 closed the plugin-distribution row above by making the marketplace a
compile-time payload of `story`. `build.rs` generates an `include_bytes!` table
for both provider marketplace manifests and every regular file beneath
`plugins/story`, including whether each file is executable. A build refuses a
symlink, special file or unsafe relative path: a release binary without the
complete marketplace it promises is not a valid artifact.

`story plugin install <provider>` materializes those exact bytes under
`<storyhook-data>/plugins/<crate-version>/`. An installer lock serializes
concurrent materialization attempts; a same-parent staged directory is
verified before rename, an exact existing tree is reused, and a damaged
same-version tree is replaced with rollback if publication fails. The edit
guard records the stable
`<storyhook-data>/plugins` parent so retained older projections remain
installer-owned and immutable too.

Both provider registrations keep the stable `story@storyhook` identity, but
their source is replaced: plugin, then marketplace, are removed idempotently
before the versioned release projection is registered and installed. This
order is necessary because neither provider promises that adding an existing
marketplace name changes its source. The Codex launcher and sandbox rule remain
unversioned; they still resolve the provider's exact enabled cache version at
call time. The local release workflow delegates both `story plugin install
claude` and `story plugin install codex` to the newly installed binary instead
of registering its checkout, so neither provider retains an older release.

This deliberately chooses binary/plugin lockstep over plugin-only releases.
The rejected fifth release asset would preserve independent plugin delivery,
but adds a separately fallible artifact and a network fetch at install time;
Claude's marketplace CLI cannot pin a repository source to a release ref, so a
Git source cannot satisfy the shared contract. `story doctor install` reports
the resulting states explicitly: current release projection, stale managed
release, unpinned Git source, or checkout.

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

## As built: a failed registration is rolled back (SH-641)

Both provider installs are remove-then-add, for the reason the section above
gives: neither provider promises that adding an existing marketplace name
changes its source. Until SH-641 nothing stood between the two removes and a
later failure — `add marketplace`, `add plugin`, Codex's payload verification
or its sandbox rule — so the provider was left with no storyhook marketplace
and no plugin. SH-640's `story doctor install` names that state
(DEREGISTERED), but naming it still cost the operator a session with no
`/story` until they reinstalled by hand. The two siblings in `src/plugin.rs`
already rolled back (`materialize_release_marketplace` renames the previous
projection aside and restores it on a failed publish; the Codex sandbox step
snapshots and restores its two files); the registration was the odd one out.

`plugin::registration` (`src/plugin/registration.rs`) is the fix, and the one
parser `story doctor install` already used moved there with it:

- **Snapshot before the removes**, from the provider's own config file
  (`known_marketplaces.json`, `config.toml`), never by invoking the provider.
  Three named states: `Registered(source)`, `Unregistered` (no file, or no
  storyhook key — a fresh install), `Unreadable(reason)`.
- **Everything after the removes is one closure.** On failure, `undo` removes
  whatever the failed run added (the same tolerant `remove_*` verbs the
  install uses, prefixed "while restoring the previous registration" so the
  install-phase wording cannot contradict the message), then re-registers the
  previous source and re-adds the plugin from it. For Codex the closure
  includes payload verification **and** the sandbox step: a plugin whose
  skills exec a launcher that was just rolled back is half-installed, and
  because that step restores its own files the two rollbacks compose — files
  first, then registration — into exactly the previous state.
- **The error says which happened**, appended to the original failure, never
  in place of it: *re-registered the previous marketplace at `<source>`*;
  *re-registered the same release source … not verified* when the previous
  source is the release this run just failed to install (the same commands
  that failed are what put it back, and verification is a claim about this
  release's payload, so it is not re-run); *removed the partial registration*
  when nothing was registered before; *nothing restored: `<reason>`* when the
  config could not be read. A restore that fails reports **both** errors
  (SH-578: a diagnosis downstream of an unchecked failure names the wrong
  layer).
- **A failing remove still stops before anything is destroyed**, with no
  restore — the pre-existing behaviour, still pinned.

Two decisions taken on the way, and the reasoning kept:

- **An unreadable config never blocks the install.** A parser narrower than
  the provider's own format must not turn `story plugin install` into a dead
  end — the SH-404/SH-405 trap. `Unreadable` proceeds with nothing to put back
  and says why in any failure message (SH-372: absence states nothing and is
  never promoted to "there was nothing").
- **"Re-registered", never "restored".** Restore puts back the *source*; a
  git or checkout source may serve a different version now than the
  provider's cache held before. The note says what was actually done.

**Stated limit:** signals are not deferred across the window. SIGKILL between
the removes and the add is unrecoverable in-process, and SIGINT/SIGTERM are
not masked: doing so needs the provider children reset to default dispositions
so a hung provider stays interruptible, and its only test is a process-group
signal race that is load-sensitive (SH-347, SH-394). The window is sub-second
against a local directory source, SH-640's detector names the state it
leaves, and the next `story plugin install` *is* the restore.

**How it is proven** (`tests/plugin_install.rs`): the fake `claude` and
`codex` CLIs record a registration in the provider's real config shape on
`marketplace add` and clear it on `marketplace remove`, so the installer's
read of the previous registration is exercised against what it reads in
production (SH-364). Fail-once modes, backed by a marker file, break one step
after the removes exactly once, which is what makes a successful restore
observable; the pre-existing global fail modes double as the restore-also-
failed case. The matrix runs both providers over every step after the removes
with a previous registration seeded at a real directory, and the invocation
log is asserted in order: removes, add(new), removes, add(previous), plugin.
The success-path control counts each verb exactly once, so a restore that ran
on success would be caught. The note phrasings and the `undo` sequencing are
unit-tested over substituted verbs without a provider.

## As built: launcher dispatch preserves the installation (SH-588)

The installed-artifact guard distinguishes edits to the installation from
story, worktree and terminal operations. SH-585's reader-only launcher
exception explicitly rejected `dispatch`; this blocked the supported
`$story do` route even though dispatch does not edit the installed helper.

The exact, byte-verified Codex launcher also admits `dispatch` with one story
ID or `--next` and the helper's supported provider, model, effort, speed and
dispatch flags. Unknown/duplicate flags, invalid flag combinations, managed
file operands, altered or redirected launchers, interpreter flags and shell
composition retain their refusals. Catalog validation and readiness checks
remain in the helper. The hook returns an inert response, leaving execution
authorization to the host; other mutating helper verbs gain no exception.

`plugin_install::protect_launcher` reproduces the original denial and covers
the accepted/rejected command forms. Its installed-launcher integration runs
real dispatch, checks the persisted claim and created Git worktree, and compares
installed file bytes and modes before/after. Provider installation and terminal
I/O are the existing fixture doubles; dispatch behavior is production code.

The observed machine also had a stale 2.4.0 plugin with its 2.4.2 CLI. Updating
that projection alone cannot repair the checkout's dispatch refusal. Ship the
corrected hook in a release, then install its packaged plugin; do not patch a
cache file or create an installed-edit override.

## As built: the plugin's own helper is admitted by identity (SH-632)

SH-588's door was the byte-verified Codex launcher, and only that. Every other
host is told by `references/helper-command.md` to run
`<plugin-root>/bin/story.sh` directly — and `<plugin-root>` is always under a
managed prefix (`~/.claude/plugins/cache/storyhook/story/<ver>` for a user-scope
install, the release projection or the Codex cache when a session is launched
with `--plugin-dir`, which is what dispatch passes). So on Claude Code the
router's own `/story do`, `view`, `list`, `capture` and `doctor` were refused
before execution: the skill said run X, the hook said X is forbidden.

The helper is now a second admitted entry point with the same argv contract,
identified by **where the hook itself was loaded from**. A host runs one copy
of a plugin's hooks per session — Claude Code's own binary states that a
`--plugin-dir` plugin "overrides installed version" — so the `<plugin-root>` the
skill resolved and the hook's own `../` are one directory; `session-start.sh`
derives the same fact for the dispatch sentinel. The check, in the launcher's
shape but without a byte compare (the helper's bytes are trusted exactly as the
hook's own are — same installer, same directory): the spelled path is a proper
path beneath a managed prefix; it resolves to the `bin/story.sh` beside the
hook; no component below `HOME` (or, outside it, below the managed prefix's
parent) is a symlink — the launcher's own redirect rule; and it opens
`O_NOFOLLOW` as a regular file. Nothing is executed to classify. The root is
derived after the substring prefilter, so the inert path still pays for no
subshell, and assigned unconditionally, so an exported variable cannot name a
root on the hook's behalf (SH-411).

**Why not a stable Claude launcher.** The Codex launcher exists because Codex's
command rules match exact argv prefixes and the cache path is versioned; Claude
has no such rule. A Claude analogue would also need an identity for "the
enabled plugin", and the only record — `installed_plugins.json` — names the
user-scope cache while a dispatched session runs from the Codex cache via
`--plugin-dir`: keyed on that record, the door would refuse every dispatched
session, including the one that filed SH-632.

**Verbs.** `capture <id>` and `doctor` join the contract: one reads a pane, the
other runs `story doctor --json` (never `--fix`) plus a tmux probe window —
terminal and domain operations, never an installed file, the distinction
SH-588 drew for dispatch. Both adapters drop their `STORY_AGENT=<provider>`
prefix: an environment assignment is not an admitted form, and Codex's
`prefix_rule` would not match it either. `story plugin run codex` sets
`STORY_AGENT=codex` for the helper it runs when the caller did not, since the
launcher is Codex's own.

**Readers.** `ls`, `find`, `wc`, `stat`, `diff` and `cmp` join the inspection
vocabulary — the adapter table says "load the matching file from
`<plugin-root>/adapters/`", which needs a directory listing. `find`'s writing
and executing primaries are refused by name. `bash -c '…'` stays refused: it is
a second shell program, and the plain form is what the adapter needs.

**Tests.** `plugin_install::protect_helper` runs an *installed copy* of the
hook — written from the tracked tree by the fixture, because the door's whole
claim is about the hook's own location — from three roots under three managed
prefixes, and proves the tracked hook and every other root's hook refuse the
same helper even with identical bytes. The argv vocabularies are shared items
so both doors are tested against one grammar. Mutation-checked in both
directions: removing the own-root comparison fails two tests, removing the
redirect check fails one, removing `-delete` from `find`'s refusals fails one.

## As built: a lost registration is not "never installed" (SH-640)

`story doctor install` — the check `protect-install.sh`'s own header calls
authoritative — printed `claude plugin  not registered` and then `every
component agrees.` Both exits of `provider_row` that found no `storyhook`
marketplace (the provider's configuration file absent, or present without the
key) returned an unflagged row, so a provider whose registration had been
destroyed read identically to a machine that never had that provider. On
2026-09-09 that hid a lost Claude Code registration for about two hours across
eight autonomous sessions: a new session of that provider gets no `/story` at
all, and the one check built to say so said the opposite — SH-306's shape, a
gate's silence read as an all-clear.

**The evidence is what the install left on disk, not the manifest.** The story
proposed reading the managed-path manifest, whose Claude entries would prove a
Claude install had happened. They would not: `managed_paths()` names *both*
providers' prefixes and `record_managed_paths()` runs before the target is
dispatched, so a Codex-only machine's manifest names the Claude prefixes too,
and that rule would have flagged every such machine. What actually survived
the incident was the provider's own plugin cache —
`~/.claude/plugins/cache/storyhook/story/<six versions>` — while
`known_marketplaces.json`, `installed_plugins.json` and
`marketplaces/storyhook` all lost their storyhook entries.
`plugin::install_residue(target)` lists the storyhook-owned artifacts present
under that provider's home: for Claude the cache, the marketplace install
directory and the legacy layout, by existence; for Codex the cache by
existence, and the launcher and rule only while they carry the marker
storyhook wrote them with — an unmarked file at the same path is the user's,
exactly as `remove_managed_file` already reads it. Residue present is a
flagged `DEREGISTERED` row naming the copies and `story plugin install
<target>`; residue absent stays the quiet `not registered` the row exists for.
This is SH-372's rule for absence, one subsystem over: an absent key states
nothing on its own and is resolved against what the reader already holds.

**One definition, both ways.** `managed_paths()` now derives its provider
directories from the same per-provider list the doctor probes, so the hook
cannot protect a prefix the doctor is blind to; a unit test pins the file half
and any prefix added by hand.

**A deliberate uninstall must leave the doctor quiet**, or every machine that
ever uninstalled reads `DEREGISTERED` for ever and the flag stops meaning
anything. Claude Code's own `plugin uninstall` leaves its cache behind (six
versions had accumulated on the filing machine), and the fake Codex `plugin
remove` mirrors the real one. `story plugin uninstall` for either provider
now sweeps the residue *directories* the doctor reads; the Codex launcher and
rule keep their own marker-checked removal that preserves a user's file.

**What caused the loss is recorded as evidence, not settled.** The candidate
the story named — `install_claude`'s remove-then-add with no rollback — did
not run: `~/.claude/plugins/marketplaces/` and its `claude-plugins-official`
entry share the exact mtime `18:04:04`, twenty seconds before the first
`/story do` in that session's history; `installed_plugins.json` and the
`2.4.2` cache entry share `18:22:12`; the release root the registration
pointed at never went away; and the plugin helper makes no marketplace call.
That is a Claude Code marketplace refresh pruning the entry — a fact about the
host, which makes the detector the whole of the fix. The no-rollback shape
remains a real gap and is filed separately.
