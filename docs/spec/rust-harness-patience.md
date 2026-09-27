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

The sweep covers 390 tracked Rust files recursively under `tests/` and
`crates/storyhook-test-support/`, including nested modules. The exact retained
inventory is `tests/timing_assertions/waits.json`: 64 expressions at 81 sites
(45 proof, 20 delegated, 16 fixture). These counts describe the lexical fence,
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
| `daemon_concurrency::wait_for` | Slow request becomes visible | Caller duration | Patience via `wait_for` |
| `daemon_concurrency::a_slow_command_does_not_block_another_client` | Baseline and concurrent commands complete | 15 s, `HOOK_SLEEP_SECS` | Patience via `graced_now`; elapsed-time comparisons unchanged |
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
