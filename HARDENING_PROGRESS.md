# Hardening run — 2026-08-02

Started 2026-08-02T00:21:48Z · 34 open stories at start · store holds 518 projects (505 junk)

Plan of record:
`/Users/mikey/.claude/plans/please-audit-the-dependency-majestic-hanrahan.md`

An autonomous run over storyhook's backlog: a dependency-and-priority audit
(done, PR #81), then **one story per context**, cleared by Freshen before executing each.
Every story gets a **`story comment`** recording how it went — successes,
failures and skips alike.

This file is the **procedure only**. The per-story record used to live below it
as a `## Log` section; it is now written on each story, which is where anyone
asking about that story already looks. The 189 entries written before the change
are in this file's git history, and the stories themselves carry their own
comments from here on.

---

## ▶ START HERE — you are resuming this run

You have no memory of the session that began it. Everything you need is in this
file and the plan above. Read the plan, then:

1. **Pick**: run `story next` (`story next --count 3` if you want a couple of
   options rather than one). As of 2026-08-11 this run dogfoods `story next`
   instead of a hand-maintained checklist — see **Dogfooding `story next`**
   below for why and what to watch for. `next` already excludes anything
   closed, blocked, awaiting an answer, a draft, or carrying a child (an epic
   never surfaces — `!has_children`, tested in `tests/story_next.rs`), so
   there is nothing left to hand-check before claiming. If it returns nothing,
   the queue is genuinely drained — stop and report that rather than
   inventing work.
2. **Claim** it immediately: `story move <id> in-progress`. Claim before you
   read the rest of it — that narrows the window in which a second session,
   running `story next` at nearly the same moment, could be handed the same
   story before either of you claims it. `next` cannot close that race by
   itself (nothing can, without a lock neither session asked for); claiming
   fast is the mitigation.
3. **Read it whole**: `story show <id>`, comments included. A story's comment
   thread can carry a re-spec or a council verdict that supersedes its own
   title or original description — the comment always wins over the title.
4. **Work it — in the story's own worktree**, never on a branch in this
   checkout (see **One worktree per story** below). Red→green TDD. Reproduce
   a bug with a failing test before changing code. Every fix ships its
   regression test. Two hats: a behaviour change and a refactor never share a
   commit. Doc comments on every public item. Warnings are errors.
5. **Gate**: `make test` must be green before you push. Never `--no-verify`,
   never `SKIP_PREPUSH_TESTS=1` for a change that touches code (a docs-only
   push may bypass per CLAUDE.md's own carve-out — confirm nothing but docs
   changed first). **`make test-daemon` and `make gate` no longer exist** —
   SH-114 collapsed the two transports into one, so there is one leg and
   `make test` is the whole gate. Run it as a supervised background command
   with a log-growth heartbeat — see **Supervising background work** below;
   this is the rule's most frequent application.
6. **Land it** — push and open the PR **from the worktree**, then merge **from
   this checkout**:
   ```
   # in .claude/worktrees/<id>:
   git -c url."https://github.com/".insteadOf="git@github.com:" push origin <branch>
   gh pr create ...
   # back in /Volumes/Code/mikeyward/storyhook:
   gh pr merge <n> --merge
   ```
   Merge commit only — squash and rebase are disabled org-wide. Verify it
   landed, return to clean `main`, `git pull --ff-only`. Stage only paths you
   changed; never `git add -A`. Story ids in commit **bodies**, never subjects.
   **Never force-push. Never bump the version. Never deploy.**
7. **Record it**: `story comment <id> "<how it went>"` — no `## Log` entry and
   no checklist box (see **Dogfooding `story next`**); `story next`/`story
   summary` are the live status. **Record before closing**, so a reader who
   opens the story never finds a `done` with no account of what happened.

   Say what shipped, what was decided and why (link the council directory when
   one was convened), what was filed rather than folded in, any deviation from
   this procedure, and any gate trouble — including how long a wedge lasted.
   The bar is unchanged: enough that someone with no memory of the session can
   tell what happened and why, without reading the diff.

   **This costs one PR per cycle, not two.** The record used to land as its own
   docs commit on a separate PR after the code PR merged — the
   SH-174/SH-180/SH-181 pattern. A comment is not a tracked file, so there is
   nothing to commit and nothing to land: a cycle is one PR now.
8. **Close it**: `story move <id> done`.
9. **Freshen, then stop.** Queue the next cycle and end your turn. Do not start
   a second story in this context:
   ```
   bash /Users/mikey/.claude/plugins/cache/agentics/freshen/2.38.0/bin/freshen.sh \
     queue "Continue the storyhook hardening run: read /Volumes/Code/mikeyward/storyhook/HARDENING_PROGRESS.md and follow its START HERE section." \
     --source storyhook-hardening --summary "<story just finished> done, next: <id>"
   ```

**One worktree per story — created for it, torn down after it.** Added
2026-08-14 at Mikey's direction; it supersedes the earlier "branch off `main`
in this checkout" instruction wherever the two disagree, including in log
entries written before that date. Every story's code, and the story's log
entry too, is written in a fresh linked worktree:

```
git pull --ff-only                                   # in this checkout first
git worktree add .claude/worktrees/<id> -b <branch>  # gitignored path
```

Work, gate and commit there; push and open the PR from there. Then **come back
to this checkout to merge** — `gh pr merge` from inside a linked worktree
fast-forwards local `main` underneath it and can strand it on a branch that no
longer exists. Merge here, `git pull --ff-only` here, and only then tear the
worktree down:

```
git worktree remove .claude/worktrees/<id>   # --force if it holds a target/
git branch -d <branch>
git push origin --delete <branch>
git worktree list                            # confirm it is gone
```

Two things this costs, both accepted: a worktree carries its own `target/`, so
its first `make test` is a cold build of the whole tree, and the checkout's
`target/` no longer warms the next story's. What it buys is a checkout that is
never mid-story — no half-finished branch to inherit, no build artifacts from
work that was abandoned, and a torn-down worktree as positive evidence that a
cycle finished rather than a branch left lying around.

**Remove only the worktree this cycle created.** Thirteen others from earlier
`/story do` sessions live under `.claude/worktrees/`; any of them may be a live
tmux session, and `tmux list-windows -a` is the check before touching one. They
are not this run's to clean up.

**Dogfooding `story next`.** This run drove off a hand-maintained checklist
(below, now removed) through 2026-08-11. It was already showing the failure
mode a live query cannot: SH-226 found a checklist box marked `done` for a
story nothing had ever touched — closed by a shell executing a runaway
dispatch charter, not by an agent — because the box trusted what it was told
rather than what the store actually held. `story next` has no state of its
own to drift from a query it runs fresh every time; the checklist's ⚠/⏸
bookkeeping (in-progress-elsewhere, awaiting-Mikey) is now just the real
`in-progress` state and `story block <id> "<reason>"`, both of which `next`
already reads live.

**Watch it while you use it — this run now doubles as its test.** If `story
next` ever recommends something unworkable — surfaces a story with children,
hands back one already claimed elsewhere, ignores a real blocker, or
otherwise stalls the queue instead of draining it — that is a defect in
storyhook itself, not a one-off to route around. **File it `critical`
priority immediately**, whatever else is in flight: a tool this run depends
on to find work, quietly recommending the wrong work, is exactly the
silent-failure shape this file exists to catch, and every session downstream
inherits it until it's fixed. File it, note the finding in this cycle's
`story comment`, then pick again — a bad recommendation is a reason to file a
story, not a reason to work the bad recommendation anyway.

**Supervising background work — never start it without a watchdog.**

This rule exists because a `make gate` run wedged in its daemon leg and sat
there for **eight hours and twenty-one minutes** before anyone looked. Nothing
was broken; the leg passed in two minutes when re-run alone. The whole loss was
supervision. A stall is the worst failure a background task can have — no
failing test name, no error, no completion notification, because a notification
only fires when work *ends* and a wedge never ends. **Waiting for a notification
is not supervision.**

Every background task — a `run_in_background` shell command, or a subagent —
gets all four of these before it starts:

1. **A heartbeat you can observe.**
   - **Subagents:** instruct them, in their prompt, to send you a progress line
     via `SendMessage` after **every tool call** — one sentence, **under ten
     words** ("Read migrate.rs, found apply()", "Ran clippy, three warnings").
     Terse on purpose: it is a pulse, not a report.
   - **Shell commands:** they make no tool calls and cannot narrate themselves.
     Redirect to a log file and treat **log growth** as the pulse:
     `wc -c < log` is the heartbeat.
2. **A stall timeout, chosen before starting.** Not the expected total runtime —
   the longest plausible *gap between signs of progress*. For this suite that is
   **120 seconds** (a single test prints its own "running for over 60 seconds"
   notice, so a shorter bound cries wolf).
3. **Polling on that timeout.** Check, compare against the last observation, and
   keep checking. Do not end a turn expecting to be woken.
4. **Kill and restart on silence.** If the timeout lands and the pulse has not
   moved — no new subagent message, no new log bytes — the task is wedged. Kill
   it, clean up after it (`scripts/check-no-orphan-servers.sh`; a killed run
   leaves daemons that make the *next* run refuse to start), and restart it —
   preferably a narrower slice, which is both faster and more diagnostic. Two
   consecutive wedges of the same slice is a finding: stop, and log it.

Every wedge and restart goes in the story's `story comment`, including how long
it was wedged. That number is the only thing that makes this rule feel worth
obeying next time.

**Autonomy — never ask the user anything.** For any decision without one
obviously correct answer, invoke the `council:council-vote` skill and implement
its verdict, recording question and verdict as a `story comment`. That also
satisfies CLAUDE.md's requirement for approval of a type-system proposal.

**On failure:** `story move <id> todo`, comment what blocked it, `story block`
if genuinely stuck, and freshen. The account of a failed cycle is the same
`story comment` a successful one gets — a story put back with no comment is
indistinguishable from one nobody ever picked up. One failure never halts the
loop.

**Refuse and record** rather than improvise if `make test` is red on arrival,
the acceptance criteria need another story to land first, or the work would
destructively touch the real store outside SH-132's sanctioned procedure. The
refusal is itself a `story comment`: a cycle that declined to act, and said
why, is a result.
