//! Shared integration-test harness for storyhook.
//!
//! Every one of storyhook's integration-test binaries used to declare its own
//! `fn story(dir)`, its own temp-directory helper, and its own idea of how
//! isolated a test needs to be. Thirty-four copies of the same four lines is
//! not merely repetitive: it is thirty-four places that have to be found and
//! corrected when the isolation contract changes, and the data-layer
//! rearchitecture changes it — storyhook is moving from per-project
//! `.storyhook/` directories to a single global store, and on the day that
//! lands, every test that was not isolating `$HOME` and the XDG directories
//! starts writing into the developer's real data.
//!
//! So the contract lives here, in one crate that compiles once:
//!
//! - [`TestEnv`] — a private `HOME`, `XDG_DATA_HOME`, `XDG_CONFIG_HOME`,
//!   `XDG_STATE_HOME` and `STORYHOOK_DATA_DIR`, applied to every command it
//!   builds, whether or not today's code reads them.
//! - [`ProjectBuilder`] — storyhook project fixtures, from a bare initialized
//!   directory up to a git repo with a local origin and a linked worktree.
//! - [`scratch_dir`] — fixture directories outside the Spotlight-indexed
//!   `$TMPDIR` (SH-53). `clippy.toml` bans the alternatives.
//! - [`serve`] and friends — dashboard servers that cannot be handed a port
//!   somebody else holds, and cannot outlive the test that started them
//!   (SH-51).
//! - [`crash_the_daemon`] and friends — killing the process that owns the write
//!   transaction, which since the transport collapsed is the daemon.
//! - [`non_temporary_dir`] — a fixture directory the store guards classify as
//!   **not throwaway**, for the handful of tests that need one (SH-258).
//!
//! # Using it
//!
//! ```no_run
//! use storyhook_test_support::TestEnv;
//!
//! let env = TestEnv::shared();
//! let project = env.project().prefix("TST").build();
//! let id = project.new_story("something to do");
//! project.run(&["move", &id, "in-progress"]).success();
//! ```

pub mod command_reference;
mod crash;
mod engine;
mod env;
mod git_identity;
mod github;
mod hooks_manifest;
mod legacy_tree;
mod project;
mod pty;
mod real_store;
mod scratch;
mod server;
mod service;
mod source_scan;
mod store;
mod tailnet;

pub use crash::{Crash, assert_no_daemon, crash_a_starting_daemon, crash_the_daemon, spawn_daemon};
pub use engine::{DispatcherCall, DispatcherStep, FakeDispatcher};
pub use env::{TestEnv, assert_the_binary_can_fire_faults, daemon_containment, story_binary};
pub use git_identity::approve_fixture_identity;
pub use github::{FakeGithubApiFactory, RecordedCall};
pub use hooks_manifest::{
    DeclaredHook, HOOKS_MANIFEST, all_declared_hooks, declared_timeout, hook_script,
};
pub use project::{
    Project, ProjectBuilder, SecondCheckout, assert_selection_is_not_inherited, git, slug_at,
};
pub use pty::{EXPECT_TIMEOUT, Pty, Watchdog, watchdog};
pub use real_store::{REAL_ROOT_VAR, non_temporary_dir};
pub use scratch::{scratch_dir, scratch_dir_named, scratch_root};
pub use server::{
    ChildGuard, DaemonGuard, STORY_COMMAND_DEADLINE, TestServer, http_status_line, port_of,
    reserve_port, run_bounded, serve, try_serve_on, wait_for_addr, wait_for_server,
};
pub use service::{FIXTURE_NOW, ServiceFixture, default_states, default_types};
pub use source_scan::without_rust_comments;
pub use store::project_id_at;
pub use tailnet::path_without_tailscale;
