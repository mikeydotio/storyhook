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
- [x] **1.7** Commit Phase 1 via `docs:` PR — merged as #81

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

- [x] **SH-129** — project settings CLI · *gates SH-124 and SH-68; nothing can go first* · **design settled, see its council comment**
- [x] **SH-124** — commit-sync transitions every mentioned story · *protects this loop's own queue*
- [~] **SH-62** — positional verbs swallow unknown `--flags` · *SH-116 requires it first* · **in flight**
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

### SH-129 — attempt 2 · done

**Outcome:** merged. `story project settings list | get | set | unset` exists,
and the setting the story was filed about now reaches the code that reads it.

**Built to the council verdict, without re-running the vote.** The comment on
SH-129 settled the verb, the grammar, the key names, the three-valued
`source`, the typed response and the error kinds; all of it was implemented as
written. The five binding constraints were treated as acceptance criteria:

1. **The corruption hazard** — `set` and `unset` read the whole settings row,
   change one field and write it back inside a single `store().write(|tx| …)`
   closure. Pinned by two tests that configure a github-sync document, run an
   unrelated `set` and an unrelated `unset`, and assert the stored
   `serde_json::Value` is unchanged. The wrong pattern (`migrate.rs:416`)
   was never reached for.
2. **Duration validation** — `parse_duration` is filtered through a
   strictly-positive check, because it parses its count as `i64` and answers
   `Some` for `0d` and `-14d`. `0d`, `-14d`, `0h`, `14`, `14x`, `d`, empty and
   whitespace are all `Validation`.
3. **Feature-flag invariance** — `github.sync` reports opaque presence
   (`configured`), never a parsed shape. Every test that touches it writes the
   document *through the store* rather than through `GithubSyncService`, so the
   file has no `#[cfg(feature = …)]` and asserts the same thing in both builds.
4. **The rollback gap** — reported, not fixed. `tests/migrate_round_trip.rs`'s
   header now says why the gap stopped being benign and names SH-133 (already
   filed) with the three places a fix has to touch. The export document was not
   widened: `golden-export.json` is compared literally and mixing that with a
   feature would be two hats in one commit.
5. **The annotation is derived** — "no command reads this yet" is a field on
   the registry row, carried through `SettingView.note` to the renderer.
   Deleting that one line is the completion criterion for whoever makes
   `story doctor` read the value; nothing at a render site has to change.

**The registry is the design, as approved.** `static REGISTRY: [SettingSpec; 3]`
is read by both the renderer and the write dispatcher, and `settable()` is
*derived* from `managed_by` rather than stored beside it — two fields can
disagree, one cannot. There is no match on key names anywhere.
`every_registry_entry_agrees_with_its_dispatch` iterates the registry rather
than naming three keys, so a fourth added later inherits the check.

**33 new tests**, all green, plus the contract suites they oblige:
`tests/service_settings.rs` (17) and `tests/project_settings.rs` (16). The
headline one is not a round trip through a column — it turns the setting off,
runs `story commit-sync` over a commit whose body names a story, and asserts
the story did **not** move. Its control asserts that it does when the setting
is untouched.

**Deviations:** none from the verdict. Two small calls it did not cover:
`get` renders the same line as `list` rather than a bare value, because the
inertness and read-only annotations are the load-bearing part and a script has
`--json`; and a boolean is accepted case-insensitively but stored canonically.

**Wire contract:** `Response::ProjectSettings(Vec<SettingView>)` is a permanent
addition, so `tests/wire_envelope.rs` grew a populated corpus entry covering
all three `SettingSource` values and both sides of `settable`, an empty one,
and four `SettingsAction` invocations. `make gate` — both legs — is green.

**Council:** not re-run. The verdict on the story was the input, as instructed.

### SH-124 — done

**Outcome:** merged. `commit-sync` still links every story a commit names; only
a commit that **claims** one moves it. The trailer shape that started this —
`Refs CAL-21, CAL-28, CAL-29` — now changes no state at all.

**Part 1 was already done.** The story asked for two things, and SH-129 had
delivered the first the day before: `sync.auto_transition` is reachable from the
CLI. So this was part 2 alone — separating a mention from a claim.

**The type was the bug, and that framing came from the council.**
`extract_story_ids` returned `Vec<String>`, which cannot say "named, but nobody
claimed work". `commit_sync` therefore had no way to tell a cross-reference from
a claim of ownership; the defect was *expressible* only because the return type
threw the distinction away. It is replaced by `scan_story_refs ->
Vec<StoryReference { id, intent }>`, and only a `ReferenceIntent::Claim` may
move a story. `extract_story_ids` is deleted, not deprecated — zero callers
remained.

**Not a new promise.** `story help commit-sync` and the plugin CLI reference
have *always* said commit-sync "auto-transitions stories based on commit
patterns (e.g. `closes SH-1`)". The keyword half had simply never been built, so
this aligns code to published contract rather than changing the contract.

**The grammar, and the one decision the council was split on.** A claim is a
claim word immediately before the id on the same line, through one of two
separators: whitespace (`Closes SH-1`, claims unconditionally) or a colon
(`Closes: SH-1`, claims only when the id run is the whole remainder of the
line). The asymmetry is git's own — `git-interpret-trailers` defines a trailer
as `token: value` whose value runs to end of line — so the colon is honoured
only where it means "trailer key". That single rule accepts `Closes: SH-1` and
rejects `fix: SH-12 broken parser`, where the colon is a Conventional Commits
type. It is the rule that dissolved a three-way split rather than splitting the
difference.

**Council: yes — and it worked as designed.** Round one was a *perfect*
three-way split, with two of three seats voting against their own proposal. One
deliberation round converged it, and the runoff was unanimous. The seats
corrected each other on facts I then verified myself against this repo's last
400 commits:

- `Closes SH-123. Refs SH-113, SH-112.` is a real commit line here, which kills
  a *global* whole-remainder rule and is why it qualifies the colon tier only.
- `Story: SH-107` appears exactly 10 times — colon trailers naming stories are
  live practice, under a non-claiming key, so `story` is pinned as never-claiming.
- `fixed SH-20`, `completed SH-27` and `start SH-41` are genuine mid-prose
  claims, so position stays unanchored. That last one killed the two narrower
  9-keyword proposals: they would have silently missed a claim shape this repo
  actually uses.
- 274 of 400 commits use a *scoped* Conventional Commits type against 37 bare —
  so the losing separator rule would have made intent depend on whether an
  author typed a scope.

Audit trail in `.council/commit-sync-mention-vs-claim/`; the verdict is a
comment on SH-124.

**The suite was pinning the defect as correct.** This is the part worth
remembering. `tests/project_settings.rs::repository_naming_a_story` built its
fixture commit body as literally `Refs {id}` — the exact trailer shape that
caused the harm — and the test above it asserted that body **must** move the
story, documented as "the control". Nobody asked whether `Refs` was the right
example to bless, because the code could not tell a reference from a claim, so
the test could not either. The premise is rewritten, not relaxed, and gains a
counterpart asserting a mention leaves the state alone *while the setting is
still on* — which is what distinguishes the two switches.

**Red before green, and the red matched the prediction.** The QA seat's verified
count was 7 failing tests. Running the suite produced exactly 7: three in
`service_git.rs`, two in `project_settings.rs`, two in `hook_execution.rs`. Two
more surfaced in `tests/story_sync_git.rs`, which no seat had enumerated — both
testing active-state *resolution* rather than the grammar, and both fixed by
giving them claiming commits.

**Four tests would have stayed green while losing coverage**, and were fixed for
that reason rather than because they failed: `commit_sync_fires_no_event_hooks`
(no state change left to suppress), `a_pinned_clock_stamps_every_event_the_run_writes`
(its loop would stop seeing a `StoryStateChanged`, so it now asserts one exists),
`the_project_setting_can_turn_the_transition_off` (would have passed on the
grammar instead of the setting it names), and
`a_story_already_out_of_the_default_state_is_commented_but_not_moved` (whose
body said `more work on {id}` — the word before the id is `on`, so it proved
nothing about the state the story was in).

**Three drift invariants**, because a grammar in two places diverges:
`REF_WORDS` is one static table read by the matcher and iterated by a test that
checks all 33 rows in *both* directions;
`every_keyword_the_merge_hook_closes_on_also_claims` reads the post-merge hook's
own alternation out of `src/hooks.rs` rather than restating it, since closing
implies working and a merge must not close a story commit-sync never saw active;
and `a_claim_is_constructed_in_exactly_one_module` keeps the grammar in
`domain.rs`, in the shape of the existing `invoker_seam.rs` source grep.

**The report names what it declined to move.** Promoted from a mitigation to
part of the design, because without it a project whose commits use no claim word
cannot tell "off" from "broken" — and the fix would have had the same silent,
accumulating shape as the defect it removes.

**Deviations:** none from the verdict. `sync.auto_transition` is untouched —
still `Option<bool>`, still default on, still `SettingKind::Boolean` — so no
schema migration, no settings migration, no wire change. Semver is **minor**,
not patch: no interface changed, but commit habits now determine outcomes.

**Accepted limitations, documented rather than patched** (all fail in the
under-claiming direction, which the report makes visible): annotated colon
trailers (`Closes: SH-1 (partial)`) do not claim; `fix: SH-1` with a bare id as
the entire description does claim, and is genuinely undecided rather than wrong;
the negation list is a five-word one-way valve, not comprehension; a re-revert
under-claims.

**Gate:** `make gate` — both legs — exits 0, 192 green test-result blocks,
clippy clean. Note for whoever runs this next: `concurrency_soak`'s
`readers_run_through_a_write_storm_without_seeing_a_partial_story` failed on the
*baseline* run before any change, and passes in isolation in 6.4s against a 30s
deadline. That is SH-94 exactly, still open, still load-sensitive.

### SH-62 — in flight

**Outcome:** implementation complete, gate running, not yet merged. This entry
is written as the work happens rather than after it, and its last section is the
part still open.

**The story understated its own extent, and measuring first is what found it.**
SH-62 reported `story new --typo x`. Running the real binary against an isolated
scratch store showed **eight** write-capable verbs accept a flag-shaped token
and exit 0 — four of which mint a durable object nobody asked for (`new`,
`epic create`, `type add`, `member add`) and four of which attach junk to a
write the user did ask for (`comment`'s body, `block`'s reason, `delete`'s audit
reason, and a literal `--typo` label). The probe script is
`scratchpad/probe.sh`; the table is `.council/cli-unknown-flag-refusal/CHAIR-EVIDENCE.md`.

**The council was convened because two defensible rules collided**, and it
mattered: `src/cli.rs` carried a deliberate prior decision (from SH-124's
neighbourhood) that comment free-text is unrestricted and a comment beginning
with `--` must never fail as an unrecognized flag. A blanket refusal overturns
that. Three seats, two rounds, **2-1 for the fail-closed flag-shape gate**.

**The winning idea is shape, not prefix.** A token containing whitespace is
never flag-shaped, so `story new "--fix the ingest path"` is one argv element
with spaces in it and stays data. That single clause is what let the rule close
all eight defects *without* gutting the free-text guarantee: only the unquoted
single-token form changes. A repo-wide search for that form across `plugin/`,
`bin/`, `scripts/`, `README.md`, `docs/` and every `.md`/`.sh`/`.rs`/`.toml`
returned exactly one hit — SH-62's own defect description in
`docs/rearch/STATE.md`. The form being withdrawn is one nobody uses.

**Fail-closed is the actual defect-CLASS fix.** A verb with no table entry
refuses every flag-shaped token, so forgetting to declare a new verb's flags is
loud at test time rather than a silent re-inheritance of this bug. That property
is why both non-authors voted against their own proposals: per-site rules fail
*open*, and this defect has already recurred once — SH-52 was the same shape for
`--help`, fixed one token position at a time, and SH-62 is the rest of the flag
space arriving two waves later.

**The suite was pinning the defect as correct — again.**
`move_if_state.rs::move_with_typoed_flag_name_immediately_after_state_is_comment_not_error`
required `story move SH-1 in-progress --if-stat todo` to *succeed*: the typo
became comment text and, in its own words, "the move proceeds unconditionally".
So a user who asked for a compare-and-swap guard silently did not get one and
the story moved anyway. The old reasoning was right that a typo must never be
*mistaken* for the real flag — that would be the worst of three outcomes — but
it blessed the second-worst instead of the correct third: refuse, and name the
token. The premise is rewritten, not relaxed. This is the second consecutive
story in this run where the test suite documented the defect as intended
behaviour; SH-124's `Refs {id}` fixture was the first.

**Three test files changed for that reason rather than because they broke
badly**: `story_update.rs` (two tests asserting the literal string
`usage: story update`), `service_migrate.rs` (`an_unknown_flag_is_a_usage_error_rather_than_a_path`,
whose premise was already right and only its expected text stale), and four
frozen `golden_cli` error snapshots. All four snapshot entries improved in the
same direction — `story list --no-such-flag` now names the token instead of
printing a twelve-flag usage line — and no exit code moved.

**Deviations from the verdict, both recorded on the story:**

1. The council sequenced the table as a behaviour-neutral commit ahead of the
   wiring. Rust makes that impossible without `#[allow(dead_code)]`: an unwired
   gate is dead code and `-D warnings` rejects it. The gate and its wiring
   therefore land together; the two-hats split is preserved by the *refactor*
   commit that precedes both.
2. The error message does not synthesize an example command. The verdict's
   draft offered `story doctor "--typo …"`, but half the verbs this fires on
   take no positional at all, so that example would not work — worse than none,
   because a reader would try it.

**Also filed:** SH-134, `add_type` accepts an unaddressable slug. All three
seats endorsed it unprompted as a *domain*-origin defect the parser gate cannot
substitute for — the gate stops `story type add --typo` but not the TUI,
`import`, `decompose`, `migrate`, the web API or a direct `InvokeRequest`, and
it would never catch `in review`, which is invalid for the same addressability
reason while not being flag-shaped at all.

**Still open at the time of writing:** `make gate` both legs, the PR, and the
merge. Baseline on arrival was green locally and red on the daemon leg through
SH-110 alone (`web_serve_binds_tailnet_ip_when_available`, a sibling of the test
in SH-110's title, passing in isolation in 0.09s — recorded on that story, which
widens it from one test to a shared expectation).
