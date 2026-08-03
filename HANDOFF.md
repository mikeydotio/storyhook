# Handoff — SH-117, C5: Verbs

*(Supersedes the SH-116 handoff. SH-116 is closed and merged as #98; so are
SH-114, SH-115, SH-94, SH-110.)*

The run itself is described by
[`HARDENING_PROGRESS.md`](HARDENING_PROGRESS.md) — read its **START HERE**
section first. That is the process; this file is only what the next story needs
on top of it.

**SH-117 is next and it is ready.** Its one blocker (SH-115) landed long ago.
No council has been convened for it, so it starts by *reading* rather than by
implementing a verdict.

## What SH-116 just changed under it

1. **`--project` is a global flag and `$STORYHOOK_PROJECT` is live.** Both are
   collapsed client-side into one `ProjectSelector` (`src/api/wire.rs`), which
   carries *how* the project was named so a bad slug can say whether the mistake
   is in the command or in the shell. `story project new` inherits all of this
   for free — it is project-*less*, so the flag is refused there and the
   variable ignored.
2. **`story project init` now registers the checkout's origin.** SH-117 retires
   `init`, so whatever replaces it must keep doing this or step 3 of resolution
   stops being reachable for new projects. `ProjectService::init` reads the
   origin *before* opening its transaction — a subprocess must never run inside
   `BEGIN IMMEDIATE`.
3. **`project_by_remote` and `link_remote` have a production caller at last**,
   so `project link origin <url>` is wiring an exercised path rather than a
   dormant one.

## Read SH-151 before designing `link origin`

Filed by SH-116 and it bears directly on this story. **`git config --get
remote.origin.url` walks up the directory tree**, so every storyhook project
inside one git repository reports the *same* origin. Consequences for SH-117:

- `project link origin` with **no URL** — which the story wants to work "when
  unambiguous, run from a checkout with a single origin" — is ambiguous in a
  monorepo in a way the story did not anticipate. Two projects in one repo both
  see one origin, and only one can hold it.
- SH-116 made `init` **skip** a collision rather than refuse, because refusing
  would make a monorepo's second project impossible to create. **`link origin`
  should refuse loudly instead**, and that asymmetry is deliberate: there the
  user typed the URL and is owed an answer about it. The reasoning is recorded
  on SH-116 and in `service::project::claimable`'s doc comment.

## Ground worth measuring before designing

- `story project new`'s interactive questionnaire has no precedent in this
  codebase — nothing else prompts. `main.rs::confirm` is the only interactive
  code there is, and it refuses under `--json` and with no terminal. Whatever
  `new` does should reuse that refusal shape rather than invent a second one.
- The **naming hazard in the story is real**: `story link`/`unlink` already
  exist as top-level aliases for `relate`/`unrelate`. Check `src/cli.rs`'s
  `"relate" | "link" =>` arm before touching the parser.
- `story init`, `deinit` and `relink` appear in `src/cli.rs`, `src/help_topics.rs`,
  `README.md`, the plugin's CLI reference and `tests/`. Retiring them is a sweep,
  and `tests/golden_cli.rs`'s frozen snapshots will move.

## Three things that bit during SH-116

- **A test can pass for the wrong reason and look green.** Two of mine did. One
  set `url.<base>.insteadOf` **backwards** — that form rewrites urls *starting
  with* the prefix, so it never applied and the test would have passed under the
  invocation it was written to reject. The other asserted exit 2 on a refusal
  that today comes from `unknown command`. Assert the *reason*, not just the
  outcome, and add a premise assertion when a fixture has to be in a particular
  state for the test to mean anything.
- **`make test` is the whole gate.** `make test-daemon` and `make gate` do not
  exist. START HERE said otherwise until SH-116 corrected it.
- **`spawn_inventory` will fail if you add a `Command::new`** anywhere in `src/`
  without classifying it. That is deliberate, and its failure message explains
  the taxonomy.

## Gate

`make test`, supervised in the background with **log growth as the heartbeat**
and a 120-second stall bound. SH-116 took six attempts to green and every
failure was a guard doing its job — `cargo fmt`, a lock race, two test premises,
and the spawn inventory. Budget for that rather than being surprised by it.
