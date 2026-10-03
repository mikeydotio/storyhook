# SH-821: Unsafe gate code could remove registration while preserving the install receipt

- **Date**: 2026-10-03; investigation started 2026-10-02; StoryHook v3.0.3.
- **Severity/Impact**: The Claude `story@storyhook` plugin, marketplace registration,
  and cache were absent on the operator's machine. The ordinary install receipt
  still reported September 23 success. Exact loss time and outage duration are unknown.
- **Status**: Historical mechanism fixed through SH-760 in `c945ac62`;
  SH-821 changes committed locally, pending central verification. Historical
  attribution is **PROBABLE / MEDIUM confidence**, not a captured writer result.

## Summary

The strongest supported explanation is that an old verification gate ran the
project-less invoker test after registration was restored, before all running
gate snapshots included SH-760's guard. That test called uninstall in-process:
provider and cache operations could reach ambient HOME while receipt removal
used a temporary StoryHook data directory. An isolated replay of the executed
historical source reproduced the complete loss pattern, prevented it with the
actual SH-760 guard, and restored it with the identical unsafe executable.
This confirms the conditional mechanism with HIGH confidence; the missing
September provider result limits historical attribution to MEDIUM confidence.
SH-821 also repairs a demonstrated rollback gap and adds durable operation
evidence, while preserving the lesson that store isolation does not establish
ownership of provider state.

## Timeline

All times below are UTC. A file mtime is the last write to that file or
directory, not the removal time of a particular registration.

| Time | Event and evidence |
|---|---|
| 2026-07-29 05:37:48 | `5ff6d774717da8255a6127eb9d79a02717913925` introduces the project-less roster, including Claude uninstall. The commit's local date is July 28. |
| 2026-09-23 01:36:54 | Daemon PID 2621 starts v3.0.3(104), build tree `6899f30da3d6a09a0352279bd2a076fcbfebbea7`. Activity line 1 records identity; line 4 records the ordinary store. |
| September 23 05:22:14–18 | Actual `story plugin install claude` succeeds. The daemon records the request at 05:22:15.619 and completion at 05:22:18.407. |
| September 23 05:22:23.834 | A direct `installed_plugins.json` read returns `story@storyhook`. This proves registration presence independently of the receipt. |
| September 23 05:40:35.985 | A direct listing proves `cache/storyhook` exists. This is the latest retained cache-presence observation found. |
| September 23 05:53:33–07:00:34 | SH-761 / PR 854 executes unsafe tree `55a7aaf8bd3ea895c9a3649556fd0057f0b2fc14`. PID 2621 launches verifier child 12846. Gate log lines 10353/10380 show actual invoker execution and roster PASS; the overall gate exits 2 for an unrelated failure. |
| September 23 07:08:27–07:43:43 | The next SH-761 gate executes unsafe tree `94b407ab057ac2894a508a05e644c445aee242fc`. Log lines 9879/9906 show execution and roster PASS; the gate exits 0. |
| September 24 03:34:25 | `c945ac62b9a4ed205bf54f2540105716bf988fad` adds the mutation guard and uninstall tombstone for SH-760. A commit timestamp does not make concurrent old gate snapshots safe. |
| September 24 03:59:31–05:39:35 | An SH-760 gate reports the new refusal test PASS at line 10545. Its proposed tree `dbbab0ab…` is unavailable. Missing source is not evidence of a missing guard. |
| September 24 09:22:41–10:19:25 | SH-758 / PR 857 executes unsafe tree `5897b078e685eec9b118353f32b9579de7bb941b`. Log lines 10068/10095 show execution and roster PASS. Relevant plugin, roster, and wrapper files match tree `55a7aaf8`. This is the latest proven unsafe run in the inspected set. |
| September 24 10:19:55–11:05:24 | SH-760's successful gate executes guarded tree `5ee114f1642a84e8144ae42c311c5c24591f5235`; refusal test PASS at 9897, roster PASS at 9900. Merge `78bd9a5a` follows at 11:05:30. |
| September 26 02:29:45.673 | A fresh cache listing has only three other entries and no `storyhook`. This first retained cache-absence observation predates the later cache-directory mtime by about 17 hours. |
| September 26 19:37:46–19:38:33 | Doctor reports `DEREGISTERED`; fresh reads confirm absent marketplace and plugin entries, absent cache, and the unchanged September 23 receipt. SH-821 records this symptom. |
| September 26 19:45–46 | Post-discovery recovery attempts include successes at 19:46:36.540 and 19:46:43.577. They are not candidates for causing the preceding loss. |
| October 2 | SH-821 commits `3318a399` and `826f8069` repair rollback and add operation evidence. Initial targeted regressions pass, but historical attribution remains incomplete. |
| October 3 | Expanded source/log analysis, controlled guard toggle, independent challenge, and architecture review support probable pre-guard loss discovered later. `34927785db5caa4f7fac04b707ec9853acc191db` strengthens two test oracles; both changed cases pass. |

Retained evidence identifiers make the chronology auditable without relying
on the investigation's ignored working notes:

| Evidence | Retained location and lines |
|---|---|
| Recovery and registry presence | Claude project session `702f0426-af30-4daa-a65f-d59548e8f95b.jsonl`, lines 529/532 and 537/540, under `~/.claude/projects/-Volumes-Code-mikeyward-storyhook/`. |
| Latest cache presence | SH-760 Claude worktree session `2f607f90-6edb-4682-9356-65a27a59ca57.jsonl`, call 794 / result 799. |
| First cache absence | SH-779 Claude worktree session `91859612-04cd-4641-8221-144e8dacbee0.jsonl`, call 463 / result 468. |
| Fresh discovery | Main-project session `307faf53-3a25-4221-a76e-ce6c191c6db6.jsonl`, lines 1345, 1377/1380, 1386/1389, and 1402/1410. |
| Recovery and first gate share a daemon | `~/.local/state/storyhook/daemons/eab76ca58d086ca4/activity/2026-09-23.jsonl`, lines 11959–11960, 14768, 14783, and 14825. |
| Exact first gate | Shared Git administration log `storyhook/verification-logs/pr-854-55a7aaf8bd3ea895c9a3649556fd0057f0b2fc14-attempt.EPYmSn`; execution receipt binds tree, head `1ee268fcfc4c2807b7a555d07da287127d46888b`, and base `c420b18d99e6bc5db61db965913fa376044ee006`. |
| Subsequent gates | Verification logs and execution receipts keyed by the complete proposed-tree IDs above establish actual executions, not cached `was PASS` inventories. |

## Root cause & trigger

### Verified code chain and inferred historical effect

Historical line references below use tree
`55a7aaf8bd3ea895c9a3649556fd0057f0b2fc14`, except where stated.

| Link | Evidence and implication |
|---|---|
| Defect: mutation lacks an ownership check | `tests/invoker_seam.rs:276–311` constructs a fixture environment but includes Claude uninstall. `src/plugin.rs:1232–1238` has no build guard. StoreInvoker isolates store access, not process HOME. |
| Trigger: roster invokes the real mutation path | `tests/invoker_seam.rs:367–381` calls StoreInvoker in-process. `src/invoke.rs:2737–2743` → `src/service/system.rs:156–158` → `plugin::uninstall`. This requires no daemon plugin RPC. |
| Infection: provider and receipt state diverge | `src/plugin.rs:94–97,133–144,183–211,1114–1138` reads HOME for provider state, invokes the PATH provider, removes HOME residue, and removes the receipt using `STORYHOOK_DATA_DIR` first. |
| Environment permits operator-state reachability | Parent build tree `6899f30d`, `src/daemon/verification.rs:1270–1278` and `src/env/spawn_env.rs:91–104,157–177`, preserves HOME/PATH. `src/env/mod.rs:283–313` excludes HOME from fixture child variables. Gate `scripts/run-tests.sh:171–192` calls `storyhook_isolate` without `--home`; `scripts/test-env.sh:84–89` redirects data/XDG but preserves HOME. |
| Failure can remain falsely green | The roster fails only for an error containing `not initialized in this directory`. Destructive success and other errors can pass. The old runner captures provider output without the later operation journal. |
| Historical opportunity is observed; effect is inferred | Three exact unsafe trees execute after proven recovery. The same daemon performs recovery and launches the first gate. No later restoration was found before discovery in retained coverage. The September provider exit and registry write are not retained. |

**ODC classification:** Checking / Missing at the provider mutation boundary;
related Interface / Incorrect in the isolation contract. The trigger is
configuration combined with an in-process test invocation. Verification reran
an old snapshot after recovery, including one unsafe gate after guarded gates
had already started.

The mechanism explains missing provider entries/cache, an unchanged ordinary
receipt, and no intervening daemon plugin RPC. It supports pre-guard loss
discovered later. It does not establish a post-fix recurrence, a specific
September provider PID, or the precise deletion time of either registry key.

### Competing explanations

| Hypothesis | Evidence and falsifier |
|---|---|
| H1: old gate removes live registration | Best supported: unsafe executions after recovery, matching environment split, same parent daemon, and conditional replay. Later confirmed presence, proof of isolated HOME or failed removals in those gates, or a later attributable writer would weaken it. A historical provider exit/write trace would strengthen it. |
| H2: installed operation is interrupted | Reachable and reproduced separately, but no retained interval invocation supports it. Removal followed by an incomplete installed operation would support it. Missing RPCs cannot exclude direct/unrecorded calls. |
| H3: external provider/import rewrite | Possible, without an affirmative incident trace. The retained Codex import is September 7, so that event is refuted as the interval actor. A dated external writer trace would support another instance. |
| H4: unsafe code or override runs after the guard fix | Possible in unretained evidence. Retrieved later trees contain the guard; refusal-test logs support guarded behavior for some missing trees. A later unsafe executable or effective override tied to removal would support it. Missing objects remain unclassified. |

The expanded scan covered 1,584 retained JSONL files, including StoryHook
Claude subagents and 2026 Codex sessions: 8,683 interval commands and 171
registration/cache matches. No additional restoration or direct mutation was
found. Other tool formats, archived logs, and unrelated Claude projects remain
outside coverage. Four daemon journal days contain no plugin RPC between the
successful recovery and post-discovery recovery; the old in-process path
bypasses that RPC, so this absence does not exclude it.

Provider-host logs yielded 184 sync summaries, all reporting zero removals or
orphan cleanup; none names StoryHook or either registry. Their timestamps lack
an explicit offset and were not inserted into the UTC timeline. No useful
interval provider debug log or registry backup was retained. Negative searches
rank alternatives; they do not prove that unrecorded activity did not occur.

### Controlled replay and confidence boundary

The replay archived exact tree `55a7aaf8` into a disposable `/private/tmp`
workspace. Its untouched roster blob is
`df7dc74aeff46647133c4a01ad3787df9221e722`. A constant adapter relocated only
the old scratch root so its helper could not sweep shared test scratch.
Each leg used fresh private HOME, XDG directories, data, cwd, and TMPDIR;
PATH contained the fixture provider directory plus `/usr/bin:/bin` only.
Fake Claude accepted only version, plugin uninstall, and marketplace removal.
It logged argv/PID/PPID/HOME and modified both fixture JSON registries;
real StoryHook performed cache and receipt cleanup. No live provider was used.

To repeat safely, build only the archived `invoker_seam` target with
`cargo test --offline --test invoker_seam --no-run`, preserving compiler
controls. Run the binary only with the explicit private environment above and
selector `--exact the_project_less_verbs_all_answer_outside_a_project --nocapture --test-threads=1`.
Seed both normal-HOME and separate-data receipts with `version 3.0.3` and
`installed_at 2026-09-23T05:22:18Z`. Snapshot receipt bytes/mtime, both registry
entries, three residue directories, and unrelated entries before each leg.
Never use ambient HOME for this replay.

| Leg | Intervention and result |
|---|---|
| Unsafe baseline | Original production path. Three provider calls; both fixture entries and three residue directories disappear. Separate-data receipt disappears; ordinary HOME receipt bytes/mtime and unrelated entries remain. Nine assertions pass. |
| Guard control | Add the actual `c945ac62` guard module and check only at uninstall entry. No receipt/tombstone changes. Zero provider calls; all seeded files retain bytes/mtime. Six assertions pass. |
| Unsafe restored | Remove the guard intervention and reuse the saved unsafe executable with fresh fixtures. The complete loss pattern returns. Nine assertions pass. |

All three roster executions report one passed case and 24 filtered cases.
This is the old oracle's false success, not evidence that registration is safe.
The experiment's 24 separate state assertions establish the contrast. Two
builds complete without compiler warning/error messages; the reverse toggle
uses identical saved bytes, not a third compilation.

| Artifact | Identity |
|---|---|
| Baseline/reverse-toggle executable SHA-256 | `24b26b85a6433d7a93b9ede1d10fcb41573add5e83bb76eb24783ba989643afd` |
| Guard executable SHA-256 | `c2f6541614c92c3c575a02bce059acdd00da509257d01db955b466c892bc2e94` |
| Applied scratch adapter | `unindexed_base().join("storyhook-tests")` becomes `unindexed_base().join("/private/tmp/sh821-rca-replay-60sno7u5/scratch")`; constant across all legs. |

The exact applied `scratch-adapter.diff`, guard diff, predictions, raw results,
and harness remain in `.rca/sh-821-registration-loss/repro/historical-replay/`.
The preparatory `prepare.py` has an equivalent but differently spelled
`PathBuf::from` expression; it is not the authoritative replay recipe.
Every archived regular source file was independently checked after restoration;
the guard probe was absent and the assigned branch was unchanged by replay.

The fake assumes successful removal and ignores XDG. Historical wrappers
changed XDG; the resolved executable, child environment, and exit are unknown.
Current [Claude plugin storage documentation](https://code.claude.com/docs/en/plugins/loading)
and [configuration documentation](https://code.claude.com/docs/en/settings)
support HOME-based storage and explicit relocation, but are not proof of the
September provider's behavior. Independent challenge accepted the mechanism as
HIGH and attribution as at most MEDIUM. The approved reproduced-equivalent
acceptance alternative is met by the historical execution connection; the
stronger captured-writer alternative remains unmet.

## Contributing factors

- Store isolation and provider ownership were separate contracts. A fixture
  value did not replace process environment; plugin code read ambient state.
- Gate data redirection protected the ordinary receipt while HOME remained
  reachable. An unchanged receipt could not exclude the old test path.
- The roster tested project-resolution reachability, not provider safety.
  Its PASS could conceal destructive success or a provider error.
- Concurrent gates used different proposed trees. Guarded and unguarded
  executions interleaved; fix creation did not mark universal safety.
- Completion-only evidence and an unjournaled old provider runner left gaps.
  A missing tombstone or RPC was inconclusive.

## The fix

Architecture review recommends **SURGICAL** completion. SH-760 already fixes
the demonstrated origin. No evidence justifies redesigning all environment
ownership or adding automatic repair. No external owner was established.

| Commit | Responsibility |
|---|---|
| `c945ac62b9a4ed205bf54f2540105716bf988fad` (SH-760) | Refuse mutation from uninstalled/test builds without an explicit override, before provider calls or filesystem effects; preserve uninstall receipts as tombstones. |
| `3318a399` (SH-821) | Include second-removal failure in rollback after the first removal changes state. Restore the previous marketplace and plugin for Claude and Codex. |
| `826f8069` (SH-821) | Publish evidence before effects, preserve failures, exclude concurrent cooperating mutations, and report incomplete/failed/unknown causes. Failed cleanup does not publish a completed uninstall tombstone. |
| `34927785db5caa4f7fac04b707ec9853acc191db` (SH-821) | Assert missing/restored fixture state before new evidence or recovery wording. Test-only completion after full RCA. |

Current boundaries are `src/plugin.rs:1034` for install/reinstall,
`src/plugin.rs:1173` for uninstall, and `src/plugin/reinstall.rs:180` for
reinstall entry. Checks precede operation evidence and provider preflight.
The sibling sweep found no further unguarded entry in the assigned scope.
These defenses neither identify the past actor nor prevent external writers
or arbitrary SIGKILL. Corrective rollback must use a new focused commit,
preserve evidence and SH-760's guard, and leave published history intact.

### Operation evidence contract

`src/plugin/operation.rs` owns one synchronous, non-nested operation. The
existing receipt format remains unchanged. Each provider has
`<data dir>/provider-installs/<target>-operations/current.json`; previous bytes
are archived as `history/<unique-id>.json` before replacement. Unreadable
evidence stops mutation; malformed readable bytes can be preserved verbatim.
Later success clears the current finding without deleting history.

Records contain version, ID, provider, verb, times, canonical HOME, StoryHook
PID, executable/build classification, override status, previous/intended source,
steps, outcome, and error. Steps record start, completion, and result. Starts
are synchronized before effects through same-directory atomic replacement,
file synchronization, and parent-directory synchronization; errors propagate.
This uses [Rust's explicit synchronization API](https://doc.rust-lang.org/std/fs/struct.File.html#method.sync_all).
The common provider runner records rollback calls too. It does not invent a
provider PID; child starts remain the process activity journal's responsibility.

The nonblocking lock is under canonical
`$HOME/.local/state/storyhook/provider-locks/<target>.lock`, independent of data
overrides. A child inherits its descriptor, retaining exclusion while it owns
effects after parent exit. This coordinates StoryHook operations only.
Evidence stays in the selected data directory; it is not a global provider audit.
Doctor only reads, reports incomplete/failed evidence even when registration
exists, and rejects inconsistent/foreign evidence. It does not equate an
incomplete record with a signal or a later loss's cause. Unexplained loss stays
`cause unknown`; provider mutation is not an atomic transaction.

## Preventative action — killing the class

| Invariant or failure class | Named regression |
|---|---|
| Refuse tests before provider calls/evidence | `plugin_guard::a_test_binary_is_refused_every_verb_before_any_provider_call`; existing `the_in_process_call_is_refused_as_a_usage_error` covers the direct library route. |
| Restore state after second-removal failure | `plugin_install::failed_marketplace_removal_restores_the_plugin_already_removed`, Claude and Codex. |
| Preserve evidence/receipt after interruption | `plugin_install::operation_tests::killed_registration_leaves_evidence_without_an_uninstall_tombstone`, install and uninstall; checks missing plugin/marketplace before evidence, plus doctor `DEREGISTERED`. |
| Refuse effects without evidence storage | `operation_evidence_failure_prevents_provider_mutation` and `loss_of_evidence_storage_after_a_provider_call_stops_the_next_effect` in `plugin_install::operation_tests`. |
| Preserve history without false attribution | `operations_publish_terminal_evidence_and_archive_the_previous_attempt`, `failed_uninstall_keeps_the_install_receipt_and_names_the_phase`, `malformed_operation_evidence_is_diagnostic_even_when_registered`, `evidence_retains_both_the_install_failure_and_the_failed_rollback`, and `unexplained_loss_is_not_attributed_to_the_last_successful_install`, in the same module. |
| Lock follows HOME and surviving provider | `a_provider_home_lock_refuses_mutations_even_with_another_data_directory` and `plugin::operation::tests::inherited_provider_lock_survives_scope_exit`. |
| Process start remains classified | `spawn_inventory::every_way_storyhook_starts_a_process_is_classified`. |

The interruption regression uses the production installer and an isolated
fixture daemon. Its fake endpoint removes fixture registration, then sends
SIGKILL only to its owned parent daemon. The strengthened oracle checks the
fixture plugin marker and marketplace source before operation evidence. This
is separate from the historical replay's two JSON-registry assertions; neither
experiment supplies a September provider result.

On October 2, 13 new/changed cases passed: nine operation cases and one each
for rollback, inherited lock, guard refusal, and spawn inventory. Scoped Clippy
with `-D warnings`, formatting, and whitespace checks passed. Original rollback
RED stopped on missing Claude recovery wording before reaching Codex; GREEN
covered both. Seven initial operation cases failed before implementation, but
the interruption oracle stopped at missing new diagnostics before independently
proving state loss. These limits remain part of the original RED evidence.

On October 3, only the two changed cases were rerun after oracle strengthening:

```sh
source scripts/test-env.sh
storyhook_isolate /tmp/sh821-tests
TMPDIR=/tmp cargo test --offline --test plugin_install operation_tests::killed_registration_leaves_evidence_without_an_uninstall_tombstone -- --exact
TMPDIR=/tmp cargo test --offline --test plugin_install failed_marketplace_removal_restores_the_plugin_already_removed -- --exact
cargo fmt --all -- --check
git diff --check
```

Both selectors passed one case with 74 filtered; durations were 5.78s and
6.30s. Cargo replayed 48 cached rustc-slot permission diagnostics from October 2
fingerprint output (23:26:17–26 UTC), including on the warm run without compilation.
They remain in the logs; no new Rust compiler warning lines appeared. Compiler
controls were unchanged. Formatting and whitespace checks exited 0. The full
suite was not run, by instruction; central verification owns that gate.

## Lessons

- Validate provider ownership at the mutation boundary. Store/cwd isolation
  cannot make process HOME safe; inherited environment must be explicit.
  [Rust Command environment semantics](https://doc.rust-lang.org/std/process/struct.Command.html#method.env_clear)
  explain why a fixture object cannot change that inheritance contract.
- Trace executed source trees, not commit dates or test binary filenames.
  A test name and PASS label do not establish safe behavior.
- Assert the state a regression claims before diagnostic wording. Preserve
  failing-run limitations when later tests improve the oracle.
- Start the incident interval at proven presence, not a later mtime. Missing
  logs exclude only the paths those logs actually observe.
- Separate verified mechanism from historical attribution. New instrumentation
  cannot retroactively supply a missing writer.
