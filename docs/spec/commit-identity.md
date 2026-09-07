# Commit identity detection (SH-574)

## Problem and boundary

Five commits reached main with a fixture's name and email. SH-572 removed the
shared-config writer; this fix detects the resulting class at commit and push.
There was no identity check in either hook. Historical differences alone are
not evidence of wrongdoing: this repository also has other contributors and
GitHub merge committers.

This is repository tooling, not an installed Storyhook feature or authentication.
An operator who bypasses Git hooks, edits the policy, or changes their global
identity can bypass it. The centralized merge verifier is unchanged.

## Policy

The default for each role is its global `author.name/email` or
`committer.name/email`, falling back field by field to global `user.name/email`.
Conditional includes are evaluated in the actual worktree. Baseline reads use a
small environment allowlist; command configuration and author/committer environment
variables cannot become the expected identity. Effective identity is read with
`git var` in the original environment. Names and emails are compared exactly.

Additional accepted identities are persistent global or local Git configuration:

```ini
[storyhookIdentity "reviewed-contributor"]
    name = Contributor Name
    email = contributor@example.test
    role = author
    reason = Preserve the author of the reviewed contribution
```

Roles are `author`, `committer`, or `both`. Every section requires all four
nonempty fields. Local fields override global fields for the same label. Unknown
fields, invalid roles and incomplete sections are configuration errors, not ignored
approvals. An alternative may establish a role's identity when no global baseline
exists. Its use is announced with its label and reason. No identity is approved
automatically from existing history or effective repository-local user settings.

## Interfaces

| Command | Behavior |
|---|---|
| `bash scripts/git-identity.sh current` | Check effective author and committer |
| `bash scripts/git-identity.sh push <remote>` | Read Git pre-push records from stdin and check outgoing stored commits |
| `bash scripts/git-identity.sh audit [revision-range]` | Inventory all refs by default; report every mismatched commit |

Exit 0 means no mismatch, 1 means identity requires review, and 2 means the check
could not run reliably. Diagnostics include role, actual identity, expected
identity or approved alternatives, and commit IDs where applicable. Audit stdout
contains identity/count tables and mismatched commit IDs; diagnostics use stderr.
Audit findings do not assert corruption. Audit never writes Git configuration,
objects, refs, index, or worktree files.

The pre-commit hook checks first, then chains to a managed pre-commit hook in the
Git common directory. Pre-push checks before receipt logic, preserving stdin for
the receipt loop. `SKIP_PREPUSH_TESTS=1` skips receipts only. `--no-verify` is Git's
own bypass and cannot be intercepted by a skipped hook.

Existing remote refs exclude the advertised old tip; new refs exclude known
destination remote-tracking history. Without a known destination baseline all
reachable commits are checked. No commit-count cap applies. Deletions and tags
targeting non-commit objects ship no commit identity; annotated commit tags are
peeled. Invalid or unavailable ranges fail loudly. Raw metadata is inspected
without mailmap, replacement refs or grafts. Published ancestors excluded by the
push range remain visible to the full audit.

## Enrollment and recovery

An enrolled clone has `core.hooksPath=.githooks`; the existing gate preflight
checks enrollment. A fresh clone does not execute tracked hooks automatically.
Configure the hooks path explicitly when starting work in a fresh clone. Preserve
foreign hook installations and arrange explicit chaining rather than overwriting
them. Linked worktrees share configuration but execute their own tracked hooks.

On refusal, inspect global and local configuration with `git config --show-origin
--show-scope --get-regexp '^(user|author|committer)\.(name|email)$'`, and inspect
`GIT_AUTHOR_*`/`GIT_COMMITTER_*` in the invoking shell. Repair unintended settings.
Record an explicit reasoned alternative only for an intentional identity.

Changing configuration does not repair existing commit objects. An unpublished
tip can be corrected with `git commit --amend --reset-author --no-edit` after
restoring the intended configuration. Multiple unpublished commits require
reviewed local history editing and an audit before push. Never rewrite published
history in this repository; report historical findings on the story.

## Validation and decisions

Real-Git regression tests cover both accepting clean identities and refusing
configuration/environment injection, long ranges, alternate authors, tags,
worktrees, and stored metadata concealment. Tests isolate HOME and Git state.
Mutation controls disable detection and over-restrict it in disposable copies.
Only new and directly impacted tests run in the agent lane.

The approved plan selected this policy autonomously because the unattended
question guard refused input and the council's required artifact writes were
unavailable in Plan mode. This is not represented as a council verdict.

References: [Git identity configuration](https://git-scm.com/docs/git-config),
[effective identity](https://git-scm.com/docs/git-var), and
[hook contracts](https://git-scm.com/docs/githooks).

## As built

Implemented in the shared Bash helper and tracked pre-commit/pre-push hooks.
Five existing hook-consuming fixture suites declare their test identities through
one sanitized local-approval helper; no production bypass was added.

The scan refuses shallow history with an explicit `fetch --unshallow` diagnostic.
This avoids certifying ancestry it cannot inspect. The revision array always
contains its tip because macOS Bash 3.2 rejects empty-array expansion under
`set -u`; clean first-push tests exposed this during implementation.

Validation: all 22 new integration tests passed. The final targeted batch passed
134 tests across identity, fixture-isolation, hooks, push/merge gates, local landing
fixtures, selective testing, scaffold, and shell-status contracts. The 10 affected
browser-gate tests also passed. Targeted Clippy with warnings denied, ShellCheck,
Bash syntax, formatting and whitespace checks passed. No full suite was run.

Mutation controls ran against disposable copies: bypassing the shared predicate
broke both bad-commit and stored-bad-push refusal controls; always refusing broke
the clean-commit control. Production passed all three.

The read-only repository audit inventoried 2,657 reachable commits and reported
792 requiring review against the current global identity. It included all five
`t <t@t>` commits. Other differences include GitHub committers, Agentsmith and
Claudac, which are not automatically classified as corruption or approved.
The story carries the audit counts and decision record; published history is intact.

Central verification subsequently exposed a fixture containment omission: clearing
the identity test command's environment also removed the daemon safeguards needed
by transitively executed managed hooks. The repair restores the shared
`daemon_containment()` settings after clearing, while retaining private identity
configuration. A real managed-hook child checks these values end to end; the
existing store-isolation source contract is unchanged.
