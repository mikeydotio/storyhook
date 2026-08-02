# Hardening run — 2026-08-02

Started 2026-08-02T00:21:48Z · 34 open stories at start · store holds 518 projects (505 junk)

Plan of record: `~/.claude/plans/please-audit-the-dependency-majestic-hanrahan.md`

An autonomous run: a backlog audit, then one subagent per story in `story next`
order, until the queue empties or the orchestrator's context reaches 80%. Every
story gets a `## Log` entry below whether it succeeded, failed, or was skipped.

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

- [ ] **SH-P** — project settings CLI · *gates SH-124 and SH-68; nothing can go first*
- [ ] **SH-124** — commit-sync transitions every mentioned story · *protects this loop's own queue*
- [ ] **SH-62** — positional verbs swallow unknown `--flags` · *SH-116 requires it first*
- [ ] **SH-125** — enforce the minimum state set
- [ ] **SH-S** — illegal state combinations + a supported purge · *hard-deletes SH-20 as its proving case*
- [ ] **SH-J** — delete the 505 fixture projects · *back up `store.db` first*
- [ ] **SH-C** — where the store-isolation invariants live · *before the epic churns `main`*
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
