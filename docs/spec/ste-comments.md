# STE checks for story text (SH-678, includes SH-680)

## Requirement

Authors must use ASD-STE100 for comments, story titles, and descriptions.
The approved scope includes human callers because caller labels are optional.
Checks apply to new text and edits, including bulk creation and generated comments.
Historical imports, replay, and undo must preserve existing text.

## Shared library

`crates/ste-lint` accepts text, a format, and a sentence limit. It has no
dependency on StoryHook. Each diagnostic contains a rule, severity, UTF-8 byte
range, one-based line and column, explanation, and repair guidance.
Markdown parsing preserves source locations. Titles can be fragments.
The library does not rewrite input or perform I/O.

StoryHook selects a maximum of 20 words per sentence for mixed text.
This is local policy: STE Issue 9 allows 25 words in descriptive sentences.
The library also detects contractions and a small, documented vocabulary set:
`utilize` and `commence`, including their regular verb forms. Their suggested
alternatives are `use` and `start`. These are checks, not a complete dictionary.
Suspected passive voice is advisory because syntax alone cannot establish intent.

## Text boundaries

Check prose in paragraphs, headings, lists, and table cells. Soft line breaks
do not end sentences. Formatting does not split a word or reset sentence counts.
Inline code counts as one technical unit; do not check its vocabulary.
Do not check fenced or indented code blocks or Markdown block quotes used for
literal evidence. Check link labels; preserve link destinations and bare URLs.
Preserve paths, identifiers, and version numbers as technical units. Authors
must not hide prose in evidence or code formatting to avoid the rules.

## Storage and feedback

Reject blocking findings before appending new authoring events. A rejected
compound operation must not change any story, allocation, or relationship.
Report the story field, rule, source location, and repair instruction. Keep
structured findings across daemon and JSON boundaries. Successful writes can
carry grammar advice. Do not silently replace the author's text.

Test the pure checker, service transactions, CLI/JSON/MCP feedback, and repairs.
Test title and description integration separately from comment integration.
Use the impacted-test selector and direct tests. The verifier owns the full suite.

## Limits and references

Passing means only that the implemented checks passed. It does not prove full
STE compliance. Meaning, terminology, grammar, and clarity remain the author's
responsibility. Do not ship or require the source PDF at runtime.

- [ASD-STE100 Issue 9](https://www.asd-ste100.org/assets/files/ASD-STE100_ISSUE9.pdf)
- [ASD guidance on checking tools](https://www.asd-ste100.org/STEsoftware.html)
- User reference: `~/Downloads/ASD-STE100_ISSUE9.pdf`

The initial council aborted because the chair did not preserve blind research.
The fallback decision and the exact approved plan are recorded on SH-678.
