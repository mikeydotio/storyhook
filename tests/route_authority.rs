//! Route table coverage: every declared [`Route`]/[`ProjectRoute`] variant
//! is actually reachable through [`classify`].
//!
//! `src/api/routes.rs` classifies every request into a `Route`, and its
//! `match` is exhaustive — a *new* route is a compile error until somebody
//! adds it there. That guarantee has one hole an exhaustive `match` cannot
//! see on its own: a variant can be declared and classified and still be
//! unreachable through `classify` in practice, in which case nothing in this
//! daemon ever produces it. The declared variants are read out of the source
//! and every one of them has to be produced by a real path in [`PROBES`].
//!
//! This file used to test more: SH-254 gave the route table a second job,
//! answering `authority(&Route)` with the credential each route needed, and
//! most of what lived here checked that table instead — a wildcard-arm scan,
//! a route-naming check, and an equivalence against SH-250's tokenless
//! loopback read exemption. SH-255 deleted `Authority`/`authority()` (the
//! scope tier they served no longer exists: a named token now authenticates
//! everything the dashboard does, so there is nothing left to classify
//! per-route), and deleted the exemption those tests measured. What survives
//! is the one property that was never about authority at all.

use std::collections::BTreeSet;
use std::path::Path;

use storyhook::api::routes::classify;
use storyhook::daemon::http1::Method;

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
    ("/api/dispatch-log", Method::Get),
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
    // Coverage is *derived* from the enum rather than counted by hand.
    // CLAUDE.md already records what a hand-maintained inventory does here —
    // "a hand-maintained count drifted three times before it stopped being
    // trusted" (SH-136).
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
        "these routes are declared and classified but no path in this suite reaches them:\n  \
         Route: {unreached_top:?}\n  ProjectRoute: {unreached_project:?}\nAdd a probe to \
         PROBES naming each one."
    );
}
