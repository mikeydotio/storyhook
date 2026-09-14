# STE writing guidance (SH-727)

## Requirement

Prompt agents to use ASD-STE100 for story titles, descriptions, plans, and comments.
Keep meaning, clarity, and accurate technical terms central to that guidance.
StoryHook does not run STE lint validation or add grammar advice.
This replaces the runtime enforcement introduced by SH-678 and SH-680.

## Storage and feedback

Store authored text without STE rejection or rewriting, for every caller.
Actor labels do not affect this behavior. Operators, agents, automation, and
undeclared callers use the same write path.

This applies to creation, edits, comments, transitions, imports, and decomposition.
Sentence length, contractions, vocabulary, and passive voice cannot block a write.
Non-text validation, transaction atomicity, and unrelated warnings remain intact.
Historical imports, replay, and undo retain their existing behavior.

Record approved plans verbatim through the ordinary comment flow. No special
STE wrapper, repair loop, or change to plan approval is required.
Existing CLI positional-token parsing still trims surrounding whitespace. The
service stores its supplied text unchanged, including line endings and whitespace.
Keep literal evidence formatting for readability and accurate diagnostics.

## Compatibility

Retain the standalone `crates/ste-lint` library and its public API.
Retain public diagnostic types and the legacy `TextLint` error wire contract.
StoryHook authoring paths no longer invoke the checker or produce that error.
The retained error representation supports existing library and wire consumers.
No new flags, caller exemptions, configuration, or database migration are needed.

## Validation

Test exact text storage through services, CLI, REST, and MCP.
Cover all caller types, compound writes, batches, and approved plans.
Assert no STE advice appears and unrelated warnings and validation remain active.
Retain historical restoration, literal evidence, and legacy wire compatibility tests.
Test that agent instructions include plans and retain STE writing guidance.

Use the impacted-test selector and direct tests. The verifier owns the full suite.
