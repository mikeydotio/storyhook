# Server-owned storyhook: explicit project selection, git as an optional layer

Design of record for epic **SH-112** and its fourteen children, all merged.

This file exists late. SH-112 was specified in its own story description and a
planning document, so for the length of the epic there was no spec to record a
deviation *in* — and three accumulated, each argued in a code comment or a
progress-log entry where nobody comparing the design against the code would find
them. The "As built" section below is the whole reason this file was written; the
design above it is here so that section has something to be built *against*.

## The problem this picked a shape for

A tracker can coherently be **filesystem-native** (markdown checklists — "which
project" is answered by which files you are editing) or **server-native** (Jira —
projects are records, selection is explicit). storyhook was neither: its data was
centralized by the rearchitecture, but *which project you mean* was still answered
by the filesystem. That hybrid produced the entire problem space — pointer files
that could go stale, recorded paths that rotted when a checkout moved, a `doctor`
audit to police those paths, and a `relink` verb to repair them.

SH-111 proposed removing the committed pointer while *keeping* filesystem-derived
resolution — the worst of both, needing a wizard and git-remote matching to
reconstruct what three lines of committed TOML did for free, and able to silently
select the wrong project. SH-112 rejected that and picked server-native.

**The invariant:** nothing about the filesystem is ever *required* to answer "which
project is this?". Filesystem inference is demoted to an optional convenience whose
failure mode is a **refusal**, never a wrong answer.

## The design as shipped

### Core

The **daemon is required**. `--local` and `STORYHOOK_INVOKER=local` are gone, the
`Invoker` seam collapsed to one implementation, and `make test-daemon` / `make gate`
retired with the second transport — `make test` is now the gate and the only one.
The web UI stays inside the daemon: `/api/v1` hard-404s off loopback and is
token-authed, so the boundary a separate process would buy already exists at the
routing layer.

### Project selection

A project-dependent command resolves in four steps (`StoreInvoker::resolve_project`),
written as four consecutive early returns so each stays separately deletable:

1. **The selector** — `--project=<slug>`, else `$STORYHOOK_PROJECT`. Binding: it
   resolves or it refuses, never falling through to the directory.
2. **The pointer walk** — the committed `.storyhook.toml`, at the working directory
   then each ancestor, bounded by the repository top. *(See As built 1 — the design
   as filed deleted this step.)*
3. **The origin** — this directory's `origin`, normalized, looked up among the
   registered ones.
4. Otherwise refuse, naming both ways out.

There is no "current project" state and no default.

**Hooks are the exception.** `story session-start` and the three installed git hooks
stay silent when no project resolves *and* when the daemon is unreachable — with
`--local` gone they have no fallback, so silence is the only correct behaviour.

### The optional git layer — two distinct associations

**`project link origin <url>`** — the auto-resolution convenience, and the only
thing step 3 consults. A URL belongs to **at most one project**; a second
registration is refused, naming the holder. One project may hold many URLs. One
normalizer runs at both registration and lookup, and the raw URL is stored beside
the normalized key so the function can improve later without losing what the user
typed. It collapses `git@host:o/r.git`, `https://host/o/r[.git]`, `ssh://git@host/o/r`,
trailing slashes and case — and deliberately does **not** collapse `github.io` into
`github.com`.

**`project link checkout <path>`** — at most one per project, **never consulted for
resolution**. It answers a different question: where do this project's repo-side
operations execute? Its only consumer is dispatch. `PathKind` and
`preferred_checkout` died with it, and it is a nullable column on `projects` rather
than a table, because nothing looks a project up by path any more.

A second checkout of one origin *is* the same project, so linked worktrees resolve
identically to their main tree by construction — no runtime git walk, no worktree
bookkeeping.

### Dispatch

`dispatch` is not a `story` verb; it lives in `plugin/claude-code/bin/story.sh` and
creates a worktree plus a tmux window. The dashboard's Dispatch button makes the
daemon invoke it, and appears only for a project with a linked checkout. Its
authorization review is `dashboard-dispatch.md`.

### Subtracted

`project_paths` and its index (schema 0008); `story relink` (now a usage error
pointing at `link checkout`); `deinit`'s repository-file cleanup; `--local`, the
second `Invoker`, and the `test-daemon` leg; `bin/story.sh`'s `repo_root()`
anchoring. `AGENTS.md` was explicitly out of scope and stays.

## The children

| Story | Scope |
|---|---|
| SH-113 | C1 — store isolation: `--store-path`, one daemon per store ([`store-isolation.md`](store-isolation.md)) |
| SH-114 | C2 — transport: daemon-only; `--local` and the second invoker deleted |
| SH-115 | C3 — identity: the remotes schema and the one URL normalizer |
| SH-116 | C4 — selection: the four steps and the refusal |
| SH-117 | C5 — verbs: `project new\|list\|show\|delete\|link\|unlink`; `init`, `deinit`, `relink` retired |
| SH-118 | C6 — bare integer ids once the project is determined |
| SH-119 | C7 — subtraction: `project_paths` and the resolution walk |
| SH-120 | C8 — dispatch plumbing: `link checkout` as the one consumer of a directory |
| SH-50 | C9 — the dashboard Dispatch button, and its authorization review |
| SH-121 | C10 — consequences: `worktree_truth.rs` rewritten, fixtures audited |
| SH-122 | C11 — residual gap: a foreign suite driving the installed binary |
| SH-150 | the TUI's own store handle — the last second writer on one store |
| SH-187 | the dashboard's mutation guard is not authentication ([`dashboard-authorization.md`](dashboard-authorization.md)) |
| SH-188 | event hooks reachable from a browser mutation (F2) |

## Acceptance criteria, and the test that pins each

Following SH-131's convention: the promise is pinned by a test, not by this file.
A change that breaks one fails the suite; one that keeps the promise by other means
does not.

| # | Criterion | Pinned by |
|---|---|---|
| 1 | The gate reduces to one leg | `unknown_flag_sweep.rs` — a script still passing `--local` fails loudly; `corruption_recovery.rs` — no diagnostic offers it. No `gate` or `test-daemon` target survives in the `Makefile`. |
| 2 | An unregistered directory refuses and names both ways out; the same command in a registered checkout resolves with no flag | `project_selection.rs::an_unregistered_directory_refuses_and_names_both_ways_out`, `::a_registered_origin_resolves_with_no_flag` |
| 3 | Registering one URL against a second project is refused, naming the holder | `remote_identity.rs::the_schema_refuses_a_second_project_claiming_one_origin` (the DB constraint), `origin_ownership.rs::project_new_refuses_an_origin_another_project_already_holds` (the message) |
| 4 | Four spellings of one URL resolve to one project; `github.io` does not collide with `github.com` | `src/domain/remote.rs`'s unit tests, which name this criterion at the assertion |
| 5 | `session-start` is silent in an unresolvable directory **and** with the daemon stopped; `git commit` prints nothing either way | `hook_silence.rs` (commit, merge, and the healthy-daemon control), `project_selection.rs::session_start_reports_no_reachable_daemon_without_a_raw_diagnosis` |
| 6 | A `--store-path` run leaves the default store byte-identical | `store_isolation.rs::a_store_path_run_leaves_the_ambient_store_byte_identical` |
| 7 | `story store new` refuses the default path | `store_isolation.rs::store_new_refuses_the_default_path` |
| 8 | The dashboard serves and mutates every project with no directory anywhere in the path, and shows Dispatch only for a project with a linked checkout | `web_test.rs::a_project_with_no_checkout_{is_listed_rather_than_hidden,still_serves_its_board,refuses_writes_and_says_why}`; `e2e/specs/dispatch.spec.ts` — absent for the `--no-attach` "Gamma Archive" fixture, present and leading for "Alpha Project" |

## As built

Three departures from the design above, each decided during implementation and
each argued where it was made. Recorded here so that comparing the design against
the code no longer requires finding a code comment first.

### 1. The committed pointer survives as an identity, and outranks the origin

**The design said** identity from `.storyhook.toml` would be deleted, the file
surviving as config only (`[plugin]`, `[hooks]`), and listed selection as three
steps ending at the origin. **What shipped** is four steps, with the pointer walk
at step 2, ahead of the origin.

SH-119 was the subtraction story that would have deleted it. It was *blocked* by
**SH-151** — "two storyhook projects in one git repository share an origin, so only
the first resolves" — and the discovery is structural, not incidental: a URL belongs
to at most one project **by construction** (that is criterion 3, and the point of
the remotes schema), so an origin can never answer for the second project in a
repository. SH-119 shipped the half that was safe, deleting `project_paths` and the
recorded-path arm of the walk; `resolve_at` has been pointer-only ever since.
SH-167 later *extended* the pointer, teaching `project link checkout` to write one.

Kept deliberately, for two things an origin cannot do:

- **A fresh clone resolves immediately**, on a machine whose store has registered
  nothing. An origin registration is a fact about *this store*; a committed uuid is
  a fact about the repository, and travels with it. `story help project` states this
  to the user as a reason to commit the file.
- **Two projects in one repository** each resolve at their own subdirectory —
  `origin_ownership.rs::a_second_project_in_one_repository_resolves_by_its_pointer`.

**Why this is cleared rather than fixed.** It costs the epic nothing it actually
promised. The invariant was that the filesystem is never *required*, and it is not:
step 1 alone always answers. The failure mode asked for was a refusal rather than a
wrong answer, and that is what a stale pointer produces — a uuid this store does not
hold falls through to `unresolvable_pointer_refusal`, and SH-151's ownership probe
stops an inherited `.git` from letting an enclosing project answer for a
sub-checkout it does not own. What the epic's subtraction list got wrong was
treating "committed identity" and "recorded path" as one mechanism. Only the second
was the stale-path problem; the first is the thing that makes a clone work.

**The ordering** — pointer before origin — is cost, not preference: the walk is a
`stat` per ancestor, the origin a `git` subprocess at 14 ms against an 11.8 ms
whole-command baseline. Its stated justification has partly expired, and the comment
at `resolve_project` now says so: SH-116 measured it when *no* project in the store
had a registered origin, which is no longer true. The subprocess cost is what
carries the ordering today, not the scarcity. The residual cost is that a pointer
outranks a registered origin when the two disagree — a checkout claiming two
projects, which is a defect rather than a preference, so `story doctor` reports it
where reporting is free instead of the resolver paying for it on every command.

### 2. `story doctor`'s orphan audit survives, with its subject narrowed

**The design said** `CatalogService::orphaned` / `deregister_orphaned` would be
deleted along with `relink`, on the ground that both exist only to police stored
paths. `relink` went. The audit stayed, and now audits exactly one thing:
`checkout_path`, the single path `project link checkout` records.

`checkout_path` did not exist when the subtraction list was written — it is C8's
own creation. Deleting the audit outright would leave `story project list` and the
dashboard printing a directory that is gone with no command to clean it up, so the
subject narrowed rather than the method going. It stays report-only until
`doctor --fix`, deliberately: a project's only checkout can be "missing" because a
volume is unplugged, and forgetting it automatically would be a worse defect than
the one being fixed.

### 3. Two children outgrew this document and took specs of their own

C1 (SH-113) and C9 (SH-50) were specified here as paragraphs and shipped as
designs large enough to need their own records — [`store-isolation.md`](store-isolation.md)
and [`dashboard-dispatch.md`](dashboard-dispatch.md), each with its own "As built".
The dispatch review's finding F1 then produced a third,
[`dashboard-authorization.md`](dashboard-authorization.md), which is where the
dashboard's credential is specified; F2 (SH-188) resolved against it rather than
separately, since a route that requires a credential cannot be reached without one.
This file does not restate them.

## Known limitations, carried deliberately

- **Project checkouts are assumed to live on the same machine as the daemon.**
  Checkouts on other machines are deferred, recorded here so the limitation is
  known rather than discovered.
- **The dashboard credential is not per-user, though it is per-device since
  SH-255.** Stale as originally written here ("one token per daemon lifetime,
  shared by every browser tab") — SH-255 replaced that master-token model with
  named, individually-revocable, 30-day tokens (`story token new <name>`), one
  per device or browser rather than one per daemon process. What the limitation
  still names correctly: there is no per-*user* identity behind a token, only a
  name an operator chose. Named and accepted in
  [`dashboard-authorization.md`](dashboard-authorization.md)'s residuals.
- **`fire_hook` kills the `sh` leader, not the process group, on timeout.** SH-188
  asked whether this should change and declined: it reverses SH-141's recorded
  council decision, contradicts what `story help hooks` promises, and does nothing
  for the concern that raised it.

## Where the rest of the record lives

Each child story carries its own execution record as `story comment`s — how it
was claimed, what it found, and what it deferred. `HARDENING_PROGRESS.md` held
that record as a `## Log` section until 2026-08-15 and is now the run's
procedure only; the entries written before the change are in its git history.
`docs/rearch/STATE.md` remains the record for the nine rearchitecture waves that
preceded this epic, not for this one.
