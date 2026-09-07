# SH-574 Handoff

## Current work

- PR #673: https://github.com/mikeydotio/storyhook/pull/673.
- Branch: `worktree-SH-574`; version v2.4.2, unchanged.
- Approved plan, decisions, reproduction and mutation evidence are on SH-574.
- `docs/spec/commit-identity.md` defines the policy and its limits.
- One Bash helper checks effective author/committer before commit and stored
  metadata before push. Explicit alternatives require a role and reason.
- Push checks precede receipt bypass, preserve stdin, and scan the full outgoing
  range. Published ancestors remain visible through the read-only audit.
- Five existing hook fixture suites explicitly approve their fixture identities
  through a sanitized shared helper. No production bypass.

## Reconciliation

- Central verification returned PR #673 for documentation/template conflicts.
- Merged origin/main f954df312 as 7bf25b6c9, preserving published history.
- Keep main's SH-585, SH-581 and SH-579 completions with SH-574's current work.
  AGENTS.md and `src/service/templates.rs` remain synchronized.
- Preserve the GitHub committers on imported main merge commits; the repository
  records an explicit committer-only alternative derived from main's metadata.
  This is the documented policy mechanism, not a hook bypass.
- Reconciliation test results and the merge commit are recorded on SH-574.

## Verifier repair

- Merge-tree 406cb64 failed the store-isolation contract: the new identity
  fixture cleared its environment without restoring daemon containment.
- Restore the shared `daemon_containment()` settings in its command builder.
  Preserve private HOME/XDG identity configuration and the environment allowlist.
- A real managed pre-commit hook now observes the required containment values;
  the existing structural store-isolation check remains unchanged.
- RED/GREEN evidence and final targeted results are recorded on SH-574.

## Verification evidence

- Original regression accepted an incorrect local email before the fix.
- Disposable mutation controls kill missing detection and overrestriction.
- Initial audit: 2,657 reachable commits, 792 requiring review, including all
  five fixture-identity commits. Differences alone do not prove corruption.
- Before reconciliation: all 22 new identity tests passed within a 134-test
  targeted batch, plus 10 browser-gate tests. Clippy denied warnings; ShellCheck,
  Bash syntax, formatting and whitespace checks passed.
- Run only new/directly impacted tests here. Central verification owns the full
  suite, merge, completion and worktree cleanup.

## Main behavior and operational constraints to preserve

- SH-579 (#671): independent remote tag audit and repeated host/Lima preflight
  observer. Atomic results distinguish missing/running/failed/stale/successful;
  every test tier reports the read-only advisory. No scheduler was installed.
- Observer setup and design: `docs/spec/release-observer.md`. Observations do
  not prove complete builds or artifact provenance. Historical tag mismatches
  remain visible; never repair published tags.
- Do not run manual release assembly concurrently with the observer: observer
  passes share a lock; existing manual release commands do not.
- SH-581 (#670): Lima builds in the extracted source subshell and preserves
  caller cwd for export. Preserve both Linux-target regression paths.
- SH-585 (#669): installed-launcher reader exceptions;
  `docs/rca/SH-585-installed-launcher-readers.md` carries the record.
- SH-584 (#668): preserve dispatch environment isolation, installer payload
  validation and verifier restoration/remediation behavior.
- Preserve `.git/storyhook/verification-recovery-SH-584-20260906`.
- SH-557 owns separating roadmap data from generated instructions; SH-560 is
  the next verifier audit. Attachment work SH-315 remains later.

## Submission boundary

Continue PR #673. Commit repairs and push without rewriting history. Record
results on SH-574, then make `story move SH-574 verifying` the absolute last
operation and stop. Repair this same PR if verification returns it again.
No full-suite, release, version, deployment, landing or cleanup from this lane.
