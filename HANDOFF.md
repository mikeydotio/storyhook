# Handoff — SH-117 part 2: `project new`, the questionnaire, `delete`, the retirement

*(Supersedes the part-1 handoff. **SH-117 part 1 is merged as #101** and the
story is still open and still `in-progress`.)*

The run itself is described by
[`HARDENING_PROGRESS.md`](HARDENING_PROGRESS.md) — read its **START HERE**
section first. That is the process; this file is only what remains.

## Do not re-run the council

`.council/sh117-project-verb-surface/DECISION.md` is the implementable document:
**22 numbered decisions, a commit plan, a test plan and a disarm matrix.** Three
seats, every one of which voted against its own proposal, and both losing authors
formally withdrew in deliberation. The verdict is also a comment on SH-117. Build
it as written; deviate only with a recorded reason, as part 1 did once.

## What part 1 landed, and what it changed under you

1. **Two catch-alls are now exhaustive over `ProjectAction`** — `forced()` and
   `project_creation_target` (`src/invoke.rs`). This is the most important thing
   to know: `project_creation_target` is the **only** route to
   `refuse_temp_project_in_real_store`, so adding `New` will be a *compile
   error* there until you classify it, and `New` **must** be a creation target
   or SH-95 reopens through the verb that replaces the one it was filed against.
   Same for `Delete` in `forced()` — miss it and the confirmation loops forever
   with no compile error.
2. **Migration 0007 added nullable `projects.checkout_path`**, no index, plus
   `set_checkout_path` / `checkout_path` on the `Store` trait with five
   conformance arms. `story project new --attach` must write **both** this and
   `project_paths`, per D3; a conformance arm already pins that linking a
   checkout writes neither the resolution index nor anything resolvable.
3. **`story project link|unlink origin|checkout` exist**, project-scoped, with
   `service::git_links::GitLinkService` and
   `service::project::origin_here` (the omitted-URL form with the SH-151
   top-level guard). `is_project_less` is now stated **positively**, so a new
   variant is project-less only if you say so.
4. **Both `/api/repos` routes go through `StoreInvoker`** via
   `rest::invoke_from_browser`, which is the door your `POST /api/repos` change
   (D11) should keep using — it is what carries the SH-95 guard.
5. `story project list` prints each project's checkout and origins.

## What is left, in the council's own commit order

Commits PR1/c4 through PR1/c10 plus all of PR 2, re-cut as one or two PRs:

- **`domain::prefix::validate` + `derive`** (D3). There is **no prefix validator
  anywhere today** — `--prefix 'hello world'` is accepted. One function, three
  callers: the CLI, the questionnaire and the REST route.
- **`story project new`** (D1): `[--prefix P] [--name N] [--attach PATH |
  --no-attach] [--no-agents-md]`, **no positional of any kind**, `--attach`
  defaulting to the client's cwd, `new` **idempotent** exactly as `init` is.
  Only `--prefix` is required non-interactively.
- **The questionnaire** (D2), stream-generic over `impl BufRead`/`impl Write` so
  its logic is unit-tested in-process; `main.rs` keeps only the `IsTerminal`
  decision and the two refusals.
- **The PTY harness** (D19) — and read its three conditions before writing a
  line of it: `daemon_containment()` on the child (SIGKILL does **not** kill a
  daemon it started, because the daemon is in its own process group), a per-file
  wall-clock watchdog as well as per-`expect`, and
  `scripts/check-no-orphan-servers.sh` as the postlude. The fallback is
  pre-agreed and is not a judgement call at the gate: if it cannot be made
  deterministic in one attempt, drop the file, keep the unit tests, accept one
  untested `is_terminal()` line, and **file the delete-prompt coverage gap**.
- **`story project delete`** (D6), no positional, selector-named, touching no
  filesystem — which deletes `repository_roots`, `agents_md_is_pristine` and
  `DeinitPlan::{files, kept}`, because `-D warnings` will not let them survive.
- **The sweep** (D22): one `refactor(test)` commit, **no `src/` change**, with
  the assertion-changing files excluded by name. After it,
  `grep -rn 'project", *"init' tests crates` must return zero.
- **The retirement** (D10, D15): redirects for `init`, `deinit` and `relink`,
  and the enumerated string sweep — including `SKILL.md`, which must gain
  `--prefix`, because an agent has no terminal.
- **The seam test** (D18), reshaped: four needles, not one, because
  `dialoguer::Select::interact()` is the worst breach in the tree and a
  `stdin()` grep does not catch it. Its allowlist names SH-152, SH-153 and
  SH-154 (below).

## Three defects part 1 filed rather than fixed

They are in the seam test's allowlist by design, so the exemptions are recorded
rather than silent — and one of them is worse than anything SH-117 is about:

- **SH-152 (critical, data loss).** `src/github/conflict.rs:43` is
  `.interact().unwrap_or(2) // default to Skip on error`. With no terminal —
  which under the daemon is **always** — every github-sync conflict silently
  resolves as Skip. The user sees a successful sync and loses every conflicting
  remote edit, with no message anywhere.
- **SH-153.** Three `Select::interact()` sites in `github/initial.rs` with no
  terminal check at all.
- **SH-154.** `confirm_undelete` prompts from the *service* layer, so
  `story reopen` can never ask and always refuses.

## Measurements you do not have to retake

- **`compact_reference()` is 2986 characters and 59 lines**, against a 3000-char
  and a 40–100-line test, and a sibling test asserts the literal `"story project
  init"`. Fourteen characters of headroom. D14 gives you the replacement text.
- **The ~130 golden snapshots are not exposed**: no `.snap` file names `project
  init`, `deinit` or `relink`.
- **280 call sites across 45 files**, 251 of them the bare two-word form. **Five
  pass a path**, and two of those are `tests/temp_project_refusal.rs::an_explicit_path`
  — SH-95's two-sided guard regression, whose refusing half is *unexpressible*
  without a path argument. That fact is why `--attach PATH` won over removing
  the positional entirely.
- `libc` is already a dependency of `storyhook-test-support`, so the PTY harness
  costs no new dependency.

## Three things that bit during part 1

- **A `python3` heredoc with three `str.replace` calls asserted out on the
  second and wrote nothing**, so the first edit silently did not land either —
  and the next `cargo build` reported the *original* errors, which reads as "the
  fix did not work". Read the script's own output before the compiler's.
- **A test can assert an invariant that a do-nothing verb also satisfies.**
  `linking_a_checkout_records_it_without_making_it_resolve` did, until it was
  made to read the link back first.
- **`make test` is the whole gate**, and it was green on the first attempt for
  part 1. Budget for that not repeating: SH-116 took six.

## Gate

`make test`, supervised in the background with **log growth as the heartbeat**
and a 120-second stall bound. Tenth consecutive story with no wedge; the streak
is worth keeping.
