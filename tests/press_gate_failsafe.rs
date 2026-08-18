//! Fences SH-401's press-gate failsafe against ever becoming a bare duration.
//!
//! The gate defers a paint while a pointer press is in flight, so a click can
//! never be swallowed by having its own target rebuilt out from under it. The
//! failsafe that bounds a hold is the one part of that design a future editor
//! could get subtly, invisibly wrong, and the council that settled the design
//! said why (`story show SH-401`, never the council's own directory — SH-363):
//! a wall clock is the *only* release in the set that can fire during a **live**
//! press, and firing there re-opens the exact defect the gate closes, for
//! exactly that press.
//!
//! So the failsafe is not a bound on how long a human may hold a button. It is
//! a bound on the **release set**: under a correct one it is unreachable, which
//! makes its firing a detector for a hole rather than a recovery from one. Two
//! properties follow, and this file pins both:
//!
//! 1. **It is derived, never a bare literal** — SH-394's standing rule, one axis
//!    over from the wall-clock ceilings in the Rust suite. The value must be
//!    written in terms of `SAFETY_POLL_INTERVAL_MS`: a gate may not hold a paint
//!    back longer than the interval at which the board resyncs regardless.
//! 2. **It is strictly below that interval.** A failsafe at or above the safety
//!    poll would let a hold outlive the very resync that would repair it.
//!
//! Derived-corpus style, like `dashboard_deadline_knobs.rs` and
//! `dashboard_mutation_deadline.rs` beside it: read the real file, find the two
//! named constants, and compare them — so the numbers cannot drift apart the way
//! SH-136 has already cost this project three times. It carries a positive
//! control, so a scanner that stopped matching fails instead of reporting a
//! clean tree (SH-364).

use std::path::PathBuf;

fn dashboard() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/web_dashboard.html");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{} must be readable: {error}", path.display()))
}

/// The right-hand side of `var <name> = <rhs>;`, trimmed, or `None`.
fn declaration<'a>(source: &'a str, name: &str) -> Option<&'a str> {
    let needle = format!("var {name} = ");
    let start = source.find(&needle)? + needle.len();
    let rest = &source[start..];
    let end = rest.find(";\n")?;
    Some(rest[..end].trim())
}

#[test]
fn the_scanner_finds_a_declaration_it_is_shown() {
    // Positive control: the parser above is dumb string scanning, so a change
    // that broke it would otherwise report every assertion below as vacuously
    // satisfied rather than failing.
    let planted = "  var SOME_PLANTED_CONSTANT = 1234;\n";
    assert_eq!(
        declaration(planted, "SOME_PLANTED_CONSTANT"),
        Some("1234"),
        "the declaration scanner must find a constant it is shown verbatim"
    );
    assert_eq!(
        declaration(planted, "A_CONSTANT_THAT_IS_NOT_THERE"),
        None,
        "the declaration scanner must not invent a constant that is absent"
    );
}

#[test]
fn the_press_gate_failsafe_is_derived_from_the_safety_poll_not_written_as_a_duration() {
    let source = dashboard();
    let rhs = declaration(&source, "PRESS_GATE_FAILSAFE_MS").expect(
        "PRESS_GATE_FAILSAFE_MS must be declared in src/web_dashboard.html -- SH-401's press \
         gate has no bound on a held paint without it",
    );

    assert!(
        rhs.contains("SAFETY_POLL_INTERVAL_MS"),
        "PRESS_GATE_FAILSAFE_MS is `{rhs}` -- it must be written in terms of \
         SAFETY_POLL_INTERVAL_MS rather than as a duration of its own (SH-394). The failsafe \
         bounds the RELEASE SET, and the only defensible bound is the interval at which the \
         board resyncs anyway; a bare number here is an opinion about how long a person holds \
         a mouse button, which is not a fact this file has."
    );
    assert!(
        rhs.contains("intFromQuery"),
        "PRESS_GATE_FAILSAFE_MS is `{rhs}` -- it must stay an intFromQuery knob so a browser \
         spec can shrink it and prove the failsafe fires AND reports, rather than asserting a \
         path nothing ever exercises (SH-306)."
    );
}

#[test]
fn the_press_gate_failsafe_expires_before_the_safety_poll_would_repair_the_board() {
    let source = dashboard();
    let poll = declaration(&source, "SAFETY_POLL_INTERVAL_MS")
        .expect("SAFETY_POLL_INTERVAL_MS must be declared in src/web_dashboard.html");
    let poll: f64 = poll
        .parse()
        .unwrap_or_else(|_| panic!("SAFETY_POLL_INTERVAL_MS must be a plain number, got `{poll}`"));
    let rhs = declaration(&source, "PRESS_GATE_FAILSAFE_MS")
        .expect("PRESS_GATE_FAILSAFE_MS must be declared in src/web_dashboard.html");

    // The only shape sanctioned today: `intFromQuery("pressGateFailsafeMs", SAFETY_POLL_INTERVAL_MS / N)`.
    // A different derivation is not forbidden -- it just has to be taught here,
    // which is the point: the relation is the invariant, and changing it takes
    // an argument rather than an edit nobody reads.
    let divisor: f64 = rhs
        .rsplit('/')
        .next()
        .and_then(|tail| tail.trim_end_matches(')').trim().parse().ok())
        .unwrap_or_else(|| {
            panic!(
                "PRESS_GATE_FAILSAFE_MS is `{rhs}`, which this test cannot evaluate. Teach it the \
                 new derivation rather than loosening it -- the invariant being pinned is that \
                 the failsafe expires strictly before SAFETY_POLL_INTERVAL_MS."
            )
        });

    assert!(
        divisor > 1.0,
        "PRESS_GATE_FAILSAFE_MS resolves to SAFETY_POLL_INTERVAL_MS / {divisor}, i.e. at or \
         above the {poll}ms safety poll. A hold must never outlive the resync that would \
         repair the board it is holding back."
    );
}
