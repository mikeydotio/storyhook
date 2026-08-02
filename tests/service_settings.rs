//! `SettingsService` — the project settings row, reachable at last (SH-129).
//!
//! `project_settings` has been written since the store cutover and read since
//! before it, and until now the only writer was `story migrate`. A project born
//! after the flip could never change either setting it holds. This file is
//! about the *service* half of closing that: the registry that decides what a
//! key is, the read-modify-write that must not destroy its neighbours, and the
//! validation that must not accept a duration of zero.
//!
//! `tests/project_settings.rs` is the other half — the CLI grammar, the exit
//! codes, and the end-to-end proof that a written setting changes what
//! `story commit-sync` does.
//!
//! # Why nothing here is `#[cfg(feature = "github-sync")]`
//!
//! The `github.sync` key must render identically with and without that cargo
//! feature: the column is unconditional (`store::types::ProjectSettings`) while
//! `GithubSyncConfig` is feature-gated, so a surface that changed with the
//! build would be reporting a property of the compiler rather than of the data.
//! Every test below therefore writes the github document *through the store*
//! rather than through `GithubSyncService`, which is what lets them compile —
//! and assert the same thing — in both builds.

use serde_json::json;
use storyhook::error::AppError;
use storyhook::output::{SettingKind, SettingSource};
use storyhook::service::{SettingsService, settings_registry};
use storyhook::store::{ProjectSettings, ReadOps, Store, WriteOps};
use storyhook_test_support::ServiceFixture;

/// The three keys the registry is expected to hold, in order.
///
/// Written out rather than derived from the registry so that *deleting* a key
/// is a failing test rather than a silently smaller loop.
const KEYS: [&str; 3] = [
    "sync.auto_transition",
    "doctor.stale_threshold",
    "github.sync",
];

/// A value the given kind accepts, for a test that must write *something*
/// without caring what.
fn sample_for(kind: SettingKind) -> &'static str {
    match kind {
        SettingKind::Boolean => "false",
        SettingKind::Duration => "30d",
        SettingKind::Document => "{}",
    }
}

/// The settings row exactly as the store holds it.
fn stored(fixture: &ServiceFixture) -> ProjectSettings {
    fixture
        .store()
        .read(|tx| tx.settings(fixture.project()))
        .expect("reading the settings row")
}

/// Installs a github-sync document without going through `GithubSyncService`,
/// so this works in a `--no-default-features` build too.
fn install_github_document(fixture: &ServiceFixture, document: serde_json::Value) {
    fixture
        .store()
        .write(|tx| {
            let mut settings = tx.settings(fixture.project())?;
            settings.github_sync = Some(document);
            tx.put_settings(fixture.project(), &settings)
        })
        .expect("installing a github-sync document");
}

// ---------------------------------------------------------------------------
// The registry is the design
// ---------------------------------------------------------------------------

/// The load-bearing test of the whole story.
///
/// The registry carries `settable` as data, and the dispatcher consults it
/// rather than matching on key names — so the failure this guards against is a
/// registry row that says one thing and a code path that does another. It
/// iterates every entry rather than naming three, which is what makes a *fourth*
/// key added later inherit the check for free.
#[test]
fn every_registry_entry_agrees_with_its_dispatch() {
    for spec in settings_registry() {
        let fixture = ServiceFixture::new();
        let ctx = fixture.ctx();
        let service = SettingsService::new(&ctx);
        let key = spec.key();
        let outcome = service.set(key, sample_for(spec.kind()));

        if spec.settable() {
            let view = outcome.unwrap_or_else(|error| {
                panic!("`{key}` claims to be settable but `set` said: {error}")
            });
            assert_eq!(
                view.source,
                SettingSource::Set,
                "`{key}` did not record as set"
            );
            assert_eq!(
                view.value.as_deref(),
                Some(sample_for(spec.kind())),
                "`{key}` did not round trip"
            );
        } else {
            let error = outcome.expect_err(&format!(
                "`{key}` claims to be read-only but `set` accepted a write"
            ));
            assert!(
                matches!(error, AppError::Usage(_)),
                "a write to the read-only `{key}` must be a usage error, not {error:?}"
            );
            let owner = spec
                .managed_by()
                .expect("a key that is not settable must name the command that owns it");
            assert!(
                error.to_string().contains(owner),
                "the refusal must name `{owner}`, the command that owns `{key}`: {error}"
            );
        }
    }
}

/// `unset` obeys the same registry row `set` does. Two dispatch paths, one
/// source of truth — and a read-only key that could be *cleared* would be no
/// more read-only than one that could be written.
#[test]
fn unset_refuses_exactly_the_keys_set_refuses() {
    for spec in settings_registry() {
        let fixture = ServiceFixture::new();
        let ctx = fixture.ctx();
        let outcome = SettingsService::new(&ctx).unset(spec.key());
        assert_eq!(
            outcome.is_ok(),
            spec.settable(),
            "`unset` and `set` disagree about whether `{}` is writable",
            spec.key()
        );
    }
}

/// The listing is the registry, in the registry's order.
#[test]
fn the_listing_is_every_registered_key_in_order() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let views = SettingsService::new(&ctx).list().expect("listing settings");

    let listed: Vec<&str> = views.iter().map(|view| view.key.as_str()).collect();
    assert_eq!(listed, KEYS, "the listing drifted from the registry");
    let registered: Vec<&str> = settings_registry().iter().map(|s| s.key()).collect();
    assert_eq!(listed, registered, "the listing drifted from the registry");
}

// ---------------------------------------------------------------------------
// The corruption hazard — the council called this decisive
// ---------------------------------------------------------------------------

/// `put_settings` rewrites every column unconditionally, by design. So a `set`
/// that builds a fresh `ProjectSettings` instead of reading the existing row
/// destroys the github-sync document — silently, on the first write, and only
/// for users who had configured it.
///
/// The comparison is on the stored `serde_json::Value` rather than on a parsed
/// config, so it holds in a `--no-default-features` build and would catch a
/// re-serialization that changed nothing but key order.
#[test]
fn setting_an_unrelated_key_leaves_the_github_document_byte_identical() {
    let fixture = ServiceFixture::new();
    let document = json!({
        "github": { "owner": "acme", "repo": "widgets" },
        "sync": { "mode": "auto", "last_sync_at": "2026-03-30T12:00:00Z" },
        "etags": { "issues": "abc123" },
        "mappings": [{ "story_id": "SH-1", "issue_number": 42 }],
    });
    install_github_document(&fixture, document.clone());

    let ctx = fixture.ctx();
    SettingsService::new(&ctx)
        .set("sync.auto_transition", "false")
        .expect("setting an unrelated key");

    assert_eq!(
        stored(&fixture).github_sync,
        Some(document),
        "the github-sync document did not survive a write to another key"
    );
}

/// `unset` runs the same read-modify-write and carries the same hazard.
#[test]
fn unsetting_an_unrelated_key_leaves_the_github_document_byte_identical() {
    let fixture = ServiceFixture::new();
    let document = json!({ "etags": { "issues": "abc123" } });
    install_github_document(&fixture, document.clone());

    let ctx = fixture.ctx();
    let service = SettingsService::new(&ctx);
    service
        .set("doctor.stale_threshold", "14d")
        .expect("setting a duration");
    service
        .unset("doctor.stale_threshold")
        .expect("clearing a duration");

    assert_eq!(
        stored(&fixture).github_sync,
        Some(document),
        "the github-sync document did not survive an unset of another key"
    );
}

/// The other direction of the same invariant: two writable keys must not
/// clobber each other either.
#[test]
fn writing_one_key_leaves_the_other_writable_key_alone() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let service = SettingsService::new(&ctx);

    service
        .set("doctor.stale_threshold", "21d")
        .expect("setting a duration");
    service
        .set("sync.auto_transition", "false")
        .expect("setting a boolean");

    let row = stored(&fixture);
    assert_eq!(row.doctor_stale_threshold.as_deref(), Some("21d"));
    assert_eq!(row.sync_auto_transition, Some(false));
}

// ---------------------------------------------------------------------------
// set, default and unset are three different answers
// ---------------------------------------------------------------------------

/// An unwritten `sync.auto_transition` is not "unset": `git.rs` reads it as
/// `true`, so the effective value is `true` and its source is the code default.
/// Reporting it as unset would tell a user nothing is in force when something
/// is.
#[test]
fn an_unwritten_boolean_reports_the_code_default_that_is_actually_in_force() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let view = SettingsService::new(&ctx)
        .get("sync.auto_transition")
        .expect("getting an unwritten boolean");

    assert_eq!(view.source, SettingSource::Default);
    assert_eq!(view.value.as_deref(), Some("true"));
    assert_eq!(view.default.as_deref(), Some("true"));
    assert!(view.settable);
    assert_eq!(view.managed_by, None);
}

/// An unwritten `doctor.stale_threshold` has no default at all — nothing in
/// `src/` supplies one — so it is genuinely unset. This is the case a
/// `is_default: bool` would have lied about.
#[test]
fn an_unwritten_duration_reports_unset_because_it_has_no_default() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let view = SettingsService::new(&ctx)
        .get("doctor.stale_threshold")
        .expect("getting an unwritten duration");

    assert_eq!(view.source, SettingSource::Unset);
    assert_eq!(view.value, None);
    assert_eq!(view.default, None);
}

/// Unsetting a written key returns it to whichever of those two it was.
#[test]
fn unset_returns_a_key_to_the_state_it_was_born_in() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let service = SettingsService::new(&ctx);

    service
        .set("sync.auto_transition", "false")
        .expect("setting a boolean");
    let boolean = service.unset("sync.auto_transition").expect("clearing it");
    assert_eq!(boolean.source, SettingSource::Default);
    assert_eq!(boolean.value.as_deref(), Some("true"));

    service
        .set("doctor.stale_threshold", "14d")
        .expect("setting a duration");
    let duration = service
        .unset("doctor.stale_threshold")
        .expect("clearing it");
    assert_eq!(duration.source, SettingSource::Unset);
    assert_eq!(duration.value, None);
}

/// Unsetting something that was never set is a no-op, not an error. The store
/// ends in the state the user asked for either way, which is the whole test of
/// an idempotent verb.
#[test]
fn unsetting_an_unwritten_key_is_a_no_op() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let view = SettingsService::new(&ctx)
        .unset("doctor.stale_threshold")
        .expect("unsetting what was never set");

    assert_eq!(view.source, SettingSource::Unset);
    assert_eq!(stored(&fixture).doctor_stale_threshold, None);
}

// ---------------------------------------------------------------------------
// The github key: presence, never shape
// ---------------------------------------------------------------------------

/// The github document is reported as opaque presence. Serializing its shape
/// would make the surface a property of the build, because the type that
/// describes that shape does not exist without the cargo feature.
#[test]
fn the_github_key_reports_presence_and_never_the_document_itself() {
    let fixture = ServiceFixture::new();
    install_github_document(&fixture, json!({ "etags": { "issues": "abc123" } }));

    let ctx = fixture.ctx();
    let view = SettingsService::new(&ctx)
        .get("github.sync")
        .expect("getting the github key");

    assert_eq!(view.kind, SettingKind::Document);
    assert_eq!(view.source, SettingSource::Set);
    assert!(!view.settable);
    assert_eq!(view.managed_by.as_deref(), Some("story github-sync"));
    let value = view
        .value
        .as_deref()
        .expect("a configured document is present");
    assert!(
        !value.contains("abc123") && !value.contains("etags"),
        "the github document leaked into the settings surface: {value}"
    );
}

#[test]
fn an_absent_github_document_reports_unset() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let view = SettingsService::new(&ctx)
        .get("github.sync")
        .expect("getting the github key");

    assert_eq!(view.source, SettingSource::Unset);
    assert_eq!(view.value, None);
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// `domain::parse_duration` parses its count as `i64`, so it answers `Some`
/// for `0d` and for `-14d`. A threshold of zero or of negative time is not a
/// threshold, and accepting one here would store a value `story doctor` could
/// never act on sensibly.
#[test]
fn a_duration_must_be_strictly_positive_and_well_formed() {
    for bad in ["0d", "-14d", "0h", "14", "14x", "d", "", "   "] {
        let fixture = ServiceFixture::new();
        let ctx = fixture.ctx();
        let error = SettingsService::new(&ctx)
            .set("doctor.stale_threshold", bad)
            .expect_err(&format!("`{bad}` must not be accepted as a duration"));
        assert!(
            matches!(error, AppError::Validation(_)),
            "a bad value for a writable key is a validation error, not {error:?}"
        );
        assert_eq!(
            stored(&fixture).doctor_stale_threshold,
            None,
            "a refused write must not reach the store"
        );
    }
}

#[test]
fn a_well_formed_duration_is_stored_trimmed_and_verbatim() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let view = SettingsService::new(&ctx)
        .set("doctor.stale_threshold", "  2w  ")
        .expect("setting a padded duration");

    assert_eq!(view.value.as_deref(), Some("2w"));
    assert_eq!(
        stored(&fixture).doctor_stale_threshold.as_deref(),
        Some("2w")
    );
}

#[test]
fn a_boolean_takes_true_or_false_in_any_case_and_nothing_else() {
    for (raw, expected) in [
        ("true", true),
        ("false", false),
        ("TRUE", true),
        ("  False  ", false),
    ] {
        let fixture = ServiceFixture::new();
        let ctx = fixture.ctx();
        SettingsService::new(&ctx)
            .set("sync.auto_transition", raw)
            .unwrap_or_else(|error| panic!("`{raw}` should be a boolean: {error}"));
        assert_eq!(stored(&fixture).sync_auto_transition, Some(expected));
    }

    for bad in ["yes", "no", "1", "0", "on", "", "maybe"] {
        let fixture = ServiceFixture::new();
        let ctx = fixture.ctx();
        let error = SettingsService::new(&ctx)
            .set("sync.auto_transition", bad)
            .expect_err(&format!("`{bad}` must not be accepted as a boolean"));
        assert!(
            matches!(error, AppError::Validation(_)),
            "a bad boolean is a validation error, not {error:?}"
        );
    }
}

/// A written boolean is canonical, so `TRUE` and `true` are one stored value
/// and the listing cannot show a spelling the user happened to use.
#[test]
fn a_written_boolean_reads_back_canonically() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let view = SettingsService::new(&ctx)
        .set("sync.auto_transition", "FALSE")
        .expect("setting a boolean");
    assert_eq!(view.value.as_deref(), Some("false"));
}

// ---------------------------------------------------------------------------
// Unknown keys
// ---------------------------------------------------------------------------

/// An unknown key is a *usage* error, not a not-found: the key set is
/// compile-time grammar rather than a store entity, and `NotFound` would
/// conflate "no such key" with "that key is not set" — the exact distinction
/// the three-valued source exists to preserve.
#[test]
fn an_unknown_key_is_a_usage_error_that_names_every_known_key() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let service = SettingsService::new(&ctx);

    for outcome in [
        service.get("sync.auto-transition"),
        service.set("nonsense", "x"),
        service.unset("sync_auto_transition"),
    ] {
        let error = outcome.expect_err("an unknown key must be refused");
        assert!(
            matches!(error, AppError::Usage(_)),
            "an unknown key is a usage error, not {error:?}"
        );
        let message = error.to_string();
        for key in KEYS {
            assert!(
                message.contains(key),
                "the refusal must name `{key}` so the user can find the right one: {message}"
            );
        }
    }
}
