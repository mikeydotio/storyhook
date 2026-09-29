//! SH-810: raw harness bounds need an exact, reviewed classification.
//!
//! This is a lexical fence, not a type checker. Aliases and generated programs
//! remain part of the documented census. Strings are deliberately opaque here.

use std::collections::BTreeMap;
use std::path::Path;

/// Removes non-code without joining tokens separated by comments.
pub(super) fn code_only(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut out = bytes.to_vec();
    let mut i = 0;
    while i < bytes.len() {
        let start = i;
        if bytes[i..].starts_with(b"//") {
            i += 2;
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
        } else if bytes[i..].starts_with(b"/*") {
            i += 2;
            let mut depth = 1;
            while i < bytes.len() && depth > 0 {
                if bytes[i..].starts_with(b"/*") {
                    depth += 1;
                    i += 2;
                } else if bytes[i..].starts_with(b"*/") {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
        } else if bytes[i] == b'r' {
            let mut quote = i + 1;
            while quote < bytes.len() && bytes[quote] == b'#' {
                quote += 1;
            }
            if quote >= bytes.len() || bytes[quote] != b'"' {
                i += 1;
                continue;
            }
            let end = format!("\"{}", "#".repeat(quote - i - 1));
            i = source[quote + 1..]
                .find(&end)
                .map_or(bytes.len(), |n| quote + 1 + n + end.len());
        } else if bytes[i] == b'"' {
            i += 1;
            while i < bytes.len() {
                match bytes[i] {
                    b'\\' => i = (i + 2).min(bytes.len()),
                    b'"' => {
                        i += 1;
                        break;
                    }
                    _ => i += 1,
                }
            }
        } else if bytes[i] == b'\'' {
            // A lifetime has no closing quote. Character literals do.
            let end = if bytes.get(i + 1) == Some(&b'\\') {
                // The escaped character itself may be a quote (`'\''`), so
                // the closing quote is searched for after it.
                source
                    .get(i + 3..)
                    .and_then(|rest| rest.find('\''))
                    .map(|n| i + 4 + n)
            } else {
                source[i + 1..].chars().next().and_then(|c| {
                    let end = i + 1 + c.len_utf8();
                    (bytes.get(end) == Some(&b'\'')).then_some(end + 1)
                })
            };
            if let Some(end) = end {
                i = end;
            } else {
                i += 1;
                continue;
            }
        } else {
            i += 1;
            continue;
        }
        for byte in &mut out[start..i] {
            if *byte != b'\n' {
                *byte = b' ';
            }
        }
    }
    String::from_utf8(out).expect("masked source remains UTF-8")
}

/// The expression up to its enclosing separator, including nested call arguments.
pub(super) fn expression_end(code: &str, start: usize) -> usize {
    let mut depth = 0_usize;
    for (offset, byte) in code.as_bytes()[start..].iter().enumerate() {
        match byte {
            b'(' | b'[' => depth += 1,
            b'{' if depth > 0 || offset == 0 => depth += 1,
            b')' | b']' | b'}' if depth > 0 => depth -= 1,
            b'&' if depth == 0 && code[start + offset..].starts_with("&&") => {
                return start + offset;
            }
            b')' | b']' | b',' | b';' | b'{' | b'}' | b'|' if depth == 0 => {
                return start + offset;
            }
            _ => {}
        }
    }
    code.len()
}

/// Normalized raw bound expressions and their occurrence counts.
fn raw_waits(source: &str) -> BTreeMap<String, usize> {
    let code: String = code_only(source).split_whitespace().collect();
    let pattern = regex::Regex::new(concat!(
        r"(?:std::time::)?Instant::now\(\)(?:\+|<=|>=|<|>)|",
        r"[A-Za-z_][A-Za-z_0-9.]*\.elapsed\(\)(?:<=|>=|<|>)|",
        r"\.(?:recv|recv_timeout|wait_within|wait_with_output_within|set_read_timeout|set_write_timeout|busy_timeout|timeout|timeout_global)\(|",
        r"(?:run_bounded|watchdog|http_status_line)\("
    ))
    .unwrap();
    let mut found = BTreeMap::new();
    for matched in pattern.find_iter(&code) {
        if code[..matched.start()].ends_with("fn") || code[..matched.start()].ends_with('_') {
            continue;
        }
        // A method match includes its opening parenthesis; only its arguments
        // are scanned so closures and their diagnostic text are not inventory keys.
        let mut argument = matched.end();
        let skip = match matched.as_str() {
            "run_bounded(" => 2,
            "watchdog(" | "http_status_line(" => 1,
            _ => 0,
        };
        for _ in 0..skip {
            argument = expression_end(&code, argument) + 1;
        }
        let end = expression_end(&code, argument);
        let expression = format!("{}{}", matched.as_str(), &code[argument..end]);
        if expression.contains("load_grace::graced_now(") {
            continue;
        }
        *found.entry(expression).or_default() += 1;
    }
    found
}

#[test]
fn recognizes_raw_waits_and_permits_explicit_grace() {
    let raw = "let end = std::time::Instant::now() + Duration::from_secs(5);\n\
               rx . recv_timeout( Duration::from_secs(2) );\n\
               while start.elapsed() < bound { poll(); }";
    assert_eq!(raw_waits(raw).len(), 3);
    assert!(raw_waits("rx.recv_timeout(load_grace::graced_now(BASE));").is_empty());
    assert!(
        raw_waits(
            "let end = Instant::now() + storyhook_test_support::load_grace::graced_now(BASE);"
        )
        .is_empty()
    );
    assert!(raw_waits("run_bounded(cmd, \"label\", load_grace::graced_now(BASE));").is_empty());
    assert_eq!(raw_waits("run_bounded(cmd, \"label\", BASE);").len(), 1);
    assert_eq!(
        raw_waits("rx.recv(); http_status_line(port, BASE); agent.timeout_global(Some(BASE));")
            .len(),
        3
    );
    assert_eq!(
        raw_waits("run_bounded({ let c = command(); c }, &format!(\"label\"), BASE);")
            .keys()
            .cloned()
            .collect::<Vec<_>>(),
        ["run_bounded(BASE"]
    );
    assert!(raw_waits("fn checks_the_watchdog() {} fn run_bounded(cmd: Command, what: &str, limit: Duration) {}").is_empty());
    assert_eq!(
        raw_waits("rx.recv_timeout(BASE); rx.recv_timeout(BASE);")
            .values()
            .copied()
            .collect::<Vec<_>>(),
        [2]
    );
}

#[test]
fn ignores_comments_strings_characters_and_lifetimes() {
    let source = r####"
        // Instant::now() + BASE;
        /* outer /* rx.recv_timeout(BASE); */ end */
        let ordinary = "rx.recv_timeout(BASE); // still a string";
        let raw = r###"Instant::now() + BASE; /* still a string */"###;
        let byte = b'"'; let character = '\'';
        fn report<'a>(label: &'a str) { rx.recv_timeout(BASE); }
    "####;
    assert_eq!(raw_waits(source).len(), 1);
    assert_eq!(raw_waits(source).values().copied().collect::<Vec<_>>(), [1]);
}

#[test]
fn an_escaped_quote_character_literal_is_masked_whole() {
    // Unformatted but valid Rust: a stray quote left after the escaped one
    // pairs with the comma into a character literal, and the double quote
    // after it then opens a string that swallows the wait.
    let source = "let quotes = ['\\'','\"']; rx.recv_timeout(BASE); let s = \"x\";";
    assert_eq!(
        raw_waits(source).keys().cloned().collect::<Vec<_>>(),
        [".recv_timeout(BASE"]
    );
    for escaped in ["'\\\\'", "'\\n'", "'\\u{7b}'", "'\\x7f'"] {
        let source = format!("let c = {escaped}; rx.recv_timeout(BASE);");
        assert_eq!(raw_waits(&source).len(), 1, "{source}");
    }
}

#[test]
fn every_raw_wait_has_an_exact_classification() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut corpus = super::tracked_test_files(root, "tests/*.rs");
    corpus.extend(super::tracked_test_files(
        root,
        "crates/storyhook-test-support/src/*.rs",
    ));
    assert!(
        corpus.contains_key("tests/engine_reset/quiescent.rs"),
        "the census must include nested modules"
    );
    assert!(corpus.contains_key("crates/storyhook-test-support/src/server.rs"));
    let actual: BTreeMap<_, _> = corpus
        .into_iter()
        .filter_map(|(path, source)| {
            let waits = raw_waits(&source);
            (!waits.is_empty()).then_some((path, waits))
        })
        .collect();
    let reviewed: BTreeMap<String, BTreeMap<String, serde_json::Value>> = serde_json::from_str(
        &std::fs::read_to_string(root.join("tests/timing_assertions/waits.json")).unwrap(),
    )
    .unwrap();
    let expected: BTreeMap<_, _> = reviewed
        .into_iter()
        .map(|(path, entries)| {
            let waits = entries
                .into_iter()
                .map(|(expression, entry)| {
                    let kind = entry["kind"].as_str().expect("classification kind");
                    assert!(
                        ["proof", "delegated", "fixture"].contains(&kind),
                        "{path}: {expression}: {kind}"
                    );
                    assert!(
                        !entry["reason"]
                            .as_str()
                            .expect("classification reason")
                            .trim()
                            .is_empty()
                    );
                    let count = entry["count"].as_u64().expect("occurrence count") as usize;
                    assert!(count > 0);
                    (expression, count)
                })
                .collect();
            (path, waits)
        })
        .collect();
    assert_eq!(
        actual, expected,
        "new, changed, or stale raw waits: grace patience; document proofs and delegated bounds in waits.json"
    );
}
