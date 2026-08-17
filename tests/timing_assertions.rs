//! No integration test compares a measured duration to a bare `Duration::
//! from_*` literal (SH-394).
//!
//! # Why this file exists
//!
//! This machine runs three-to-four concurrent worktree suites at once, and
//! `docs/rearch/baseline/golden-export.json`'s own SH-110 verdict already
//! measured what that does to a hand-picked ceiling: identical byte-for-byte
//! trees failing 0, 27, 57, 2, 44, 0, 0, 60, 0, 0 times in ten consecutive
//! runs. A ceiling written as `elapsed < Duration::from_secs(2)` states two
//! things at once — the production deadline it is meant to disprove was not
//! spent, and an opinion about how fast that disproof should look on this
//! machine, today — and the second claim is the one that flakes. SH-394 swept
//! every site this shape had accumulated in `tests/` (and, incidentally, three
//! more in `crates/storyhook-test-support/src/`, fixed for the same reason
//! though this fence does not reach that crate) onto a named constant: a
//! production deadline (`storyhook::daemon::lifecycle::SPAWN_DEADLINE` and
//! its siblings, most of them newly `pub` for exactly this), or a local
//! constant whose doc says what margin it buys and why.
//!
//! # What this scan can and cannot judge
//!
//! It cannot tell whether a given margin is *wide enough* — that took reading
//! each site's own production deadline and reasoning about what a process
//! spawn or a genuinely wedged mechanism costs, which is what SH-394 actually
//! did. What it can enforce mechanically, forever, is the *shape*: an
//! `assert!` comparing anything to a Duration literal that is not routed
//! through a name. A bare literal hides the question a reviewer would
//! otherwise ask — "why this number, and why is it safe under load?" — behind
//! nothing. Naming it does not answer that question by itself, but it is the
//! one habit that keeps the question askable, and it is what every other
//! derived-scan fence in this project already does for its own shape
//! (`tests/dead_public_surface.rs`, `tests/release_targets.rs`,
//! `tests/event_kind_vocabulary.rs`) rather than hand-listing offenders.
//!
//! # What is deliberately allowed
//!
//! A **definition** — `const CEILING: Duration = Duration::from_secs(5);` —
//! is exactly the fix, not a violation: the comparison site names `CEILING`,
//! and the literal lives once, in the one place whose doc can explain it.
//! Only a comparison whose ENTIRE compared quantity is the bare constructor —
//! immediately preceded by a comparison operator, or immediately followed by
//! one — is flagged. `baseline * 4 + Duration::from_millis(500)`
//! (`tests/daemon_concurrency.rs`) is not: the literal there is a small
//! additive margin on an already self-calibrated quantity, not the ceiling
//! itself, and [`the_analyzer_does_not_flag_a_margin_added_to_a_named_or_
//! calibrated_quantity`] pins that distinction directly against the real
//! line so a future edit to the scanner cannot re-widen it by accident.
//!
//! # Derived, not hand-listed, and its own positive control
//!
//! Every tracked `tests/*.rs` file is read via `git ls-files`, the same
//! style `tests/dead_public_surface.rs` and `tests/store_isolation.rs`
//! established — a hand-maintained list of "known-safe" sites is exactly the
//! shape that let ten stray items accumulate unnoticed in SH-198.
//!
//! This file needs no exemption for itself, on purpose: its own fixtures in
//! [`the_scanner_recognizes_a_bare_ceiling_in_either_direction`] are
//! assembled from [`DURATION_CTORS`] at run time via `format!`, so the
//! flagged text never appears as a contiguous literal in this file's own
//! source — the same discipline `tests/council_citations.rs` uses for its
//! marker, for the same reason: a test that had to exempt itself would be one
//! edit away from exempting everything.

use std::collections::BTreeMap;
use std::path::Path;

/// The `Duration` constructors this scan recognizes.
const DURATION_CTORS: [&str; 4] = [
    "Duration::from_secs(",
    "Duration::from_millis(",
    "Duration::from_nanos(",
    "Duration::from_micros(",
];

/// Order does not matter for `ends_with`/`starts_with` (no operator here is a
/// suffix of another), but longest-first reads clearest.
const COMPARISON_OPS: [&str; 4] = ["<=", ">=", "<", ">"];

fn ends_with_comparison(text: &str) -> bool {
    let trimmed = text.trim_end();
    COMPARISON_OPS.iter().any(|op| trimmed.ends_with(op))
}

fn starts_with_comparison(text: &str) -> bool {
    let trimmed = text.trim_start();
    COMPARISON_OPS.iter().any(|op| trimmed.starts_with(op))
}

/// One bare-literal ceiling: where it was found, and the line it came from.
#[derive(Debug, PartialEq, Eq)]
struct BareCeiling {
    line: usize,
    text: String,
}

/// Every bare-literal `Duration` ceiling in one file's text.
///
/// A "bare literal" is a `Duration::from_*(` call whose argument starts with
/// an ASCII digit and nothing else — `Duration::from_secs(HOOK_TIMEOUT_SECS)`
/// and `Duration::from_secs(SPAWN_LOCK_DEADLINE.as_secs() + 10)` both start
/// with a letter and are never matched, which is the whole point: naming or
/// deriving the argument is the fix, not a shape this scan still recognizes.
///
/// A bare literal is only a **ceiling** — the thing this scan cares about —
/// when it is the entire compared quantity: immediately preceded by a
/// comparison operator (`elapsed < Duration::from_secs(2)`) or immediately
/// followed by one in the reversed form (`Duration::from_secs(2) < elapsed`).
/// A literal folded into a larger expression on either side of the operator
/// (`baseline * 4 + Duration::from_millis(500)`) is a margin, not a ceiling,
/// and is not flagged.
fn bare_duration_ceilings(source: &str) -> Vec<BareCeiling> {
    let mut found = Vec::new();

    for (index, line) in source.lines().enumerate() {
        for ctor in DURATION_CTORS {
            let mut search_from = 0;
            while let Some(offset) = line[search_from..].find(ctor) {
                let ctor_start = search_from + offset;
                let args_start = ctor_start + ctor.len();
                search_from = args_start;

                let rest = &line[args_start..];
                let digits_len = rest.chars().take_while(char::is_ascii_digit).count();
                if digits_len == 0 {
                    continue; // a name or an expression, not a bare literal
                }

                // Only whitespace may sit between the digits and the closing
                // paren — anything else means the digits are part of a
                // larger argument expression this scan does not need to see,
                // because naming that expression's own root is the fix.
                let after_digits = rest[digits_len..].trim_start();
                let Some(close_rest) = after_digits.strip_prefix(')') else {
                    continue;
                };

                // Strip a fully-qualified path prefix before checking
                // adjacency, so `elapsed < std::time::Duration::from_secs(1)`
                // is caught the same as the unqualified form every real site
                // in this corpus actually uses.
                let before = line[..ctor_start]
                    .strip_suffix("std::time::")
                    .unwrap_or(&line[..ctor_start]);
                if ends_with_comparison(before) || starts_with_comparison(close_rest) {
                    found.push(BareCeiling {
                        line: index + 1,
                        text: line.trim().to_string(),
                    });
                }
            }
        }
    }

    found
}

/// Every tracked `tests/*.rs` file's text, keyed by its path relative to the
/// repository root.
fn tracked_test_files(root: &Path) -> BTreeMap<String, String> {
    let listed = std::process::Command::new("git")
        .current_dir(root)
        .args(["ls-files", "-z", "--", "tests/*.rs"])
        .output()
        .expect("listing this repository's tracked test files");
    assert!(
        listed.status.success(),
        "`git ls-files` failed, so this scan proved nothing: {}",
        String::from_utf8_lossy(&listed.stderr)
    );

    listed
        .stdout
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
        .filter_map(|path| {
            let relative = std::str::from_utf8(path).ok()?.to_string();
            let text = std::fs::read_to_string(root.join(&relative)).ok()?;
            Some((relative, text))
        })
        .collect()
}

#[test]
fn no_test_file_compares_a_duration_to_a_bare_literal_ceiling() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let corpus = tracked_test_files(root);

    // Positive control on the corpus itself, in the style of
    // `tests/council_citations.rs`: every assertion below is about what the
    // scan did *not* find, and a reader that silently stopped reading files
    // would satisfy all of them for the wrong reason.
    assert!(
        corpus.len() > 100,
        "only {} tracked test files were read; this scan proved nothing",
        corpus.len()
    );
    assert!(
        corpus.contains_key("tests/push_gate.rs"),
        "tests/push_gate.rs was not in the corpus; this scan proved nothing"
    );

    let findings: Vec<String> = corpus
        .iter()
        .flat_map(|(path, text)| {
            bare_duration_ceilings(text)
                .into_iter()
                .map(move |c| format!("{path}:{}: {}", c.line, c.text))
        })
        .collect();

    assert!(
        findings.is_empty(),
        "a bare Duration literal is used directly as a comparison ceiling — this hides \
         the question \"why this number, and why is it safe under load?\" behind nothing. \
         Route it through a named constant instead: a production deadline \
         (storyhook::daemon::lifecycle::* and siblings), or a local one whose doc says \
         what margin it buys (SH-394):\n{}",
        findings.join("\n")
    );
}

#[test]
fn the_scanner_recognizes_a_bare_ceiling_in_either_direction() {
    // The control the scan above cannot be: fixtures assembled at run time
    // from DURATION_CTORS, held here so they cannot rot on disk and so the
    // flagged text never appears as a contiguous literal in this file's own
    // source. A scanner that stopped recognizing a shape this corpus has
    // actually used would still report a clean tree; it fails here instead.
    let ctor = DURATION_CTORS[0]; // "Duration::from_secs("

    let forward = format!("        elapsed < {ctor}2),\n");
    assert_eq!(
        bare_duration_ceilings(&forward),
        vec![BareCeiling {
            line: 1,
            text: format!("elapsed < {ctor}2),")
        }],
        "the ordinary shape — a measured elapsed() compared to a bare literal — must be caught"
    );

    let reversed = format!("        {ctor}2) < elapsed,\n");
    assert_eq!(
        bare_duration_ceilings(&reversed),
        vec![BareCeiling {
            line: 1,
            text: format!("{ctor}2) < elapsed,")
        }],
        "the reversed shape must be caught too, or it is a one-line bypass of the rule above"
    );

    let le_and_ge = format!("assert!(a <= {ctor}5) && b >= {ctor}9));\n");
    assert_eq!(
        bare_duration_ceilings(&le_and_ge).len(),
        2,
        "both a <= and >= comparisons must be recognized, and both occurrences on one line"
    );

    // Fully qualified rather than the bare form every real site actually
    // uses (`use std::time::Duration` at the top of the file) — a scanner
    // that only matched the unqualified path would have a one-import bypass.
    let qualified = format!("elapsed < std::time::{ctor}3),\n");
    assert_eq!(
        bare_duration_ceilings(&qualified),
        vec![BareCeiling {
            line: 1,
            text: format!("elapsed < std::time::{ctor}3),")
        }],
        "a fully-qualified std::time::Duration::from_secs(N) ceiling must be caught too"
    );
}

#[test]
fn the_scanner_does_not_flag_a_named_constant_or_a_derived_expression() {
    let ctor = DURATION_CTORS[0];

    let named = format!("const CEILING: Duration = {ctor}5);\n        elapsed < CEILING,\n");
    assert_eq!(
        bare_duration_ceilings(&named),
        vec![],
        "a comparison against a named constant must not be flagged — naming the literal \
         at its one definition site is the fix this scan exists to require"
    );

    let derived = format!("elapsed < {ctor}HOOK_TIMEOUT_SECS / 2),\n");
    assert_eq!(
        bare_duration_ceilings(&derived),
        vec![],
        "an argument that starts with an identifier, not a digit, is a derived expression \
         and must not be flagged even though a literal appears later in it"
    );
}

/// `tests/daemon_concurrency.rs`'s own line, verbatim, pinned directly rather
/// than reconstructed — so a future change to either the scanner or that
/// line is caught by name, not just by shape.
#[test]
fn the_analyzer_does_not_flag_a_margin_added_to_a_named_or_calibrated_quantity() {
    let margin = "    concurrent < baseline * 4 + Duration::from_millis(500),\n";
    assert_eq!(
        bare_duration_ceilings(margin),
        vec![],
        "a literal folded into a larger expression by +, *, or similar is a margin on an \
         already-calibrated quantity, not the ceiling itself, and must not be flagged"
    );
}
