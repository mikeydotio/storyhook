# Rust harness patience (SH-810)

## Contract

A harness wait for expected progress uses `storyhook_test_support::load_grace`.
Polling uses `Patience` and observes the condition before expiry. An API that
takes a whole duration receives `graced_now` at its patience-owning caller.
The idle allowance and polling cadence do not change merely because grace is
added. Failure diagnostics report the condition and the granted patience.

A production-deadline proof is different. Its assertion, injected deadline,
negative observation window, and fixture stimulus remain unscaled. Low-level
`ChildGuard`, `run_bounded`, explicit `Pty::timeout`, and socket operations use
their supplied duration literally. A graced caller must not be graced again.

Channels distinguish an empty queue from disconnection. Only the former can
become ready later. Release senders belong inside their scoped worker region
so a failing assertion releases workers before their join.

## Census

The sweep covers 398 tracked Rust files recursively under `tests/` and
`crates/storyhook-test-support/`, including nested modules. The exact retained
inventory is `tests/timing_assertions/waits.json`: 65 expressions at 82 sites
(45 proof, 20 delegated, 17 fixture). These counts describe the lexical fence,
not every blocking operation in a test. Each inventory entry states its owner
and reason; occurrence counts prevent a new use from inheriting an old waiver.

The tables cover existing explicit bounds, readiness loops, channel barriers,
and fixed sleeps that stand in for readiness. Ordinary synchronous test work,
including filesystem calls and unbounded command APIs without an explicit
harness allowance, does not acquire a new timeout policy in this change.

| Owner | Condition | Idle bound | Class and mechanism |
|---|---|---|---|
| test support `server::try_serve_on` / `wait_for_report` | Server reports its own bound address or startup error | 10 s | Patience; receive before expiry; disconnection returns an error |
| test support `server::wait_for_addr` | Listener accepts | `ACCEPT_DEADLINE`, 5 s | Patience via `wait_for` |
| test support `server::DaemonGuard::drop` | Stop command completes | `STOP_DEADLINE`, 15 s | Patience via `graced_now` at caller |
| test support `env::probe_fault_capability` | Test binary reports its capabilities | `FAULT_PROBE_DEADLINE` | Patience via `graced_now` at caller |
| test support `pty::Pty::spawn` | Default prompt and child completion allowance | `EXPECT_TIMEOUT`, 30 s | Default patience via `graced_now`; explicit override stays literal |
| `change_feed_subscriber::wait_for` | Expected fixture state | Caller duration | Patience via `wait_for` |
| `change_feed_subscriber::a_subscriber_survives_its_daemon_restarting` | Reconnect notification, then write notification | 15 s, 10 s | Patience; the subscriber's 500 ms poll is unchanged |
| `daemon_concurrency::wait_for` | Hook publishes its held state | Caller duration | Patience via `wait_for` |
| `daemon_concurrency::a_slow_command_does_not_block_another_client` | Concurrent list and released hook complete | 30 s, 15 s | Patience via `graced_now`; ordering is proved by explicit hook release, not elapsed-time ratios |
| `daemon_concurrency::a_hook_that_calls_story_never_queues_behind_its_own_parent` | Nested command workers finish | 20 s | One graced allowance shared by all receives |
| `daemon_lifecycle::wait_for` | Lifecycle state or process retirement | 5 s | Patience via `wait_for`; negative observation loops unchanged |
| `verification_withdrawal::wait_for` | Worker state reaches the expected transition | 8 s | Patience via `wait_for` |
| `dispatch_endpoint::poll_until_finished` | Async dispatch result | 5 s | Patience |
| `daemon_engine_controls::finished` | Engine run finishes | 10 s | Patience |
| `api_reset::reset_requires_auth_and_confirmation_then_polls_a_scoped_durable_receipt` | Async reset succeeds | 10 s | Patience; an error response still fails immediately |
| `block_delivery_authority::wait_for` | Fixture barrier is reached | 5 s | Patience; negative acknowledgement observation unchanged |
| `block_delivery` CLI delivery cases | Native interrupt and resume markers | 8 s each | Patience |
| `daemon_invoke::hooks_still_fire_through_the_daemon` | Hook marker appears | 5 s | Patience; observe marker before expiry |
| `daemon_parent_identity` parent mismatch case | Daemon retires after parent identity mismatch | 2 s | Patience; observe liveness before expiry |
| `tailnet_probe_budget::probes_after_bind_confirmed` | Authoritative tailnet bind is published | `SETTLE_DEADLINE`, 5 s | Patience; final probe-count assertion unchanged |
| `tailnet_rebind::a_daemon_that_missed_its_tailnet_bind_self_heals_without_a_restart` | Tailnet bind heals | `REBIND_DEADLINE` | Patience |
| `crash_matrix::concurrent_daemon_starts_migrate_exactly_once_even_when_one_is_killed` | All racers exit | 60 s | Patience; all finished wins before expiry |
| `verification_retry_shell` retry scenario | Real gate announces readiness | 40 s | Patience; early worker exit still fails immediately |
| `merge_gate` verifier command callers | Captured verifier output completes | 120 s | Patience via `graced_now` |
| `land_pr::wait_for` | Fixture marker appears | 10 s | Patience via `wait_for` |
| `machine_lock::wait_for`, `wait_for_process_exit` | Marker appears or child exits | `poll_ceiling()` | Patience via `wait_for` |
| `gate_lock::wait_for_gone`, `wait_for_pid` | Lock disappears or positive PID publishes | Lock poll period times allowed cycles | Patience via `wait_for` |
| `golden_cli::settled` | Block delivery reaches a terminal state | `IDLE_POLL * 10` | Patience; golden result assertions unchanged |
| `story_reset/native::settle_claim_resume` | Delivery settles and releases ownership | `IDLE_POLL * 10` | One Patience shared by both observations |
| `story_reset/delivery::wait_for` | Delivery barrier | 10 s | Patience |
| `story_reset/orphan` reset scenario | Git hook entered, then orphan releases lock | 15 s, 10 s | Patience |
| `story_reset/quiescent`, `engine_reset/quiescent` | Effect leader reaches barrier | 10 s | Patience; 500 ms negative child-quiescence proof unchanged |
| `engine_adoption_identity::Dispatch` setup | Pane reports the expected executable | 5 s | Patience |
| `verification_queue::ProjectGateActuator` family | Worker enters, remains held, then drains | 30 s | Existing SH-811 repair `772bb843`, with its regression and scoped release senders |

### Remaining migrated owners

| Owners under `tests/` unless qualified | Condition and policy |
|---|---|
| `activity_log`, `activity_script` | Script output, journal arrival and descendant completion; the two hook-ceiling output proofs stay literal |
| `build_slots` | Compiler start, contention report, slot release, shell probes and child completion; serial-duration floor stays literal |
| `battery_completion`, `plugin_runner` | Runner startup/exit and generated overlap barriers; parent computes grace once and passes the allowance into generated Rust or shell |
| `commit_identity`, `commit_sync_termination`, `concurrency_soak` | Real command completion; existing bounds remain the idle values |
| `continuation_lost_reply` | One Patience covers capture, admission and final hook completion; injected delay, feedback floor and milestone ordering stay unchanged |
| `crash_reports`, `daemon_lifecycle` | Command and cleanup completion; lifecycle fixed-count publication loop becomes Patience |
| `dispatch_tmux_context`, `store_isolation` | Remaining fixture commands and concurrent clients |
| `e2e_browser_coverage`, `e2e_pool`, `e2e_selection` | Wrapper completion and pool startup markers |
| `e2e_provider_doubles` | Double generation only; actual double calls retain their production TMUX_TIMEOUT proof |
| `engine_reset`, `engine_run_model`, `story_reset/card`, both reset `quiescent` modules | Positive channel handshakes; senders drop inside worker scopes; negative receives and dispatch-deadline assertions remain literal |
| `verification_control`, `verification_shutdown_drain`, `block_delivery_authority` | Positive worker/channel/bus observations and releases; negative windows remain literal |
| `verification_queue` | Readiness and process disappearance; lost workers fail immediately; optional post-kill PID observation remains a literal fixture window |
| `verification_progress_timeout`, `verification_retry_shell` and its `callback` | Watchdog, child completion and callback read; stall stimulus remains literal |
| `verifier_mirror_isolation`, `verify_window`, `worktree_truth` | Subprocess result capture |
| `move_if_state`, `story_claim`, `story_unclaim` | Concurrent client completion |
| `mcp_removal`, `plugin_install_freshness`, `protect_install_hook`, `support/protect_*` | Command and generated-hook completion |
| `session_start_degraded`, `session_start_hook`, `spawned_child_environment` | Hook and isolated environment probes |
| `attachment_http`, `attachment_upload`, `handoff_endpoint`, `token_endpoint`, `token_exchange`, `api_reset` | Whole client I/O allowances; attachment rejection-before-body proofs retain PEER_IO_TIMEOUT/2 |
| `daemon_wedge` | Raw request I/O backstops; elapsed proof against PEER_IO_TIMEOUT stays literal |
| `daemon_fd_hygiene` | Pipe EOF after all expected writers close |
| `daemon_timeouts` | Outer client completion and churn-remeasurement budget; inner deadlines, publication cadence and negative observations stay literal |
| `daemon_invoke`, `hook_bounds` | Whole hook-chain allowance and post-hook marker arrival; hook timeout assertions remain literal |
| `daemon_token_clipboard`, `pty_interactive` | Outer watchdogs enclose graced PTY conversations |
| `gate_lock` | Command completion and observed journal marker in place of a startup sleep |
| `machine_lock`, `land_pr` | Child completion, progress feeding and released-lock disappearance; wrapper signal-before-sleep proof stays literal |
| `orphan_check` | Process visibility before the measured operation; script grace-period proofs stay literal |
| `project_recovery/worker` | Provider startup is graced; cancellation clock starts after readiness and stays below the named fixture delay |
| `schema_lineage`, `service_project_set_prefix` | Positive contention/acquisition observation; SQLite inputs and lock-hold measurement stay literal |
| `story_reset/native` | Pane death is observed with Patience instead of a fixed retry count |
| `tailnet_startup`, `web_test` | Listener and SSE arrival; SSE quiet backstop; short socket poll and quiet-period measurement remain literal |
| support `crash` | Startup-to-fault, client completion and reaped-lock settling; already-armed death retains DELIVERY_BACKSTOP proof |
| support `server` tests | Successful child/pipe output, command and HTTP completion; explicit timeout regressions remain literal |

### Manual checks outside the lexical fence

- Generated programs: battery and plugin overlap barriers receive parent-computed
  grace. Their sleeps that keep children alive or establish completion order are
  fixture stimuli. Embedded shell release loops remain controlled by owned
  release files, stdin, signals or process-group cleanup.
- Fixed-count loops: lifecycle publication and native-reset pane death now use
  Patience. Workload loops (clients, mutations, retries of idempotent operations,
  allocations) are finite test inputs. The orphan fixture's bounded best-effort
  kill sweep is cleanup, not a success observation.
- Fixed sleeps: delayed publication regressions, hook-chain stimuli, negative
  liveness observations, store age/mtime and debounce measurements stay literal.
  The gate-progress readiness sleep is replaced by a marker observation. Socket
  test staging sleeps do not certify readiness; serving is established by the
  fixture and later response assertions provide the evidence.
- SQLite busy timeouts and the schema busy-handler retry limit are fixture
  inputs. Test policy must not silently change the behavior they configure.
- The prefix write-lock release receive and fixture file/stdin barriers have
  explicit owners. The positive acquisition receive is now bounded and graced.
- Aliased elapsed comparisons were checked separately: deadline proofs retain
  their production or fixture owner. Whole-operation patience, such as the
  lost-reply milestone and hook chain, is no longer an idle-only assertion.

## Regression fence

`tests/timing_assertions/waits.rs` derives its corpus from `git ls-files`; nested
modules and shared support must be present. It masks comments and Rust literals
before finding deadline construction/comparison, elapsed comparisons, and
bounded calls. Whitespace-normalized expressions and occurrence counts must
equal the inventory in `waits.json`. Each retained expression needs a proof,
delegation, or fixture-timing classification and a reason. No file-wide waiver
is permitted. A removed expression also requires removing its stale inventory
entry.

This fence is a lexical review aid, not a control-flow proof. Aliases, generated
fixture programs, fixed retry counts, sleeps, and production timeout inputs
need the manual census above. In particular,
do not grace a fixture's delay merely because it uses the same `Duration` type
as the harness bound around it.

## Evidence

The extracted legacy readiness receive failed the controlled rising-contention
test after its idle allowance. It also lacked the required disconnect wording
and patience diagnostics. All three regressions pass after the shared readiness
fix. Sampler injection is local to the test; there is no environment override.

The shared helper also tests that a value already ready wins with zero remaining
time. The subscriber wrapper has the same regression against its old
clock-before-observation loop. Existing explicit child and PTY timeout tests
remain literal so they continue to prove enforcement and cleanup.


The source fence was mutation-tested through its real tracked-file entry point:
an added raw deadline failed, an extra retained occurrence failed, and restoring
the source and inventory passed. Shared support, subscriber and worker regressions
also cover ready-before-expiry and buffered-before-disconnect behavior. Direct
integration tests exercise the migrated production flows; the central verifier
owns the full suite.

## Remediation after the SH-827 merge

The merged base adds two patience sites: the journal-warning readiness loop in
`activity_log` and the fixture tmux completion in `verification_queue/reconcile_hold`.
Both now use shared load grace. The latter module uses `RECEIVE_POLL` only as
a receive quantum inside an outer `Patience`; the exact inventory records it
as fixture timing. The source fence caught all three new expressions.

The verifier also found a fake child pane that ignored its configured lifetime.
The merge includes the landed SH-819 repair and its behavioral regression: parent
and child share the graced lifetime, and the parent reaps the child. The regression
failed on this branch before the merge. No notify guard was relaxed.

The reported selective-gate helper timeout was outside the Rust source scan:
`tests/support/selective_receipt.py` bounded fixture commands at 30 seconds. It
now samples shared Python load grace for each command. A deterministic regression
covers unknown, idle, busy and capped load, and preserves timeout errors. The
`selective_gate` target runs that regression and the production receipt scenarios.

## Second verification remediation

The concurrency test used a baseline-times-four plus 500 ms assertion. The gate
measured a 10 ms baseline and a 588 ms concurrent list, so it failed despite the
30-second hook still being active. A one-second delayed observer reproduces that
false failure. The test now holds the hook until list returns, releases it, and
requires its successful release marker. The hook uses the production timeout
ceiling unchanged; client and readiness allowances use load grace.

The plugin fixture daemon died before readiness. macOS recorded a Gatekeeper
rejection for the exact PID and mutable Cargo artifact path. The fixture now
selects `story_binary()` for both direct execution and packaged copies. A
regression rejects bypassing the shared binary lease. This does not retry a
killed process or change OS security policy; an OS rejection still fails loudly.

## Third verification investigation

Merge tree `994c5aa395916292a3ae6633519b5dad3f5ef3e0` failed one
`handoff_endpoint` request write with macOS `ENOTCONN`. The target finished in
0.73 seconds, which excludes expiry of the server's 30-second peer allowance.
The report did not identify which request failed: arm, redeem, or cookie use.
The relevant server, API, and test-support code matches this branch.

The nine endpoint tests passed 100 consecutive runs with temporary server read
error tracing (900 test executions). OS logs identify the process and listener
but do not establish the cause of the loopback failure. The temporary server
tracing was removed. The disconnect remains unexplained; these passes do not
prove it is fixed.

Request-write failures now report the method, route, original I/O error and both
socket endpoints captured before the write. They exclude headers, tokens and
coupons. A real socket with its write side shut down proves the old diagnostic
omits the route and verifies the new diagnostic preserves context without
printing credentials. This is a diagnostic regression, not a reproduction of
the original disconnect. Requests are not retried: a failed write can already
have delivered bytes, and coupon redemption is single-use. No production
transport behavior or deadline changed.

## Lib tests and production subprocess bounds (SH-836)

### Why

The census above reads `tests/` and the shared test support. Five lib unit
tests in `src/` went RED together in the SH-822 central gate (load about 38 on
10 cores, utility QoS) because a fake `tmux` that answered at once missed the
production `TMUX_TIMEOUT` (3 s): the capture clock includes the spawn, and a
starved spawn missed it. The bound was named inside production code
(`ShellDispatcher`, adoption, the census), so no scan of the test could see it
and the test could not grace it.

### As built

- **One read point.** Every production site that holds a subprocess to a bound
  a lib test can reach reads it through `Environment::subprocess_bound(production)`.
  A shipped build returns `production`. Routed today: every per-call tmux bound
  (`TMUX_TIMEOUT`: engine probe, kill and census, adoption, `resources::tmux`,
  story-reset kill-window, the restart sweep's shared deadline), the python3
  artifact guards that borrow it, hygiene's `git ls-files`
  (`TRACKED_CHECK_DEADLINE`), `CAPABILITIES_TIMEOUT`, the dropped-cleanup
  identity read and the continuation submission reads. Leaves take
  `&Environment` from a caller that already holds one; the engine's per-call
  cap and the SH-809 shared deadline travel together as `TmuxBudget`.
- **A declared policy, carried on the `Environment`** (council D1 on SH-836, in
  its comments). A lib test builds its `Environment` with
  `.with_subprocess_patience()` when it waits for an answer (each bound is
  graced by `load_grace::graced_by` at one contention reading taken then) or
  `.with_subprocess_proof()` when it proves the production bound.
  `.with_subprocess_patience_under(stated)` lets a regression prove grace at
  idle. The declaration travels with the `Environment`, so a daemon or worker
  thread that production hands a clone applies it too. It follows the
  existing `#[cfg(test)]` builders on `Environment` (`with_test_verifier_mirror`).
- **Undeclared fails.** An `Environment` a lib test builds starts undeclared;
  reading a routed bound through it panics, naming SH-836, the bound, the test
  thread and both builders. The next test that reaches a routed bound states
  its intent on its first run, at idle.
- **One reading per declaration** (decision D2): a test derives deadlines and
  fixture delays from the bound, and production must read the same bound after
  it. Each test declares afresh.
- **The census** (`tests/timing_assertions/src_bounds.rs`, `src_bounds.json`):
  every production `run_captured*` bound not read through `subprocess_bound` is
  `delegated` or `unrouted` with a reason; every raw mention of a routed
  constant is a bypass; every `run_captured*` bound in `src/` test code that is
  not graced or routed is a `proof`, `fixture` or `delegated` local.
- **Regression**: `src/service/engine/tmux_grace_tests.rs` makes each fake
  answer after 1.5 × `TMUX_TIMEOUT`. Under proof it is not waited for; under
  patience at a stated contention of 3 it is; a slow wrong answer still gives
  the wrong verdict.

### Limits, named

- Code a lib test reaches through `storyhook_test_support` links a non-test
  build of storyhook, and integration tests in `tests/` run the non-test lib:
  both read the production value, with no grace and no panic. SH-846 owns the
  census of those and of the raw harness waits in `src/` test code.
- A panic on a thread production spawns can be swallowed by a `catch_unwind`
  (`api/rpc.rs`, `daemon/serve.rs`, `verification/batch_preview.rs`) or a
  discarded join. Carrying the declaration on the `Environment` means a
  declared test is graced there too; an undeclared read there is loud only as
  far as that thread's failure reaches the test.
- The census is lexical: an alias of a routed constant (`claim_comment.rs`'s
  `TMUX_PROBE_TIMEOUT`, whose test passes whether or not tmux answers) or a
  helper that wraps a `run_captured*` call is only as visible as that call.
- One reading cannot follow a burst that starts after the declaration: the
  `graced_now` limit above. A capture the bound already killed cannot be
  extended.


## Constructor bounds and CLI fixtures (SH-863)

The raw-wait census now checks the idle, control and termination arguments of
`ShellVerificationActuator::with_paths_and_timing` separately. An argument must
use `load_grace::graced_now` or have an exact classification in `waits.json`.
Grace on one argument does not exempt the others. The retry and withdrawal
fixtures wait for results; their constructor budgets receive grace. Capture
completion uses one graced local duration. Process-group timeout proofs retain
their driven deadlines and named classifications. The retry callback's generated
Python socket deadline receives grace before the program is written. Terminal
retry failures include the tick, incident, activity, owned processes and journal.

The sibling census found `ExchangeBound::After(DRIVEN)` calls in daemon-timeout
experiments: those are the deadlines under test and stay literal. The progress
writer experiment derives its publication cadence from the same graced idle
budget it gives the actuator. This remains a lexical census, not data-flow
analysis: aliases, wrappers and generated programs still require review.

Churn classification is a test-only measurement component driven by elapsed
observations. Deterministic tests cover clean early results, the exact stale
threshold on both sides of publication, delayed results after a pause, clean
stretch completion and budget exhaustion. The live adapter still publishes real
in-flight records and receives real client results. A pause invalidates the
whole attempt; later activity never turns that attempt into a pass.

### Explicit patience across the CLI boundary

Plugin tests build with `cargo build --features test-seam`. An owning `lib.sh`
fixture clears any inherited `STORYHOOK_TEST_SUBPROCESS_PATIENCE_MS`, computes a
30-second base allowance through `scripts/tests/load_grace.py`, reports its
contention reading, and exports the allowance in milliseconds before starting
the fixture daemon. Nested fixtures sharing that daemon retain its declaration.

`Environment::from_process` accepts a positive integer in `1..=900000` only in a
`test-seam` build. Invalid values and attempts to enable it in a default build
fail with a named error. An absent declaration preserves production policy.
`Environment::at` does not read this ambient setting. The resolved declaration
survives clones and `child_vars`; routed subprocess calls use the greater of the
production bound and the declared floor. Existing lib-test proof/patience
policies remain authoritative and are not widened by this CLI floor.

Production deadlines, cancellation and resource-query failure semantics are
unchanged. An unanswered tmux query still refuses mutation. The plugin regression
injects a four-second resource answer: declared patience permits safe unclaim;
without it the three-second timeout preserves the claim, worktree and branch.
A test that deliberately proves a CLI timeout must establish its declaration
before daemon startup, or stop its own daemon before changing it.

The ceiling applies to each subprocess. Sampling once cannot account for a
later load burst or guarantee scheduling; diagnostics remain necessary. This
floor reaches only bounds routed through `Environment::subprocess_bound`.

### Returned-gate corrections

The native fixture probe allowance is 30 seconds before load grace. A measured
load below one does not guarantee a three-second shell/Python startup, especially
under utility QoS. The delayed-resource regression uses the harness declaration
itself; removing that declaration still proves the production timeout and safe
refusal.

A complete retry or six-scenario submission fixture may use the existing
15-minute harness ceiling. Retry gate startup and actuator idle each have a
120-second base; control helpers have 30 seconds. These phase limits still
fail independently. Readiness failures include activity, incident and journal
state. The generated gate-release wait also receives grace.

The process-group capture regression starts its driven 100 ms only after a
resistant descendant publishes readiness. A 200 ms startup delay is an explicit
control. Both the normal wrapper and the readiness-driven test use the same
quiescent capture routing. The production wrapper retains its original absolute
timeout. No production deadline or cancellation behavior changes.

## The window proof after a release, and answer evidence (SH-840)

`test-unclaim.sh` failed once at contention 0.89 (PR 885, tree 4257f68c) with
only `ok: false`. The other assertions in that case passed. In `cmd_unclaim`,
that answer comes only from `RELEASE_WINDOW_ERROR`: the claim release
succeeded, and then `_close_story_window` could not prove that the window was
absent. That fixture has no pane, so only the post-release resource inventory
could fail. At that tree, the inventory ran tmux with the fixed 3-second
`TMUX_TIMEOUT`. The CLI floor above now covers that inventory. One local run
confirmed the attribution: a real 4-second delay, armed after the release and
with no declared floor, gives the same answer. The cause is deduced, not
observed, because the gate log lost the answer.

### As built

- `plugins/story/tests/fakes/tmux`: the resource inventory fails with the text
  of `$STATE/resource_fail` when that file exists.
- `fakes/story-post-release-fault`: a `STORY_BIN` proxy runs the real
  `unclaim` and arms that fault only after a successful release.
  `test-unclaim.sh` uses it to pin the SH-840 answer without timing: the
  release stands, the failed step and its cause are named, and nothing on disk
  changes. A delay cannot do this: the two inventories before the release would
  then race the same production deadline. `test-subprocess-patience.sh` also
  names the post-release proof in its patient case.
- `assert_ok <answer> <expected> <label>` (`lib.sh`) prints the whole answer
  when the top-level `.ok` is wrong. A pass is the same as before. All ok
  assertions in the plugin suite and in the `tests/support/protect_*.rs` shell
  fixtures use it (council D1 on SH-840).
- `test-answer-assertions.sh` proves the helper. It also fails the leg on any
  tracked `assert_eq` over the `.ok` field read by `jqf`, and its message gives
  the one-line `sed` fix.

### Limits, named

The guard covers the top-level `.ok` verdict only. About 1000 other field-only
`jqf` assertions remain; when `ok` is correct, they still print only their
field. The sweep shortens diagnosis. It does not prevent a load-dependent
failure.

### Fake server publication after SH-825 (SH-876, fixed in SH-840)

SH-825 commit 90a4a55a takes a caller's tmux socket as a pure parse of `$TMUX`.
Before it, the helper called `tmux display-message -p '#{socket_path}'` before
every resource inventory. That call was the only place where `fakes/tmux`
published its server model: the socket, the caller's `FAKE_TMUX_PANES` rows and
the worktree directories. The daemon reads that model with an environment that
has no `FAKE_*` knob. Dev b904c137 therefore failed 16 plugin scripts and one
`plugin_install` case. It reached dev through a manual verifier override.

As built:

- `lib.sh` publishes the fake server before each helper run. A `bash` wrapper
  (the `git` wrapper's pattern) catches `bash "$SCRIPT"` and installed
  `.../story.sh` copies. It publishes only when the server answers with this
  fixture's own socket. It links `$TMUX_TMPDIR/tmux-<uid>/default` to that
  socket for callers outside tmux, only inside the test home, and only over an
  earlier link. The link stays for the rest of the test, because
  `env ... bash "$SCRIPT"` runs and daemon work after a run still use it.
  `_register_tmp_tmux_session` removes it before a test starts a real server
  on that path. It removes only a link to a regular file, never a link to a
  real socket.
- Every `lib.sh` instance keeps `TMUX_TMPDIR` inside `STORYHOOK_TEST_HOME`. A
  nested instance under a harness that clears its environment had asked the
  machine's real default server.
- `fakes/tmux` refuses an inventory of a server that nobody published or
  seeded, and its message names the fix. A published or seeded empty server
  is still an empty inventory.
- `test-fake-tmux-state.sh` pins the publication without a production probe:
  socket, panes, default link, no link to a real server, no replacement of a
  non-link, nothing outside the home, and the loud refusal.

- Three tests that the removed probe had hidden were repaired. In
  `test-dispatch-plugin-binding.sh`, `$TMUX` now names the case's own fake
  server. In `test-dispatch-lane-budget.sh`, each engine lane gets its own
  `TMUX_TMPDIR` and default server. `test_tmux_server_env.py` now composes the
  verification view with `view_program`. Its hand-made copy had no
  `process_observation`.

Limit: entry points that run the helper without `bash` from a `lib.sh` shell
(an `exec`, or a daemon-run dispatch script) do not publish. They reach the fake
only after an earlier publication, a seeded `windows` file, or their own
publication (as `test-dispatch-lane-budget.sh` does). If they do not, the fake
refuses loudly.
