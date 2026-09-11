# Obviation review — SH-673

Before beginning or resuming implementation, an agent checks whether work on
another story has likely made its assigned work unnecessary. The agent compares
requirements and implementation evidence; StoryHook supplies facts, not a
similarity score or an automatic closure decision.

## Read contract

`story load-context --story <id>` adds an obviation review to the ordinary
project context. `--format json` exposes `obviation_review` with a full `target`
StoryView, a numerically ordered `candidates` array, and the canonical review
`procedure`. Each candidate includes
the full StoryView, `reasons`, and an optional `completed_at`. Markdown renders
the same evidence. Without `--story`, existing output is unchanged.

In one read transaction, collect every other story in this project that is
currently `in-progress` or `verifying`, or that entered `done` strictly after
the target's creation. Use recorded state transitions, including historical
creation/closure events, rather than `updated_at` or the current `closed_at`.
Repeated events naming `done` while already done are not new completions.
Reopened and archived completions remain candidates; show their current state.
Computed epic states use the existing project projection. No ready-only,
keyword, preview-size, or default archive filter may drop candidates.

Compare parsed RFC3339 instants, including offsets and fractional seconds.
Missing targets and unreadable or invalid evidence fail with context, never an
empty successful review. The query does not write story events or judgments.

## Agent and human responsibilities

`story help obviation-review` owns the procedure. All built-in dispatch modes,
resumes, claim/context skills, session guidance, and scaffolds point to it.
The MCP `story_context` tool accepts the same optional `story` argument through
the CLI parser. Its description directs agents to review before implementation
and on resume; the result embeds the same procedure for MCP-only clients.
Explicit custom prompt overrides retain their existing wholesale semantics.
The SessionStart hook carries a short pointer, not the potentially large review.

The agent reads every candidate and follows linked implementation evidence for
plausible matches. Shared names, ancestry, or a planned dependency alone do not
establish obviation. With no strong evidence, proceed. For high-likelihood
obviation, comment the evidence and original state, add `obviated-by` for each
matching story, then move to `blocked` with a human-review awaiting reason and
an expected-state guard. Stop work without closing, releasing, or waiting for
an interactive answer. Failures of those writes must remain visible.

This is a human decision, not a dependency waiting to finish: no `blocked-by`
edge or automatic unblock. The existing readiness predicate already treats
`obviated-by` as unconditional. A human accepting the finding closes the story
with a reason. A human rejecting it removes the rejected obviation edges,
clears the review reason, and restores the appropriate open state while
preserving other blockers. Retained obviation edges continue to block readiness.

## Validation

Service tests pin event and timestamp boundaries, candidate completeness,
read-only behavior, and rich evidence. CLI tests cover grammar, JSON/Markdown,
project isolation, failures, and the real relationship/block/review lifecycle.
Contract and rendered-dispatch tests keep every built-in startup door connected
and preserve custom prompts and shell inertness. Run only new and impacted
tests here; the central verifier owns the full suite.

## References

- [SQLite snapshot isolation](https://www.sqlite.org/isolation.html)
- [RFC3339 ordering constraints](https://www.rfc-editor.org/rfc/rfc3339#section-5.1)
- [MCP tool discovery and input schemas](https://modelcontextprotocol.io/specification/2025-11-25/server/tools)
