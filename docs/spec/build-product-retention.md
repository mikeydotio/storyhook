# Retention for detached StoryHook debug products

StoryHook v3.0.3. This opt-in implementation changes no project configuration,
installed binary, automation setting or verifier setting. Absent configuration
is inactive. An opted-in retention policy defaults to dry-run.

## Ownership and worktree removal

SH-835 already proves fresh managed dispatch enrollment, the exact Verifying
submission generation, project permission, cleanup lease, private Git identity,
and exclusive settled build custody before atomically detaching products.
Retention adds a fresh enrollment nonce and stable project UUID. The UUID must
match both the store project and the checkout pointer. Old or reused worktrees
are not adopted.

With `[build_products.retention]`, native detachment renames directly into:

```
<common-git>/storyhook-retained-products-v2/
  project-<SHA256(stable-project-UUID)>/
    namespace.json
    retention.lock
    source-<fresh-enrollment-nonce>/
      source.json
      generation-<submission-sequence>/
        journal.json
        purge.lock
        products/
```

The common-Git namespace survives ordinary linked-worktree removal and Reset.
It does not survive deletion of the repository itself and is not a backup.
The namespace records common-Git and project-directory identities. Each source
records its private-Git/worktree identities, nonce, complete cleanup lease and
configuration. Each journal copies those records and every complete native
whole-build settlement receipt. No later prune needs the disposable source
worktree or its private Git directory to exist. Shared Git administration keeps
each project's authority and retention count separate.

The same-filesystem rename remains inside the existing workspace, product and
store-generation guards. Reset reserved before the rename prevents detachment.
Reset after the rename may remove the worktree while its retained products,
pins and receipts survive in common Git. Rebuilt original products stay outside
prune authority. Reset's existing removal and patience guarantees are unchanged.

A permanent project namespace lock serializes native publication with inventory
and selection. It is acquired nonblockingly before the store write, and released
before foreground project code. A fully published `detached` journal is eligible;
prepared, unknown, malformed and interrupted entries stop that project's pruning.
Incomplete manifest publication fails closed and requires exact manual review.
No retention operation restores products over the original target.

## Eligible products and policy

This policy covers only a dedicated, reproducible StoryHook **debug-only** Cargo
target from the managed protocol. It refuses release/custom-target layouts,
unknown root outputs, debug fixtures, uncertain ownership and legacy jobs.
The debug layout allowlist is an additional check, not ownership evidence.
Never put source, evidence or non-reproducible inputs in an enrolled target.

It does not adopt task targets, global Cargo caches, old verifier directories,
current targets, release installations, logs, frozen campaign inputs, backups
or user data. Process absence, age, names and successful builds cannot establish
ownership. Current frozen campaigns and legacy caches retain their separate
ownership and approval procedures.

**Proposed activation policy:** retain the newest two intact submission
sequences across this project's registered common-Git pool, plus all pins and
all generations younger than seven days. This is explicitly a project-wide
count, including retired worktrees, rather than two generations for every
retired source. Pins add to the retained floor. Future timestamps retain bytes.
There is no disk-pressure override or automatic tightening of the policy.

Each apply deletes at most one eligible generation, oldest first. Dry-run is
the CLI and configuration default; only explicit `--apply` / `mode = "apply"`
permits deletion. The inventory is bounded to 1,024 jobs and 1,024 source entries.
Unknown, locked or incomplete jobs retain the entire pool. Purged journals and
permanent locks remain as receipts and do not consume the intact-generation
count. Receipt aging/removal is not implemented; reaching the bound requires
manual review, not automatic evidence deletion.

Selection is revalidated while holding namespace exclusion. The selected
permanent job lock protects the final journal, product identity, layout and pin
checks through deletion. A pin written before that lock is acquired wins; a pin
attempt during deletion refuses rather than claiming success. Namespace
exclusion is released before recursive I/O, allowing other native detachments
and current builds to proceed. A second pruner observes the busy job and defers.
The old direct purger refuses all v2 jobs without the retention callback.

## Pins, previews and recovery

The supported wrapper chooses a compatible Python runtime. The native worker
supplies the project UUID and common-Git path/device/inode from its registered
checkout; it does not discover arbitrary worktrees or scan target directories.

```sh
# J is an exact already-detached common-store journal from the native handoff.
bash scripts/retain-detached-products.sh stage "$J"
bash scripts/retain-detached-products.sh pin "$J"
# Explicit operator action only, after the artifact is no longer needed:
bash scripts/retain-detached-products.sh unpin "$J"

# UUID, COMMON_GIT, DEV and INO must be checked against the registered checkout.
# The scheduled native runner supplies these values without shell interpolation.
bash scripts/retain-detached-products.sh prune-common \
  --project-uuid "$UUID" --common-git "$COMMON_GIT" \
  --common-dev "$DEV" --common-ino "$INO" --keep 2 --min-age-days 7
```

Stage preserves a native v2 retention record and never refreshes its clock or
clears its pin. Pin an enrolled generation before using it as an external input;
references cannot be inferred. Retained bytes can be inspected or copied to a
separately owned destination while holding the job lock. Normal recovery after
purge is a rebuild from retained source and locked dependencies, without a
promise of byte-identical historical reconstruction.

Deletion uses the existing descriptor-anchored purger. Its write-ahead `purging`
state precedes recursive I/O. A partial I/O failure reports `purge-incomplete`,
the durable journal state, and unsuccessful CLI status; it never reports that
partially deleted bytes were kept. Subsequent automatic passes preserve the
entire pool pending exact recovery review. Original paths are never inspected
or adopted by recovery. There is no automatic recovery of uncertain publication.

The old v1 `prune` command remains an explicit compatibility operation in one
private-Git namespace with its original custody checks. It is not used by the
scheduler, migrated to v2, or made durable across worktree removal by this change.

## Existing cleanup worker integration

`src/daemon/cleanup.rs::tick` calls the configured retention runner once per due
project attempt, then performs its existing workspace cleanup. It retains the
existing `automations.enabled` permit, `cleanup.auto`, `cleanup.interval` (daily
by default), registered checkout, bounded subprocess custody and activity report.
Failures are reported under `build-retention` and wait for the next cadence.
Closure retries do not invoke retention. `story daemon gc` remains unrelated.
No new daemon, launch agent, cron job or host-wide sweep is introduced.

Turning off project automations or `cleanup.auto` suppresses scheduled retention.
Stopping only the verifier is not a retention permission control. Configuration
absent/disabled runs nothing. Retention enabled with no mode remains dry-run.
The runner is project-owned executable argv, like the existing foreground hook;
configure a trusted script, not unreviewed project code.

## Activation proposal — no activation performed

After code review, authorized merge, release/install validation and separate
activation approval, configure a **fresh**, dedicated debug-only target:

```toml
[build_products]
enabled = false
path = "products"
managed_entry = "scripts/managed-cargo.sh"
hook = ["bash", "scripts/retain-detached-products.sh", "stage"]
timeout_seconds = 120

[build_products.retention]
mode = "dry-run"
keep = 2
min_age_days = 7
runner = ["bash", "scripts/retain-detached-products.sh"]
```

The example remains disabled. Approval should explicitly select the dedicated
target, project-wide two-generation/seven-day policy, and whether future manual
previews or normal daily worker execution is wanted. For StoryHook, automations
are intentionally off; leave them off. Manually invoke read-only `prune-common`
for the preview phase. Do not silently turn project automations on to obtain
scheduled pruning: that also permits other project automation. If automations
must remain off permanently, the existing scheduler intentionally stays inactive;
a separate permission design would need approval before implementation.

Once the installed build and configured hook are approved, enable build-product
enrollment only for newly dispatched managed lanes. First prove a synthetic
fresh-lane handoff, ordinary removal with retained pin, and a production read-only
inventory excluding all historical/frozen products. Review actual candidate
reports and filesystem identities before separately approving `mode = "apply"`.
Approve project automations independently if scheduled execution is wanted.
Keep the existing daily cadence and one-generation cap. Record exact pre/post
journals and filesystem free-space observations; `would-purge` is not reclaimed
space. No merge, install, stage-hook change, provider enrollment or deletion is
performed by this draft.

Rollback before deletion: set mode to dry-run or disable build products; keep
common-store records and pins. Rollback after deletion: preserve receipts and
rebuild the affected output. Do not remove common-store evidence to roll back.

## Validation scope

Focused Python regressions use synthetic targets and real local locks. Native
integration tests exercise actual Git worktree removal, Reset reservations,
common-store publication and the existing cleanup worker. They cover project
isolation, count/age/pins, stale selection, uncertain publication, copied custody,
substituted identities, partial purge, and rebuilt-original preservation.
The normal Rust retention wrapper includes the Python suite. Focused validation
is not the integrated merge gate, release validation or a performance campaign.
