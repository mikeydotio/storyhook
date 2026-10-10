# Retained batch trigger evidence (SH-841)

The offline report makes the pair-green part of council D10 reproducible.
It reads explicitly supplied SH-830 NDJSON files and writes JSON to stdout.
It never contacts the daemon or GitHub, starts a gate, changes configuration,
or authorizes production activation. Batching and smoothing remain dormant.

```sh
python3 -B scripts/batch_trigger_evidence.py /path/to/project.ndjson
# Include an available rotation from the same project as another argument.
python3 -B scripts/tests/test_batch_trigger_evidence.py
```

Supply one project's complete retained files. Each source is identified by
its basename, byte length and SHA-256; absolute local paths are omitted.
Identical repeated attempt records are deduplicated. Conflicting duplicate
attempts, invalid identities, malformed JSON and invalid timestamps refuse
the whole report rather than emitting a partial favorable result. Exit 0
means a report was produced; it does not mean a trigger passed.

For every clean preview with at least two members and cap at least two,
take its first two members in queue order. This models the proposed initial
cap of two even where the recorded cap was larger. Smoothed pairs are
excluded because they cannot justify enabling unsmoothed batching.
Each member joins by story ID and full commit identity to its earliest own
attempt computed at or after that preview. Earlier attempts do not qualify;
a later green retry never replaces the first red or unknown result.
An intervening own attempt without commit identity and multiple attempts
at the earliest matching timestamp remain unknown.

Only `certified` and `tests-failed` with `gate_tree == preview.head_tree`
are judged. A pair is green only if both members are green; one proved red
member makes the pair red, while missing partner evidence stays separately
visible. All eligible dequeues remain in the denominator. The historical
pair threshold requires at least 30 dequeues, at least 50% green, and complete
member verdict evidence. Incomplete evidence never qualifies automatically.

This is a descriptive join of each member's own gate result, not a claim
that the pair's combined tree was tested or that later bases were identical.
Record selection, freshness, cohort adequacy and the supervised first
production batch still require operator review. `activation_authorized`
is always false, including when the historical threshold is met.

The separate 62% submission-green path remains unavailable from these logs
alone. Retention can hide an earlier attempt; a first retained record is
not proof of a first submission result. That path needs submission-history
provenance and confirmation that bisection has shipped. The report exposes
head counts as descriptive data only and never substitutes them for the
submission metric.

## Retained StoryHook snapshot, 2026-10-10

The source hash was
`a046e8f10eb809b6dd14d3ae128a8bbad29226df9491b9f0c17551d734c57c30`
(65,124 bytes, 78 records, last completion 2026-10-05T17:36:43Z).
There were 14 eligible pairs: 3 green, 9 red and 2 unknown. Three pairs had
at least one unresolved member, including one already proved red by its
other member. Thus known pair-green was 3/14 (21.4%), below both the sample
size and rate requirements. The story remains open; neither production
enablement nor its supervised acceptance has occurred.

Only synthetic input appears in the committed regression suite. Live
records, absolute paths and story discussions remain local. The native
`batch_trigger_evidence` integration test runs the same Python cases during
the normal suite. Running the Python cases directly does not claim that
the native wrapper, a merge gate, or a release gate has run.
