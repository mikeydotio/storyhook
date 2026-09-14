# Daemon activity journal (SH-590)

Each store has a continuous activity window in the `storyhook-verifier`
session. Its chronology covers daemon startup, requests, committed story
events, engine and verifier work, and script stdout/stderr. SH-662 separates
store journals from project verification windows; see
[Concurrent verifier views](verifier-windows.md).

## Contract

- Each store owns `activity/YYYY-MM-DD.jsonl` under its daemon state directory.
  Days and timestamps use UTC. Restart appends; midnight selects a new file.
  Files are private, contain no terminal escapes, and are retained until the
  operator removes them. This is an operational journal, not a transactional
  audit log; the store remains the source of truth.
- Records carry timestamp, level, source, stream, process id, context, and
  message. Requests record operation and project, never payloads or credentials.
  Story records describe committed event kinds and state changes, not bodies.
- The window follows today's journal, crosses midnight, and colors labels.
  `story daemon logs [--follow] [--json]` reads the same files without starting
  or contacting a daemon. Redirected output has no color; JSON is one record
  per line. Missing tmux never blocks daemon startup or verification.
- One fixed session on the default tmux server remains the attach point.
  Each canonical store has its own activity window. Project tails and banners
  use separate verification windows, so neither projects nor stores replace
  another store's continuous reader.
- Subprocess output is observed from regular files, using independent offsets.
  A descendant holding a descriptor cannot hold a reader at EOF. Observation
  ends with the owned command, including failure and timeout, flushing a final
  partial line. Large lines are chunked; stdout and stderr retain separate labels.
- Verifier shell steps use a file-backed observer which preserves stdout,
  stderr, exit status, stdin, working directory and the inherited process group.
  Nested gate output identifies its producing step. The existing per-attempt
  verification log and machine JSON results continue to work.
- Logging errors are reported and never change the work's outcome. Known token
  shapes are redacted and terminal control characters are escaped. No logger
  records command arguments wholesale.
- `STORYHOOK_VERIFIER_MIRROR=0` still prohibits all tmux calls. Test isolation
  clears inherited activity destinations before fixture processes run.

## Verification

Test UTC rollover and restart, concurrent complete JSON records, color/plain
rendering, control characters and redaction, store separation, live stdout and
stderr before exit, final fragments, output larger than diagnostic capture,
nonzero exits, and descendants retaining output descriptors. Exercise the real
daemon lifecycle and event-hook paths in an isolated store. Run directly
affected Rust and script tests; the central verifier owns the full suite.

## As built

The native daemon owns lifecycle, request, committed-event, engine, verifier,
hook and captured-process records. Background and launchd daemon diagnostics
are observed from the existing stderr file; a foreground daemon's terminal
diagnostics remain on its terminal. Structured activity is recorded in either
mode. An orderly exit explicitly flushes observers because `process::exit`
does not run Rust destructors. A hard kill can lose unflushed output.

Verifier shell steps use Python 3's standard library. If Python is absent,
the helper reports the limitation and runs the command normally. Calling back through
`story` would scrub the private Git environment that the speculative gate must
inherit. `pread` keeps observation separate from each writer's seek cursor;
per-record `flock` keeps Rust and Python appenders from interleaving JSON.
The enclosing verifier owns group cancellation; observers drain cleanup output
without forwarding a second signal to a child already handling it.

Nested output may appear under both its producing step and the parent script
that relays it. This preserves the complete per-attempt log and the parent's
machine-readable response. Blank output lines are omitted, carriage-return
progress updates become individual records, and lines over 16 KiB are chunked.
Timestamps describe observation time; separate stdout/stderr streams have no
total ordering guarantee. Known token formats are redacted, but arbitrary
script output is not a safe place to print secrets.

Daily journals are retained without expiry; UTC filenames are also the archive
interface. No schema, release version, deployment or verifier ownership changes
are part of this story.
