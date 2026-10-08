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

## Remaining SH-835 integration

This custody increment does not delete products, install a reclaim hook, enroll
providers, or complete SH-835. Project automations remain off. The integration
must reconcile two existing contracts before destructive behavior is added:

* SH-835's proposed reservation retains the exact Verifying generation through
  hook descendant settlement and rejects a concurrent return for repair.
* Card Reset is a final recovery lever. `src/service/story_reset.rs` proceeds
  after 60 seconds even if workspace exclusion cannot be acquired.

The pending decision is whether to detach validated products atomically during a
short generation-guarded operation and purge only detached files afterward, or
extend Reset's waiting contract through reclaim settlement. A long-running hook
must not execute under a store write transaction. A state snapshot followed by
unreserved deletion is not an acceptable implementation of either choice.

After that decision, acceptance still requires configured-hook validation,
generation/repair/reset race fixtures, provider enrollment and legacy deferral,
cancellation/crash retention, exact-path refusal, and a returned-story rebuild.
