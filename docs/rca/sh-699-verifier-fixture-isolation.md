# SH-699: verifier fixtures lost terminal-mirror isolation

- **Date**: 2026-09-12
- **Severity/Impact**: SH-699 reported 14 fixture banner windows and two day-old test-binary activity readers in the operator's `storyhook-verifier` tmux session. This investigation reproduced the isolation defects; it did not repeat that historical resource census. No data loss was reported.
- **Status**: Implemented locally in `a82829ce3` and `bbd74a2eb` on baseline `db827a0224a02e2f666fa0c095b4c29f20f0a304`. Continuation merge `33d82f770` integrates SH-698's landed prerequisite repairs. All adopted scope and directly impacted checks are complete; central verification remains pending.

## Summary

Verifier tests could create persistent tmux readers outside their fixture lifetime even when the test environment disabled mirroring. The verification allowlist discarded the disabling variable, and `Environment::at` isolated filesystem paths without resolving terminal-mirror policy. The repair makes policy part of `Environment`, propagates it through child boundaries, and gives explicitly enabled tmux fixtures ownership of a private foreground server. Production windows remain persistent as specified; the durable lesson is that isolating paths does not isolate child-process behavior or resource lifetime.

## Timeline

| Date | Evidence and event |
|---|---|
| 2026-08-30 | Commit `c0b092fd1a31c001a3886f2b08d8e8e190af0367` (SH-521) introduced centralized verification and its narrow extra-variable allowlist. This anchors the relevant boundary; it does not establish a verified last-known-good revision for the later mirror behavior. |
| Before this investigation | SH-545 and SH-662 added mirror capabilities and persistent per-project windows without adding mirror policy to that verification boundary. |
| 2026-09-11–12 | SH-699's discussion reported retained fixture windows and activity readers. Historical PIDs were not used as fresh reproduction evidence. |
| 2026-09-12 | On baseline `db827a0224a02e2f666fa0c095b4c29f20f0a304`, the new child-environment matrix failed twice before production edits. Independent shipping-script probes showed that disabled `banner`, `tail`, and `logs` commands made no tmux calls. |
| 2026-09-12 | Commits `a82829ce3` (policy and regressions) and `bbd74a2eb` (fixture containment, ownership, and regressions) record the repair. The policy matrix and all 18 mirror tests passed. Disposable mutations removing propagation, server ownership, and activity isolation were detected. Restored checks and targeted Clippy with `-D warnings` passed. |
| 2026-09-13 | The operator identified the stale dependency: SH-698 had landed all three retained prerequisite repairs in `d3a01a0e2`. Merge `33d82f770` preserves that history and the three SH-699 commits. The combined tree passes 299 integration tests and 75 focused unit tests. |

## Root cause & trigger

**ODC classification:** Interface / Missing; triggered by fixture configuration crossing a subprocess boundary.

The verified chain has two entry points:

| Defect | Incorrect state reaching the helper | Result |
|---|---|---|
| `src/env/spawn_env.rs` omitted `STORYHOOK_VERIFIER_MIRROR` from the verification allowlist. | An explicit ambient `0` became an absent variable. | The shell's enabled-by-default policy permitted a persistent mirror. |
| `Environment::at` did not carry a resolved mirror policy, and activity startup consulted ambient process state. | Fixture children could inherit `1`, or an absent variable; activity could start its detached window opener. | Filesystem-isolated fixtures could still create terminal readers outside their ownership. |

The matrix in `tests/verifier_mirror_isolation.rs` launches separate processes for absent, `0`, `1`, empty, and `false` settings. It observes the actual verification allowlist and actual verify, notify, reap, and submit command construction through recording endpoints. Before the repair, examples included `0` disappearing at verification and fixture notify/reap/submit children inheriting `1`.

The competing explanation that the shell ignored its switch was refuted: disabled shipping `banner`, `tail`, and `logs` commands returned without invoking recording tmux, while an enabled control did invoke it. Disabling mirrors still recorded the banner through the activity journal. A `story daemon logs --follow` process is a log reader; its command name does not mean it started or owns a daemon.

The failure becomes visible when a fixture reaches one of these incomplete boundaries while tmux is available. Correct production persistence then keeps its reader alive after the invoking test finishes. No verified historical good revision was established.

## Contributing factors

- The existing test-environment table already disabled mirrors for contained child processes, but an in-process `Environment::at` fixture did not enforce the same contract.
- Absence enables mirrors in production. Dropping a safety setting therefore changed behavior rather than merely losing optional configuration.
- Standalone verification-script tests relied on outer-runner containment instead of applying it at their command helper.
- A private socket prevented collisions but did not itself own a daemonized tmux server. The former live fixture relied on a cleanup command whose failure was ignored.
- Persistent production views are intentional under [the verifier-window specification](../spec/verifier-windows.md). Expiring every window would change product behavior without repairing the fixture boundary.

## The fix

**Verdict: SURGICAL.** This repairs the existing environment and fixture-lifetime contracts. It introduces no production expiration policy and performs no cleanup of historical operator resources.

| Boundary | Maintainer contract |
|---|---|
| `src/env/mod.rs` | `Environment::from_process` resolves mirror policy once: only the exact OS-string value `0` disables it. Absent, empty, `1`, and `false` remain enabled. `Environment::at` always disables it, independent of ambient state. |
| `Environment::child_vars` | Publish the resolved store path, `XDG_STATE_HOME`, and normalized mirror value `0`/`1` together. Apply these after any environment-clearing allowlist. Values are `OsString` because the contract contains paths and policy. |
| Verification | Preserve the mirror variable in the narrow allowlist, then overlay the actuator's resolved policy on the verification command. Do not widen the allowlist to unrelated store settings or credentials. Other story-running helpers receive `child_vars`. |
| `src/daemon/activity` | Check the resolved policy before starting the detached opener. Pass `child_vars` into its shell command so state location and policy agree with its parent. Disabling a mirror must preserve activity journaling. |
| `tests/merge_gate.rs` | Standalone command helpers apply the shared table-derived `daemon_containment()` settings themselves. |
| `tests/support/verify_window_live.rs` | An enabled fixture starts and owns `tmux -D -S <private socket> -f /dev/null` through `ChildGuard` before exposing mirror commands. All clients use that socket; teardown is bounded, reaps the owned server, checks reader identities, and reports failures even during assertion unwinding. |

Explicit `Command::envs` mappings override inherited environment values, which supports applying the resolved policy at the child boundary. See [Rust's `Command::envs` reference](https://doc.rust-lang.org/std/process/struct.Command.html#method.envs). The foreground `-D` mode and explicit `-S` socket allow the fixture to own its server independently of an operator's server; see [the tmux manual](https://man.openbsd.org/tmux).

Local evidence at the time of this record: policy matrix 1/1, mirror tests 18/18, hygiene tests 4/4, merge-gate tests 54/54, environment units 60/60, activity units 6/6, `spawn_inventory` 2/2, and `spawned_child_environment` 4/4 passed. Targeted Clippy with `-D warnings`, formatting, and whitespace checks passed. Mutations removing policy propagation, private-server ownership, and activity isolation were detected. SHA-256 checks confirmed restoration of the mutated source files; restored activity 2/2, policy 1/1, and ownership 1/1 checks passed. An initial restored ownership run could not create a private Unix socket under the sandbox; a repeated red-to-green comparison with the same reviewed permissions removed that confounding condition.

A separate cleanup mutation transferred fixture resources to an independent
emergency owner instead of closing them at fixture teardown. The real
reader-survival assertion failed at its unchanged deadline. Emergency cleanup
then closed the private resources through the original cleanup implementation;
its receipt confirmed both server identities and all three reader identities
were dead, with no cleanup errors. The restored teardown regression then
passed in 5.23 seconds. This tests teardown separately from startup ownership
without leaving deliberately leaked resources on the machine.

The initial integration run also found a missing `dashboard_local_time` impact declaration and a real-reap fixture lacking an origin, independently diagnosed on SH-702. A serial rerun of the verification queue's `shell_` tests passed 14 and failed two: the real-reap case and `shell_notification_classifies_absence_by_the_helpers_reason_slug`, which reported `could not read process group for verifier-notify pid 197`. At that point the process and lifecycle ownership sources were unchanged from baseline. SH-699 adopted the registration investigation and remained open with these failures recorded.

## Continuation: prerequisite repairs and validation

SH-698 subsequently reproduced and repaired all three failures, then passed central verification and landed as `d3a01a0e25815c388f6ad95d4b697dfcd86adedf`. The operator removed the stale SH-702 dependency. SH-699 integrated that landed history in `33d82f770`, with no conflicts or rewritten commits.

| Landed commit | Resolution and regression evidence |
|---|---|
| `c38d8e0da` | Declare `dashboard_local_time` against `src/web_dashboard.html` and require the exact row in the manifest regression. Both this row and SH-699's fixture-hygiene row survive integration. |
| `b58639762` | Give the original leased-reap repository a private bare origin advertising its default branch. The production origin-authority refusal and unrelated replacement checkout remain intact. |
| `574286228` | Handle the macOS exit window where `getpgid` reports `ESRCH` before a child becomes waitable. Observe the group before `try_wait` can reap and release the PID, then retain ordinary bounded capture when exit evidence explains registration failure. Live-child failures, unrelated lookup errors, and wait errors remain fatal. |

SH-698's native probe observed the exit window in 64 of 64 children. Its deterministic syscall-state regression covers absent-but-not-waitable, completed success/nonzero status, live group, unrelated lookup error, and wait error. The live-child refusal and real-registry capture controls also pass on the integrated tree. This provides stronger evidence than merely rerunning the formerly intermittent notification test until it passes. See [Apple's getpgid reference](https://developer.apple.com/library/archive/documentation/System/Conceptual/ManPages_iPhoneOS/man2/getpgid.2.html) and [Rust's Child lifecycle reference](https://doc.rust-lang.org/std/process/struct.Child.html).

Fresh continuation validation:

| Checks | Result |
|---|---|
| 23 directly impacted integration targets | 299 passed, zero failed or ignored; includes all 85 queue cases, 54 merge-gate cases, the lifecycle harness, policy matrix, fixture guards, and 18 real-mirror cases |
| Focused library units | Process 9/9, environment 60/60, activity 6/6 |
| Static checks | Targeted Clippy with `-D warnings`, formatting, and staged/unstaged whitespace checks pass |
| Resource ownership | Normal and panic teardown remove owned banner/activity readers; the second private control server remains unaffected |

The selector ran against the actual combined tree and returned `ALL` because certified baseline `7d8c74f5d3b2de80fe6e44c1db2e89ff4dd98247` has no coverage map. Only the new/directly impacted targets ran; the full suite remains central-verifier owned. Logs are `/tmp/sh699-continuation-{impacted,process,env,activity,clippy}.log`. Earlier mutation evidence remains applicable because the tested containment boundaries are unchanged.

Native tests used reviewed access to shared compiler locks and private sockets. Cargo replayed cached dependency slot-refusal diagnostics from the initial session (newest cache timestamp 2026-09-12 16:47 local); current Clippy records successful shared-slot waiting/acquisition. No guard was disabled. A separate installed-helper grammar refusal is owned by SH-712: use the supported direct `story load-context --story SH-699` reader until a release containing that guard repair is installed and the managed-launcher form is validated. No installed files or historical operator resources were changed.

## Preventative action — killing the class

| Guard | Failure it detects |
|---|---|
| `fixture_mirror_policy_survives_every_verifier_child` in `tests/verifier_mirror_isolation.rs` | Loss of the resolved policy across fixture/process constructors and actual verifier child boundaries, across the five ambient settings. |
| `fixture_activity_start_keeps_journaling_without_window_launch` in `src/daemon/activity/tests.rs` | An isolated activity producer starts a detached opener, contacts tmux, or loses journal events. A synchronous test-only launch counter prevents thread scheduling from hiding a forbidden launch. |
| `activity_window_child_receives_resolved_policy_and_state_home` in the same file | The real activity shell boundary inherits the wrong policy or paths. An enabled recording-tmux control proves the probe can observe a launch. |
| `a_private_server_is_owned_before_any_mirror_command` and `fixture_teardown_reaps_banner_and_activity_readers_even_while_unwinding` in `tests/support/verify_window_live.rs` | Private server ownership is missing, or banner/activity readers survive normal or panic teardown. Native process identities avoid confusing PID reuse with a survivor; another private server must remain untouched. |
| `verifier_commands_are_contained_at_their_function_or_command_helper` in `tests/verifier_fixture_hygiene.rs` | A tracked test directly invokes a verifier script or opts into mirrors without a recognized containment pattern. Calibration tests reject sibling-function and helper-name laundering. |

The fixture hygiene check is a textual fence, not a Rust parser or arbitrary dataflow analysis. It recognizes the repository's command patterns and one known `run` helper; behavioral boundary tests and owned-resource tests remain necessary. New launch abstractions must add a meaningful guard/control rather than merely adding a marker that satisfies the scan.

## Lessons

- Treat child policy and filesystem location as one resolved environment contract. Re-reading process-global state in a fixture consumer defeats that contract.
- Resolve enabled-by-default settings explicitly before crossing an environment-clearing boundary; absence may carry unsafe fixture semantics.
- Resource isolation needs both a private address and an owner. A server running at a temporary socket can outlive the temporary directory unless teardown owns and observes the process.
- Verify negative behavior with a positive control and a completion boundary. “No call observed yet” does not prove that a detached thread will never make one.
- Preserve the distinction between persistent product behavior and disposable test resources when choosing the repair.
