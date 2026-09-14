# Determinism hardening — SH-687

StoryHook v2.4.2 already owns most operational workflow in code. This audit
traces model-facing behavior from entry to recovery, including instructions
whose mechanical steps still depend on a model following prose. The objective
is to remove unnecessary interpretation without pretending deterministic
syntax can replace semantic judgment.

## Changes and compatibility

The council chose a strict whole-message JSON implementation-plan request:
`{"type":"storyhook.implementation-plan","version":1,"story_id":"SH-687","plan":"complete plan text"}`.
Only built-in autonomous Codex charters advertise it, in Default mode. Plan mode
continues to advertise the native `proposed_plan` envelope. A structured request
received in Plan mode redirects to that native review without permitting writes
or implementation. Attended sessions and custom prompts retain their behavior.

The parser distinguishes absent, invalid, and valid protocol data. Valid requests
bypass Luna; invalid, quoted, embedded, duplicate-key, mismatched, and unknown
version requests never fall through to model classification. Ordinary prose
retains Luna. The hook retains its root/session/turn/cwd/mode checks, eligibility
recheck, and locked, fsynced at-most-once receipt. Receipt hashes identify both
the whole request and the exact decoded plan. Fixed feedback requires that plan
verbatim as the first implementation comment. The plan text is never executed
or interpolated into a command by the hook.

This declares readiness; it does not prove semantic completeness. Approval
remains limited to the implementation plan and cannot grant network, credential,
deletion, deployment, scope, or unresolved-choice permissions.

`story session-eligibility <id>` supplies one typed read of the story, configured
active role, and dependency readiness, in one existing store transaction. It
does not claim, approve, or modify the story. Missing or unreadable evidence is
an error, not eligible. Provider identity remains the hook's responsibility.

Triage must not turn failed reads into empty successful findings. Actual cycle
members must be distinguished from downstream dependents; both can be blocked,
but only a member needs an edge in that cycle repaired.

## Audit inventory

Scope: all nine shipped skills, four references, both provider adapters, every
registered hook, every `story.sh` verb, and their CLI/MCP/service/daemon paths.
The audit follows behavior, rather than treating a shell command mentioned in a
prompt as a model implementation. Paths below are repository-relative;
`plugin/` abbreviates `plugins/story/`. “Existing tests” identify coverage to
preserve, not a claim that the entire suite ran in this worktree.

| Behavior and implementation | Model dependency and disposition | Compatibility / evidence |
|---|---|---|
| Intent routing: `plugin/skills/story/SKILL.md`, `plugin/references/helper-command.md`, `story.sh` verb switch | Natural language selects a verb; every explicit verb already dispatches deterministically. Retain interpretation and expose the same direct commands. | Preserve host-specific absolute helper resolution and arguments. `tests/plugin_skill_determinism.rs`, `test-claim-route.sh`. |
| Story discovery/list/view: `cmd_list`, `cmd_view`, `src/service/query.rs` | Filtering, search, order, readiness and rendering are code. Selecting a semantically relevant match remains judgment. No similarity heuristic can safely replace duplicate review. | Preserve project scoping, ready order, excluded closed stories. `tests/story_context.rs`, `test-context.sh`. |
| New story: `plugin/references/story-new.md`, `cmd_create`, service mutations | Drafting scope, title, acceptance and labels requires understanding; argv validation, description-file transfer and single creation are deterministic. Retain drafting. | Preserve user authorization and no retry after uncertain creation; no idempotency guarantee invented. `test-create.sh`. |
| Priority/type/dependencies: new/triage/router instructions; `src/domain.rs`, service mutations | Impact assessment and which relationship is true remain judgment. Valid types, rank sorting, edges, inverse edges and transaction constraints are code. | Read live rubrics/configuration; never derive priority from keywords. `tests/priority_rubric.rs`, `tests/story_claim.rs`. |
| Feature decomposition: `plugin/skills/story-plan/SKILL.md`, `src/decompose.rs` | Model authors/refines the input specification. Markdown/YAML parsing, wave dependencies, dry run and creation already run in code. Retain semantic breakdown. | Preserve preview before creation and unique scratch files; do not silently turn arbitrary prose into an authoritative task graph. `tests/story_decompose.rs`. |
| CLI availability/install/update: install/update skills, `references/ensure-cli.md`, `cmd_ensure_cli`, `src/plugin.rs`, `src/plugin/reinstall.rs` | Provider/installer choice and permissions are user decisions. Detection, asset selection, version comparison, replacement and package registration are code. | Preserve installation consent, provider ownership and working-directory guards. `tests/plugin_install.rs`, `tests/plugin_install_freshness.rs`, `tests/story_update.rs`. |
| Project setup: setup skill, `src/service/config.rs`, `cmd_scaffold_agents_md` / `cmd_scaffold_claude_md` | Prefix and optional hooks require intent; project creation and sentinel merges are code. The small `[plugin] enabled` TOML edit is still model-authored. A future typed config setter could remove this mechanical edit. | A setter must preserve comments/unrelated keys and existing configuration semantics. This is an interface opportunity, not a reproduced defect. `tests/scaffold.rs`, `test-scaffold-agents-md.sh`, `test-scaffold-claude-md.sh`. |
| Context: context skill, `cmd_context`, `hooks/session-start.sh`, query service | Data gathering, defaults, project identity, full/assigned-story context and rendering are code. Relevance synthesis remains model work. | Preserve complete assigned-story evidence and hook deadlines. `tests/story_context.rs`, `tests/session_start_hook.rs`, `test-context.sh`. |
| Obviation: `src/service/query/obviation.rs`, `docs/spec/obviation-review.md`, router/charter | Complete candidates since creation and evidence are code. Whether completed work removes a requirement is semantic. Retain review; do not auto-close by title similarity. | Preserve every candidate, history and human-review block. A future structured review-result command could atomically record the agent's decision without making it. `tests/story_obviation.rs`, `tests/service_obviation.rs`, `test-obviation-review.sh`. |
| Claim/queue/epics: router, `src/domain.rs`, service claim, `cmd_dispatch_epic` | Selection ordering, labels, draft exclusion, computed epic state and atomic claim are code. A typed epic starts its descendant run rather than a coding session. | Preserve compare-and-swap and configured state roles. `tests/story_claim.rs`, `tests/engine_labels.rs`, `test-dispatch-epic.sh`. |
| Triage gathering/classification: triage skill, `cmd_triage`, `lib/blocking_cycles.py` | Already deterministic, but audit reproduced hidden read failures and false cycle members. Fixed all read boundaries and exact SCC membership. Resolution remains semantic. | Preserve finding categories and contextual diagnostics; downstream stories remain blocked findings. `test-triage-read-failure.sh`, `test-triage.sh`, `test_blocking_cycles.py`. |
| Sync: sync skill, `cmd_sync`, `hooks/post-git.sh`, `src/hooks.rs` | Commit-ID extraction, linking, effective Git hook detection and idempotency are code. The model may choose an explicit historical window; it does not parse commits. | Preserve Git configuration, deadlines and delegated CLI defaults. `tests/story_sync_git.rs`, `test-sync.sh`, `test-post-git-hooks-path.sh`. |
| Handoff: handoff skill, `cmd_handoff`, `hooks/stop-handoff.sh`, query service | Windowed activity, state and blockers are deterministic reports. The skill's accomplished/next-work narrative is intentional synthesis. | Preserve explicit window and tracker facts; narrative is not an authority to mutate. `tests/story_handoff.rs`, `test-handoff.sh`. |
| Provider/model capabilities: adapters, `cmd_capabilities`, dispatch option validation | Allowed models/effort/speed, provider mapping, defaults and override checks are code. Workload-based selection can remain judgment. | Preserve provider-specific IDs and wholesale custom launch/prompt overrides. `tests/dispatch_options_endpoint.rs`, `test-dispatch-model.sh`, `test-dispatch-launch-override.sh`. |
| Dispatch setup: `cmd_dispatch`, `lib/session.sh`, `lib/codex-bootstrap.sh`, `lib/stop-dispatch-pane.py` | Claim, fresh base, worktree, pane identity, capacity, readiness, initialization receipt, paste and rollback already run in code. No model substitute needed. | Preserve exact package/session/pane binding and resources on uncertain cleanup. `test-dispatch-codex-bootstrap.sh`, `test-dispatch-codex-hook-binding.sh`, `test-dispatch-failure-cleanup.sh`. |
| Charter assembly: builtin prompt constants in `story.sh`, provider adapters | Code selects autonomous/attended/council/solo/host clauses. The agent still interprets the development task and fulfills its plan. Added an explicit Default-mode JSON contract only for built-in autonomous Codex. | Preserve attended/custom text and final extra clause placement. `test-codex-provider.sh`, `test-charter-inert.sh`. |
| Native plan approval: `hooks/full-auto.sh`, exact-pane watchers in `lib/session.sh` | Tool/event recognition and native UI acceptance are deterministic. Plan design remains model work. Preserve native Plan behavior for both hosts. | No generic terminal typing or permission broadening. `test-full-auto-hook.sh`, `test-codex-plan-approval-resilience.sh`, `test-claude-plan-approval-resilience.sh`. |
| Codex Stop plan recognition: `hooks/codex_stop.py`, `hooks/plan_request.py` | New exact whole-message protocol eliminates model classification for declared plans. Plain prose retains the bounded classifier; regex guessing would lose semantic quality. | Reject malformed/quoted/embedded requests; preserve identity, eligibility recheck, receipt and native Plan redirection. `test_plan_request.py`, `test_codex_stop.py`, `probe_codex_stop.py`. |
| Prose fallback: `hooks/codex_classifier.py`, classifier instruction/schema/catalog | The only dedicated runtime LLM classification call found. Keep semantic approve/other/uncertain interpretation for compatibility. Output never supplies commands. | Preserve measured Codex 0.154.0 zero-tool contract, timeout, output bounds and refusal on unknown runtime. `test_codex_classifier.py`, real provider probe; existing live evaluation corpus remains applicable. |
| Eligibility: `src/service/query/eligibility.rs`, CLI/invocation routing | Moved hook-local joins of three independent CLI reads to one typed transactional query using existing readiness and active-role predicates. Called twice around continuation. | No new persisted state or approval permissions; reject unknown schema/identity and errors. `tests/session_eligibility.rs`, hook response-contract tests. |
| Autonomous questions/council: `hooks/full-auto.sh`, builtin charters | Question-tool denial is deterministic. Whether alternatives merit a council, researching them, and voting are substantive reasoning. Preserve external council or researched fallback. | Retain immediate durable story comments; a local council directory is not the system of record. `test-full-auto-hook.sh`, `test-full-auto-inert.sh`. |
| Implementation/testing judgment: story charter and repository instructions | Research, code changes, regression design and interpreting failures are the intended agent workload. Automating command execution cannot establish correctness of arbitrary changes. | Preserve approval scope, two-hat commits and repository test policy; no automatic widening of privileges. Existing provider/charter tests cover instruction transport, not semantic quality. |
| CLI/MCP/API/TUI: `src/cli.rs`, `src/invoke.rs`, `src/mcp/tools.rs`, API/services | Parsing, schemas, canonical IDs and execution are code. MCP's model caller selects arguments; it does not implement the service. New eligibility query stays outside the curated MCP tool list. | Preserve read-only routing and common domain semantics across clients. `tests/mcp_tool_drift.rs`, `tests/bare_integer_ids.rs`, CLI unit tests. |
| Full Auto engine: `src/service/engine.rs`, `src/daemon/engine.rs` | Scheduling, leases, capacity, retries, reconciliation, stalls and quarantine are deterministic. Dispatched story work remains model-driven. | Preserve single ownership, graph progress, run configuration and restart recovery. `tests/engine_run_model.rs`, `tests/engine_reconcile.rs`, `tests/engine_hardening.rs`, `tests/engine_restart.rs`. |
| Submission/verification: `cmd_submit_leased`, `src/service/verification.rs`, `src/daemon/verification.rs`, verifier scripts | Push/adopt/create/link, generations, receipts, checks and outcomes are code. Red-test repair is model work; outcome recognition is not a model verdict. | Preserve exact refs, independent verifier ownership, infrastructure-vs-repair errors and existing PR reuse. `test-submit-leased.sh`, `tests/verifier_lifecycle.rs`, `tests/verification_queue.rs`. |
| Recovery/notification: `cmd_notify`, capture/doctor, dispatch resume, daemon verifier | Exact-pane delivery, liveness, readiness and resume are code. Diagnostic interpretation and repair remain model work. | Preserve identity, dirty work and no unsafe redispatch on ambiguous liveness. `test-notify-redispatch.sh`, `test-dispatch-resume.sh`, `tests/engine_restart.rs`. |
| Completion/release/cleanup: `references/story-complete.md`, complete/unclaim/reset/reap verbs, leased variants | Preview, state restoration, ownership, merged/dirty/protected checks and resource deletion are code. User intent selects destructive actions when applicable. | Preserve self-window refusal and resource guards. `test-complete-plan.sh`, `test-unclaim.sh`, `test-reap.sh`, `test-reap-leased-completion-state.sh`. |
| Hook/package protection: `hooks/hooks.json`, `hooks/lib.sh`, `hooks/protect-install.sh`, plugin registration | Event routing, payload parsing, kill switch, budgets and installed-artifact protection are code. No additional model dependency. | Preserve host hook compatibility and error visibility. `tests/plugin_contract.rs`, `tests/protect_install_hook.rs`, `tests/hook_budgets.rs`, `test-hook-kill-switch.sh`. |

The interface opportunities above are optional follow-on designs. They do not
justify replacing semantic judgment or changing established permissions. No
additional verified defect was left adopted but unfinished. SH-686's separate
verifier cancellation work remains independent.

## Validation recorded in this worktree

- Structured protocol, typed eligibility, charter declaration, triage read
  failures and downstream-cycle behavior each failed their new regression
  before implementation and passed afterward.
- Hook suites: 24 Stop, six structured-request and seven classifier tests.
  Structured cases cover exact text, escaping, duplicate/unknown keys, malformed
  and quoted payloads, wrong story/version, stale turn/state, repeat delivery,
  both modes, and attended/subagent refusal.
- Four real Codex 0.154.0 loopback probe scenarios: prose uses one classifier
  call in either mode; structured requests use zero. Each exercises production
  hooks and the real isolated tracker. Only model response data is mocked.
- Four real CLI eligibility tests cover custom active roles, readiness,
  closed/verifying/draft states, awaiting, dependencies, obviation, missing
  configuration/story and read-only behavior.
- Triage: 15 query-failure combinations; all 512 directed three-node graphs;
  long chains, disconnected cycles, duplicate edges and self-loops; real CLI
  downstream and closed-cycle-member cases. Failure diagnostics survive.
- Directly impacted CLI unit, MCP drift, bare-ID, context, package and skill
  determinism tests passed, as did provider/charter tests and scoped Clippy
  with warnings denied. Formatting and whitespace checks passed.
- The impacted-test selector on the actual staged tree returned `ALL` because
  the certified baseline has no coverage map. Per this session's instruction,
  only new/directly impacted tests ran; the centralized verifier owns the full
  suite and merge. No release, deployment or cleanup action belongs here.

## Decision record

The full council verdict is persisted in SH-687 comments. API designer, software
architect, and security researcher independently proposed strict protocols;
after one deliberation all ranked the explicit type/version schema first.

The read query uses existing domain predicates and a transaction instead of
reimplementing role/readiness rules in Python. No new persisted workflow state
or storage migration is needed.

## References

- [Workflow versus agent orchestration](https://www.anthropic.com/engineering/building-effective-agents)
- [SQLite snapshot isolation](https://www.sqlite.org/isolation.html)
- [Exact strongly connected components](https://algs4.cs.princeton.edu/42digraph/KosarajuSharirSCC.java.html)
- [JSON object contracts](https://json-schema.org/understanding-json-schema/reference/object)
- [Existing prose continuation contract](codex-auto-plan-continuation.md)
