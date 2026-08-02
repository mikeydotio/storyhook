//! The two guarantees about git-origin identity that the conformance suite
//! cannot express.
//!
//! Everything about *behaviour* — many origins per project, one project per
//! origin, a refusal that names the holder — lives in the store conformance
//! suite, because it must hold for any `Store` implementation. Two things do
//! not fit there:
//!
//! - the **schema** half of the uniqueness rule, which has to be attacked with
//!   a raw connection, because nothing reachable through `WriteOps` can trip it;
//! - a rule about the **source tree**, which is not a runtime property at all.

mod store_support;

use std::path::Path;

use store_support::{new_store, raw, seed_project};
use storyhook::domain::remote::RemoteUrl;
use storyhook::store::{ReadOps, Store, WriteOps};

const AT: &str = "2026-08-02T12:00:00Z";

fn remote(raw_url: &str) -> RemoteUrl {
    RemoteUrl::normalize(raw_url).expect("the fixture url should normalize")
}

// ---------------------------------------------------------------------------
// The index is a real constraint, not just a message
// ---------------------------------------------------------------------------

/// `link_remote` reads the current holder before inserting, so that its refusal
/// can name the project. That check is the *message*; this is the **rule**.
///
/// The distinction matters because the two can be separated: an importer, a
/// migration, a second binary sharing the store, or a future call site that
/// writes the table directly would all bypass the read and none of them would
/// bypass this. Asserted against a raw connection for exactly that reason —
/// there is no way to reach it through the store's own API.
#[test]
fn the_schema_refuses_a_second_project_claiming_one_origin() {
    let (_dir, store) = new_store();
    let alpha = seed_project(&store, "alpha", "SH");
    let beta = seed_project(&store, "beta", "SH");
    store
        .write(|tx| tx.link_remote(alpha, &remote("https://github.com/acme/widgets.git"), AT))
        .expect("the first registration should be accepted");

    let error = raw(&store)
        .execute(
            "INSERT INTO project_remotes (project_id, normalized, raw, registered_at) \
             VALUES (?1, 'github.com/acme/widgets', 'https://github.com/acme/widgets', ?2)",
            rusqlite::params![beta.get(), AT],
        )
        .expect_err("the unique index should have refused this");
    assert!(
        error.to_string().contains("UNIQUE"),
        "expected a uniqueness failure, got: {error}"
    );

    // And the holder is untouched, which is the property that actually matters:
    // resolution still has exactly one answer.
    assert_eq!(
        store
            .read(|tx| tx.project_by_remote(&remote("git@github.com:acme/widgets")))
            .unwrap()
            .expect("the origin should still resolve")
            .id,
        alpha
    );
}

/// A project may hold many origins — so the uniqueness is on the origin, not on
/// the project. Without this, a schema that made the whole thing a one-to-one
/// mapping would pass the test above and still be wrong.
#[test]
fn the_schema_allows_one_project_to_hold_many_origins() {
    let (_dir, store) = new_store();
    let alpha = seed_project(&store, "alpha", "SH");
    for url in [
        "https://github.com/acme/widgets.git",
        "https://github.com/old-owner/widgets.git",
        "/Volumes/nas/git/widgets.git",
    ] {
        store
            .write(|tx| tx.link_remote(alpha, &remote(url), AT))
            .unwrap_or_else(|error| panic!("`{url}` should have been accepted: {error}"));
    }
    assert_eq!(store.read(|tx| tx.project_remotes(alpha)).unwrap().len(), 3);
}

/// The empty-string guards are unreachable through `RemoteUrl`, which refuses an
/// empty remote before the store ever sees one. They exist for the writer that
/// does not go through it.
#[test]
fn the_schema_refuses_an_empty_key_or_an_empty_raw_form() {
    let (_dir, store) = new_store();
    let alpha = seed_project(&store, "alpha", "SH");

    for (normalized, raw_url) in [
        ("", "https://github.com/acme/widgets"),
        ("github.com/a/b", ""),
    ] {
        let error = raw(&store)
            .execute(
                "INSERT INTO project_remotes (project_id, normalized, raw, registered_at) \
                 VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![alpha.get(), normalized, raw_url, AT],
            )
            .expect_err("the CHECK constraint should have refused this");
        assert!(
            error.to_string().contains("CHECK"),
            "expected a check-constraint failure, got: {error}"
        );
    }
}

// ---------------------------------------------------------------------------
// One grammar, and one reaction to its refusals
// ---------------------------------------------------------------------------

/// `NormalizeError` is registration-shaped: its variants exist so a user typing
/// a URL can be told *which* mistake they made. A resolution path has exactly
/// one reaction to every variant — this origin does not name a project, try the
/// next rule — and telling them apart there is the bug, not the feature.
///
/// `RemoteUrl::normalize_for_lookup` exists so that path never has to name the
/// enum at all. This asserts that nothing outside the normalizer's own module
/// does.
///
/// **This test is green the day it lands, and that is understood.** Nothing
/// resolves a project by origin yet — SH-116 writes that. It is here rather than
/// in SH-116's own PR because it is a specification of intent that the later
/// code must not violate, and because the failure it guards against is silent:
/// a lookup that matches on `NormalizeError` compiles, passes review as
/// "handling the error properly", and quietly makes a malformed origin behave
/// differently from an absent one.
///
/// The assertion greps the source, in the shape `invoker_seam.rs` already uses
/// for this kind of rule: it is what a human reviewer would do, it needs no
/// build graph, and it cannot be satisfied by a re-export.
#[test]
fn a_normalize_error_is_never_matched_outside_the_normalizer() {
    fn sources(dir: &Path, into: &mut Vec<(String, String)>) {
        for entry in std::fs::read_dir(dir).expect("reading src/") {
            let path = entry.expect("a directory entry").path();
            if path.is_dir() {
                sources(&path, into);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                let text = std::fs::read_to_string(&path).expect("reading a source file");
                into.push((path.to_string_lossy().into_owned(), text));
            }
        }
    }

    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    sources(&root, &mut files);
    assert!(
        files.len() > 20,
        "expected the whole tree, got {} — the walk is broken, not the rule",
        files.len()
    );

    let home = root.join("domain").join("remote.rs");
    assert!(
        home.exists(),
        "the normalizer moved; this rule needs its new home"
    );

    let offenders: Vec<&str> = files
        .iter()
        .filter(|(path, text)| {
            !Path::new(path).ends_with("domain/remote.rs") && text.contains("NormalizeError::")
        })
        .map(|(path, _)| path.as_str())
        .collect();

    assert!(
        offenders.is_empty(),
        "these files name a `NormalizeError` variant: {offenders:?}\n\
         A resolution path must call `RemoteUrl::normalize_for_lookup`, which answers `None` to \
         every refusal, rather than deciding which refusals count. If a registration path needs \
         to tell a user which mistake they made, let the error's own `Display` do it — that is \
         what `From<NormalizeError> for AppError` is for."
    );
}
