# SH-794: merge-gate fixture cost and coverage audit

Baseline source: `3b6cc7c6c339caccd51695b402abc8305b2dd2a5` (v3.0.3).
Historical comparison: `8bd307cfc16c6b95b17a20d3c371c0bca9e52c1c`,
the last first-parent revision on 2026-09-08 in this checkout's ancestry.
That revision has 33 tests; this baseline has 71. The story's 66-test count
was the earlier SH-783 snapshot. All 33 historical names survive. This date-matched source anchor is not claimed
to be the unidentified exact commit behind the story's original 8.6 s log.

## Decision and boundaries

All 71 existing tests and all their parameterized cases are retained. No complete
execution was proven identical; superficially similar cases test different
entry points, state transitions, ownership boundaries, or evidence channels.
The table records those distinctions. SH-793 owns consolidation of the Python
orphan regression into the retained Rust orphan case and its cancellation
repair. SH-795 owns integration binary/link cost; neither is duplicated here.

Four CPU spins were replaced with a Python leaf using `signal.pause()` or
blocking Unix-socket receive. The replacements keep default signal deaths, the explicit
four-second delayed handler, atomic PID publication, and the hook's verifier
ancestry lookup. Only an explicit release byte lets the hook succeed; EOF and
invalid bytes fail loudly. Parent waits use existing load grace and ChildGuard.
No production code, public API, timeout, scheduling class, or concurrency
budget changes. Three helper regressions are added in the same test binary.

## Coverage census

Every row is **KEEP**. `old` identifies the September 8 cohort. All fixtures
use independent real Git repositories and commits. Costs below name extra
work: P = production merge-preflight; S = production speculative-run;
V = verification-gate chain (locks, supervision, progress and cleanup);
writer = production enrollment/receipt writer. Multipliers count scenario
invocations, not every subprocess inside them. Mocked GitHub responses are
external data; local Git, production scripts and receipt behavior remain real.

| Test | Cohort | Cases / observable | Production path / extra cost | Why coverage stays |
|---|---|---|---|---|
| `speculative_run_preserves_clean_shared_poller_when_base_ref_advances` | old | Advanced symbolic base; two speculative runs, clean shared index after each | S×2, P×2 | Ref movement while cleaning differs from ordinary restoration |
| `verifier_distinguishes_poller_preparation_failure_from_test_failure` | old | Dirty poller refusal before command; clean exit 7 and 0 controls | V×3 | Preparation failure remains distinct from judged failure |
| `a_gate_is_refused_before_it_starts_while_its_measured_disk_is_not_free` | added | Insufficient recorded disk; no command or new measurement | V refused | Different admission branch from successful sampling |
| `a_gate_records_its_disk_use_and_keeps_the_last_five` | added | Keep last five disk samples; measure a real 2 MiB product | V | Retention and measurement differ from first-use behavior |
| `the_first_gate_runs_without_a_floor_and_leaves_the_first_measurement` | added | No prior sample permits gate and writes first measurement | V | Empty-history boundary |
| `a_gate_terminated_by_signal_is_infrastructure_never_red` | added | TERM to gate leaf; retryable infrastructure, no completion; subsequent admission | V, signal | Different recipient from terminating verifier |
| `a_terminated_verifier_reports_the_termination_and_leaves_no_completion_record` | added | TERM to verifier group; signal re-raised, one truthful verdict, no surviving leaf | V, signal | Different owner and completion cleanup path |
| `verifier_names_a_failure_before_hundreds_of_not_rerun_entries` | old | Decisive failure precedes 611 unknown entries; bounded report | V | Ordering, not compiler trust or count limits |
| `a_red_gate_whose_test_left_an_orphan_is_red_not_infrastructure` | added | Red gate leaves sleep orphan; reap and retain red, next gate admitted | V×2 | Keep for SH-793; its Python duplicate is owned there |
| `verifier_does_not_promote_test_output_to_compiler_diagnostics` | added | Plain and JSON compiler-looking test output stays test output | V | Untrusted channel cannot become compiler evidence |
| `verifier_bounds_each_diagnostic_class_without_changing_its_meaning` | old | 25 failures and 12 real compiler errors; caps 20/10; skips/reuse; fresh next attempt | V×2, real offline Cargo | Real adapter end-to-end and stale evidence differ from empty artifacts |
| `verifier_reports_unreadable_compiler_evidence_without_inventing_errors` | added | Unreadable compiler evidence reports capture failure, no invented errors | V | Read failure differs from empty/untrusted output |
| `foreign_gate_registers_its_exact_attempt_log_before_ordinary_output` | added | Two attempt IDs bind exact log inode before stdout/stderr; one binding each | V×2 | Attempt authentication differs from distinct filenames |
| `foreign_gate_lifecycle_is_running_inside_the_command_then_records_its_exit` | added | Gate-internal running snapshot then passed/failed for exits 0/9 | V×2 | Temporal lifecycle before/after execution |
| `foreign_gate_lifecycle_preserves_completed_exit_when_cleanup_is_signalled` | added | TERM during restoration preserves completed exit 0/9 and passed/failed | V×2, hook ancestry | Terminal execution evidence survives cleanup interruption |
| `verifier_reports_a_failed_gate_without_inventing_a_diagnosis` | old | Exit 7 without diagnostic evidence stays undiagnosed | V | Empty-output failure is not noisy bounded-report case |
| `verifier_holds_the_gate_across_the_complete_speculative_run` | old | Inner gate lock held; stdout/stderr journal; acquisition lifecycle | V | Nested lock identity and output forwarding |
| `same_tree_verification_attempts_keep_distinct_logs` | old | Same-tree attempts retain two logs and two empty compiler artifacts | V×2 | Evidence file ownership, not stale compiler summary |
| `verifier_preserves_tracked_edits_before_and_during_the_gate` | old | Before/during × staged/unstaged edits; refusal or retained recovery unit | V×4 | Four distinct index/worktree ownership states |
| `preflight_owns_and_cleans_speculative_objects_without_inserting_them_in_source` | old | Private predicted objects never enter source; owned scratch removed | P | Callee-owned object lifetime |
| `caller_owned_objects_remain_resolvable_and_source_owned_paths_are_refused` | old | Caller object lease remains usable; source object directory refused | P×2 | Opposite ownership boundary from callee cleanup |
| `speculative_run_uses_the_exact_tree_and_restores_after_success_or_failure` | old | Scrubbed environment; exact private commit/tree success; exit 42 restoration | S×3 | Keep isolation, success and failure independently |
| `speculative_git_directory_cannot_be_inferred_as_bare` | old | Reinitializing private git-dir cannot make source bare | S | Git directory classification regression |
| `speculative_run_accepts_the_symbolic_base_ref_used_by_the_verifier` | old | Symbolic base ref works through private administration | S | Production passes a ref, not only OID |
| `speculative_run_keeps_shared_worktree_refs_resolvable_while_gate_is_blocked` | old | Blocked private gate permits sibling fetch/pull/gc/fsck/repack/prune/bisect | S, real Git maintenance | Shared-ref reachability during execution |
| `speculative_run_recovers_a_poller_whose_private_head_is_unavailable` | old | Missing private HEAD recovered after object lease disappears | S | Broken administration recovery |
| `verifier_rebuilds_legacy_private_object_metadata_before_fetch` | old | Legacy broken metadata repair; healthy reuse clears cache; stale marker keeps index | worktree repair×3 | Three repair/reuse states |
| `speculative_run_refuses_invalid_cleanup_budgets_before_mutation` | added | Reject wat/-1/3999/100000000/1.5 cleanup budgets before mutation | S admission×5 | Boundary validation introduced after old cohort |
| `speculative_run_forwards_hup_and_term_and_cleans_before_reraising` | old | HUP/TERM forward; four-second handler; restored HEAD; private objects removed | S×2, intentional 8 s | Keep cancellation budget regression; replace spin only |
| `preflight_and_speculative_run_succeed_with_immutable_source_objects` | old | Immutable source objects permit preflight and speculative run (macOS) | P×2, S | Filesystem-enforced ownership, not only final absence |
| `an_uncertified_merge_tree_is_reported_as_uncertified_and_names_it` | old | Uncertified tree returns status 1 and names tree | P | Human-readable missing receipt contract |
| `certifying_the_predicted_tree_through_the_production_writer_clears_it` | old | Production enrollment and receipt writer certify predicted tree | P×2, writer | Producer-reader compatibility |
| `the_predicted_tree_matches_a_real_merges_tree_exactly` | old | Predicted merge tree equals actual real merge tree | P, real merge | Independent Git equality oracle |
| `a_branch_that_already_contains_main_is_certified_via_its_own_receipt` | old | Branch containing base uses its own qualifying receipt | P, writer | Fast-forward-shaped tree identity |
| `a_textual_conflict_is_reported_distinctly_and_prints_no_tree` | old | Text conflict yields no tree and conflict identities | P×2 | Text/JSON conflict protocols |
| `a_new_commit_after_certification_produces_an_uncertified_tree_again` | old | Commit after certification changes tree; old tree stays certified | P×3, writer | Receipt scope and non-invalidation |
| `a_changed_tier_receipt_does_not_certify_a_merge_even_for_the_exact_tree` | old | Exact merge with changed-tier receipt still refused | P×2, writer×2 | Tier sufficiency through real producer |
| `verifier_metadata_accepts_false_booleans_without_confusing_them_for_absence` | old | False booleans accepted; true draft/fork refused; string boolean and missing branch invalid | metadata×5 | Parsing distinct from PR refresh or recheck |
| `a_pull_ref_lagging_its_branch_is_retried_not_reported_as_a_conflict` | added | Pull ref lags branch, then convergence succeeds | real fetch/refresh | Branch authority differs from API disagreement |
| `a_pull_ref_disagreeing_with_the_api_is_retryable_not_permanent` | added | API disagrees with pull ref, retryable | real fetch/refresh | Distinct inconsistent projection |
| `an_agreed_head_proceeds_whether_or_not_it_conflicts` | added | Agreed clean/conflicting heads proceed; no branch tracking-ref mutation | real fetch/refresh, P | Convergence does not pre-judge conflict |
| `a_head_branch_absent_from_origin_is_an_invalid_submission` | added | Deleted head branch is invalid submission | real fetch/refresh | Missing authority differs from transient lag |
| `verifier_restart_recovers_only_a_certified_merge_on_the_current_base` | old | Restart: uncertified refused, certified merged ancestry accepted, wrong base refused | recovery×3, writer | Restart entry boundary |
| `landing_refusal_recovers_only_the_certified_actual_merged_tree` | old | Landing refusal: actual merge gate/changed receipt accepted/refused | reconciliation×2, writer | Same tier rule at a different orchestration boundary |
| `landing_refusal_retries_only_a_new_tree_for_the_same_submission` | old | Unchanged certified tree retry; advanced base requests reverify without receipt | reconciliation×2, writer | Current-tree refusal versus new-tree retry |
| `landing_refusal_keeps_missing_proof_and_changed_identity_distinct` | old | Missing proof; PR/state/base/head change; metadata unavailable; fetch failure | reconciliation×7 | Distinct permanent, invalid and retryable branches |
| `landing_refusal_reports_a_conflict_in_the_refreshed_tree` | old | Refreshed base conflicts with head | reconciliation | Conflict produced during refusal refresh |
| `verifier_preserves_the_landing_scripts_terminal_classifications` | old | Landing exit 2/0/3 remains conflict/merged/invalid | classification×3 | Wire status conversion differs from real merge recovery |
| `a_pull_request_on_the_wrong_base_is_an_invalid_submission_before_any_gate_runs` | added | Wrong PR base refused before gate, one metadata read | public verifier | Integration-branch authority at entry |
| `missing_arguments_are_refused_with_a_usage_message` | old | Missing preflight arguments show usage | P refusal | CLI misuse boundary |
| `a_conflict_verdict_is_discarded_when_the_head_moves_before_it_is_posted` | added | Conflict discarded after API and branch head advance; old/new identities retained | public verifier | Race at conflict verdict |
| `a_conflict_verdict_is_discarded_when_only_the_branch_has_moved` | added | Conflict discarded when only authoritative branch moves | public verifier | Projection lag at verdict |
| `a_conflict_on_a_head_that_stayed_put_is_still_reported_as_a_conflict` | added | Stable head retains conflict after second metadata read | public verifier | Positive control for conflict-race guard |
| `a_red_verdict_is_discarded_when_the_head_moves_during_the_gate` | added | Red gate advances head; stale red discarded, log retained | public verifier, V | Race at post-gate verdict |
| `a_red_on_a_head_that_stayed_put_is_still_reported_as_tests_failed` | added | Stable head retains red with tree/log/status 3 | public verifier, V | Positive control for red-race guard |
| `a_recheck_that_finds_a_different_pr_or_a_closed_one_posts_no_verdict` | added | Recheck sees different PR or CLOSED; no stale conflict | public verifier×2 | Identity versus state change |
| `the_public_path_refuses_to_run_without_a_named_gate` | added | Missing gate argv refused before GitHub read | public verifier | Command contract before external metadata |
| `a_configured_gate_runs_with_the_argv_it_was_named_with` | added | Configured gate receives --ci/unit exactly and reports red | public verifier, V | argv forwarding on red execution |
| `a_configured_gate_that_exits_green_but_certifies_nothing_is_refused_before_landing` | added | Green gate without receipt: project fault, truthful lifecycle and remedy | public verifier, V | Success alone cannot certify or land |
| `structured_preflight_distinguishes_missing_insufficient_and_certified_receipts` | added | Missing/changed/other/CRLF/gate/full/legacy receipts, JSON and legacy interfaces | P×14 | Parsing compatibility matrix; forged bytes intentional here |
| `gate_configuration_comes_from_the_pinned_merge_not_the_registered_checkout` | added | Pinned merge configuration overrides broken checkout; public fault names pinned gate | snapshot, public verifier | Source-of-truth and public integration |
| `gate_snapshot_distinguishes_project_configuration_from_git_failure` | added | Invalid syntax/missing executable/non-executable; wrong-tree read failure | snapshot×6 | Project faults differ from Git evidence failure |
| `gate_snapshot_judges_a_configuration_longer_than_the_diagnostic_bound_whole` | added | 96 KiB valid whole-file hash; invalid suffix; >8 MiB answer refusal | snapshot×3, 9 MiB blob | Truncation boundaries and digest integrity |
| `gate_snapshot_resolves_only_committed_files_and_in_tree_symlinks` | added | Absent pointer defaults; committed in-tree links; absolute/parent/missing/cyclic refusals | snapshot×6 | Symlink trust matrix |
| `pinned_invalid_gate_is_a_project_fault_before_any_execution` | added | Invalid pinned gate refuses before execution artifacts | public certify phase | Fault must precede any gate |
| `structured_preflight_never_calls_reader_failures_missing_certification` | added | Unreadable receipt directory and missing ref are inspection errors | P×2 | Cannot classify failed reads as missing proof |
| `a_base_that_moves_during_the_gate_is_a_conflict_for_the_story_never_a_halt` | added | Base moves to conflicting tip during green gate; story conflict, no queue halt | public verifier, writer | Moving base after execution, not moving head |
| `a_configured_gate_that_certifies_through_the_production_writer_proceeds_to_landing` | added | Foreign gate production writer certifies and reaches landing | public verifier, writer | Positive control for missing-receipt refusal |
| `durable_landing_phase_checks_admitted_head_and_tree_before_sending_a_merge` | added | Wrong admitted head/tree do not send; exact merge then recovery sends once | landing×4, writer | Durable admission guard |
| `durable_landing_recovery_never_resends_and_requires_exact_merged_evidence` | added | Recover OPEN; lost merge reply; wrong tree/head; exactly one send | landing×4, writer | Uncertain recovery and lost response |
| `private_repair_admission_precedes_gate_and_fails_closed` | added | Deferred unchanged/budget; proceed; callback nonzero; malformed/unknown replies | public certify×6 | Admission callback fail-closed matrix |


## Changes behind the historical growth

These are verified source changes, not inferred timing multipliers. The
September 25 figure in SH-783 predates several current costs; the two periods
must not be collapsed into one explanation.

| Date / commit | Changed execution | Cases affected | Interpretation |
|---|---|---|---|
| September 11, `0aa6a8ad` (SH-683) | Python session owners, workspace recovery and durable mutation journals replace shell-only speculative state management | S/V paths, repair cases and public verifier paths in the census | More process launches, file/directory fsyncs and machine-wide session censuses per old test; required ownership guarantees |
| September 11, `bb2fc6c9` (SH-685) | Real offline Cargo compilation and structured adapter replace printed pretend compiler errors; second attempt checks artifact isolation | `verifier_bounds_each_diagnostic_class_without_changing_its_meaning` | This old test actually does more work; retaining it proves a real compiler-to-verdict integration |
| September 12, `3404c788`, `1a27f1fa`, `9907f962` | Gate/verifier signal cases, orphan reaping and completed-verdict preservation | Added signal, orphan and restoration cases; V cleanup | Added coverage plus supervision. Two leaf spins and a restoration spin start here |
| September 15–16, `f2ee7438`, `6864a0d0` | Origin-bound GitHub helpers and explicit credential/transport boundary; `5759975f` installs a Python Git endpoint adapter | PR refresh, public verification and landing cases | Formerly direct local remotes now pay wrapper startup per Git invocation plus real CLI/authority work, even though remote responses are fixtures |
| September 26, `fb7df7bb` (SH-785) | Gate subprocesses use utility QoS on macOS | S/V and public gate commands | Current runs intentionally yield under load; this cannot explain the September 25 number |
| September 28, `71aee001` (SH-826) | HUP and TERM fixture cleanup each delays four seconds | `speculative_run_forwards_hup_and_term_and_cleans_before_reraising` | Eight seconds of deliberate serial delay; retained to exceed the old two-second wrapper grace |
| September 28, `bbb8453f` (SH-822) | Disk admission and post-gate measurement | Added disk cases and every V path | Required production checks; postdates original 134 s report |
| September 29, `1acfcc23` (SH-844) | Each preflight uses fresh bare administration and empty attribute source | Every P, plus embedded preflights in S/V/landing | More Git processes and scratch files to exclude local merge attributes; also postdates original report |

The immutable-source, caller-owned/callee-owned objects, metadata parsing,
receipt-reader, and CLI-refusal cases have no need for a gate session. They
are controls when comparing common-cohort timings: blanket claims that every
test pays the same supervision cost are false. New gate configuration, durable
landing and repair-admission cases are additions, not slower executions of the
old 33 tests. The census identifies their exact parameter matrices.

The signal helper's existing spin predates September 8; this story removes
that spin as well as the three introduced later. The four-second delay was
added much later and is a separate cost from CPU spinning.

## New fixture regression coverage

| Test in `blocking_fixture` | Observable |
|---|---|
| `signal_waiter_is_a_leaf_and_ready_for_hup_and_term` | Atomically published PID is the child itself, no descendants, default HUP/TERM signal death after readiness |
| `barrier_requires_release_and_refuses_eof_or_invalid_bytes` | Published verifier PID, alive before release, explicit success byte, EOF and invalid byte failures |
| `merge_gate_has_no_shell_busy_waits` | Narrow source guard catches all four original shell spins |

The first run of these tests against the stub/helper-free fixture was RED:
0/3 passed, and the source guard named all four spins. The existing four
production signal/restoration tests remain the end-to-end regression oracles.


## Measurement method

The benchmark selects the same 71 existing cases in both current executables:
`--test-threads=4 --skip blocking_fixture::` for three whole-target runs each,
then each listed case with `--exact NAME --test-threads=1`. The three new
helper regressions are validated separately; they do not inflate the timing
comparison. The old 33-case executable is built from a `git archive` of the
historical revision using `cargo test --locked --offline --test merge_gate
--no-run`; it runs each old case once. No historical tracked source is edited.
Each exact-case measurement starts a fresh test process, including its one-time
fixture-binary lease and scratch-root setup. Summing these values does not
predict a shared-process, four-thread suite: setup is then shared and test
work overlaps. The historical harness also lacks the newer leased StoryHook
binary used by origin-bound CLI calls.

Builds and Clippy finish before the comparison starts. No other suites are
run by this lane during measurement; other sessions' work remains ambient.

The runner sources the current `scripts/test-env.sh`, creates a fresh `/tmp`
root for each phase, and calls `storyhook_isolate`. Current before/after runs
pin `CARGO_BIN_EXE_story` to the same frozen baseline production binary.
The Rust test executables are copied to stable paths beside their original
Cargo artifact. All Git fixtures use the normal scratch directory policy.
The baseline executable includes the new helper module but filters it out;
its 71 existing test bodies retain the original spin programs. Compilation
and linking are outside every recorded execution interval.

Wall time uses Python `time.monotonic`; child user/system CPU uses differences
of `resource.getrusage(RUSAGE_CHILDREN)` before spawn and after wait. A sampler
records one-minute load every second. Commands, statuses, load summaries and
unrounded measurements are kept in the accompanying data file. CPU covers
reaped children, not total host CPU. These are measurements, not test ceilings.
Python documents the accounting semantics in
[Resource usage](https://docs.python.org/3/library/resource.html).

| Environment | Observed value |
|---|---|
| Measurement date | 2026-10-02 |
| Host | macOS 26.6.2, build 25G83; 10 logical cores |
| Rust | rustc 1.98.0 (88d9e12ae); cargo 1.98.0 (797e8a9bc) |
| Git | Apple Git 2.54.0 (Apple Git-157) |
| Python | Homebrew Python 3.14.7 |
| Current baseline | `3b6cc7c6` |
| Fixture repair | `17f8c020` |
| Historical source | `8bd307cf`, package version 2.4.2 |

Two exploratory runs are excluded: 431.697 s overlapped this lane's regression
build; 303.208 s ran the newly compiled 74-case binary including the deliberately
RED source guard (73 passed, only that guard failed). They exposed why the
measured executable and selection must be frozen. Neither is a valid speedup
baseline. Earlier sandboxed compilation also could not open the machine-wide
compiler slots; it was stopped and restarted with the required access. Cached
Cargo stderr later replayed those messages. They are not test failures or
measurement samples.

## Whole-target results

All six samples passed the same 71 cases, with four test threads. Times are
seconds; load is the sampled one-minute host load on ten logical cores.
Raw commands and unrounded values are in
[sh-794-timings.jsonl](data/sh-794-timings.jsonl).

| Fixture | Run | Wall | User CPU | System CPU | Load min / mean / max |
|---|---:|---:|---:|---:|---|
| Original | 1 | 678.003 | 268.734 | 227.098 | 80.56 / 113.50 / 153.22 |
| Original | 2 | 334.374 | 233.420 | 192.073 | 43.51 / 67.62 / 102.48 |
| Original | 3 | 221.294 | 195.073 | 163.800 | 31.42 / 42.75 / 58.58 |
| Blocking | 1 | 350.960 | 218.574 | 184.155 | 29.85 / 47.94 / 63.66 |
| Blocking | 2 | 254.421 | 204.534 | 177.588 | 26.80 / 34.85 / 52.47 |
| Blocking | 3 | 185.065 | 193.809 | 155.700 | 19.14 / 25.69 / 41.24 |

| Metric | Original median (range) | Blocking median (range) |
|---|---|---|
| Wall | 334.374 (221.294–678.003) | 254.421 (185.065–350.960) |
| User CPU | 233.420 (195.073–268.734) | 204.534 (193.809–218.574) |
| System CPU | 192.073 (163.800–227.098) | 177.588 (155.700–184.155) |
| Total CPU | 425.494 (358.873–495.832) | 382.123 (349.510–402.729) |

The observations improve, but **do not establish a causal whole-target
speedup**. The three original runs preceded the three final runs; load fell
through both blocks. Even the unchanged baseline varied more than threefold.
Process-census size, storage contention, and utility QoS also change costs;
the load average is context, not a correction factor. The isolated leaf probe
below directly demonstrates the removed CPU waste. A quiet-host or interleaved
repeat would be needed to estimate a reliable end-to-end effect size; it is
not required to establish that a blocking leaf no longer spins.

## Individual cases and historical comparison

All 71 original and 71 blocking-fixture exact cases passed. The unmodified
September 8 archive also passed all 33 exact cases. The
[complete case table](data/sh-794-cases.csv) joins each name to its cohort,
historical wall/CPU, current original wall/CPU, and current blocking wall/CPU.
The raw JSON retains separate user/system time, load, command and exit status.
Blank historical cells mean the case did not yet exist, not zero cost.

| Source / cohort | Cases | Sum of isolated wall | Sum of child CPU | Median case wall | Median case CPU |
|---|---:|---:|---:|---:|---:|
| September 8 source, old cohort | 33 | 79.895 | 45.155 | 1.552 | 1.271 |
| Current original, old cohort | 33 | 181.763 | 87.793 | 3.360 | 2.079 |
| Current blocking, old cohort | 33 | 205.020 | 94.854 | 3.876 | 2.059 |
| Current original, added cohort | 38 | 535.128 | 251.885 | 6.049 | 3.646 |
| Current blocking, added cohort | 38 | 513.089 | 260.841 | 7.144 | 4.192 |

These sums describe separate, single-case process invocations. They are not
the four-thread target wall time. Added cases account for 74.15% of original
and 73.33% of final isolated CPU. The median of the 33 individual current/old
CPU ratios is 1.93 for the original fixture and 1.90 for the blocking fixture.
This is evidence of changed work, not a controlled estimate of the original
September regression: toolchains, fixture setup, load and assertions differ.
Old-cohort load ranged from 11.76–54.68 historically, 12.37–50.77 for the
current original, and 12.73–52.95 for current blocking. Added-case runs reached
87.21 and 143.35 respectively. Similar ranges do not imply matched load per case.

The source history explains distinct changes rather than one universal
multiplier. Selected examples below show wall / CPU seconds; the complete
name-by-name measurements remain in the CSV.

| Test | September 8 | Current original | Current blocking | Source evidence |
|---|---:|---:|---:|---|
| `missing_arguments_are_refused_with_a_usage_message` | 0.204 / 0.170 | 0.343 / 0.203 | 0.229 / 0.197 | CLI refusal does not enter supervision |
| `the_predicted_tree_matches_a_real_merges_tree_exactly` | 0.845 / 0.585 | 0.609 / 0.521 | 2.501 / 1.005 | Real Git/preflight control; no universal eightfold change |
| `speculative_run_forwards_hup_and_term_and_cleans_before_reraising` | 1.491 / 1.185 | 14.948 / 9.539 | 14.094 / 8.854 | New supervision plus eight intentional seconds of cleanup delay; both retained |
| `verifier_bounds_each_diagnostic_class_without_changing_its_meaning` | 2.086 / 1.436 | 6.669 / 4.090 | 18.495 / 7.556 | Real compiler adapter and second attempt replace printed diagnostics; also sensitive to compiler-slot contention |
| `speculative_run_uses_the_exact_tree_and_restores_after_success_or_failure` | 2.110 / 1.721 | 11.010 / 4.909 | 7.457 / 4.048 | Three runs now traverse durable session/recovery machinery |
| `landing_refusal_retries_only_a_new_tree_for_the_same_submission` | 1.770 / 1.429 | 9.346 / 4.848 | 10.410 / 5.540 | Origin-bound authority and Python Git adapter added |
| `verifier_preserves_tracked_edits_before_and_during_the_gate` | 8.666 / 5.523 | 15.174 / 7.469 | 17.142 / 9.472 | Four ownership states now retain durable recovery evidence |

The diagnostic integration still compiles its scratch crate inside the test;
that is the behavior under audit. Only building/linking the enclosing test
executable is excluded from measurements. Unchanged cases sometimes grew
between current original and final measurements while others shrank. This
reinforces the limit on attributing their timing differences to the four
fixture replacements. The report does not claim that every old test got
slower, or reconstruct the exact 8.6-to-134-second change from unrelated runs.

### Repeated dominant cases

After the primary pass, the largest original wall-time case and the
largest original CPU case were repeated once per current executable. Both pairs passed.
The CSV and raw data retain first and repeat samples; no slow value is replaced.

| Case | Fixture | First wall / CPU | Repeat wall / CPU | Mean load, first / repeat |
|---|---|---:|---:|---:|
| Production-writer gate reaches landing | Original | 111.476 / 16.841 | 42.170 / 21.810 | 66.75 / 33.93 |
| Production-writer gate reaches landing | Blocking | 26.111 / 13.017 | 40.881 / 21.746 | 119.09 / 29.20 |
| Private repair admission, six outcomes | Original | 64.041 / 41.143 | 63.792 / 40.766 | 29.81 / 19.06 |
| Private repair admission, six outcomes | Blocking | 70.644 / 44.925 | 54.086 / 36.409 | 27.44 / 9.51 |

The first case is
`a_configured_gate_that_certifies_through_the_production_writer_proceeds_to_landing`;
the second is `private_repair_admission_precedes_gate_and_fails_closed`.
Neither uses a changed waiting fixture. Their spread is direct evidence
against assigning every before/after difference to this change. The repeat
selection and interpretation were recorded on SH-794 before execution.
Python's [timing guidance](https://docs.python.org/3/library/timeit.html#timeit.Timer.repeat)
also warns that competing processes affect measurements and recommends
examining repeated observations. These integration measurements retain the
complete observations rather than selecting a best-case speedup.

## Isolated leaf CPU evidence

Three paired probes hold the original shell spin and the replacement signal
leaf ready for four seconds, then TERM and reap them. Startup is included;
this deliberately makes the Python helper pay its import cost. The processes
run sequentially and consume no production data.

| Waiter | Median CPU (user + system) | CPU range | Median wall | Load range |
|---|---:|---:|---:|---:|
| Original shell spin | 0.810042 s | 0.791152–2.147102 s | 4.227 s | 103.36–109.73 |
| Blocking Python leaf | 0.053062 s | 0.049421–0.059957 s | 4.344 s | 103.36–109.73 |

Median CPU fell **93.45%** in this controlled probe. Its wall time stays about
four seconds because waiting is the requested behavior. This demonstrates the
removed CPU waste; it is not a claim of a 93% end-to-end suite improvement.

Raw pairs are in [sh-794-leaf-cpu.jsonl](data/sh-794-leaf-cpu.jsonl).
Each pair runs the spin first, then the blocking leaf. These probes also ran
under contention, so the wall difference includes scheduling and startup.
The helper uses the standard blocking operations documented by Python:
[signal.pause](https://docs.python.org/3/library/signal.html#signal.pause) and
[socket.recv](https://docs.python.org/3/library/socket.html#socket.socket.recv).

## Focused validation

| Check | Result |
|---|---|
| New helper regressions against the original fixture/stub | RED, 0/3; source guard found all four spins |
| Three helper checks and four changed signal/restoration cases | GREEN, 7/7 |
| `fixture_isolation` | GREEN, 5/5 |
| `timing_assertions` | GREEN, 28/28 |
| `verifier_fixture_hygiene` | GREEN, 6/6 |
| `cargo clippy --test merge_gate -- -D warnings` | Clean |
| `cargo fmt --check` and `git diff --check` | Clean |
| Audit census versus executable listing | All 71 existing names exactly once; 33 old, 38 added |
| Measurement data versus captured test logs | 185 successful runs: six 71-case targets, 175 single cases, four repeats |

The three policy targets were rerun after the helper sources were committed;
all 39 checks passed against the final tracked inventory. This matters because
their source scans use `git ls-files` and cannot prove coverage of an untracked
helper. No builds or extra test targets overlapped the final timed measurements.

The full repository suite belongs to central verification. No production
supervisor, test concurrency, or receipt policy was changed by SH-794.
