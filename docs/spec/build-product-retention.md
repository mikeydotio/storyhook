# Retention for detached StoryHook debug products

StoryHook v3.0.3. This is an opt-in draft; no project setting, scheduler, live
cache, provider enrollment, automation or verifier setting changes with it.

## Boundary

SH-835 already proves fresh managed dispatch enrollment, exact Verifying
submission generation, project permission, cleanup lease, Git-private identity,
and exclusive settled build custody before atomically detaching products. Its
existing foreground hook immediately purges the detached inode. The retention
hook instead leaves that quarantine intact and records a retention enrollment.
Neither hook follows the original product path. Recreated current products are
outside its authority.

This policy covers only a dedicated StoryHook **debug-only** Cargo target from
that existing protocol. It refuses release/custom-target layouts, unknown root
outputs, debug fixtures, uncertain ownership, unfinished detachment and legacy
jobs without explicit retention enrollment. It cannot infer safety from age,
process absence, directory names, or a successful build. It does not inspect or
adopt shared task targets, global Cargo caches, retired verifier directories,
source, release installations, logs, campaign inputs, backups or user data.
The debug layout allowlist is an additional check, not evidence of ownership.
The native fresh-enrollment and cooperating managed-entry contract remain the
ownership proof. A target carrying any non-reproducible evidence is ineligible.

Do not enroll campaign inputs. Pin an enrolled job **before** intentionally using
one of its artifacts as an input. Pins protect the entire detached generation;
they are never cleared by stage retries. External file references cannot be
inferred by the pruner. Current frozen campaigns and legacy caches require their
existing separate ownership and approval procedures.

## Retention and commands

All commands use the repository's selected Python runtime:

```sh
# Read-only report for the current checkout's private Git namespace.
bash scripts/retain-detached-products.sh prune

# A previously authorized native detachment hook supplies this exact journal.
# This step only preserves/enrolls the existing quarantine; it frees no space.
bash scripts/retain-detached-products.sh stage /absolute/private-git/storyhook-detached-products-v1/generation-123/journal.json

bash scripts/retain-detached-products.sh pin /absolute/private-git/storyhook-detached-products-v1/generation-123/journal.json
# Unpin is a separate explicit action, after the artifact is no longer needed.
bash scripts/retain-detached-products.sh unpin /absolute/private-git/storyhook-detached-products-v1/generation-123/journal.json

# Destructive, opt-in application of the freshly recomputed policy.
# Do not enable this in an unattended job until its scope/policy is reviewed.
bash scripts/retain-detached-products.sh prune --keep 2 --min-age-days 7 --apply
```

The default keeps the newest two intact generation numbers in this **one**
private Git namespace, all pins, and everything staged less than seven days ago.
Pinned older generations are retained in addition to that count. Future clocks
retain products. There is no directory-mtime inference, size/pressure override,
or automatic tightening of retention when a disk fills. Arguments must retain
at least one generation and one day. Reports name every job and its action and
reason; `would-purge` is a proposal, never evidence that space was reclaimed.

Only `--apply` removes bytes, using the existing descriptor-anchored detached
purger. It acquires exclusive whole-build custody nonblockingly and checks every
custody record. Any current build, unfinished owner or unknown record defers.
The inventory is revalidated under exclusion; the selected exact journal, inode,
layout and pin are rechecked inside the permanent job lock before deletion.
Each apply removes at most one eligible generation, oldest first. Once that job
lock protects the decision, build exclusion is released before recursive I/O so
a returned story can rebuild its separate current target. Another pruner sees
the busy job and defers. Further generations require a fresh invocation. Pin and
stage writes use that same lock. The older direct purge command refuses
retention-enrolled jobs, so it cannot accidentally bypass their pins or policy.
A held job lock, malformed entry, or partial inventory retains the whole set.
The scan is bounded to 1,024 jobs and never searches other worktrees.

Purged journals and locks remain as receipts and do not consume the retained
intact-generation count. Interrupted purges remain `purging` and stop automatic
pruning for that namespace; exact operator recovery must be reviewed. No retry
silently expands to the original path. Before deletion the products are still
in the native quarantine and can be inspected or copied out to a separately
owned destination while holding the job lock. No restore command overwrites a
current target. Deleted products require a rebuild from retained source and
locked dependencies; byte-identical historical reconstruction is not promised.

## Future activation, deliberately not performed here

After review, an operator may choose this foreground hook for a **new**, dedicated
debug-only managed worktree target:

```toml
[build_products]
enabled = false
path = "products"
managed_entry = "scripts/managed-cargo.sh"
hook = ["bash", "scripts/retain-detached-products.sh", "stage"]
timeout_seconds = 120
```

This example stays disabled. Changing it is a separate deployment decision.
Automations disabled, missing enrollment, restored/manual lanes, or unleased
handoffs still suppress detachment. No scheduler is installed in this draft.
A future approved scheduler can invoke the same `prune --apply` command in one
explicit checkout; it must retain reports, surface deferrals and never substitute
a broad `find`, `cargo clean`, or worktree sweep. Start with dry-run reports and
review the observed candidate set and policy before enabling deletion.

The quarantine lives in the worktree's private Git administration, as in SH-835.
Retention is a constraint on **this pruner**, not a backup or a guard against
separate `git worktree remove`, reset, or manual deletion. Removing that worktree
can remove its quarantine. Durable campaign inputs must live outside disposable
worktree administration. Extending retention across worktree removal requires a
separate common-store custody design; this draft does not claim that behavior.

## Validation

`scripts/tests/test_build_product_retention.py` uses synthetic target contents
and real local file locks. It covers dry-run preservation, age/count/pins, stale
plans, exclusive build custody, crash records, symlink and inode substitution,
release/evidence refusal, interrupted purge and preserved receipts/current data.
`tests/build_product_retention.rs` registers that suite in the normal Rust gate.
Focused direct Python validation does not certify the Rust wrapper, integrated
merge gate, deployment or a performance campaign.
