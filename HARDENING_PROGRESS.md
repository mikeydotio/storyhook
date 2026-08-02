# Hardening run — 2026-08-02

Started 2026-08-02T00:21:48Z · 34 open stories at start · store holds 518 projects (505 junk)

Plan of record:
`/Users/mikey/.claude/plans/please-audit-the-dependency-majestic-hanrahan.md`

An autonomous run over storyhook's backlog: a dependency-and-priority audit
(done, PR #81), then **one story per context**, cleared by Freshen between them.
Every story gets a `## Log` entry below — successes, failures and skips alike.

---

## ▶ START HERE — you are resuming this run

You have no memory of the session that began it. Everything you need is in this
file and the plan above. Read the plan, then:

1. **Pick** the first unchecked story in the Phase 2 queue. Confirm it is ready
   (`story list --ready`). Skip **SH-112** — it is an epic and closes when its
   children do.
2. **Claim** it: `story move <id> in-progress`.
3. **Read it whole**: `story show <id>`, comments included. Several stories
   carry re-spec notes that contradict their own titles (SH-42, SH-43, SH-44,
   SH-109), and **SH-129 carries a complete council verdict — do not re-run
   that vote.** The comment always wins over the title.
4. **Work it.** Red→green TDD. Reproduce a bug with a failing test before
   changing code. Every fix ships its regression test. Two hats: a behaviour
   change and a refactor never share a commit. Doc comments on every public
   item. Warnings are errors.
5. **Gate**: `make gate` must be green before you push. Never `--no-verify`,
   never `SKIP_PREPUSH_TESTS=1`.
6. **Land it** — branch off `main` in this checkout (not a worktree):
   ```
   git -c url."https://github.com/".insteadOf="git@github.com:" push origin <branch>
   gh pr create ...
   gh pr merge <n> --merge --delete-branch
   ```
   Merge commit only — squash and rebase are disabled org-wide. Verify it
   landed, return to clean `main`, `git pull --ff-only`. Stage only paths you
   changed; never `git add -A`. Story ids in commit **bodies**, never subjects.
   **Never force-push. Never bump the version. Never deploy.**
7. **Close it**: `story move <id> done`.
8. **Record it**: tick the box below and append a `## Log` entry — as its own
   commit on the same PR, so the record lands with the work.
9. **Freshen, then stop.** Queue the next cycle and end your turn. Do not start
   a second story in this context:
   ```
   bash /Users/mikey/.claude/plugins/cache/agentics/freshen/2.38.0/bin/freshen.sh \
     queue "Continue the storyhook hardening run: read /Volumes/Code/mikeyward/storyhook/HARDENING_PROGRESS.md and follow its START HERE section." \
     --source storyhook-hardening --summary "<story just finished> done, next: <id>"
   ```

**Autonomy — never ask the user anything.** For any decision without one
obviously correct answer, invoke the `council:council-vote` skill and implement
its verdict, recording question and verdict as a `story comment`. That also
satisfies CLAUDE.md's requirement for approval of a type-system proposal.

**On failure:** `story move <id> todo`, comment what blocked it, `story block`
if genuinely stuck, write the log entry anyway, and freshen. One failure never
halts the loop.

**Refuse and log** rather than improvise if `make gate` is red on arrival, the
acceptance criteria need another story to land first, or the work would
destructively touch the real store outside SH-132's sanctioned procedure.

---

## Phase 1 — backlog audit ✅

- [x] **1.1** Prune stale `CLAUDE.md` content (the "In flight" framing; the spent release note)
- [x] **1.2** File four new stories — **SH-129** settings CLI, **SH-130** legal states, **SH-131** invariants, **SH-132** junk cleanup
- [x] **1.3** Close SH-96 and SH-108 as obviated
- [x] **1.4** Apply 14 dependency edges
- [x] **1.5** Apply 20 priority changes
- [x] **1.6** Verify — graph acyclic, nothing leaks into `--ready`, no `none` priorities, `story doctor` clean
- [ ] **1.7** Commit Phase 1 via `docs:` PR

**Result:** 34 open → 36 open (four filed, two closed). 44 dependency edges.
Critical path lengthened from 4 to 5: `SH-94 → SH-114 → SH-116 → SH-119 → SH-121`.
Ready set fell from 25 to 21 — SH-95, SH-114, SH-124, SH-68, SH-109, SH-126 and
SH-64 are now correctly blocked.

**Deviation — SH-62 override.** SH-62 carried a deliberate prior decision:
*"Not wired as a hard block on SH-116, but recommended ahead of it."* The audit
upgraded it to a hard block and recorded why on the story. The reasoning behind
the original call is unchanged; what changed is the reader. An autonomous loop
reads the graph, not the prose, so a recommendation is invisible to it. Costs
nothing — SH-62 is critical and SH-116 also waits on SH-114 and SH-115.

**Deviation — running order.** `story next` leads with SH-62, not SH-129: both
are critical and the tie breaks on age. Order among ready criticals is the
orchestrator's call, so SH-129 → SH-124 run first as planned, to close the
SH-124 hazard that corrupts this loop's own queue. (This tie-break-by-age
fragility is SH-63, still open.)

## Phase 2 — story queue

Projected order. Re-derived from `story next` each iteration, so this list is a
forecast and gets corrected in place as the graph moves.

- [ ] **SH-129** — project settings CLI · *gates SH-124 and SH-68; nothing can go first* · **design settled, see its council comment**
- [ ] **SH-124** — commit-sync transitions every mentioned story · *protects this loop's own queue*
- [ ] **SH-62** — positional verbs swallow unknown `--flags` · *SH-116 requires it first*
- [ ] **SH-125** — enforce the minimum state set
- [ ] **SH-130** — illegal state combinations + a supported purge · *hard-deletes SH-20 as its proving case*
- [ ] **SH-132** — delete the 505 fixture projects · *back up `store.db` first*
- [ ] **SH-131** — where the store-isolation invariants live · *before the epic churns `main`*
- [ ] **SH-115** — C3 Identity: remotes schema + one URL normalizer
- [ ] **SH-94** — concurrency_soak's load-sensitive 30s deadline · *gates SH-114*
- [ ] **SH-110** — tailnet bind flake · *gates SH-114*
- [ ] **SH-114** — C2 Transport: daemon-only
- [ ] **SH-116** — C4 Selection: `--project`, `STORYHOOK_PROJECT`, the refusal
- [ ] **SH-117** — C5 Verbs: `project new|list|delete|link|unlink`
- [ ] **SH-119** — C7 Subtraction: delete `project_paths` and the resolution walk
- [ ] **SH-121** — C10 Consequences: rewrite `worktree_truth.rs`, audit fixtures
- [ ] **SH-118** — C6 Ids: bare integers
- [ ] **SH-120** — C8 Dispatch plumbing
- [ ] **SH-50** — C9 Dispatch button + authorization review
- [ ] **SH-95** — retire the temp-path heuristic
- [ ] **SH-109** — prefix confirmation / `set-prefix` residual
- [ ] **SH-63** — `story next` nondeterminism
- [ ] **SH-64** — story-id ordering
- [ ] **SH-67** — export drops unknown event kinds
- [ ] **SH-68** — `sync.mode = auto` has no implementation
- [ ] **SH-65** — dead `AppError::SyncConflict`
- [ ] **SH-66** — `context --format json` double-encodes
- [ ] **SH-70** — pre-#18 import `[git]` comments
- [ ] **SH-122** — C11 Residual gap
- [ ] **SH-126** — WebUI Blocked column
- [ ] **SH-42** — project selector dropdown
- [ ] **SH-43** — archive
- [ ] **SH-49** — linked PRs
- [ ] **SH-44** — web form defaults
- [ ] **SH-127** — remove the status flash
- [ ] **SH-128** — column sort options

---

## Log

_Entries appended as work completes. Newest last._

### Architecture change — 2026-08-02, after the first attempt

The run began with **one subagent per story**. That is withdrawn.

The SH-129 subagent compacted mid-story. Nothing it wrote reached a commit and
the revert was clean — but the failure mode is what matters: an autonomous agent
holding destructive instructions (`git add -A`, force-push, version bump, merge
authority) inside a context I could not observe, where I had no way to tell
whether those prohibitions had survived the compact. Re-asserting the contract
by message treats the symptom; the design was wrong.

**Replacement:** work each story directly in the main context, where every step
is visible and interruptible, and use **Freshen** to clear between stories. One
story per context. The 80% context ceiling is retired — it no longer binds, so
the run ends when the queue empties rather than when a window fills.

Two rules relaxed by the change, deliberately: `HARDENING_PROGRESS.md` is now
safe to stage (one writer, not many), so each story's log entry rides in that
story's own PR as a separate commit.

### SH-129 — attempt 1 · killed and reverted · no PR

**Outcome:** killed mid-flight on Mikey's call, after a suspected context
compact made its correctness across the interruption unjudgeable.

**What was reverted:** 9 modified tracked files (`src/cli.rs`,
`src/help_topics.rs`, `src/invoke.rs`, `src/output.rs`, `src/service/mod.rs`,
`README.md`, and three test files) plus 3 new files (`src/service/settings.rs`,
`tests/project_settings.rs`, `tests/service_settings.rs`). Reverted with git,
not by hand. Nothing was committed, pushed, or merged, so `main` was untouched
and the branch was deleted unmerged.

**What survived, and matters:** the agent ran its council vote *before*
implementing, and that verdict is now a comment on SH-129 — three seats, two
rounds, unanimous on first preference. It settles the verb (`settings`, not
`config`, after Seat 1 verified the collision against the code and abandoned
their own naming), the grammar, dotted key names, and three-valued
unset-vs-default semantics. It also attaches five binding constraints found by
reading the code, the decisive one being a **corruption hazard**: `put_settings`
rewrites every column unconditionally, there are two call sites, and the one
that reads more naturally (`migrate.rs:416`) silently destroys a configured
github-sync document on the first `settings set`. **Attempt 2 starts from that
comment and does not re-run the vote.**

**Council:** yes — recorded on SH-129, audit trail in
`.council/project-settings-cli-surface/`.

**Deviation:** the agent honoured every prohibition it was given — it never
touched `HARDENING_PROGRESS.md`, never committed, never pushed. The kill was
precautionary, not a response to a violation.

**Discovered:** `.council/` and `.freshen/` were not gitignored, so a single
`git add -A` would have swept council transcripts and ephemeral signal files
into a commit. Fixed in the same PR as this entry.
