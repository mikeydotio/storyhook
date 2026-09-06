# SH-574 Handoff

## Current work

- Branch: `worktree-SH-574`; base version v2.4.2, unchanged.
- Approved plan is posted verbatim on SH-574. Decisions, reproduction and
  mutation evidence are in subsequent comments.
- `docs/spec/commit-identity.md` defines the policy and its limits.
- One Bash helper checks effective author/committer before commit and stored
  metadata before push. Explicit alternatives require a role and reason.
- The push check precedes the receipt bypass, preserves all stdin records,
  and scans the full outgoing range. Existing published ancestors remain
  visible through the read-only audit.
- Five existing Git-hook fixture suites explicitly approve their fixture
  identity through a shared sanitized test helper. No production bypass.
- Identity tests are grouped into policy and push/audit modules.

## Verification

- Initial isolated regression accepted an incorrect local email before the fix.
- Disposable mutation controls kill both missing detection and overrestriction.
- Full audit: 2,657 reachable commits, 792 requiring review against current
  global identity, including the five fixture-identity commits. Historical
  contributors and GitHub committers are differences, not proven corruption.
- All 22 new identity tests pass; final targeted batch: 134 tests passed,
  plus 10 browser-gate tests. Targeted Clippy denies warnings; ShellCheck,
  Bash syntax, formatting and whitespace checks pass. PR URL is on SH-574.
- Run only new/directly impacted tests in this lane. Central verification owns
  full-suite testing, merge, completion and worktree cleanup.

## Prior context to preserve

- SH-585: installed-launcher reader exceptions; PR #669 and
  `docs/rca/SH-585-installed-launcher-readers.md` carry its record.
- SH-584 merged as #668. Preserve daemon dispatch environment isolation,
  installer payload validation and verifier restoration/remediation behavior.
- The dirty verifier preserved at
  `.git/storyhook/verification-recovery-SH-584-20260906` must not be removed.
- AGENTS.md remains synchronized with `src/service/templates.rs`; SH-557 owns
  separating project roadmap data from generated instructions.
- Next: verifier lifecycle audit SH-560; attachment work SH-315 remains later.

## Submission boundary

Commit and push without rewriting history. Open exactly one PR with SH-574 in
its title and body; link it and comment results on the story. Make
`story move SH-574 verifying` the absolute last action, then stop.
Repair the same PR if verification returns it. No full-suite, version, release,
deployment, merge or cleanup operation from this worktree.
