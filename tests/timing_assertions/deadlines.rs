//! SH-803: a deadline must name or derive its duration, even when the exact
//! wait inventory classifies it as an intentional observation window.
//!
//! This lexical fence does not resolve aliases or infer timing semantics.

use std::path::Path;
use std::sync::OnceLock;

use super::BareCeiling;

/// Bare duration literals added directly to the current instant.
fn bare_deadlines(source: &str) -> Vec<BareCeiling> {
    static SHAPE: OnceLock<regex::Regex> = OnceLock::new();
    let shape = SHAPE.get_or_init(|| {
        regex::Regex::new(concat!(
            r"(?x)\b (?:std\s*::\s*time\s*::\s*)? Instant\s*::\s*now\s*\(\s*\) ",
            r"\s*\+\s* (?::: \s*)? (?:(?:std|core)\s*::\s*time\s*::\s*)? ",
            r"Duration\s*::\s*from_(?:secs|millis|micros|nanos)\s*\(\s* ",
            r"(?:0x[0-9a-fA-F_]+|0o[0-7_]+|0b[01_]+|[0-9][0-9_]*) ",
            r"(?:[ui](?:8|16|32|64|128|size))? \s*,?\s*\)"
        ))
        .expect("the bare deadline pattern is valid")
    });
    // The shared masker preserves bytes and newlines, so diagnostics refer
    // to the original file even after multiline comments or Unicode strings.
    let code = super::waits::code_only(source);
    shape
        .find_iter(&code)
        .map(|found| {
            let line = source[..found.start()]
                .bytes()
                .filter(|byte| *byte == b'\n')
                .count();
            BareCeiling {
                line: line + 1,
                text: source
                    .lines()
                    .nth(line)
                    .expect("match is within source")
                    .trim()
                    .into(),
            }
        })
        .collect()
}

#[test]
fn recognizes_every_constructor_and_integer_spelling() {
    for unit in ["secs", "millis", "micros", "nanos"] {
        for literal in [
            "0",
            "5",
            "1_000",
            "5u64",
            "5_u64",
            "0xff",
            "0x_ff_u64",
            "0o17",
            "0o_17",
            "0b101",
            "0b_10_1u64",
        ] {
            for instant in ["Instant", "std::time::Instant", "::std::time::Instant"] {
                for duration in [
                    "Duration",
                    "std::time::Duration",
                    "::std::time::Duration",
                    "core::time::Duration",
                    "::core::time::Duration",
                ] {
                    let source =
                        format!("let end = {instant}::now() + {duration}::from_{unit}({literal});");
                    assert_eq!(
                        bare_deadlines(&source),
                        vec![BareCeiling {
                            line: 1,
                            text: source.clone(),
                        }],
                        "{source}"
                    );
                }
            }
        }
    }
}

#[test]
fn recognizes_multiline_tokens_comments_and_trailing_commas() {
    let source = "// heading\nlet end = std :: time :: Instant :: now ( )\n\
                  /* margin */ + std :: time :: Duration :: from_millis (\n\
                  1_000_u64,\n);";
    assert_eq!(
        bare_deadlines(source),
        vec![BareCeiling {
            line: 2,
            text: "let end = std :: time :: Instant :: now ( )".into(),
        }]
    );
}

#[test]
fn reports_every_site_at_its_original_line() {
    let deadline = "Instant::now() + Duration::from_secs(5)";
    let source = format!(
        "/* α\n nested /* comment */\n */\nlet label = \"☃\"; let a = {deadline}; let b = {deadline};\n\nlet c = {deadline};"
    );
    let findings = bare_deadlines(&source);
    assert_eq!(
        findings.iter().map(|f| f.line).collect::<Vec<_>>(),
        [4, 4, 6]
    );
    assert_eq!(findings[0].text, source.lines().nth(3).unwrap());
    assert_eq!(findings[2].text, source.lines().nth(5).unwrap());
}

#[test]
fn ignores_prose_and_literal_fixtures_without_hiding_adjacent_code() {
    let deadline = "Instant::now() + Duration::from_secs(5)";
    for prose in [
        format!("// {deadline}\n"),
        format!("/* outer /* {deadline} */ end */"),
        format!("let s = {deadline:?};"),
        format!("let s = r###\"{deadline}\"###;"),
        format!("let s = br###\"{deadline}\"###;"),
        "let chars = ['\\'', '\"']; fn f<'a>(s: &'a str) {}".into(),
    ] {
        assert!(bare_deadlines(&prose).is_empty(), "{prose}");
        assert_eq!(
            bare_deadlines(&format!("{prose}\nlet end = {deadline};")).len(),
            1,
            "live code after {prose}"
        );
    }
}

#[test]
fn permits_names_derivations_and_other_duration_uses() {
    for source in [
        "const BOUND: Duration = Duration::from_secs(5);",
        "let end = Instant::now() + BOUND;",
        "let end = Instant::now() + Duration::from_secs(BOUND_SECS);",
        "let end = Instant::now() + Duration::from_secs(BOUND.as_secs() * 2);",
        "let end = Instant::now() + Duration::from_secs(2 * BOUND_SECS);",
        "let end = Instant::now() + BOUND + Duration::from_millis(5);",
        "let end = Instant::now() + load_grace::graced_now(BASE);",
        "let elapsed = Instant::now() - Duration::from_secs(5);",
        "let other = OtherInstant::now() + Duration::from_secs(5);",
        "let other = Instant::now() + OtherDuration::from_secs(5);",
        "thread::sleep(Duration::from_secs(5));",
    ] {
        assert!(bare_deadlines(source).is_empty(), "{source}");
    }
}

#[test]
fn no_test_builds_a_deadline_from_a_bare_duration_literal() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut corpus = super::tracked_test_files(root, "tests/*.rs");
    corpus.extend(super::tracked_test_files(
        root,
        "crates/storyhook-test-support/src/*.rs",
    ));
    for required in [
        "tests/daemon_lifecycle.rs",
        "tests/engine_reset/quiescent.rs",
        "tests/timing_assertions/deadlines.rs",
        "crates/storyhook-test-support/src/server.rs",
    ] {
        assert!(corpus.contains_key(required), "{required} was not scanned");
    }
    let findings: Vec<_> = corpus
        .iter()
        .flat_map(|(path, source)| {
            bare_deadlines(source)
                .into_iter()
                .map(move |found| format!("{path}:{}: {}", found.line, found.text))
        })
        .collect();
    assert!(
        findings.is_empty(),
        "bare Duration literal in an Instant deadline (SH-803): derive the duration from its \
         production owner or use a documented local constant; wait classification is not \
         an exemption:\n{}",
        findings.join("\n")
    );
}
