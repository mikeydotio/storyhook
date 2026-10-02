# Browser operation patience (SH-804)

Explicit per-call bounds bypass Playwright's configured assertion timeout.
API requests and Python SQLite connections also have independent defaults.
Under contention these shorter clocks can fail while the test watchdog still
has time left. SH-765 fixed the cleanup subprocess; SH-804 covers the other
operation bounds, keeping the SH-347 policy and SH-222 idle budgets.

## Policy

| Bound | Owner |
|---|---|
| Ordinary five-second patience | `gracedPatience()`, sampled at call entry |
| Longer or shorter existing patience | Named idle base through `gracedOperationBudget(baseMs)` |
| Fixture HTTP request | `gracedRequestBudget()`, preserving Playwright's 30-second idle base |
| Engine observation loop | One sampled budget, monotonic deadline, remaining time for each request |
| Cleanup read and SQLite busy wait | The barrier's remaining budget; parent process owns expiry |
| Whole test | Existing load-grace watchdog and absolute wall-clock ceiling |
| Timing proof | Named, reviewed deadline; no contention multiplier |

Operation budgets honor `E2E_LOAD_GRACE=0`, use the existing capped multiplier,
and clamp the result to 15 minutes. Dispatch and engine completion are patience:
their 45-second constants describe subprocess headroom, not a product promise.
Notification lifetime, delayed-data, and assertion-delegation deadlines remain
proofs. Deliberate stimulus delays and injected product timeouts retain their
experimental meaning.

## Enforcement and evidence

The Rust load-grace test derives its JavaScript/TypeScript corpus from tracked
files. Its lexical option scanner handles multiline/nested expressions and
quoted keys, excluding comments and string payloads. It rejects literals,
arithmetic and unreviewed names. Path-qualified exceptions have exact counts,
so moving, adding or deleting an exemption requires review. This is a wiring
fence, not TypeScript alias or control-flow analysis. Embedded Python is checked
by the separate exact subprocess-payload audit.

Node tests execute grace arithmetic, a real delayed HTTP endpoint with an
independent request default, and real SQLite locks. The lock holder uses a
private DELETE-journal database to force reader contention; production WAL
readers rarely wait. Its readiness receipt and stdin-owned lifetime ensure the
test releases its own lock and observes child exit. Parent timeout, missing
store, invalid budget and exact story identity remain covered.

SH-813 owns default assertion sampling. SH-805 owns subprocess discovery.
This change neither retries failed operations nor changes product deadlines.
