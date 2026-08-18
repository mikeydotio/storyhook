//! Fences every page-side XHR deadline in `src/web_dashboard.html` against a
//! bare numeric literal (SH-347).
//!
//! Three of `fetchData()`, `fetchReposOnce()` and `api()`'s GET default used
//! to hard-code `xhr.timeout = 8000`/`10000` with nothing naming the number.
//! A browser spec that deliberately holds one of those reads across its own
//! assertions is racing that exact clock — `board-readiness.spec.ts`'s own
//! SH-291 doc comment records the hazard directly: *"The one clock left is
//! the page's own `xhr.timeout = 8000`, which settles a held request whether
//! or not a test asks it to."* An unnamed literal is a race no test, and no
//! reviewer, can see coming.
//!
//! The fix names every one and makes the three a browser spec actually holds
//! past into `intFromQuery`-backed knobs (`?boardFetchTimeoutMs=`,
//! `?catalogFetchTimeoutMs=`, `?apiGetTimeoutMs=`), the same mechanism
//! `mutationTimeoutMs`, `sseWatchdogIntervalMs` and `sseStaleAfterMs` already
//! use — SH-312's own doc comment on `api()` explains why: production
//! behaviour is unchanged (every default is preserved), and a spec that needs
//! to move the boundary now does so explicitly, in the URL, rather than
//! guessing whether it can outrun an 8-second clock nobody wrote down.
//!
//! This is the derived-corpus style `tests/dashboard_mutation_deadline.rs`,
//! `tests/e2e_browser_coverage.rs` and `tests/dead_public_surface.rs` already
//! use: read the real file, scan every `xhr.timeout =` assignment, and fail
//! if the right-hand side is bare digits rather than a name. It carries its
//! own positive control and a corpus floor, so a parser that stopped
//! matching cannot report a clean tree by accident (SH-364's own doctrine).

use std::path::PathBuf;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn dashboard() -> String {
    let path = repo_root().join("src/web_dashboard.html");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{} must be readable: {error}", path.display()))
}

/// Every `xhr.timeout = <rhs>;` assignment's right-hand side, in source
/// order. Deliberately dumb string scanning, not a JS parser -- the shape
/// this page writes is one fixed idiom (`xhr.timeout = <expr>;`), the same
/// assumption `dashboard_mutation_deadline.rs` already makes about
/// `intFromQuery("mutationTimeoutMs", <n>)`.
fn xhr_timeout_assignments(source: &str) -> Vec<String> {
    let marker = "xhr.timeout = ";
    let mut found = Vec::new();
    let mut rest = source;
    while let Some(at) = rest.find(marker) {
        let after = &rest[at + marker.len()..];
        let Some(end) = after.find(';') else {
            break;
        };
        found.push(after[..end].trim().to_string());
        rest = &after[end..];
    }
    found
}

/// A right-hand side is a "bare literal" if it is nothing but ASCII digits --
/// `8000`, not `BOARD_FETCH_TIMEOUT_MS`, not `opts.timeoutMs || FOO`, not
/// even `HANDOFF_TIMEOUT_MS` (a name, however short). Mirrors
/// `tests/timing_assertions.rs`'s own "starts with a digit and nothing else"
/// rule for `Duration::from_secs(<n>)`, applied to this file's own idiom.
fn is_bare_literal(rhs: &str) -> bool {
    !rhs.is_empty() && rhs.chars().all(|c| c.is_ascii_digit())
}

#[test]
fn no_xhr_timeout_assignment_is_a_bare_literal() {
    let source = dashboard();
    let assignments = xhr_timeout_assignments(&source);

    assert!(
        assignments.len() >= 5,
        "expected to find at least 5 `xhr.timeout = ` assignments in src/web_dashboard.html \
         (api()'s GET default, fetchData(), fetchReposOnce(), the token exchange, the handoff \
         request), found {}: either several were deleted, or xhr_timeout_assignments() no \
         longer matches this file's shape -- in which case this fence is clearing a tree it \
         never read",
        assignments.len()
    );

    let bare: Vec<&String> = assignments
        .iter()
        .filter(|rhs| is_bare_literal(rhs))
        .collect();

    assert!(
        bare.is_empty(),
        "{bare:?} assign `xhr.timeout` a bare numeric literal. A held read races this exact \
         clock (SH-347, board-readiness.spec.ts's own SH-291 doc comment) -- name it as a \
         constant, and if a browser spec needs to hold past it, route the default through \
         `intFromQuery(\"<name>\", <n>)` the way `mutationTimeoutMs`/`boardFetchTimeoutMs`/\
         `catalogFetchTimeoutMs`/`apiGetTimeoutMs` already do."
    );
}

/// A positive control: the scan must be capable of failing, and must not
/// flag the named forms already in the file.
#[test]
fn the_scan_can_still_see_a_bare_deadline() {
    // Assembled at run time, in the same style
    // `dashboard_error_reporting.rs::the_scan_can_still_see_an_offending_line`
    // uses, so the literal never sits in this file as something the real
    // scan over the real tree could trip on.
    let offender = format!("    xhr.{} = {};", "timeout", "8000");
    let assignments = xhr_timeout_assignments(&offender);
    assert_eq!(assignments, vec!["8000".to_string()]);
    assert!(
        is_bare_literal(&assignments[0]),
        "a bare literal must still match"
    );

    for (label, named) in [
        ("a plain constant", "BOARD_FETCH_TIMEOUT_MS"),
        (
            "a ternary over two constants",
            "opts.timeoutMs || (method === \"GET\" ? API_GET_TIMEOUT_MS : MUTATION_TIMEOUT_MS)",
        ),
    ] {
        let clean = format!("    xhr.timeout = {named};");
        let assignments = xhr_timeout_assignments(&clean);
        assert_eq!(
            assignments,
            vec![named.to_string()],
            "{label} must parse cleanly"
        );
        assert!(
            !is_bare_literal(&assignments[0]),
            "{label} must not be flagged as bare"
        );
    }
}
