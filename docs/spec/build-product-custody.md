# Whole-build custody for worktree products

SH-835, StoryHook v3.0.3. The approved supported boundary is a repository-local
managed Cargo entry. Compiler wrappers alone cannot cover a cached build script,
Cargo's own phases, or surviving descendants. No global Cargo shim, toolchain
replacement, or user configuration change is part of this boundary.

## Managed entry

Use `scripts/managed-cargo.sh` with the ordinary Cargo arguments. The entry selects
the repository's supported Python runtime, resolves Cargo once, and retains whole
build custody before starting it. Cross-build callers can preserve an exact
toolchain with `--cargo-executable /absolute/cargo -- <arguments>`.

The entry owns a shared product lock in the checkout's private Git directory,
under `storyhook-build-products-v1`. A separate fsynced record identifies every
managed invocation. The blocked launcher is registered before it executes Cargo.
The process session and an inherited lifetime guard must both settle before the
record becomes finished. Root exit alone is insufficient: surviving session
members are drained, and a descendant outside the session that retains the guard
prevents settlement. Deliberately escaping both the session and every custody
descriptor is outside this cooperating managed-entry contract.

Cargo's argv, standard streams, normal exit status and signal termination are
preserved. Nested calls in the same checkout inherit product exclusion. A nested
call for another checkout establishes that checkout's own custody while retaining
the outer inheritable descriptors. Cargo jobserver descriptors remain inherited.

Product exclusion operates even when host admission is disabled. When admission
is enabled, the `cargo-managed` entry requires its measured policy workload; no
resource estimates are invented here. An existing host grant is preserved, and
the admission adapter does not reserve a second root. Product custody never
creates a host grant. Existing compiler-slot and binary-lease mechanisms remain.
This change does not enable or calibrate host admission.

The Makefile, test runner and pool, test discovery, compiler confirmation probe,
coverage preparation, baseline capture, browser build, scratch builds, and native
release builds use the managed entry. The isolated causal Rust diagnostic already
has its own admitted owner and separate output directory; the Lima release builds
an extracted source archive under its guest owner, outside a Git worktree. Neither
exception enrolls a handed-off story worktree for reclamation.

## Unknown ownership and recovery

The lock inode is permanent. It is never unlinked, replaced, or explicitly
unlocked while descendants could hold it. A reclaim attempt must acquire the
exclusive product lock and inspect every custody record. Any unfinished,
unreadable, unknown, symlinked or multiply linked authority defers reclamation.
Age, a vanished pane, an absent supervisor PID, and an idle compiler semaphore
are not settlement evidence. Unfinished records are deliberately retained after
a crash; this increment does not silently recover or discard them.

Bare Cargo and legacy providers do not acquire this managed guarantee. Calling
the wrapper once does not enroll a legacy worktree or prove its unknown owners
settled. A complete reclaimer must separately prove the exact project, cleanup
lease, Git-private identity, managed dispatch enrollment and current Verifying
generation before invoking a project-configured foreground hook. No target path
is a generic Rust default, and a shared, external or symlinked target is never
eligible for deletion. Retired verifier directories require independent owner
evidence; directory names and modification times do not authorize a sweep.

## Guarded detachment and configured foreground purge

Atomic detachment followed by deletion is the approved strategy. Reset retains
its existing 60-second workspace-lock fallback. The short native store write
checks the exact Verifying event generation captured by `story move`, its
adjacent cleanup lease, the automation generation fence, and fresh dispatch
product enrollment. It checks exclusive whole-build custody and anchors both
rename parents with directory descriptors. The rename itself occurs under the
store write guard; no shell, subprocess, recursive traversal, or deletion does.
Reset and repair can proceed while the later purge runs. New products created at
the original path are outside the purge authority.

Reclamation is opt-in. This repository does **not** enable it in its pointer
file in this change. An operator can configure a cooperating project with:

```toml
[build_products]
enabled = false
path = "target"
managed_entry = "scripts/managed-cargo.sh"
hook = ["bash", "scripts/purge-detached-products.sh"]
timeout_seconds = 120
```

The generic service has no default product path. The path is one directory
component inside the enrolled linked worktree. Absolute, nested, dot-prefixed,
tracked, symlinked, shared/external and cross-filesystem roots are refused. The
configured hook is trusted project code, invoked as argv without a shell unless
explicitly configured; its sole appended argument is a detached journal, never
an original path. It must implement this detached-only protocol and stay in the
foreground. The supplied hook selects the supported Python runtime, owns its
job lock, validates journal and inode identity, and walks open descriptors without
following symlinks. Concurrent callers cannot purge the same job. Hook failure or
cancellation preserves the journal and any remaining detached products.

Fresh dispatch enrollment occurs only when the plugin just created the worktree,
the configured target does not exist (even an empty directory refuses), and the
provider is given the managed-entry charter before its story task. Enrollment is
exclusive and fsynced in private Git administration. Reused/restored/manual lanes
are not retroactively enrolled. A config or cleanup identity change defers cleanup.
The cooperating provider must route every product-generating command through the
managed entry; arbitrary bare Cargo, escaped descriptors, or hostile same-user
processes are outside this contract. This software does not enroll any live lane.

`story move ... verifying` captures the exact event identity in its commit.
Delayed callbacks cannot adopt a later stay, including after disable/re-enable.
`--no-hooks` and disabled project automations suppress reclamation. Alternative
state-edit paths that do not record a cleanup lease retain products. A live owner,
held lock, crashed supervisor or ambiguous custody record also retains products;
a later explicit submission can retry with its own fresh generation. There is no
age-based sweep or verifier-directory name heuristic.

Before rename a durable job records the original product inode and planned
quarantine identity. Crash before rename leaves the original untouched. Crash
after rename can be recovered solely from the detached inode, even when the
journal still says `prepared`. To retry an interrupted detached purge, run the
configured hook with that **one exact** private Git journal path. The recovery
hook never reads or deletes the original path. Missing products are success only
with a durable `purging` or `purged` record; missing unproved detachment is a
refusal. Records remain for audit after successful removal. Retired verifier
worktrees without equivalent exact enrollment and custody remain untouched.

A returned story runs the same managed Cargo entry: Cargo creates its absent
output directory normally. Builds wait up to 30 seconds, cancellably and before
launch, for a short detachment lock; reclamation itself never waits for a live
build. This closes the brief return/rebuild overlap without holding Reset behind
a long purge. Reclamation does not delete sources, Git state,
provider/session evidence, global caches, or shared verifier caches, and does not
enable host admission, project automation, provider enrollment on existing lanes,
or production rollout. Those are separate operator actions.
