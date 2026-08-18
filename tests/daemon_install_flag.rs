//! `--this-binary` belongs to `story daemon install` and to nothing else.
//!
//! The flag is the way through [`storyhook::daemon::install_guard`]'s two
//! refusals (SH-411). Scoping it is what stops it becoming a word every
//! `daemon` subcommand silently ignores — the SH-357 shape, one layer over:
//! there, a *positional* argument landed nowhere and the command answered
//! anyway.
//!
//! **Why this file exists rather than the derived fence.**
//! `tests/trailing_arguments.rs` asks the parser whether appending a
//! meaningless word leaves the [`Invocation`] unchanged, over a vocabulary it
//! derives from `src/cli.rs` — but it drops every `-`-prefixed word, and its
//! own header names this exact residual as its blind spot: *a flag a verb
//! declares but a subcommand ignores*. So this is proof rather than fix. The
//! fix is structural: `VERB_FLAGS` carries a `(daemon, Some("install"))` entry,
//! and `declared_flags` prefers it over the verb's own row, so every sibling
//! refuses `--this-binary` by construction. What is asserted here is that the
//! entry stayed *scoped* — folding it into the `subcommand: None` row would
//! open the flag to all six subcommands with nothing else failing.
//!
//! The sibling list is derived from the parser's own usage string rather than
//! hand-kept, so a seventh `daemon` subcommand is covered the day it is added.
//! A hand-maintained list is what SH-136, SH-198, SH-258, SH-260/276 and
//! SH-364 each cost this project once already.
//!
//! Nothing here installs anything: `parse_invocation` is pure, which is the
//! same property that lets `tests/trailing_arguments.rs` provoke
//! `story daemon install` without registering a launchd agent.

use storyhook::cli::{DaemonAction, Invocation, parse_invocation};

/// Parses `story <args…>`, the way the binary's own `main` does.
fn parse(args: &[&str]) -> Result<Invocation, storyhook::error::AppError> {
    let argv: Vec<String> = std::iter::once("story".to_string())
        .chain(args.iter().map(|word| (*word).to_string()))
        .collect();
    parse_invocation(&argv[1..])
}

/// Every `daemon` subcommand the usage string names, minus `install` itself.
///
/// Derived from the parser's own text rather than written out here: the usage
/// line is what a seventh subcommand has to touch, so deriving it means this
/// test covers that subcommand without an edit.
fn sibling_subcommands() -> Vec<String> {
    let usage = parse(&["daemon"]).expect_err("a bare `daemon` prints its usage");
    let usage = usage.to_string();
    let (_, listed) = usage.split_once("story daemon ").expect("a usage line");
    let siblings: Vec<String> = listed
        .split('|')
        .filter_map(|part| part.split_whitespace().next())
        .filter(|word| *word != "install")
        .map(str::to_string)
        .collect();
    assert!(
        siblings.len() >= 4,
        "the usage line should name several subcommands, found {siblings:?} in {usage}"
    );
    siblings
}

#[test]
fn install_accepts_the_flag() {
    assert_eq!(
        parse(&["daemon", "install", "--this-binary"]).expect("install accepts its own flag"),
        Invocation::Daemon {
            action: DaemonAction::Install { this_binary: true }
        }
    );
}

/// The control: without the flag the invocation is a different value, so the
/// word is landing in a field rather than being dropped (SH-357's own test).
#[test]
fn install_without_the_flag_is_a_different_invocation() {
    assert_eq!(
        parse(&["daemon", "install"]).expect("install parses bare"),
        Invocation::Daemon {
            action: DaemonAction::Install { this_binary: false }
        }
    );
    assert_ne!(
        parse(&["daemon", "install"]).unwrap(),
        parse(&["daemon", "install", "--this-binary"]).unwrap(),
    );
}

/// The scoping proof. A `(daemon, None)` declaration would let every one of
/// these through the flag gate and into a parser that ignores the word.
#[test]
fn no_sibling_subcommand_accepts_the_flag() {
    for sibling in sibling_subcommands() {
        let refused = parse(&["daemon", &sibling, "--this-binary"]);
        assert!(
            refused.is_err(),
            "`story daemon {sibling} --this-binary` must be refused: the flag belongs to \
             `install` alone, and a word that lands nowhere is answered anyway (SH-357)"
        );
    }
}

/// `install` still refuses a word it has no field for — adding a flag does not
/// repeal SH-357's rule for the arm it was added to.
#[test]
fn install_still_refuses_a_word_that_lands_nowhere() {
    assert!(parse(&["daemon", "install", "psamathe"]).is_err());
    assert!(parse(&["daemon", "install", "--this-binary", "psamathe"]).is_err());
    assert!(parse(&["daemon", "install", "--zzz-not-a-flag"]).is_err());
}
