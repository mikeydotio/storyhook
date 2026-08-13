//! The guards that make the route table load-bearing rather than decorative
//! (SH-254).
//!
//! `src/api/routes.rs` classifies every request into a [`Route`] and answers
//! `authority(&Route)` with the credential it needs. Both `match` expressions
//! that consume it are exhaustive, so a *new* route is a compile error until
//! somebody answers for it. That is the guarantee the story asked for, and
//! these tests exist because it has three holes an exhaustive `match` cannot
//! see on its own:
//!
//! 1. **A wildcard arm added later.** `_ => Authority::Session` compiles, reads
//!    as tidying up, and silently re-opens the entire surface. An exhaustive
//!    `match` cannot catch its own widening, so the source is read instead —
//!    the `tests/invoker_seam.rs::the_legacy_write_path_is_gone` idiom.
//! 2. **A variant nobody ever reaches.** A route can be declared, answered and
//!    classified and still be unreachable through `classify`, in which case the
//!    authority answer is a statement about nothing. The declared variants are
//!    read out of the source and every one of them has to be produced by a real
//!    path.
//! 3. **`Public` drifting from the exemption it describes.** `Authority::Public`
//!    claims a route is one SH-250's tokenless loopback read exemption already
//!    admits. Nothing but this test makes that claim true, and `token_exempt`
//!    is byte-frozen, so the two could disagree forever in silence.

use std::collections::BTreeSet;
use std::path::Path;
use std::time::Instant;

use chrono::Utc;

use storyhook::api::admission::admission;
use storyhook::api::http::TrustedHosts;
use storyhook::api::routes::{Authority, authority, classify};
use storyhook::api::session::SessionRegistry;
use storyhook::api::tokens::TokenRegistry;
use storyhook::daemon::http1::{Header, Method};

/// The route table's own source.
fn routes_source() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/api/routes.rs");
    std::fs::read_to_string(&path).expect("reading src/api/routes.rs")
}

/// The variant names declared by `enum <name>` in `source`.
///
/// A deliberately small parser: variants are the four-space-indented lines
/// inside the braces that begin with an uppercase letter. That holds because
/// `cargo fmt` runs on this repository, and if it ever stops holding the count
/// assertion below turns red rather than the set quietly shrinking.
fn declared_variants(source: &str, enum_name: &str) -> BTreeSet<String> {
    let header = format!("pub enum {enum_name}");
    let start = source
        .find(&header)
        .unwrap_or_else(|| panic!("`{header}` is not declared in src/api/routes.rs"));
    let body = &source[start..];
    let mut variants = BTreeSet::new();
    for line in body.lines().skip(1) {
        if line == "}" {
            break;
        }
        let trimmed = line.trim();
        if trimmed.starts_with("///") || trimmed.starts_with("//") {
            continue;
        }
        if !line.starts_with("    ") || line.starts_with("     ") {
            continue;
        }
        let name: String = trimmed
            .chars()
            .take_while(|c| c.is_alphanumeric())
            .collect();
        if name.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
            variants.insert(name);
        }
    }
    assert!(
        variants.len() > 5,
        "parsed only {} variants out of `{enum_name}` — the parser has drifted from the source",
        variants.len()
    );
    variants
}

/// Every path this test suite knows how to reach, and the method that reaches
/// it. One entry per route the daemon serves; the coverage test below is what
/// keeps this honest when a route is added.
const PROBES: &[(&str, Method)] = &[
    ("/", Method::Get),
    ("/", Method::Post),
    ("/api/nope", Method::Get),
    ("/api/v1/hello", Method::Get),
    ("/api/events", Method::Get),
    ("/api/repos", Method::Get),
    ("/api/repos", Method::Post),
    ("/api/repos/p", Method::Delete),
    ("/api/repos/p/data", Method::Get),
    ("/api/repos/p/data", Method::Put),
    ("/api/repos/p/nope", Method::Get),
    ("/api/repos/p/story", Method::Post),
    ("/api/repos/p/story/SH-1", Method::Get),
    ("/api/repos/p/story/SH-1", Method::Patch),
    ("/api/repos/p/story/SH-1", Method::Delete),
    ("/api/repos/p/story/SH-1/move", Method::Post),
    ("/api/repos/p/story/SH-1/nope", Method::Post),
    ("/api/repos/p/story/SH-1/dispatch", Method::Post),
    ("/api/repos/p/story/SH-1/dispatch/h4nd1e", Method::Get),
    ("/api/repos/p/states", Method::Get),
    ("/api/repos/p/states", Method::Post),
    ("/api/repos/p/states", Method::Patch),
    ("/api/repos/p/states/todo", Method::Patch),
    ("/api/repos/p/states/todo", Method::Delete),
    ("/api/repos/p/states/todo/archive", Method::Post),
    ("/api/repos/p/relate", Method::Post),
    ("/api/repos/p/unrelate", Method::Post),
];

fn segments(path: &str) -> Vec<&str> {
    path.split('/').filter(|s| !s.is_empty()).collect()
}

/// Every variant name [`PROBES`] actually produces, for both enums.
fn reached() -> (BTreeSet<String>, BTreeSet<String>) {
    let mut top = BTreeSet::new();
    let mut project = BTreeSet::new();
    for (path, method) in PROBES {
        let parts = segments(path);
        let route = classify(&parts, method);
        top.insert(route.name().to_string());
        if let Some(inner) = route.project_route() {
            project.insert(inner.name().to_string());
        }
    }
    (top, project)
}

#[test]
fn every_declared_route_is_reachable_through_classify() {
    // Hole 2, and the council's binding amendment: coverage is *derived* from
    // the enum rather than counted by hand. CLAUDE.md already records what a
    // hand-maintained inventory does here — "a hand-maintained count drifted
    // three times before it stopped being trusted" (SH-136).
    let source = routes_source();
    let (top_reached, project_reached) = reached();

    let unreached_top: Vec<String> = declared_variants(&source, "Route")
        .difference(&top_reached)
        .cloned()
        .collect();
    let unreached_project: Vec<String> = declared_variants(&source, "ProjectRoute")
        .difference(&project_reached)
        .cloned()
        .collect();

    assert!(
        unreached_top.is_empty() && unreached_project.is_empty(),
        "these routes are declared and classified but no path in this suite reaches them, \
         so their authority answer is a statement about nothing:\n  Route: {unreached_top:?}\n  \
         ProjectRoute: {unreached_project:?}\nAdd a probe to PROBES naming each one."
    );
}

#[test]
fn the_authority_table_has_no_wildcard_arm() {
    // Hole 1. A stray `_ =>` inside either authority function is the single
    // edit that re-opens the whole surface while every other test stays green,
    // because an exhaustive `match` is exhaustive over the variants that exist
    // and says nothing about a catch-all somebody adds beside them.
    //
    // `{ .. }` is *not* forbidden: it ignores a named variant's fields while
    // still naming the variant, which is the whole point. What is forbidden is
    // a pattern that matches routes nobody has thought about.
    let source = routes_source();
    for function in ["pub fn authority(", "fn project_authority("] {
        let start = source
            .find(function)
            .unwrap_or_else(|| panic!("`{function}` is gone from src/api/routes.rs"));
        let body = &source[start..];
        let end = body
            .find("\n}\n")
            .expect("every function in this file closes at column zero");
        let body = &body[..end];
        assert!(
            !body.contains("_ =>"),
            "`{function}` grew a wildcard arm. Every route must be classified by name: a \
             catch-all here grants (or refuses) routes nobody has considered, which is the \
             defect SH-254 exists to close."
        );
    }
}

#[test]
fn the_authority_table_names_every_route_it_classifies() {
    // The positive half of the test above: a wildcard scan alone passes if
    // somebody deletes the functions entirely, and passes if a variant is
    // routed to another variant's arm. Every declared name has to appear in the
    // authority source.
    let source = routes_source();
    let start = source
        .find("pub fn authority(")
        .expect("`authority` is gone from src/api/routes.rs");
    let table = &source[start..];

    let mut missing = Vec::new();
    for enum_name in ["Route", "ProjectRoute"] {
        for variant in declared_variants(&source, enum_name) {
            if !table.contains(&format!("{enum_name}::{variant}")) {
                missing.push(format!("{enum_name}::{variant}"));
            }
        }
    }
    assert!(
        missing.is_empty(),
        "the authority table does not name these routes: {missing:?}"
    );
}

#[test]
fn public_means_exactly_what_the_loopback_read_exemption_admits() {
    // Hole 3. `Authority::Public` claims "SH-250's tokenless loopback read
    // exemption already admits this". `token_exempt` is byte-frozen by this
    // story, so nothing else can keep the claim true — this asks the real gate,
    // through `admission`, with no credential anywhere.
    //
    // The two deliberately-excluded classes are named rather than skipped
    // silently. `NotFound`/`MethodNotAllowed` are `MasterToken` so a session
    // cannot map the surface by the difference between a 403 and a 404 — but
    // the exemption still admits a loopback `GET` of an unrouted path, so they
    // would fail this equivalence for a reason that is the design rather than a
    // drift. `/api/v1/*` never reaches `token_exempt` at all: `admission`
    // returns before it, leaving that surface to `rpc::admission`.
    const TOKEN: &str = "the-real-token";
    let loopback_read = [Header::from_bytes("Host", "127.0.0.1:3456").unwrap()];

    for (path, method) in PROBES {
        let parts = segments(path);
        let route = classify(&parts, method);
        if matches!(route.name(), "ControlSurface" | "Shell") {
            continue;
        }
        let fails_closed_by_design = matches!(route.name(), "NotFound" | "MethodNotAllowed")
            || route.project_route().is_some_and(|inner| {
                matches!(
                    inner.name(),
                    "NotFound" | "MethodNotAllowed" | "StoryActionUnknown"
                )
            });
        if fails_closed_by_design {
            continue;
        }

        let exempt = admission(
            &parts,
            method,
            None,
            &loopback_read,
            &TrustedHosts::default(),
            TOKEN,
            true,
            Instant::now(),
            // Empty, and no capability or named token is offered above: this
            // asks about the exemption alone, which is the claim
            // `Authority::Public` makes.
            &SessionRegistry::new(),
            &TokenRegistry::new(Utc::now(), Instant::now()),
            "storyhook_test",
            Utc::now(),
        )
        .is_none();

        assert_eq!(
            authority(&route) == Authority::Public,
            exempt,
            "{method} {path}: the route table says {:?} and the loopback read exemption \
             says {}",
            authority(&route),
            if exempt { "admitted" } else { "refused" }
        );
    }
}
