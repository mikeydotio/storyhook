//! Fences the shape of the tap-target sweep's comparison and its settle wait
//! (SH-420).
//!
//! **This is a wiring fence, and calling it anything more would be the SH-360
//! mistake.** It proves that the comparison still routes through a bound
//! derived from the rect's own coordinates rather than a bare `<` or a hand-
//! picked slack constant, and that the settle wait is still scoped to the
//! swept subtree rather than the whole document. It cannot prove the bound is
//! the right size, has the right sign, or catches anything: only a browser
//! can, and `responsive.mobile.spec.ts` carries three executed tests that do
//! -- the 64-offset sub-ulp sweep, the modal's planted 43px positive control,
//! and the entering-card test whose targets read 42.71px mid-flight.
//!
//! It exists because the executed half is expensive (the release tier, ~24
//! minutes) while the failure it guards against is cheap and silent: someone
//! reading `(Math.abs(a) + Math.abs(b)) * Math.pow(2, -24)` as noise and
//! replacing it with `- 0.05`, which is a bare literal ~800x wider than the
//! error it must absorb and would start hiding real shortfalls in
//! [43.95, 44). That substitution passes every browser test in the suite.

use std::path::PathBuf;

const SPEC: &str = "e2e/specs/responsive.mobile.spec.ts";

fn read_spec() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(SPEC);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{} must be readable: {error}", path.display()))
}

/// The body of the first function in `source` whose declaration contains
/// `signature`, brace-matched from its opening `{`. Returns `None` when the
/// signature is absent, so a rename fails the test that asked rather than
/// silently scanning an empty string.
fn function_body(source: &str, signature: &str) -> Option<String> {
    let start = source.find(signature)?;
    let open = start + source[start..].find('{')?;
    let mut depth = 0usize;
    for (offset, ch) in source[open..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(source[open..=open + offset].to_string());
                }
            }
            _ => {}
        }
    }
    None
}

fn comparison_body() -> String {
    let spec = read_spec();
    function_body(&spec, "async function findSmallTargets(")
        .unwrap_or_else(|| panic!("{SPEC} must still declare `findSmallTargets`"))
}

fn settle_body() -> String {
    let spec = read_spec();
    function_body(&spec, "async function settleAndReadTapMin(")
        .unwrap_or_else(|| panic!("{SPEC} must still declare `settleAndReadTapMin`"))
}

/// The positive control for this file's own parser. Every assertion below is
/// of the form "the extracted body contains X"; a `function_body` that had
/// quietly started returning an empty string, or a signature that had been
/// renamed, would make all of them vacuous. This proves the extractor picks
/// out a real body and stops at its real end.
#[test]
fn the_body_extractor_finds_a_real_function_and_ends_at_its_brace() {
    let fixture =
        "prefix\nasync function target(a) {\n  if (x) { y(); }\n  return 1;\n}\nsuffix {}";
    let body = function_body(fixture, "async function target(").expect("the fixture declares it");
    assert!(body.starts_with('{') && body.ends_with('}'), "{body}");
    assert!(body.contains("return 1;"), "{body}");
    assert!(
        !body.contains("suffix"),
        "the extractor ran past the closing brace: {body}"
    );

    assert!(
        function_body(fixture, "async function absent(").is_none(),
        "an absent signature must be None, never an empty body that reads as compliant"
    );

    assert!(
        comparison_body().len() > 200,
        "the real comparison body is implausibly short -- the extractor has drifted"
    );
    assert!(
        settle_body().len() > 200,
        "the real settle body is implausibly short -- the extractor has drifted"
    );
}

/// Both axes are compared against a bound derived from the two coordinates
/// the axis was measured between -- never a bare `<` against the threshold.
#[test]
fn each_axis_is_compared_against_a_bound_derived_from_its_own_coordinates() {
    let body = comparison_body();
    for call in [
        "representationError(r.left, r.right)",
        "representationError(r.top, r.bottom)",
    ] {
        assert!(
            body.contains(call),
            "`findSmallTargets` must derive each axis's tolerance from that axis's \
             own rect coordinates (SH-420); `{call}` is gone.\n\n{body}"
        );
    }
    for bare in ["r.width < minPx", "r.height < minPx"] {
        assert!(
            !body.contains(bare),
            "`{bare}` is the bare comparison SH-420 removed: WebKit reports rect \
             coordinates as float32, so a control sized exactly at the minimum \
             reads under it at 16 of every 64 sub-pixel positions.\n\n{body}"
        );
    }
}

/// The tolerance is arithmetic on float32's own significand width, not a
/// number somebody liked the look of.
#[test]
fn the_tolerance_is_derived_from_float32s_significand_never_a_bare_literal() {
    let body = comparison_body();
    assert!(
        body.contains("const FLOAT32_SIGNIFICAND_BITS = 24;"),
        "the bound must name what it derives from.\n\n{body}"
    );
    assert!(
        body.contains("Math.pow(2, -FLOAT32_SIGNIFICAND_BITS)"),
        "the bound must be computed from FLOAT32_SIGNIFICAND_BITS.\n\n{body}"
    );

    // The banned shape: any slack subtracted from, or added to, the threshold
    // as a literal. `44` itself never appears here -- the threshold arrives as
    // the `minPx` parameter -- so any decimal literal in the comparison is
    // slack by construction, except the significand width named above.
    let comparison_only = body.replace("const FLOAT32_SIGNIFICAND_BITS = 24;", "");
    for banned in ["minPx -", "minPx +", "minPx *"] {
        for line in comparison_only.lines() {
            let Some(rest) = line.split_once(banned).map(|(_, rest)| rest.trim_start()) else {
                continue;
            };
            assert!(
                !rest.starts_with(|c: char| c.is_ascii_digit()),
                "a numeric literal applied to the threshold is slack chosen by hand, \
                 not derived (SH-420 / the SH-394 bare-literal rule): `{}`",
                line.trim()
            );
        }
    }
}

/// A document-wide settle wait would block a sweep of one surface on a toast
/// animating somewhere else -- trading SH-420's false red for a false hang.
#[test]
fn the_settle_wait_is_scoped_to_the_swept_subtree_not_the_document() {
    let body = settle_body();
    assert!(
        body.contains("getAnimations({ subtree: true })"),
        "the settle wait must ask the swept root for its own subtree's \
         animations (SH-420).\n\n{body}"
    );
    assert!(
        !body.contains("document.getAnimations("),
        "the settle wait must not be document-wide: an unrelated toast or card \
         flash elsewhere on the page would hold up a sweep it has nothing to do \
         with.\n\n{body}"
    );
}

/// The sweep reads `--tap-min` from a real coarse-pointer engine, and refuses
/// anything but the coarse value -- so weakening the token fails this suite
/// instead of quietly lowering the bar it sweeps against.
#[test]
fn the_threshold_is_read_from_the_page_and_pinned_to_the_coarse_value() {
    let spec = read_spec();
    assert!(
        spec.contains("const COARSE_TAP_MIN = 44;"),
        "{SPEC} must still pin the coarse-pointer minimum it holds targets to"
    );
    let body = settle_body();
    assert!(
        body.contains("getPropertyValue(\"--tap-min\")"),
        "the sweep must read what --tap-min actually computes to, which the \
         stylesheet grep in web_test.rs cannot do.\n\n{body}"
    );
    assert!(
        body.contains("toBe(COARSE_TAP_MIN)"),
        "the computed token must be pinned to the coarse value, or lowering \
         --tap-min would silently lower this sweep's bar instead of failing \
         it.\n\n{body}"
    );
}
