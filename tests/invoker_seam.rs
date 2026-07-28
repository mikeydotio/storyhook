//! The Invoker seam's equivalence contract.
//!
//! [`LegacyInvoker`] exists to be *provably* the same thing as calling
//! `app::run`, so that adopting it across the CLI and the web server is a
//! no-op that later implementations can be swapped into. It forwards
//! verbatim, so equivalence is true by construction — this file pins it
//! anyway, because "forwards verbatim" is exactly the kind of property a
//! well-meaning refactor breaks quietly.

use storyhook::app;
use storyhook::cli::{CliOptions, Invocation};
use storyhook::error::AppError;
use storyhook::invoke::{InvokeRequest, Invoker, LegacyInvoker};
use storyhook::output::{render_error, render_response};
use storyhook_test_support::TestEnv;

/// The same four rendering modes `tests/wire_envelope.rs` checks.
const RENDER_MODES: [(bool, bool); 4] =
    [(false, false), (true, false), (false, true), (true, true)];

/// Reading and writing invocations both produce, through the seam, exactly
/// what a direct `app::run` produces — compared as rendered bytes, in every
/// rendering mode, because rendered bytes are what a caller sees.
#[test]
fn the_seam_renders_what_a_direct_app_run_renders() {
    let env = TestEnv::shared();
    let project = env.project().build();
    let id = project.new_story("A story to look at");

    let cases = [
        Invocation::Show { id: id.clone() },
        Invocation::List {
            state: None,
            assignee: None,
            flagged: false,
            priority: None,
            label: None,
            created_after: None,
            updated_after: None,
            blocked: false,
            ready: false,
            stale: None,
            phase: None,
            story_type: None,
        },
        Invocation::Summary,
        // A mutation as well as reads: the seam must not change what a write
        // returns either. Idempotent, so running it twice is safe.
        Invocation::SetPriority {
            id: id.clone(),
            priority: "high".to_string(),
        },
    ];

    for invocation in cases {
        let direct = app::run(
            project.path(),
            CliOptions {
                json: false,
                quiet: false,
                no_hooks: false,
                invocation: invocation.clone(),
            },
        )
        .expect("the direct call must succeed");

        let seamed = LegacyInvoker::new(project.path())
            .invoke(InvokeRequest::new(invocation.clone()))
            .expect("the seam must succeed");

        for (json, quiet) in RENDER_MODES {
            assert_eq!(
                render_response(&direct, json, quiet),
                render_response(&seamed, json, quiet),
                "{invocation:?} rendered differently through the seam \
                 (json={json}, quiet={quiet})"
            );
        }
    }
}

/// Errors take the same path: same variant, same exit code, same rendering.
#[test]
fn the_seam_reports_errors_unchanged() {
    let env = TestEnv::shared();
    let project = env.project().build();
    let invocation = Invocation::Show {
        id: "SH-404".to_string(),
    };

    let direct = app::run(
        project.path(),
        CliOptions {
            json: false,
            quiet: false,
            no_hooks: false,
            invocation: invocation.clone(),
        },
    )
    .expect_err("showing a missing story must fail");

    let seamed = LegacyInvoker::new(project.path())
        .invoke(InvokeRequest::new(invocation))
        .expect_err("showing a missing story must fail through the seam too");

    assert!(matches!(direct, AppError::NotFound(_)));
    assert!(matches!(seamed, AppError::NotFound(_)));
    assert_eq!(direct.exit_code(), seamed.exit_code());
    for json in [false, true] {
        assert_eq!(render_error(&direct, json), render_error(&seamed, json));
    }
}

/// `no_hooks` is the only execution setting that crosses the seam, so its
/// default matters: a request built without saying otherwise must run hooks,
/// which is what an un-flagged `story` command does.
#[test]
fn a_request_runs_hooks_unless_told_not_to() {
    let request = InvokeRequest::new(Invocation::Summary);
    assert!(!request.no_hooks, "hooks are on by default");
    assert!(request.clone().no_hooks(true).no_hooks);
    assert!(!request.no_hooks(false).no_hooks);
}

/// The request envelope crosses a process boundary in later waves, so it has
/// to survive one now. (`tests/wire_envelope.rs` proves the same for the
/// `Invocation` it carries.)
#[test]
fn a_request_survives_a_wire_hop() {
    let request = InvokeRequest::new(Invocation::Show {
        id: "SH-1".to_string(),
    })
    .no_hooks(true);
    let encoded = serde_json::to_string(&request).expect("an InvokeRequest must serialize");
    let decoded: InvokeRequest =
        serde_json::from_str(&encoded).expect("an InvokeRequest must deserialize");
    assert_eq!(request, decoded);
}
