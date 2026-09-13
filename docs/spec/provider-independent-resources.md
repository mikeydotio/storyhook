# SH-709: Resolve existing resources independently of the caller’s LLM

## Summary

Fix StoryHook v2.4.2’s resource-discovery boundary. Reset and completion currently derive worktree paths from provider configuration; resume uses separate discovery logic. Leased cleanup already carries exact identity and must retain its protections.

Introduce one native resource-resolution service, expose it through a read-only CLI query, and use it across existing-resource operations. Provider selection remains relevant only to launching sessions and speaking their terminal protocol.

## Implementation sequence

1. Execute `story comment SH-709 <this exact approved plan text>`, passing the entire approved plan verbatim as one safely quoted argument, before changing files or running tests.
2. Repeat `story show SH-709 --json`, `story help obviation-review`, and `story load-context --story SH-709`. Read every candidate’s complete discussion, relationships, and relevant linked implementation in bounded sections. The initial large response was truncated; do not treat it as a completed review. Follow the prescribed procedure if strong obviation evidence appears.
3. Record the review and implementation decisions using **Context, Question, Decision, Rationale**. SH-700’s pending adoption inspector overlaps identity validation but does not replace this fix; preserve its adoption semantics.
4. Write `docs/spec/provider-independent-resources.md`, covering the resolution contract and command audit. Add failing behavioral regressions before production changes.

## Resolution contract and integration

### Shared native resolver

- Add a focused Rust resource service shared by CLI discovery, cleanup, and lease validation. Keep observation separate from command-specific mutation policy.
- Add `story resources <id> [--lease-json JSON] [--window-name NAME] [--worktree-root PATH] [--json]`. Use ordinary project selection and story-ID canonicalization. Optional name/root arguments preserve existing helper configuration as discovery hints; they never exclude other candidates or override verified identity.
- Carry these inputs through the existing invocation/daemon transport explicitly. Do not consult the daemon’s or caller’s `STORY_AGENT` when resolving resources.
- Return a typed report containing canonical project/story identity, repository, branch, worktree registration/existence, lease evidence, tmux targets, candidate provenance, and contextual diagnostics. Distinguish resolved, absent, ambiguous, invalid, and unavailable evidence. Preserve the existing CLI envelope.
- Inventory Git with `git worktree list --porcelain -z`, including locked, detached, and prunable records. Parse complete records without lossy path conversion. Git recommends this stable, NUL-delimited interface for machine consumers. [Git documentation](https://git-scm.com/docs/git-worktree)

### Identity and reconciliation

- Gather explicit leases, latest recorded cleanup leases, engine dispatch bindings, validated private worktree markers, and registered worktrees holding the story branch. Search recorded repositories as well as the configured checkout; verify project association before accepting either.
- Inspect both legacy provider locations and configured locations for conflicting or unregistered artifacts. Directory names identify candidates, not ownership. Registered custom paths remain discoverable without repeating the original configuration.
- Deduplicate matching evidence by canonical repository/worktree/branch identity. An explicit lease binds an exact target; never redirect it silently.
- Prefer verified recorded identity over naming conventions, but never use precedence to suppress contradictory live evidence. Multiple worktrees claiming the same story, reused branches, mismatched markers, or inconsistent repository identity refuse mutation and list the candidates.
- Treat malformed or unreadable metadata and failed Git queries as errors, not absence. Missing directories with surviving registrations are stale resources, not “already gone.” Preserve them and provide repair diagnostics.
- Historical resources may be disregarded only when their absence is positively established and a newer valid dispatch identifies the replacement. Unverifiable or conflicting history remains a refusal.
- With no worktree but an existing unambiguous story branch, return branch-only identity. With neither resource, return confirmed absence. Never automatically repair registration, prune unrelated entries, or delete unregistered directories.

### Tmux and provider boundaries

- Resolve recorded sockets and exact window/pane identities before operating on a session. Use the standard tmux server only as a legacy fallback when no socket was recorded; do not require the caller to be inside tmux.
- Refuse duplicate or conflicting targets instead of selecting the first matching window. Address subsequent operations by socket and ID, and recheck identity before mutation. Tmux IDs are server-local. [tmux documentation](https://man.openbsd.org/tmux)
- Capture and resource status require no provider configuration. Notify and interruption select terminal behavior from the verified target provider; unknown target protocol refuses delivery.
- Resume resolves the surviving worktree before choosing a launch provider. Explicit `--agent` may change the relaunched provider while preserving that worktree. Otherwise use recorded provider identity, then the legacy resource’s provider convention; if neither establishes a provider, require explicit `--agent` for the launch.
- Preserve provider defaults for genuinely new dispatches and provider-specific doctor probes. Move provider initialization out of deterministic command routing so unset, stale, or unsupported `STORY_AGENT` values cannot break those commands.
- Scope the Codex launcher’s default-provider injection to launch/probe operations. Keep its enabled-plugin identity and installation protections intact.

### Command integration and safety

| Area | Required change |
|---|---|
| Reset, complete plan/execute, ordinary reap | Replace reconstructed paths with shared resolution before any state or resource mutation. |
| Helper unclaim and leased operations | Resolve and validate targets consistently; preserve native `story unclaim` as a store-only operation. |
| Dispatch/resume and recovery | Use shared inventory for reuse, collisions, and provider changes; preserve launch rollback ownership. |
| Capture, notify, interruption, session status | Use verified socket/window/pane identity; remove caller-provider assumptions. |
| Rust cleanup and verifier/engine callers | Share identity validation; retain their stricter lease and lifecycle requirements. |
| Other deterministic commands | Audit routing, help, hooks, and adapters; remove unnecessary provider initialization without changing provider installation or launch interfaces. |

Preserve dirty, unpushed, locked, protected-branch, current-worktree, self-window, ownership, and mergedness policies. In particular, reset’s existing `--force` overrides remain unchanged; it cannot override ambiguous ownership.

Revalidate resolved identity immediately before destructive operations. Report partial failures and verify exact postconditions before claiming success. Update daemon refusal classification for new identity errors so they cannot trigger speculative redispatch.

Keep existing lease formats readable and retain established helper success fields. Add resource diagnostics without changing store-only command semantics. Update CLI help, plugin routing references, and both provider adapters to eliminate provider-impersonation workarounds.

## Test plan

- First reproduce Codex-launcher reset against a real Claude-location worktree: assert actual registration, directory, branch, and claim outcomes. Add the reverse-provider and plain-terminal controls.
- Exercise caller environments unset, Claude, Codex, and unsupported against Claude, Codex, and custom resource locations. Resolution and refusal results must agree.
- Cover neither path, either path, both paths, duplicate branch registrations, branch-only resources, detached/wrong branches, stale registration, malformed/stale/conflicting leases, changed checkout, changed configuration, and provider changes during recovery.
- Cover spaces and unusual path characters, symlink aliases, failed inventory, inaccessible recorded repositories, duplicate windows, wrong sockets, dead/replaced panes, and identity changes between discovery and action.
- Preserve every existing safety gate with and without `--force`. Assert identity refusals leave story state, branches, worktrees, and windows untouched.
- Exercise real helper/CLI/daemon flows using isolated stores, Git repositories with private origins, and test-owned tmux sockets. Provider doubles may supply terminal behavior; never mock resource resolution.
- Run `scripts/select-tests.sh` against the actual changed tree before choosing direct tests. Run new and directly impacted suites, relevant formatting checks, and targeted warnings-as-errors checks. Record `ALL` if returned; leave the full suite to the verifier.
- Add impact declarations and a command-routing regression preventing deterministic commands from reacquiring provider-dependent discovery.

## Delivery

Commit focused, passing changes with their regressions; keep behavior-preserving extraction separate from behavior fixes. Record the command audit, test evidence, commits, and subsequent decisions on SH-709.

Do not push, open or link a PR, run the full suite, merge, release, version, or remove this worktree.

After all approved work is committed and documented, execute `story move SH-709 verifying` from this worktree as the **absolute last action**, then stop.


## Implemented command audit

| Boundary | Resource source | Mutation policy |
|---|---|---|
| `story resources` | Store leases, active lanes, validated private markers, NUL Git inventory, socket-bound tmux panes | Read only; candidates and diagnostics retained. |
| Helper reset/unclaim/complete/reap | Shared native report, re-observed before changes | Existing dirty/locked/protected/current/claim checks; only reset force permits its established recoverability exceptions. |
| Helper capture/notify | Resolved socket and pane; target provider tag for notify | Capture outside tmux; notify validates process/protocol and rechecks identity. |
| Dispatch resume | Native surviving resource identity before provider selection | Explicit provider changes preserve the worktree; unknown custom-path provider requires a launch choice. |
| Automatic cleanup | Same identity kernel and terminal reader; a real lease remains mandatory | Clean, unlocked, inactive, merged work only; no unrelated registration pruning. |
| Engine monitoring | Recorded lane socket | No ambient daemon terminal may substitute for the dispatch server. |
| Stable Codex bridge | Provider default injection only for launch/capabilities/doctor | Deterministic helper commands receive no synthesized caller provider. |

The CLI additionally accepts `--tmux-socket PATH`. Client terminal locators cross
the invocation wire explicitly; the daemon never supplies its own provider or
terminal context. Selected and caller socket paths are canonicalized before
server-local ID comparisons. A missing socket retains its locator.

Closing a terminal compares its current window/pane/PID/cwd/provider identity
with the preflight report, then kills that window ID on that socket. After Git
removal, the helper supplies its already verified snapshot as an ephemeral exact
lease for observation; it never persists that lease. Duplicate windows refuse,
even when a previous implementation treated every same-named window as garbage.

Historical identity remains until a validated replacement exists and all old
resources are absent. A surviving historical custom branch blocks retirement.
An explicit lease cannot be superseded. Unreadable/malformed private markers
prevent a clean inventory rather than silently degrading to naming guesses.

## Regression evidence

The original behavioral matrix failed before production changes: cross-provider
and custom-path reset reported failure while the Git worktree/branch remained;
same-provider legacy controls passed. Native reader regressions initially failed
because the command did not exist. The resulting tests exercise the real CLI,
store, bridge, Git inventory and private tmux servers. Endpoint fixtures model
provider registration and tmux observations only; discovery is production code.

Coverage includes both provider roots/custom paths crossed with bridge, Claude,
Codex, terminal and unknown callers; force-resistant ambiguity; missing/stale/
unregistered resources; wrong branches and main-checkout protection; historical
custom windows/branches; conflicting and malformed markers; exact sockets and
cross-server pane IDs; capture from outside tmux; and explicit resume provider
changes. Directly impacted lifecycle, cleanup, protocol and wire tests retain
command-specific safety controls. The selector has no baseline coverage map and
returns ALL; the centralized verifier owns the full suite.

Final integration also revalidates resumed identity before claim effects and again
before replacing a surviving pane or its session witness. The regression changes
the endpoint PID during the real worktree HEAD read: before the guard, respawn
proceeded and replaced the witness; after it, the helper refuses with
`resource-identity-changed` and preserves the claim, worktree and witness.

When a live provider tag is absent, a unique active engine lease/provider binding
precedes the legacy path convention. Conflicting provider bindings remain an
explicit launch choice. The configured checkout must not name another project
UUID. These checks do not add authority to delete resources.

The terminal reader accepts tmux's exact `no server running on <socket>` response
as confirmed absence after the final window exits, even while the socket entry
remains. Other failed observations remain errors.

## Integration with installed-artifact protection

The SH-708 guard runs on the native report's repository, common Git directory
and actual worktree before completion preparation fetches or mutates anything.
An explicit configured container is also checked as a safety hint; it never
selects ownership. Branch-only and absent results have no recursive-removal
target, while repository and metadata write locations remain protected.

Combined regressions cover protected Claude, Codex and custom worktrees,
Codex installed entry points operating on Claude worktrees, redirected hints,
and branch-only/absent cleanup with an installed-path manifest present.
