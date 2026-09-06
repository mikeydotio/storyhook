# SH-581 Handoff

## Change

The Lima guest now runs Cargo in a subshell inside the extracted source tree.
The caller's working directory remains available to the export phase.
The regression exercises both Linux targets from a directory with no ancestor
manifest and observes canonical cwd, archived manifest and marker contents,
locked release arguments, pinned rustc, linker selection and executable export.

## Evidence

- RED: the strengthened regression failed before the production change:
  Cargo cwd was `no-manifest-here`, not the extracted source root.
- GREEN: 59 tests passed across `release_targets`, `scaffold`, and `scope_rubric`.
- Command: `bash scripts/run-tests.sh --only-no-doc release_targets scaffold scope_rubric -- --test-threads=4`.
- Shell syntax, changed Rust formatting and whitespace checks passed.
- Final Clippy result and PR URL are recorded on SH-581.
- No real Lima build was run; Cargo/toolchain shims observe the real guest script.

## Submission boundary

Use the single PR on `worktree-SH-581`. The story holds the approved plan,
validation evidence and linked PR. Central verification owns the full suite,
merge, completion and cleanup. If returned, add repair commits to that PR,
run only new and impacted tests, push, record evidence, then make
`story move SH-581 verifying` the last action. Never rewrite published history.

## Preserved context

SH-584 and SH-585 merged as #668 and #669. Do not remove SH-584's preserved
`.git/storyhook/verification-recovery-SH-584-20260906` recovery directory.
SH-557 owns separating the project roadmap from the generated template;
AGENTS.md and its template remain synchronized here.
