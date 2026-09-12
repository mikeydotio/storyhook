# Local time display: stored in UTC, shown in the reader's zone

Design of record for **SH-679**.

## The problem

Every storyhook timestamp is RFC3339 UTC at one-second precision
(`service::Clock::System`, `to_rfc3339_opts(SecondsFormat::Secs, true)`).
That is the right thing to store and to put on the wire: the SQL comparators,
the lexical threshold filters (`--created-after`, `--updated-after`,
`--stale`, `handoff --since`) and the dashboard's sort keys all compare those
strings as text and rely on the fixed `Z`-suffixed width
(`docs/spec/recency-ordering.md`, `docs/spec/board-ordering-and-placement.md`).

It is the wrong thing to *show* a person. Until SH-679 every human surface
printed the stored string either raw or sliced with its `Z` chopped off, so a
UTC wall-clock read as if it were local:

| Surface | Before |
|---|---|
| Dashboard list "Updated" (desktop and mobile) | `updated_at.slice(0, 10)` — the UTC date, so an evening edit west of Greenwich showed tomorrow |
| Dashboard comment, commit, PR and mention meta | `at.replace("T", " ").slice(0, 16)` — UTC to the minute with no marker |
| Dashboard halted-verifier banner | the raw RFC3339 string |
| `story show` (comments, `closed_at`, `archived`, `referenced_by`), `story log`, `story engine status` | the raw RFC3339 string |
| `story report --html` | `Generated %Y-%m-%d %H:%M UTC`; Updated column `[..10]` of the UTC string |
| TUI story detail | raw Created/Updated; `[..10]` date prefixes |

## The rule

**Stored, transported, journaled: UTC. Shown to a person: the reader's zone.**

Stays UTC, with an explicit `Z`:

- SQLite rows, the event log, and every `--json` / RPC / REST payload. The MCP
  server renders with `json = true`, so agents see no change.
- Comment bodies composed by the verifier and gate progress, crash reports and
  crash ids, backup filenames, the activity journal and its viewer
  (`docs/spec/activity-log.md`: days and timestamps are UTC).
- Every sort key and threshold filter.
- Diagnostic prose composed **inside the daemon**: `story doctor` findings,
  `story load-context --story`'s "Entered done", the install-receipt finding.
  These strings are built by the RPC handler in the daemon process, the CLI's
  zone is not the daemon's to know, and they already carry an explicit `Z`, so
  they do not mislead. Carrying an offset as data for the client to render is
  a redesign this story did not ask for.

Goes local:

- **Dashboard**: the browser's zone. `localStamp(at, mode)` converts the stored
  instant with the browser's own date getters into the same ISO-like shape the
  metadata columns were tuned for (`YYYY-MM-DD`, `YYYY-MM-DD HH:MM`,
  `YYYY-MM-DD HH:MM:SS`), and `timeNode(at, mode)` wraps it in
  `<time datetime="<stored instant>" title="<stored instant> UTC · shown in
  <IANA zone>">`. Locale formatting (`Intl.DateTimeFormat` with the viewer's
  locale) was rejected: the story is about the zone, not the locale; the
  ISO-like shape keeps the fixed-width columns fixed, matches what the CLI
  prints, and keeps browser assertions independent of the runner's locale.
- **CLI text and TUI**: the process's zone (`TZ`, then `/etc/localtime`).
  `src/local_time.rs` is the one door: `stamp(at)` keeps the RFC3339 grammar
  and changes only the offset (`2026-09-12T20:31:59Z` becomes
  `2026-09-12T13:31:59-07:00` in Los Angeles), and `day(at)` is the local
  calendar date. `use_z` prints `Z` when the offset is zero, so
  `TZ=UTC story show …` reproduces the stored string byte for byte — the
  escape hatch, and the reason there is no flag or setting. Unparseable input
  is shown unchanged: the stored value is the only evidence, and hiding it
  would be worse than showing it unconverted.

The `git` precedent is the model: local time with an explicit offset for a
person, strict ISO for a machine.

## The report, and where rendering happens

The plan assumed every CLI text surface is rendered in the CLI process. The
binary test proved otherwise for `story report --html`: under `TZ=Asia/Tokyo`
on the child its "Generated" subtitle still carried the machine's offset,
because `Invocation::Report` composed the whole document inside the daemon
and shipped it as `Response::Message`. `src/api/rpc.rs` already states the
rule: the answer is a `Response`, never text; rendering is the client's job.
So the daemon now returns `Response::HtmlReport(Box<ReportData>)` and
`render_human` builds the document in the CLI process through
`render_html_report_data`. `render_json` produces the same envelope as before
(the document escaped into `message`), so the `--json` wire is unchanged.

## What guards each piece

| Claim | Pinned by |
|---|---|
| Every absolute time in the dashboard goes through `timeNode()`; no raw `*_at` slice, `replace("T", " ")` or raw `*_at` child survives | `tests/dashboard_local_time.rs` (comment-stripped source scan with positive controls) |
| The list date, drawer minute, incident second, `datetime` and `title` are the viewer's zone, on both sides of the date line | `e2e/specs/local-time.spec.ts` under `Asia/Tokyo` and `Pacific/Honolulu` |
| Sorting by Updated still orders by the stored instant, not the displayed string | `e2e/specs/local-time.spec.ts` |
| The mobile "Updated" detail row is the local date | `e2e/specs/local-time.mobile.spec.ts` |
| Zone arithmetic: both date rollovers, zero offset prints `Z`, sub-second input, stored non-`Z` offsets, unparseable passthrough | `src/local_time.rs` unit tests over `FixedOffset` |
| `story show`, `story log`, `closed_at` and the report carry `+09:00` under `TZ=Asia/Tokyo` and `Z` under `TZ=UTC`; `--json` still ends in `Z` | `tests/local_time_display.rs` (`TZ` set on the child process only) |
| The TUI's dated rows are the local calendar date and not a byte-slice | `src/tui/components/story_detail.rs` `mod tests` |
| `Response::HtmlReport` round-trips the wire and is named in the variant corpus | `tests/wire_envelope.rs` |
| Golden human output redacts a local offset as well as `Z` | `tests/golden_cli.rs::filters` |

## As built

One deviation from the plan, recorded on SH-679 as Decision 5: the HTML
report moved from daemon-side composition to a client-rendered `Response`
variant, because the test showed it was daemon-rendered. Everything else
landed as planned. The daemon-composed diagnostics listed above remain UTC;
if a later story wants them local, the shape is a `Response` field carrying
the instant for the client to render, not a daemon that reads `TZ`.
