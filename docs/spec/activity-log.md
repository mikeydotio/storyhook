# Daemon activity journal (SH-590)

Each store retains its daemon journal. Each project also has a continuous
verification journal in its registered checkout at `.storyhook/logs/` and a
`<project-slug>:verification` tmux view. See [Project verification views](verifier-windows.md).
The store journal no longer opens a separate activity window.

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
- `story daemon logs --directory PATH [--follow] [--json]` reads project logs
  without contacting a daemon. Omitting the directory keeps the store behavior.
  The same UTC rotation, permissions, rendering, and archive rules apply.
- A typed verifier scope selects the project journal and story/attempt context.
  Each output observer owns an immutable snapshot; no process-global environment
  mutation or message parsing routes logs. Child commands receive the selected
  destination explicitly. The existing source field labels the producing subsystem.
  Scope drop restores the previous context, including during unwinding.
- Subprocess output is observed from regular files, using independent offsets.
  A descendant holding a descriptor cannot hold a reader at EOF. Observation
  ends with the owned command, including failure and timeout, flushing a final
  partial line. Large lines are chunked; stdout and stderr retain separate labels.
- Verifier shell steps use a file-backed observer which preserves stdout,
  stderr, exit status, stdin, working directory and the inherited process group.
  Nested gate output identifies its producing step. The existing per-attempt
  verification log and machine JSON results continue to work.
- A supervisory child whose success is the steady state — the project reader
  reconcile (`verifier-windows.md`) — is journaled only when it fails: no start
  record, no mirrored output, one ERROR on a non-zero exit or timeout (SH-761).
- Logging errors are reported and never change the work's outcome. Known token
  shapes are redacted and terminal control characters are escaped. No logger
  records command arguments wholesale.
- `STORYHOOK_VERIFIER_MIRROR=0` still prohibits all tmux calls. Test isolation
  clears inherited activity destinations before fixture processes run.
- Every journal directory ignores itself (SH-771). Before a writer opens a
  journal file, it makes sure the directory holds `.gitignore` with exactly one
  comment line naming storyhook and `*`. Both writers do this: the daemon and
  `scripts/activity-run.py`. The `*` also ignores the ignore file. Git gives the
  deeper file precedence, so a repository's own rules cannot re-include the
  journal, and other `.storyhook/` content stays trackable. Storyhook never
  edits a repository's tracked `.gitignore`. A writer that cannot write the
  ignore file writes no record. The verification view never creates the
  directory: the daemon prepares it first.
- The daemon's hygiene sweep runs at start and every minute. It repairs the
  ignore file of every registered checkout that has a journal, and asks git
  which journal files each index tracks. `story daemon status`, `story verifier
  status` and the dashboard show each tracked case with its fix: `git rm -r
  --cached .storyhook/logs` in that checkout, then a commit. Storyhook never
  changes an index, commits or pushes.

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

### Self-ignoring journals (SH-771)

`activity/ignore.rs::prepare` creates the directory with mode 0700. It reads
`.gitignore` without following a symlink or blocking on a FIFO, and returns if
the bytes match `JOURNAL_IGNORE`. Otherwise it renames a complete temporary
copy over the file. The directory is not staged and renamed as pytest's cache
directory is. Git never shows an empty directory, and every writer prepares
before its first open, so a staging directory only adds leftovers and a
lost-race branch. Writers check on every record: the fast path is one small
read. Readers (`story daemon logs`, `--directory`, the tmux reader) open only
day files and never write. Tests that read a whole journal directory read only
`*.jsonl` files. `tests/activity_script.rs` pins the Python and Rust bytes as
identical.

`activity/hygiene.rs` covers checkouts that nothing is writing to. The sweep
skips a checkout without a journal directory, so it never creates one. It runs
`git ls-files` only when a `.git` entry exists in the checkout or above it, so
a checkout that is not a repository costs no process. Its git child follows the
SH-761 rule: no record on success, one ERROR per failure, plus the sweep's WARN.
It publishes findings to `journal-hygiene.json` in the daemon state directory.
The daemon removes that file after it takes its lifetime lock and before it
publishes its portfile. `story daemon status` therefore reads the file only
while a daemon runs, and any file it reads is that daemon's own. The verifier
snapshot reads the same file into `journal_warning`, which the JSON omits when
it is absent. That field is separate from `warning` because a tracked journal
is not a queue fault, and the dashboard shows it in its own banner.
