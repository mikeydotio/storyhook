//! Every `story ...` invocation README.md shows must be one the CLI actually
//! accepts, and every command the CLI actually dispatches must be documented
//! somewhere in the command reference (SH-167).
//!
//! # The defect this guards against
//!
//! README.md's `## Command reference` and `## Quick start` documented an
//! id-first grammar (`story SH-1 assign mikey`, `story SH-1 is done`, `story
//! <a> <relationship> <b>`) that `dispatch` (`src/cli.rs`) has never had — every
//! one of those lines exited 2 with `unknown command`. The Relationships
//! section had the same defect in a second shape: it named twelve relationship
//! inputs `domain::relation_edges` has never accepted, and omitted four it
//! does. README is the first thing a new user or agent reads; a page that
//! fails at exit 2 the moment you try its examples reads as a broken install,
//! not stale prose.
//!
//! # What this test does and does not check
//!
//! This validates that every documented invocation's tokens are ones
//! `cli::split_global_flags` and `cli::parse_invocation` accept — the same
//! two functions `main.rs` calls, in the same order, on every real
//! invocation. It does **not** check flag *combinations* (two optionals
//! together, three-way interactions) — that is `tests/cli_grammar.rs` and
//! `tests/unknown_flag_sweep.rs`'s job — and it does not execute anything:
//! no fixture, no store, no daemon, hermetic by construction, in the shape of
//! `tests/help_topic_references.rs`.
//!
//! # The format contract the Command reference block follows
//!
//! So the extractor here is not guessing at Markdown in general, the
//! reference block (and every other fenced `story ...` example in this file)
//! follows a fixed contract:
//!
//! 1. One invocation, one line — no continuations. A line either is one
//!    invocation or it isn't a candidate at all.
//! 2. No top-level ` | `. `story project settings list | get <key> | ...`
//!    is written as four separate lines. A top-level ` | ` found anyway is a
//!    contract violation and fails loudly, naming the line, rather than being
//!    guessed at.
//! 3. `|` glued directly onto a word with no surrounding spaces
//!    (`OPEN|CLOSED`, `agents-md|claude-md|cursor-rules`) is a value
//!    alternation and is expanded — one argv per alternative.
//! 4. `|` inside a bracketed or parenthesized group with spaces
//!    (`[--attach <PATH> | --no-attach]`, `(--all | <request-id>)`) is a
//!    group alternation and is expanded the same way.
//! 5. `<placeholder>` tokens are substituted from the table below. An
//!    unrecognized placeholder fails the test naming it — there is
//!    deliberately no default fallback, because a default could make this
//!    test pass while documenting a value the CLI actually rejects
//!    (`--count <n>` needs an integer, `--port <PORT>` a `u16`, `--json
//!    <json>` a token starting with `{` or `split_global_flags` treats it as
//!    the global output flag instead).
//!
//! # Expansion: one optional at a time, never combined
//!
//! A naive powerset over every `[...]` group is both wrong and explosive.
//! Wrong, because this grammar has real mutual exclusions — `story update
//! [--check] [--force]` (`src/cli.rs`, `--check` and `--force` are declared
//! mutually exclusive), `state set [--description ...] [--no-description]`,
//! `project new [--attach <PATH> | --no-attach]` — so "include every
//! optional" produces an argv the parser is *right* to refuse, which this
//! test would then misreport as a documentation bug. Explosive, because
//! `story list` alone has ten optionals: a full powerset is 1024 argvs for
//! one line.
//!
//! So instead: for every combination of *required* alternatives (there are
//! never more than a handful), emit the bare form with every optional
//! omitted, then — once per optional group — emit that same bare form with
//! exactly *one* alternative of exactly *one* group added. Two optionals are
//! never present in the same argv, so a mutual exclusion is unreachable by
//! construction. This validates every documented token in some legal
//! context; it does not validate that every combination of them is legal,
//! which is a different, narrower promise than "the reference is real."

use std::path::{Path, PathBuf};

use storyhook::cli::{parse_invocation, split_global_flags};
use storyhook::domain::is_relation_input;

fn manifest_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

fn readme_text() -> String {
    let path = manifest_root().join("README.md");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

fn cli_source() -> String {
    let path = manifest_root().join("src/cli.rs");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

fn domain_source() -> String {
    let path = manifest_root().join("src/domain.rs");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

// ---------------------------------------------------------------------------
// Extracting candidate `story ...` lines from README's fenced blocks
// ---------------------------------------------------------------------------

/// A documented invocation, with the line it came from for attributable
/// failures.
struct Entry {
    line: usize,
    raw: String,
}

/// Every fenced-block line that names a `story` invocation.
///
/// A candidate is a line, inside a ``` fence, whose first word (after
/// stripping an optional `$ ` console prompt) is `story`. Everything else
/// inside a fence — prose, other commands, TOML keys, JSON, a console
/// block's `error: ...` output — does not start with that word and is
/// silently not a candidate, which is what lets `## Storage model`'s TOML
/// block and the Story ids section's `error:` output share a file with this
/// scanner without either confusing it.
fn extract_entries(markdown: &str) -> Vec<Entry> {
    let mut entries = Vec::new();
    let mut in_fence = false;
    for (idx, line) in markdown.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if !in_fence {
            continue;
        }
        let candidate = trimmed.strip_prefix("$ ").unwrap_or(trimmed);
        if candidate != "story" && !candidate.starts_with("story ") {
            continue;
        }
        entries.push(Entry {
            line: idx + 1,
            raw: strip_trailing_comment(candidate),
        });
    }
    entries
}

/// Strips a trailing ` # comment`, outside quotes.
///
/// Only a `#` immediately preceded by a space is a comment marker, so a value
/// that legitimately contains `#` (none do today, but the rule should not
/// depend on that) is not mistaken for one just by following whitespace
/// elsewhere on the line.
fn strip_trailing_comment(line: &str) -> String {
    let chars: Vec<char> = line.chars().collect();
    let mut in_quotes = false;
    for i in 0..chars.len() {
        match chars[i] {
            '"' => in_quotes = !in_quotes,
            '#' if !in_quotes && i > 0 && chars[i - 1] == ' ' => {
                return chars[..i].iter().collect::<String>().trim_end().to_string();
            }
            _ => {}
        }
    }
    line.to_string()
}

// ---------------------------------------------------------------------------
// Tokenizing one invocation into required/optional pieces
// ---------------------------------------------------------------------------

/// One sub-token inside a phrase — a bare word or a `"..."` quoted value.
/// The distinction survives tokenization so pipe-expansion can treat them
/// differently: a bare word's `|` is alternation (`local|remote`), but a
/// quoted value is one literal however many `|` characters it might contain
/// (none do today, but a quoted string is data, never grammar).
#[derive(Debug, Clone)]
enum SubTok {
    Word(String),
    Quoted(String),
}

#[derive(Debug, Clone)]
enum Piece {
    /// A single top-level word or quoted token.
    Sub(SubTok),
    /// A `[...]` optional group. Each inner `Vec<SubTok>` is one alternative
    /// phrase's raw (unsubstituted) tokens; more than one alternative means
    /// the group's contents were split on a top-level ` | `.
    Optional(Vec<Vec<SubTok>>),
    /// A `(...)` required alternation group, same inner shape as `Optional`.
    Choice(Vec<Vec<SubTok>>),
}

/// Splits `inner` (the text between a group's delimiters) on a top-level
/// ` | ` into its alternative phrases, sub-tokenizing each with
/// [`simple_tokenize`]. A group with no ` | ` is one alternative.
fn split_alternatives(inner: &str) -> Vec<Vec<SubTok>> {
    inner.split(" | ").map(simple_tokenize).collect()
}

/// Whitespace-splits `s`, treating a `"..."` span as one [`SubTok::Quoted`]
/// (quotes stripped). Used for the contents of a `[...]`/`(...)` group,
/// which never nest further brackets in this grammar.
fn simple_tokenize(s: &str) -> Vec<SubTok> {
    let chars: Vec<char> = s.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i].is_whitespace() {
            i += 1;
            continue;
        }
        if chars[i] == '"' {
            let start = i + 1;
            let mut j = start;
            while j < chars.len() && chars[j] != '"' {
                j += 1;
            }
            tokens.push(SubTok::Quoted(chars[start..j].iter().collect()));
            i = j + 1;
        } else {
            let start = i;
            while i < chars.len() && !chars[i].is_whitespace() {
                i += 1;
            }
            tokens.push(SubTok::Word(chars[start..i].iter().collect()));
        }
    }
    tokens
}

/// Tokenizes a whole invocation (minus the leading `story`) into top-level
/// [`Piece`]s: words, quoted tokens, and bracketed/parenthesized groups.
fn tokenize_top(s: &str) -> Vec<Piece> {
    let chars: Vec<char> = s.chars().collect();
    let mut pieces = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i].is_whitespace() {
            i += 1;
            continue;
        }
        match chars[i] {
            '"' => {
                let start = i + 1;
                let mut j = start;
                while j < chars.len() && chars[j] != '"' {
                    j += 1;
                }
                pieces.push(Piece::Sub(SubTok::Quoted(chars[start..j].iter().collect())));
                i = j + 1;
            }
            '[' => {
                let start = i + 1;
                let mut j = start;
                let mut depth = 1;
                while j < chars.len() && depth > 0 {
                    match chars[j] {
                        '[' => depth += 1,
                        ']' => depth -= 1,
                        _ => {}
                    }
                    if depth > 0 {
                        j += 1;
                    }
                }
                let inner: String = chars[start..j].iter().collect();
                pieces.push(Piece::Optional(split_alternatives(&inner)));
                i = j + 1;
            }
            '(' => {
                let start = i + 1;
                let mut j = start;
                let mut depth = 1;
                while j < chars.len() && depth > 0 {
                    match chars[j] {
                        '(' => depth += 1,
                        ')' => depth -= 1,
                        _ => {}
                    }
                    if depth > 0 {
                        j += 1;
                    }
                }
                let inner: String = chars[start..j].iter().collect();
                pieces.push(Piece::Choice(split_alternatives(&inner)));
                i = j + 1;
            }
            _ => {
                let start = i;
                while i < chars.len()
                    && !chars[i].is_whitespace()
                    && chars[i] != '['
                    && chars[i] != '('
                    && chars[i] != '"'
                {
                    i += 1;
                }
                pieces.push(Piece::Sub(SubTok::Word(chars[start..i].iter().collect())));
            }
        }
    }
    pieces
}

// ---------------------------------------------------------------------------
// Placeholder substitution
// ---------------------------------------------------------------------------

/// The value a `<placeholder>` token substitutes to. Exact match only — a
/// token that looks like a placeholder (contains `<`) and is not listed here
/// fails the test naming it, rather than silently passing through or
/// defaulting to something that might not be legal for its flag.
fn placeholder(token: &str) -> Option<&'static str> {
    Some(match token {
        "<id>" | "<a>" | "<b>" | "<story-id>" | "<epic-id>" | "<expected>" => "SH-1",
        "<n>" | "<N>" => "1",
        "<PORT>" => "3456",
        "<PREFIX>" => "SH",
        "<NEW-PREFIX>" => "ZZ",
        "<json>" => "{\"title\":\"x\"}",
        "<relationship-type>" => "relates-to",
        "<name <email>>" => "Ada Lovelace <ada@example.com>",
        "<github-handle>" => "adalovelace",
        "<slug>" => "todo",
        "<slug,slug,...>" => "todo,in-progress,done",
        "<topic>" => "storage",
        "<duration>" => "3d",
        "<date>" => "2026-01-01",
        "<key>" => "greeting",
        "<value>" => "hello",
        "<level>" => "high",
        "<levels>" => "high,medium",
        "<labels>" | "<name>" => "backend",
        "<labels-csv>" | "<csv>" => "backend,api",
        "<text>" => "example text",
        "<title>" => "Example title",
        "<reason>" => "example reason",
        "<comment>" => "example comment",
        "<member>" => "mikey",
        "<query>" => "auth",
        "<file>" => "export.json",
        "<path>" | "<PATH>" => "/tmp/example",
        "<url>" => "https://github.com/acme/widgets",
        "<NAME>" => "Example Project",
        "<glyph>" => "🐛",
        "<event_type>" => "pre-commit",
        "<request-id>" => "req-1",
        "<crash-id>" => "crash-1",
        "<target>" => "claude",
        _ => return None,
    })
}

fn substitute(token: &str) -> Result<String, String> {
    // A leading `<` is what marks our own placeholder convention. `contains`
    // would be too broad: a real, already-concrete example value can contain
    // `<` without being one — `"Mikey Ward <mw@mikey.io>"` in Quick start is
    // a real name-and-email argument, not `<name <email>>` needing a table
    // entry.
    if !token.starts_with('<') {
        return Ok(token.to_string());
    }
    placeholder(token).map(str::to_string).ok_or_else(|| {
        format!(
            "unknown placeholder `{token}` — add it to `placeholder()` in \
             tests/readme_command_reference.rs with a value that is actually legal for \
             its flag (an unvalidated default would let this test pass while documenting \
             a value the CLI rejects)"
        )
    })
}

// ---------------------------------------------------------------------------
// Building argv variants
// ---------------------------------------------------------------------------

enum Verdict {
    /// Not a real invocation to test — either a shape illustration whose verb
    /// position is itself a placeholder (`story --project foo <command>`),
    /// or a verb answered before the parser ever sees it (`tui`, dispatched
    /// in `main.rs` ahead of `parse_invocation`).
    Skip,
    Argvs(Vec<Vec<String>>),
}

/// Verbs that never reach `parse_invocation` in a real run, so testing them
/// through it directly would report a false failure. Written down with a
/// reason, same idiom as `EXCLUDED_VERBS`.
const PARSED_ELSEWHERE: &[(&str, &str)] = &[
    (
        "tui",
        "src/main.rs dispatches it ahead of parse_invocation, so `story tui --help` explains \
         rather than launches",
    ),
    (
        "mcp",
        "src/main.rs dispatches it ahead of parse_invocation, exactly like `tui` above, so \
         `story mcp --help` explains rather than starting the stdio server",
    ),
];

/// Global flags that precede the verb and consume a following value, used
/// only to find the verb position for [`Verdict::Skip`] — this is a raw-token
/// heuristic, run *before* substitution, and is independent of the real
/// `split_global_flags` the actual parse below uses.
fn skip_leading_globals(pieces: &[Piece]) -> usize {
    let mut i = 0;
    while let Some(Piece::Sub(SubTok::Word(w))) = pieces.get(i) {
        match w.as_str() {
            "--project" | "--store-path" | "--deadline" => i += 2,
            "--quiet" | "--no-hooks" => i += 1,
            _ => break,
        }
    }
    i
}

/// Expands `|`-alternation within the bare words of one phrase (cross
/// product across positions), then substitutes placeholders. A
/// [`SubTok::Quoted`] is never pipe-split — it is one literal value whatever
/// it contains.
fn expand_phrase(phrase: &[SubTok]) -> Result<Vec<Vec<String>>, String> {
    let mut variants: Vec<Vec<String>> = vec![Vec::new()];
    for tok in phrase {
        let alts: Vec<String> = match tok {
            SubTok::Quoted(q) => vec![substitute(q)?],
            SubTok::Word(w) => {
                if w == "|" {
                    return Err(
                        "a top-level ` | ` alternation was found outside any [ ] or ( ) \
                         group — split this into separate lines, one invocation per line"
                            .to_string(),
                    );
                }
                let mut out = Vec::new();
                for alt in w.split('|') {
                    out.push(substitute(alt)?);
                }
                out
            }
        };
        let mut next = Vec::new();
        for variant in &variants {
            for alt in &alts {
                let mut extended = variant.clone();
                extended.push(alt.clone());
                next.push(extended);
            }
        }
        variants = next;
    }
    Ok(variants)
}

/// Turns one documented invocation (with `story` already stripped) into
/// every argv this test will feed the real parser, or a reason to skip it.
fn build_argvs(raw_without_story: &str) -> Result<Verdict, String> {
    let pieces = tokenize_top(raw_without_story);

    let verb_at = skip_leading_globals(&pieces);
    if let Some(Piece::Sub(SubTok::Word(w))) = pieces.get(verb_at) {
        if w.starts_with('<') && w.ends_with('>') {
            return Ok(Verdict::Skip);
        }
        if PARSED_ELSEWHERE.iter().any(|(verb, _)| verb == w) {
            return Ok(Verdict::Skip);
        }
    }

    let mut required: Vec<Vec<Vec<String>>> = Vec::new();
    let mut optional: Vec<Vec<Vec<String>>> = Vec::new();

    for piece in &pieces {
        match piece {
            Piece::Sub(tok) => {
                required.push(expand_phrase(std::slice::from_ref(tok))?);
            }
            Piece::Choice(alts) => {
                let mut phrases = Vec::new();
                for alt in alts {
                    phrases.extend(expand_phrase(alt)?);
                }
                required.push(phrases);
            }
            Piece::Optional(alts) => {
                let mut phrases = Vec::new();
                for alt in alts {
                    phrases.extend(expand_phrase(alt)?);
                }
                optional.push(phrases);
            }
        }
    }

    let combos: usize = required.iter().map(Vec::len).product();
    if combos > 16 {
        return Err(format!(
            "{combos} required-alternative combinations is more than this line should ever \
             need — split it rather than let the cross product grow unbounded"
        ));
    }

    let mut base_forms: Vec<Vec<String>> = vec![Vec::new()];
    for position in &required {
        let mut next = Vec::new();
        for base in &base_forms {
            for phrase in position {
                let mut combined = base.clone();
                combined.extend(phrase.iter().cloned());
                next.push(combined);
            }
        }
        base_forms = next;
    }

    let mut argvs = Vec::new();
    for base in &base_forms {
        argvs.push(base.clone());
        for group in &optional {
            for phrase in group {
                let mut with_one = base.clone();
                with_one.extend(phrase.iter().cloned());
                argvs.push(with_one);
            }
        }
    }

    Ok(Verdict::Argvs(argvs))
}

/// Confirms an entry actually parses, given the whole real pipeline:
/// `split_global_flags` first (exactly as `main.rs` calls it), then
/// `parse_invocation` on what's left.
fn parses(argv: &[String]) -> Result<(), String> {
    let (_flags, filtered) = split_global_flags(argv).map_err(|e| e.to_string())?;
    parse_invocation(&filtered)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Test 1 — every documented invocation parses
// ---------------------------------------------------------------------------

#[test]
fn every_story_command_in_the_readme_parses() {
    let markdown = readme_text();
    let entries = extract_entries(&markdown);
    assert!(
        entries.len() > 60,
        "found only {} `story ...` lines in README.md's fenced blocks — the extractor may \
         have broken, or the reference shrank without this bound being revisited",
        entries.len()
    );

    let mut failures = Vec::new();
    let mut checked = 0usize;
    for entry in &entries {
        let without_story = entry
            .raw
            .strip_prefix("story")
            .map(str::trim_start)
            .unwrap_or(&entry.raw);

        let verdict = match build_argvs(without_story) {
            Ok(v) => v,
            Err(reason) => {
                failures.push(format!(
                    "README.md:{}: `{}` — {reason}",
                    entry.line, entry.raw
                ));
                continue;
            }
        };
        let argvs = match verdict {
            Verdict::Skip => continue,
            Verdict::Argvs(argvs) => argvs,
        };

        for argv in argvs {
            checked += 1;
            if let Err(reason) = parses(&argv) {
                failures.push(format!(
                    "README.md:{}: `{}` (as `story {}`) — {reason}",
                    entry.line,
                    entry.raw,
                    argv.join(" ")
                ));
            }
        }
    }

    assert!(
        checked > 60,
        "checked only {checked} expanded argv variants across {} entries — the expansion or \
         placeholder table may have broken, since the reference alone has more lines than this",
        entries.len()
    );

    assert!(
        failures.is_empty(),
        "these documented invocations do not parse:\n{}",
        failures.join("\n")
    );
}

// ---------------------------------------------------------------------------
// Test 2 — every dispatchable verb is documented
// ---------------------------------------------------------------------------

/// Verbs `dispatch` recognizes that the reference must not document, each
/// with the reason it is legitimately absent. The same idiom as
/// `tests/checkout_path_readers.rs`'s `ALLOWED`: an entry cannot be added
/// without writing down why, which is the review this list exists to force.
const EXCLUDED_VERBS: &[(&str, &str)] = &[
    ("-h", "short alias; the reference documents `story --help`"),
    (
        "-V",
        "short alias; the reference documents `story --version`",
    ),
    (
        "init",
        "a tombstone: `dispatch` returns \"`story init` is now `story project new`\" and never \
         reaches a real invocation, so documenting it would itself fail Test 1",
    ),
    (
        "relink",
        "a tombstone: `dispatch` returns \"`story relink` is now `story project link \
         checkout`\" and never reaches a real invocation",
    ),
];

/// Slices `source` from `start_marker` to the next line that is exactly `}`
/// at zero indentation — the function's closing brace. Exact-line matching
/// rather than brace-depth counting, because a brace-counter would misread
/// the literal `{}` inside `dispatch`'s own `"unknown command `{}`."` format
/// string as depth, not content.
fn slice_to_top_level_close<'a>(source: &'a str, start_marker: &str) -> &'a str {
    let start = source
        .find(start_marker)
        .unwrap_or_else(|| panic!("marker not found in source: {start_marker}"));
    let rest = &source[start..];
    let mut consumed = 0usize;
    for line in rest.split_inclusive('\n') {
        consumed += line.len();
        if line.trim_end_matches('\n') == "}" {
            return &rest[..consumed];
        }
    }
    panic!("no zero-indent closing brace found after: {start_marker}");
}

/// Every quoted string literal appearing before a `=>` on lines of `body`.
/// `dispatch`'s arms are always `"verb" => ...` or `"a" | "b" => ...`, so
/// this yields every verb string the match recognizes, and nothing from the
/// arm bodies (only the first `=>` on a line bounds the scan, and the
/// catch-all `_ => ...` contributes nothing, since `_` is never quoted).
fn quoted_literals_before_arrow(body: &str) -> Vec<String> {
    let mut found = Vec::new();
    for line in body.lines() {
        let Some(arrow) = line.find("=>") else {
            continue;
        };
        let prefix = &line[..arrow];
        let mut in_quotes = false;
        let mut current = String::new();
        for ch in prefix.chars() {
            if ch == '"' {
                if in_quotes {
                    found.push(std::mem::take(&mut current));
                }
                in_quotes = !in_quotes;
            } else if in_quotes {
                current.push(ch);
            }
        }
    }
    found
}

fn dispatch_verbs() -> Vec<String> {
    let source = cli_source();
    let body = slice_to_top_level_close(
        &source,
        "fn dispatch(args: &[String]) -> Result<Invocation, AppError> {",
    );
    quoted_literals_before_arrow(body)
}

/// The reference's documented verbs: the first word of every `story ...`
/// line inside the `## Command reference` fenced block specifically (not
/// Quick start or scripting examples, which repeat a subset).
fn reference_block_verbs(markdown: &str) -> Vec<String> {
    let heading_at = markdown
        .find("## Command reference")
        .expect("README.md must have a `## Command reference` section");
    let after_heading = &markdown[heading_at..];
    let fence_start = after_heading
        .find("```")
        .expect("`## Command reference` must contain a fenced block");
    let after_open = &after_heading[fence_start + 3..];
    let fence_end = after_open
        .find("```")
        .expect("the `## Command reference` fenced block must close");
    let block = &after_open[..fence_end];

    let mut verbs = Vec::new();
    for line in block.lines() {
        let trimmed = line.trim();
        let Some(rest) = trimmed.strip_prefix("story ") else {
            continue;
        };
        if let Some(verb) = rest.split_whitespace().next() {
            verbs.push(verb.to_string());
        }
    }
    verbs
}

#[test]
fn every_dispatchable_verb_appears_in_the_command_reference() {
    let verbs = dispatch_verbs();
    assert!(
        verbs.len() > 40,
        "the dispatch scan found only {} verbs — it may have broken",
        verbs.len()
    );

    let markdown = readme_text();
    let documented = reference_block_verbs(&markdown);
    assert!(
        documented.len() > 40,
        "the reference block scan found only {} verbs — it may have broken",
        documented.len()
    );

    let excluded: Vec<&str> = EXCLUDED_VERBS.iter().map(|(verb, _)| *verb).collect();
    let mut missing: Vec<&str> = verbs
        .iter()
        .map(String::as_str)
        .filter(|verb| !excluded.contains(verb) && !documented.iter().any(|d| d == verb))
        .collect();
    missing.sort_unstable();
    missing.dedup();

    assert!(
        missing.is_empty(),
        "`dispatch` accepts these verbs but the command reference never shows one starting \
         with them: {missing:?}"
    );
}

#[test]
fn every_excluded_verb_is_really_a_dispatch_arm_and_says_why() {
    let verbs = dispatch_verbs();
    let mut seen = std::collections::BTreeSet::new();
    for (verb, reason) in EXCLUDED_VERBS {
        assert!(
            verbs.iter().any(|v| v == verb),
            "`{verb}` is on EXCLUDED_VERBS but `dispatch` does not recognize it any more — \
             remove the stale entry"
        );
        assert!(
            reason.len() > 20,
            "the EXCLUDED_VERBS entry for `{verb}` needs a real reason, got {reason:?}"
        );
        assert!(seen.insert(*verb), "`{verb}` is on EXCLUDED_VERBS twice");
    }

    // Same governance for the other written-down skip list, minus the
    // dispatch-membership check: `PARSED_ELSEWHERE` exists precisely for
    // verbs `dispatch` does *not* recognize, because something upstream of
    // it answers first (`tui`, in `main.rs`) — requiring dispatch membership
    // here would be asserting the opposite of what the table is for.
    let mut seen_elsewhere = std::collections::BTreeSet::new();
    for (verb, reason) in PARSED_ELSEWHERE {
        assert!(
            reason.len() > 20,
            "the PARSED_ELSEWHERE entry for `{verb}` needs a real reason, got {reason:?}"
        );
        assert!(
            seen_elsewhere.insert(*verb),
            "`{verb}` is on PARSED_ELSEWHERE twice"
        );
    }
}

// ---------------------------------------------------------------------------
// Test 3 & 4 — the Relationships section names the real set, exactly
// ---------------------------------------------------------------------------

/// Every backticked, lowercase-and-hyphen token on a *bullet* line in the
/// `## Relationships` section's direct-input list — the bullets before the
/// "Derived, read-only" subsection, which documents computed relationships
/// that are never valid input to `relate`/`unrelate` and must not be checked
/// as if they were.
///
/// Scoped to lines starting with `- \`` (one line per bullet, no wrapping —
/// the same "one line, one thing" contract the command reference follows),
/// rather than scanning the whole section's backtick pairs: a bullet's own
/// trailing prose may reasonably reference another command in backticks
/// (`` `story graph` ``, say), and that must not be mistaken for a
/// relationship input just because it is also all lowercase and hyphens.
fn readme_relationship_inputs(markdown: &str) -> Vec<String> {
    let heading_at = markdown
        .find("## Relationships")
        .expect("README.md must have a `## Relationships` section");
    let after_heading = &markdown[heading_at..];
    let next_heading = after_heading[2..]
        .find("\n## ")
        .map(|at| at + 2)
        .unwrap_or(after_heading.len());
    let section = &after_heading[..next_heading];

    let direct_end = section.find("Derived, read-only").unwrap_or(section.len());
    let direct = &section[..direct_end];

    let mut found = Vec::new();
    for line in direct.lines() {
        let trimmed = line.trim_start();
        if !trimmed.starts_with("- `") {
            continue;
        }
        let mut rest = trimmed;
        while let Some(at) = rest.find('`') {
            rest = &rest[at + 1..];
            let Some(end) = rest.find('`') else { break };
            let token = &rest[..end];
            if !token.is_empty() && token.chars().all(|c| c.is_ascii_lowercase() || c == '-') {
                found.push(token.to_string());
            }
            rest = &rest[end + 1..];
        }
    }
    found
}

/// The `_ => None` catch-all means every quoted literal before an `=>` in
/// `relation_edges` is a real, accepted input — the same idiom Test 2 uses
/// for `dispatch`.
fn real_relation_inputs() -> Vec<String> {
    let source = domain_source();
    let body = slice_to_top_level_close(
        &source,
        "pub fn relation_edges(input: &str) -> Option<Vec<(&'static str, &'static str)>> {",
    );
    quoted_literals_before_arrow(body)
}

#[test]
fn every_relationship_the_readme_lists_is_one_the_cli_accepts() {
    let markdown = readme_text();
    let documented = readme_relationship_inputs(&markdown);
    assert!(
        documented.len() > 5,
        "found only {} relationship inputs in README's Relationships section — the scan may \
         have broken",
        documented.len()
    );

    let fake: Vec<&String> = documented
        .iter()
        .filter(|input| !is_relation_input(input))
        .collect();
    assert!(
        fake.is_empty(),
        "README's Relationships section lists these as valid `story relate` inputs, but \
         `domain::is_relation_input` refuses them: {fake:?}"
    );
}

#[test]
fn every_relationship_input_the_cli_accepts_is_listed_in_the_readme() {
    let real = real_relation_inputs();
    assert!(
        real.len() > 5,
        "the `relation_edges` scan found only {} inputs — it may have broken",
        real.len()
    );

    let markdown = readme_text();
    let documented = readme_relationship_inputs(&markdown);

    let undocumented: Vec<&String> = real
        .iter()
        .filter(|input| !documented.contains(input))
        .collect();
    assert!(
        undocumented.is_empty(),
        "`domain::relation_edges` accepts these relationship inputs, but README's \
         Relationships section never names them: {undocumented:?}"
    );
}

// ---------------------------------------------------------------------------
// Test 5 — the scan would notice a command that does not exist
// ---------------------------------------------------------------------------

/// Vacuity guard for Test 1's whole mechanism: feeds the extractor and
/// expander an inline fixture containing a real fenced block around an
/// invocation this CLI has never had, and asserts the pipeline reports it as
/// a failure rather than silently accepting it. If a future change to the
/// extractor or expander made this pass by construction (skipping the
/// invocation, or accepting anything), this test would go red and say so —
/// the same role `checked > 0` plays in `help_topic_references.rs`.
#[test]
fn the_readme_scan_would_notice_a_command_that_does_not_exist() {
    let fixture = "# fixture\n\n```bash\nstory SH-1 assign mikey\n```\n";
    let entries = extract_entries(fixture);
    assert_eq!(
        entries.len(),
        1,
        "the fixture should yield exactly one candidate"
    );

    let without_story = entries[0]
        .raw
        .strip_prefix("story")
        .map(str::trim_start)
        .unwrap();
    let Verdict::Argvs(argvs) =
        build_argvs(without_story).expect("a plain literal line always builds")
    else {
        panic!(
            "`story SH-1 assign mikey` is not a verb-position placeholder and must not be skipped"
        );
    };
    assert_eq!(
        argvs.len(),
        1,
        "no optional groups here, so there is exactly one argv"
    );
    assert!(
        parses(&argvs[0]).is_err(),
        "`story SH-1 assign mikey` is id-first grammar this CLI has never accepted — the scan \
         must report it as a failure, not pass it"
    );
}
