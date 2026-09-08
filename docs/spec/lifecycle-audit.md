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

The committed collector, pure analysis module, and HTML template generate
`docs/reports/SH-560-lifecycle-audit.html` from `SH-560-evidence.json` and
`SH-560-findings.json` beside it. The dataset contains 4,243 events across the
36 cohort stories plus SH-560. It includes `command` (daemon-derived) and
`actor` (self-attested) separately. The evidence viewer labels both and marks
retracted comments as historical statements.

Independent SQL confirms 36 cohort members, 40 Done state writes, and 75
Verifying state writes. Independent adjacent-state arithmetic confirms 105,645
seconds of aggregate Verifying residence. This is **not** gate runtime.
The report identifies 14 findings and six selected children, SH-603–SH-608,
under Lifecycle Hardening SH-602. Existing host-hook work remains with
Agentics AGE-63–65. No runtime code, installed artifact, public API, or store
schema changed.

| Action | Command |
|---|---|
| Regenerate from committed evidence | `python3 -B scripts/lifecycle-audit.py render` |
| Recollect the frozen prefix from an explicit store | `python3 -B scripts/lifecycle-audit.py collect --store /path/to/store.db --output /tmp/SH-560-evidence.json` |
| Run analysis and artifact contracts | `cargo test --test lifecycle_audit` |
| Run Python contracts directly | `python3 -B tests/support/lifecycle_audit.py` |
| Install pinned browser dependencies | `npm ci --prefix e2e --ignore-scripts` |
| Run offline Chromium checks | `node tests/support/lifecycle_audit_ui.cjs` |
| Open the report on macOS | `open docs/reports/SH-560-lifecycle-audit.html` |

The browser check uses the existing Playwright browser cache, no daemon, and
no network requests from the page. It validates filtering, cross-filter
selection retention, JSON download, evidence navigation, command/actor display,
Escape dismissal, and mobile width. It caught missing visible provenance and
an overflowing long test name; both are covered by the same real-report test.
Playwright's locator assertions wait for asynchronous hash navigation, following
its [assertion guidance](https://playwright.dev/docs/test-assertions).

Collection and rendering are deterministic; a contract rejects a stale committed
HTML artifact. There are 15 Python contracts, also run through the Rust wrapper.
Formatting and targeted warning-denied Clippy pass. Browser launch requires
macOS bootstrap permission outside the filesystem sandbox; the ordinary sandbox
launch failure was environmental, before report navigation.

Limitations are also visible in the report. Completion-based sampling excludes
still-open failures, full-history counts include pre-window work, and statements
are not certificates. Raw verification logs under the main checkout were not
opened; preserved excerpts, event provenance, source inspection, and five
GitHub PR merge records provide the available evidence. Missing runtime timing
stays unknown. Post-cutoff observations never alter the frozen cohort. Browser
inspection is available through the command above; no user browser was operated.
