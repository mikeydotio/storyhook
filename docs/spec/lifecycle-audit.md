# Lifecycle audit — SH-560

## Purpose and boundaries

Audit the complete histories of stories entering Done in the inclusive UTC window
2026-09-06T04:45:28Z–2026-09-08T04:45:28Z. The approved cohort contains 36 stories.
This is a reproducible historical report and a hardening backlog, not a runtime
behavior change. The approved plan and subsequent decisions live on SH-560.

## Evidence and method

The installed `story export` exceeds its transport's 10 MiB response limit.
The collector uses an explicit SQLite `mode=ro` connection, `query_only`, and one
read transaction. It resolves the project by `.storyhook.toml` UUID, never a
hard-coded numeric database ID. The frozen project event watermark is 22571;
events beyond that sequence or the timestamp cutoff cannot enter the evidence.
SQLite's [WAL snapshot isolation](https://www.sqlite.org/isolation.html) keeps
concurrent daemon writes outside the collection transaction's view.

Store every cohort event, including superseded comments, with its sequence and
timestamp. Keep SH-560's incident comment as supplemental evidence. Preserve
raw event text separately from analysis. A retracted comment is historical
evidence of a statement, not a current assertion or proof that it was true.

Order by sequence, not timestamp. Count entries into Done, not identical archive
markers. A repeated Verifying event starts a new generation even when its state
does not change. Preserve reopenings and unknown durations. Measure residence
in each state from creation through the final completion; separate it from
any runtime observations reported by comments. Do not infer process ownership
or test runtime from a queue position or time in Verifying.

Each finding records observed evidence, causal confidence, present repair status,
impact/priority rationale, recommendation, acceptance criteria, and disposition.
Check existing stories and current source before filing. Use diagnostic follow-ups
where observations are firm but the mechanism is unproven. Do not refile fixed
historical problems. Runtime work belongs to the approved hardening epic.

## Report and validation

Generate one offline HTML document with summary tables, per-story timelines,
ranked/filterable findings, evidence links, and selection/download controls.
Render all source text as text and safely encode embedded JSON. No external
assets, backend calls, or automatic story creation from the report. Selections
are an export aid; filed story IDs show the durable autonomous decisions.

Unit tests exercise lifecycle arithmetic, cutoff selection, generations, comment
retractions, malformed evidence, and deterministic generation. A browser check
exercises actual filtering, selection, export, and internal links. Only these
new/directly affected tests run here; the central verifier owns the full suite.

## As built

Implementation details, exact reproduction commands, and final limitations will
be recorded here before submission.
