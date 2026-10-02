# Browser subprocess ownership (SH-805)

The v3.0.3 browser harness must execute Story through the lease exposed by
`storyBinary()`. Its original text audit saw only direct calls, so aliases,
promisification and namespace imports could bypass ownership classification.
The reporter's Python test was an actual unclassified call.

## Source convention

The Rust browser-coverage contract enumerates every tracked file under `e2e/`.
Its subprocess checker accepts only single-line, semicolon-terminated named
imports or `const` destructuring from `node:child_process`. The only bindings
are unaliased `execFile` and `execFileSync`. Either quote style, horizontal
whitespace and a trailing comma in the binding list are supported. Every other
module mention fails with a path and line.

Outside those declarations, bindings may occur only as bare direct calls with
the opening parenthesis immediately after the name. Passing, exporting,
renaming, promisifying, constructing, optional calling and member access fail.
The checker scans raw source, including comments and strings, conservatively:
an unfamiliar spelling requires rewriting into the supported convention.
Comment stripping cannot hide a bypass. This is a repository convention fence,
not a JavaScript sandbox; arbitrary evaluation and encoded module construction
are outside its guarantee. There is no additional parser or Node dependency
in the Rust fence.

## Authority inventory

Every accepted call must use `storyBinary()` (or the existing constant binding
form) or match one exact reviewed command. Executable, location, arguments,
payload and process-bound expression are pinned. Inventory entries must appear
exactly once; moving a call into a helper does not remove the requirement.

| Exception | Authority |
|---|---|
| Cleanup fixture setup | Exact bounded SQLite database creation |
| Cleanup barrier reader | Exact bounded read of the isolated store |
| Reporter test runner | Exact bounded Python script with parent-pipe monitoring |

The reporter exception also pins the Python script's SHA-256. Editing the script
requires reviewing its subprocesses and updating the pin intentionally. It is
not a general Python exception. SH-804 owns the separate SQLite busy-timeout and
broader per-operation grace changes.

## Reporter lifecycle

The reporter stays in the engine-free Node project. Its outer base is the four
Python cases times their existing 60-second nested bound, plus the normal
15-second test allowance: 255 seconds. Existing load grace applies at operation
entry, respects `E2E_LOAD_GRACE=0`, and caps the outer allowance at 15 minutes.
The process receives that allowance minus `gracedPatience()` for completion
and cleanup. The Rust contract couples the constants to the Python cases;
executable tests cover idle, loaded, disabled and capped arithmetic.

The helper refuses nonpositive, fractional and nonfinite process bounds before
starting Python. It uses an asynchronous direct call and settles on process
close, including AbortSignal cancellation. A successfully sent termination
signal remains failure even if its recipient subsequently exits zero.
Errors name the command, bound, status/signal, output and original cause.

Python starts each nested Playwright runner in a new session. Its `finally`
block kills that private process group, drains output and reaps the immediate
child on success, failure, timeout or cancellation. It also cleans up a group
whose leader exited while a worker still holds stdout. SIGTERM/SIGINT handlers
record cancellation without throwing during process-handle publication.
The Node owner holds Python's stdin open; EOF triggers cleanup if the worker
dies. A 100-ms polling cadence controls cancellation responsiveness, not a
patience deadline. Descendants must stay in the runner's process group.

## Regression evidence

The old call scanner passed namespace imports and promisification; the new
matrix rejects them along with other process APIs and indirect references.
The original Python subprocess runner died on SIGTERM without owned cleanup.
Real process fixtures cover timeout, SIGTERM, SIGINT, parent-pipe EOF, success
with a pipe-holding descendant, and failed exit with preserved output.
The Node spec additionally exercises real command endpoints for timeout,
active cancellation, failed exit and executable lookup failure. The four
existing real-Playwright reporter assertions remain intact.

References: [Node subprocess lifecycle](https://nodejs.org/api/child_process.html),
[Python subprocess sessions](https://docs.python.org/3/library/subprocess.html),
[Playwright test timeouts](https://playwright.dev/docs/test-timeouts).
