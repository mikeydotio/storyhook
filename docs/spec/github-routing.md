# SH-734: Origin-bound GitHub access through `gh`

## Summary

Replace StoryHook’s PAT integration and implicit GitHub routing with shared origin resolution and noninteractive `gh` execution. Preserve PR ownership, merge certification, and verifier lifecycle rules.

Investigation found direct HTTP access in PR monitoring and self-update, public-host constants in installation/release tooling, and PR validation against historical registered remotes rather than current checkout origin. The worktree is clean. The successful obviation review returned no candidates.

## Implementation sequence

1. **First implementation action:** execute `story comment SH-734 <exact-approved-plan>`, passing this entire approved plan verbatim as one safely quoted argument.
2. Repeat `story help obviation-review` and `story load-context --story SH-734`; assess every candidate before implementation. Record the result.
3. Create `docs/spec/github-routing.md` covering the contracts below, migration guidance, and the complete caller inventory.
4. Implement in focused, passing commits with regression tests. Record subsequent decisions immediately using **Context, Question, Decision, Rationale**.
5. Run new and directly impacted tests, commit completed work, and record results. Execute `story move SH-734 verifying` from this worktree as the absolute last action.

All work remains under SH-734. Submission, full-suite verification, PR creation, merge, and cleanup belong to the centralized verifier.

## Repository and execution boundaries

### Authoritative destination

- Add a shared Rust resolver returning validated checkout identity, host, owner, repository, web URL, and HTTPS transport URL.
- Resolve each project through its registered checkout. Read the actual `remote.origin.url` through Git’s configuration machinery, without transport rewrites or ambient Git-directory overrides. Validate project identity and checkout ownership.
- Linked worktrees must agree with the registered checkout’s origin. Verifier mirrors receive the authoritative source checkout explicitly and validate their destination against it; their cwd or local mirror origin cannot become an independent authority.
- Accept HTTPS, SCP-style SSH, and `ssh://`, with optional `.git`. Reuse the existing remote grammar without changing unrelated project-registration semantics.
- Preserve HTTPS ports. Treat SSH port 22 as the default transport port; reject nonstandard SSH ports with guidance to supply an HTTPS origin, because an SSH port does not identify an HTTPS/API port. Reject credential-bearing passwords, malformed paths, unsupported schemes, ambiguous origins, and unavailable checkouts with contextual errors.
- Resolve afresh at operation boundaries; do not introduce a persistent destination cache. Revalidate before destructive actions. Refresh existing mirror or submission snapshots from the authoritative checkout after a failed read, allowing one retry only if the origin changed. Never blindly retry an ambiguous write.
- Repository moves require an updated origin. Historical registrations and server redirects do not authorize acting on a different identity.

### Shared `gh` execution

- Introduce local-only `story github resolve`, `story github exec`, and `story github git` helper entry points, usable without opening the daemon store when supplied an explicit checkout. Rust callers use the same underlying implementation.
- Shell and Python callers use these helpers rather than maintaining separate parsers or authentication logic. Include required helpers in installed plugin and verifier bundles.
- API calls use explicit relative endpoints and `--hostname`; repository commands use fully qualified `--repo HOST/OWNER/REPO`. Reject conflicting caller-supplied destinations and validate PR URLs before forwarding.
- Let `gh` select public versus Enterprise API paths. Remove StoryHook’s API-base construction and `[github].api_url`; report that obsolete configuration with migration guidance rather than silently ignoring it. These explicit targeting mechanisms follow the [GitHub CLI API contract](https://cli.github.com/manual/gh_api).
- Override ambient `GH_HOST`/`GH_REPO`, disable prompts, paging, debug output, and update notifications, and use bounded subprocess execution. Report missing executable, authentication, authorization, network, timeout, and malformed-response errors without exposing credentials.
- Preserve `gh`’s supported credential configuration, including Enterprise token variables, only within authorized GitHub subprocess boundaries. Keep credentials out of agent/test children and remove StoryHook token transport. Follow the [documented `gh` environment contract](https://cli.github.com/manual/gh_help_environment).
- Git remains responsible for object transport. Use command-scoped HTTPS URLs and `gh auth git-credential`, scoped to the resolved host. Do not change global Git configuration. Reject conflicting push destinations or rewrites that would redirect the operation.

## Behavior and migration

### PR lifecycle and monitoring

- Replace the HTTP-backed `GithubClient` with the shared `gh` runner; change `GithubApiFactory` to accept validated repository context instead of a token and API base.
- Apply current-origin validation at link creation for close-on-merge links, during checks, and before submission, verification, and merge. Informational links may remain foreign, but cannot trigger actions.
- Preserve existing merge receipts, head/base checks, fork rejection, certification requirements, and refusal to complete uncertified verifying stories.
- Remove `github-auth`, PAT/keychain code, token-bearing request/context fields, obsolete feature dependencies, and associated help/environment contracts. Retain HTTP dependencies still used outside GitHub.
- Add `[github].poll = true|false`, defaulting to `false`. This replaces the old credential-based monitoring consent with explicit project control. Read it every tick; manual `pr-check` remains available independently.
- Keep the existing polling cadence. Skip projects without actionable links, isolate failures by project, and emit contextual diagnostics without preventing other projects from being checked.
- Leave legacy `storyhook-github` keychain entries untouched and unused. Document removal through the OS credential manager and optional PAT revocation. Never inspect, migrate, or delete `gh` credentials.

### Scripts, installation, and update

- Route submission, verification, merge, release, clone/fetch/push, observers, and generated operational instructions through the shared boundary. Preserve existing default-branch discovery and ownership rules.
- Replace direct GitHub HTTP downloads with `gh release view`/`download`; preserve staging, smoke checks, atomic replacement, and plugin reinstall behavior. GitHub supports explicit repository targeting for [release downloads](https://cli.github.com/manual/gh_release_download).
- Add an explicit fully qualified release source to installation and `story update --source HOST/OWNER/REPO`. Persist it in installation metadata only after successful installation/update.
- Self-update uses that recorded source, never the current project’s origin. Existing installations without source metadata fail with the exact recovery command. Bootstrap installation requires an explicit source; no public-host default is embedded.
- Release tooling derives its source from its own checkout origin. Modify and test release code without publishing or executing release/version operations.
- Audit every tracked GitHub literal and direct-access call site. Preserve historical evidence and static attribution links; remove operational destination constants. Update documentation, plugins, generated prompts, fixtures, and packaging.

## Test and acceptance plan

- Start with failing production-flow regressions for stale registered remotes, ambient host/repository redirection, Enterprise submission transport, and direct-HTTP update behavior.
- Exercise public and Enterprise fixtures, including `github.example.com`, supported origin forms, ports, malformed origins, missing checkouts, linked worktrees, mirrors, origin changes, and renamed repositories.
- Run two projects on different hosts through one daemon. Assert exact subprocess destinations and prove failures or foreign PR links cannot mutate the other project.
- Cover missing `gh`, missing/expired authentication, permission failures, network errors, timeouts, invalid JSON, polling opt-in/out, and credential redaction/isolation.
- Exercise real submission and verifier flows with controlled external executables, preserving certification, PR adoption, fork, head, and base checks.
- Test release-source persistence, missing-source migration, unrelated cwd isolation, downloads, and failed updates preserving the installed binary.
- Add architectural regression checks for operational public-host literals, direct GitHub HTTP/PAT access, and callers bypassing shared boundaries. Register cross-file tests in the impact manifest.
- Run `scripts/select-tests.sh` against the actual changed tree before choosing direct tests. Execute new tests and selected impacted targets; if it returns `ALL`, record that result and leave the full gate to the verifier while running the directly impacted suites.
- Check affected feature configurations, formatting, and warnings. Do not run `make test`, push, open a PR, or perform release operations.

## Implementation inventory and progress

| Boundary | Callers to migrate |
|---|---|
| Identity | domain/github_remote, domain/pr_url, service/project, service/pr_link, daemon/verification |
| API/authentication | github/client, github/api, github/credential_store, service/github, service/pr_check, daemon/github_poll |
| CLI and wire | cli, main, invoke, api/wire, service/Ctx, domain/secret, env/secrets, env/spawn_env, env/test_environment |
| Transport/submission | plugins/story/lib/submission-git.sh, plugins/story/bin/story.sh, plugins/story/lib/session.sh |
| Verification/landing | scripts/verify-pr.sh, land-pr.sh, merge-watch.sh, origin-default-branch.sh, landing-intent.sh, verifier-worktree.py |
| Release/install/update | update.rs, install.sh, release.sh, render-release-body.sh, release observers and tag helpers |
| Distribution | build.rs embedded verifier bundle, plugin packaging, help_topics, README, plugin references and skills |

- Complete: shared raw-origin resolver; local `resolve`, `exec`, and `git`
  helpers; explicit gh destinations; private bounded capture; Enterprise token
  allowlists and gate scrubbing. The transport helper supports fetch, push, and
  ls-remote against an explicit origin operand. It refuses matching pushInsteadOf
  rules instead of reconstructing Git's push rewrite precedence.
- Complete: current registered-project and mirror validation; PR client/check/link
  migration; opt-in polling; PAT/CLI/wire removal; submission and verifier/landing
  routing; HTTPS clone support; safe unpinned read refresh; explicit installation
  and update sources with gh downloads and matching per-binary metadata.
- In progress: generic session/dispatch/cleanup observations and their focused
  regression suite.
- Pending: release scripts and observer routing, coverage observer transport,
  generated operational references, final authentication/host/bypass audit and
  remaining directly impacted validation.

### Local helper contract

`story github resolve --checkout PATH` emits the canonical checkout and validated
`identity` object (`host`, `owner`, `repo`) as JSON. `exec` and `git` take the same
checkout option followed by `--` and operation arguments. These calls never open
the daemon store. Callers must supply the authoritative checkout; the project
service and verifier integration must establish that authority before forwarding.

`exec` currently supports repository REST endpoints, PR/release commands, and
`repo view`. API endpoints go immediately after `api`; routing/header overrides
are refused. Repository commands receive fully qualified destinations. Metadata
answers reaching 64 KiB are refused rather than silently truncated; release asset
downloads must write to files. Process deadlines are 120 seconds for network
operations and 30 seconds for local Git reads. Neither boundary retries writes.


### Explicit source and clone execution

The local helper accepts `--authority PATH` after `--checkout PATH`. It resolves
both checkouts on each call and rejects differing repository identities before
starting gh or a Git network command. Shell callers that use a leased checkout
must pass the registered source explicitly.

`story github git --checkout PATH -- clone [--mirror|--bare|--no-checkout] origin DEST`
requires an absolute destination. The shared transport boundary converts the
source to HTTPS and supplies the host-scoped gh credential helper. Native Git
retains responsibility for refusing an existing nonempty destination.

Multi-step callers can pass `--expected HOST/OWNER/REPO`, using the canonical
identity from `resolve`. Each call still reads actual origin and refuses a
changed identity. The verifier pins this identity before its first observation
and preserves it through landing; it never reinterprets a certified PR number
in a new repository. Start a fresh operation after an intentional origin change.

Unpinned helper discovery reads (`ls-remote`, or JSON `pr list/view`, `repo view`,
and `release list/view`) may refresh once after failure if current origin differs.
The retry rechecks source authority and explicit PR URLs. Unchanged-origin
failures, pinned flows, API calls tied to existing links, ref updates, downloads,
and writes are not automatically retried. A failed retry retains the first error
as context and ends the operation.

### Installation source protocol

The updater uses `ReleaseSource`, separate from checkout authority. It accepts
`--source HOST/OWNER/REPO`, or reads `<resolved-executable>.source.json` with
`version: 1`, canonical `source`, and the installed executable's `sha256`.
Missing, invalid, or mismatched metadata requires explicit recovery; `--check`
does not write it. `gh release view --json tagName` selects the tag and
`gh release download` writes the named asset to staging. No direct HTTP client
or API-host assumption remains in the updater.

Bootstrap requires Python 3 and gh because no story binary is available yet.
Its embedded release-only adapter applies explicit routing, bounded private
capture, token redaction, closed stdin, and process-group deadlines. It accepts
pinned old releases and does not need a command introduced by this migration.
Both installers hold `<resolved-executable>.install.lock` with exclusive flock,
stage beside the executable, smoke-test before replacement, then atomically
publish the binary and matching metadata. An interrupted metadata publication
reports explicit recovery; a digest mismatch cannot authorize a later update.
Provider plugin reinstall remains after replacement.

### Generic Git observations

Dispatch and cleanup also support local-only projects. `OriginObservation`
resolves the actual checkout origin and permits only `ls-remote` and `fetch`.
URL origins use the strict GitHub HTTPS/gh transport. Filesystem origins are
canonicalized, reject URL rewrites, receive no credential variables, and run
with `GIT_ALLOW_PROTOCOL=file`. This separate observation case grants no PR
or remote-write authority; `Repository::resolve` still rejects local paths.
Cleanup pins the origin across its default-branch read and fetch. Generic
shell callers use `story github observe`; fetch failures retain diagnostics
while preserving dispatch's documented stale-base fallback policy.
