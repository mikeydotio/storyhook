//! No command silently swallows a trailing argument (SH-357).
//!
//! `story daemon token new psamathe` printed the daemon's master bearer token
//! and exited 0. There is no `new` subcommand under `daemon token`; the arm
//! was a bare unit variant (`"token" => DaemonAction::Token`) that never
//! looked at `args[2..]`, so both trailing words were discarded without a
//! word. The output was indistinguishable from success, which manufactured
//! the specific wrong belief "I minted a scoped, revocable token" on a
//! credential-printing command, and carried a wrong hypothesis into SH-319.
//!
//! This file pins the **class**, not the four arms that were found. It is
//! derived, in the style of `tests/dead_public_surface.rs` and
//! `tests/store_isolation.rs`'s scans, rather than listing today's
//! subcommands by hand — a hand-maintained list is exactly what lets the next
//! arm be written the same way and go uncounted.
//!
//! The derivation is behavioural, and deliberately stronger than a scan for
//! the shape the bug happened to have. It takes only the **vocabulary** from
//! the source — every string literal appearing as a match-arm pattern in
//! `src/cli.rs` — and then asks the parser itself the question that matters:
//!
//! > If `parse_invocation(P)` succeeds, does `parse_invocation(P + [junk])`
//! > succeed **with the same invocation**?
//!
//! Equality, not acceptance, is the whole distinction. Plenty of commands take
//! an optional trailing value and must keep doing so: `story import` reads
//! stdin, `story import <file>` reads a file, and both are correct because the
//! extra word landed in a field. A word that lands nowhere — leaving an
//! invocation byte-identical to the one parsed without it — was discarded, and
//! that is the defect, whatever shape the arm was written in. A new arm is
//! caught the day it is added, because the property is about what the parser
//! *does*, not how it is spelled.
//!
//! `parse_invocation` is pure — it inspects its slice and builds an
//! `Invocation`, touching no file, environment variable or process — so
//! exercising tens of thousands of argument vectors here costs milliseconds
//! and cannot escape into a real store. That purity is what lets this test
//! provoke `story daemon install` and `story purge` without installing or
//! purging anything.
//!
//! **Deliberate scope.** Only positional words are enumerated; anything
//! starting with `-` is skipped. Flags have their own guard
//! (`reject_unknown_flags`, which runs before dispatch), and `--help` is
//! answerable at any depth by design, so including them would report
//! `story daemon --help extra` as a violation of a rule it does not break.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use storyhook::cli::parse_invocation;

/// The meaningless word appended to argument vectors that already parse.
/// Long and self-describing so that a failure message says what was done to
/// provoke it.
const TRAILING: &str = "storyhook-trailing-argument-probe";

/// Stands in for an arbitrary value while exploring — a story id, a title, a
/// path. Distinct from [`TRAILING`] so that a parser which joins its
/// positional arguments, or keeps only the last, is never mistaken for one
/// that discarded them.
const VALUE: &str = "storyhook-value-probe";

/// How many words deep to keep exploring. A prefix of four plus the trailing
/// word covers every command this CLI has — `story project settings set <key>
/// <value>` is the longest — with a level of headroom.
const MAX_PREFIX_WORDS: usize = 4;

/// The repository root, from this test binary's manifest directory.
fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Every string literal `src/cli.rs` matches an argument against.
///
/// Read off the left-hand side of each `=>`, which is where a match-arm
/// pattern lives and where a usage string never does. Over-collection is
/// harmless and deliberate: a word that no command means simply fails to
/// parse and is skipped, so the scan errs toward asking more questions rather
/// than fewer.
fn command_vocabulary(root: &Path) -> BTreeSet<String> {
    let source = std::fs::read_to_string(root.join("src/cli.rs"))
        .expect("reading src/cli.rs, the file this scan is derived from");

    let mut words = BTreeSet::new();
    for line in source.lines() {
        let Some(pattern) = line.split_once("=>").map(|(before, _)| before) else {
            continue;
        };
        for literal in string_literals(pattern) {
            // Flags are out of scope — see this file's header.
            if literal.starts_with('-') {
                continue;
            }
            // A pattern literal is one word. Anything else came from a
            // construct this crude reader mis-read, and is not a command.
            if literal.is_empty() || literal.split_whitespace().count() != 1 {
                continue;
            }
            words.insert(literal);
        }
    }

    assert!(
        words.len() > 50,
        "only {} command words were read out of src/cli.rs, which is too few for this \
         file to have scanned anything real — the reader has drifted from the source",
        words.len()
    );
    words
}

/// The contents of every `"..."` in `fragment`, without escape handling —
/// no command word contains a backslash, and one that did would be dropped
/// rather than mis-parsed.
fn string_literals(fragment: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut rest = fragment;
    while let Some(open) = rest.find('"') {
        let after = &rest[open + 1..];
        let Some(close) = after.find('"') else { break };
        found.push(after[..close].to_string());
        rest = &after[close + 1..];
    }
    found
}

/// The usage text `path` is refused with, or `None` if it is accepted.
fn refusal(path: &[String]) -> Option<String> {
    parse_invocation(path).err().map(|error| error.to_string())
}

/// Whether `path` parses, and keeps parsing into the **same invocation** with
/// [`TRAILING`] appended.
///
/// Equality of the parsed [`Invocation`], rather than mere acceptance, is what
/// separates the defect from the many commands that legitimately take an
/// optional trailing value. `story import` reads stdin and `story import
/// <file>` reads a file: both parse, and the second is not a bug, because the
/// word went somewhere. `story daemon token` and `story daemon token new`
/// parse into the byte-identical invocation, which is the proof that the word
/// went nowhere.
fn swallows_trailing_word(path: &[String]) -> bool {
    let Ok(alone) = parse_invocation(path) else {
        return false;
    };
    let mut with_junk = path.to_vec();
    with_junk.push(TRAILING.to_string());
    matches!(parse_invocation(&with_junk), Ok(after) if after == alone)
}

/// Whether the parser treats `word` as meaning something at this position,
/// rather than as an arbitrary value.
///
/// Compared against [`VALUE`] in the same slot: a word the parser recognizes
/// either parses when an arbitrary value does not, or is refused differently.
/// Two identical refusals mean the position takes a value, not a keyword, and
/// the [`VALUE`] branch already covers whatever is below it.
fn is_recognized(prefix: &[String], word: &str, value_outcome: &Option<String>) -> bool {
    let mut candidate = prefix.to_vec();
    candidate.push(word.to_string());
    refusal(&candidate).as_ref() != value_outcome.as_ref()
}

/// Renders one explored argument vector for a human, with a value position
/// shown as `<VALUE>` — `story show <VALUE>` reads as the command it stands
/// for, where the sentinel word itself reads as a typo.
fn render(path: &[String]) -> String {
    path.iter()
        .map(|word| {
            if word == VALUE {
                "<VALUE>"
            } else {
                word.as_str()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Walks every argument vector reachable from `prefix`, collecting the ones
/// that keep parsing with a meaningless word appended.
///
/// Two collapses keep the report one line per defect rather than one line per
/// sighting, which is what makes a failure readable:
///
/// - A position that takes a **value** rather than a keyword is explored once,
///   through [`VALUE`] itself. Every unrecognized word behaves identically
///   there by definition, so `story epic create add` and `story epic create
///   archive` are the same finding — `story epic create <VALUE>` — not two
///   hundred.
/// - A vector that swallows is **not descended into**. A command that ignores
///   its trailing arguments ignores every longer vector built on it, so the
///   subtree below would report derived sightings and bury their one cause.
fn walk(prefix: &[String], vocabulary: &BTreeSet<String>, found: &mut Vec<Vec<String>>) {
    if prefix.len() >= MAX_PREFIX_WORDS {
        return;
    }

    let mut value_path = prefix.to_vec();
    value_path.push(VALUE.to_string());
    let value_outcome = refusal(&value_path);

    let recognized = vocabulary
        .iter()
        .filter(|word| is_recognized(prefix, word, &value_outcome))
        .map(String::as_str);

    for word in recognized.chain(std::iter::once(VALUE)) {
        let mut candidate = prefix.to_vec();
        candidate.push(word.to_string());

        if swallows_trailing_word(&candidate) {
            found.push(candidate);
            continue;
        }

        walk(&candidate, vocabulary, found);
    }
}

/// The whole property: nothing this CLI accepts stays accepted when a
/// meaningless word is appended to it.
///
/// A new subcommand written as a bare unit variant fails here the day it is
/// added, without anyone remembering to extend a list.
#[test]
fn no_accepted_command_still_parses_with_a_meaningless_word_appended() {
    let vocabulary = command_vocabulary(&repository_root());
    let mut found = Vec::new();
    walk(&[], &vocabulary, &mut found);

    assert!(
        found.is_empty(),
        "{} command{} silently discard trailing arguments. Each is already complete, yet \
         accepts a word it has no meaning for and reports success — which the caller cannot \
         tell from having been understood:\n{}",
        found.len(),
        if found.len() == 1 { "" } else { "s" },
        found
            .iter()
            .map(|path| format!("  - story {} {TRAILING}", render(path)))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// The instance that was found by hand, pinned on its own so the regression
/// is legible without reading the scan (SH-357).
///
/// `story daemon token` prints the daemon's master bearer token. Anything
/// after it is a subcommand that does not exist, and the one thing it must
/// never do is print the credential anyway.
#[test]
fn daemon_token_refuses_a_subcommand_it_does_not_have() {
    for trailing in [
        vec!["new"],
        vec!["new", "psamathe"],
        vec!["list"],
        vec!["revoke", "psamathe"],
        vec!["anything-at-all"],
    ] {
        let mut argv = vec!["daemon".to_string(), "token".to_string()];
        argv.extend(trailing.iter().map(|word| (*word).to_string()));

        let refused = refusal(&argv).unwrap_or_else(|| {
            panic!(
                "`story {}` parsed. `daemon token` has no subcommands — the named-token \
                 verbs are `story token new|list|revoke` — so this printed the master \
                 token instead of a usage error",
                argv.join(" ")
            )
        });
        assert!(
            refused.contains("usage: story daemon"),
            "`story {}` was refused, but not with `parse_daemon`'s usage string: {refused}",
            argv.join(" ")
        );
    }
}
