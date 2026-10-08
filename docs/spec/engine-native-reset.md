# Native Stop Now teardown (SH-890)

Stop Now keeps one `EngineReset` owner per leased active lane and calls the
same native resource authorization, recovery capture, window closure, Git
removal and residue observation used by story reset. It does not reserve a
`StoryReset`: that final-lever operation supersedes other owners, restores Todo,
and may proceed without workspace exclusion. Those broader permissions do not
belong to Stop Now.

The run owns admission and lane bookkeeping. Verifying and closed stories are
released without cleanup; missing leases preserve work and the existing hold;
reserved labels retain the non-destructive leased unclaim. Each other lane
keeps its exact run, index, token and creation-time lease. Independent lanes
continue after a failure, and the run finishes only when all lanes are idle.

## Durable request and progress

Migration 58 adds nullable `engine_runs.stop_origin_json`. The initial Stop Now
intent commits the request's original cwd, terminal identity and hook policy
alongside `operator-stopped-now`, before waiting for outstanding dispatch.
A duplicate request keeps the first origin. A legacy row with no origin can
adopt one from a new explicit request; a background retry never substitutes
its own terminal. If an old intent reaches teardown without recorded origin,
it leaves resources with an awaiting reason for explicit story-reset review.

Existing `StoryReset.origin` cannot carry this intent: creating that owner would
supersede all engine lanes. The per-lane `EngineReset` does not yet exist at this
crash boundary, and a still-dispatching lane may not have a cleanup lease. The
run column preserves request protections without inventing resource authority
or reserving a lane before dispatch quiescence.

An optional typed `EngineReset.cleanup` pins one resource report, filesystem
identities and recovery facts before any removal. Old JSON decodes without it.
Progress replacement checks the current exact owner and expected progress,
then removes/reinserts that same owner inside one store transaction. It never
upserts after observing a missing owner, and the ordinary immutable reservation
write contract stays unchanged. Completion is persisted before lane release;
a lost acknowledgement reuses it without repeating deletion or replacing the
original recovery tip with observations taken after removal. A retry observes
new collisions and leaves replacements with a dispatch hold.

## Narrow removal policy

Stop Now retains its original regular, exact cleanup-marker requirement.
Filesystem replacements, changed branch tips, protected branches, foreign
markers, installed artifacts and the requester's own cwd/window withhold
removal. It retains the strict workspace lock and checks the exact run/lane/
reservation owner before each destructive attempt. Each Git/directory attempt
also revalidates the original pinned filesystem identities; each window kill
repeats the original pane, PID and endpoint proof. A failed attempt never grants
authority over a replacement that appears before retry. A story reset can supersede
that owner; no stale progress, completion or failure writer recreates it.

Unlike a final-lever story reset, Stop Now must prove the leased window absent
before touching Git. Uncertainty and failed removal become durable residue and
an awaiting reason. Completion reports actual retained evidence instead of
inventing the old all-resources-absent receipt. Recovery and residue survive in
the story comment and the lane's outcome after its reservation is removed.
Remote branches and pull requests remain untouched.

The filesystem and SQLite cannot form one atomic transaction. A removal already
in flight may finish after supersession; workspace exclusion and quiescent child
ownership keep that operation bounded, and subsequent attempts recheck authority.

## Transport and compatibility

The shell engine-reset actuator is deleted. The plugin explicitly refuses old
`STORYHOOK_ENGINE_RESET_V1` input so it cannot fall through to the higher-authority
ordinary reset. CLI caller identity is optional on the existing Stop wire variant
for older clients; empty identity retains its old encoding. HTTP carries no
terminal. The ordinary native route does not require an installed shell plugin;
only a reserved-label unclaim resolves that existing helper.

The Dispatcher side-effect seam remains for deterministic lifecycle tests. Its
default reset implementation invokes the native actuator supplied by the engine;
production no longer overrides it with shell transport. Legacy test receipts
retain their strict all-absence validation; native receipts must match completed
progress persisted under the current exact reservation.

SH-889's initial bounded `control_write` must be retained when the branches are
integrated. An expired admission deadline must never govern an already-reserved
teardown or its patient progress/finalization writes. SH-891's shared read-only
window proof and no-index-refresh recovery observation are retained in the
shared cleanup module; its preview overlap adapter remains a sibling concern.

## Validation

New story-specific cases are in `tests/engine_native_reset.rs`,
`src/service/story_reset/cleanup_revivify_tests.rs`,
`src/service/story_reset/cleanup_retry_tests.rs` and
`plugins/story/tests/test-engine-reset-retired.sh`. They are authored, not executed:
the integrated checkpoint and protected installation own the validation lane.
Runtime results must be recorded separately before integration.
