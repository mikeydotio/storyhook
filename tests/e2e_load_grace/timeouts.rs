use super::{read, tracked_e2e_files};

// Lexical fence for harness option objects, not a TypeScript type/data-flow
// checker. Template payloads and product-side `.timeout =` assignments are
// stimuli, not harness options; the SQLite payload has its own exact audit.
#[derive(Debug)]
struct TimeoutSite {
    line: usize,
    rhs: String,
}

fn timeout_sites(source: &str) -> Vec<TimeoutSite> {
    let b = source.as_bytes();
    let mut tokens: Vec<(String, usize)> = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i].is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if b[i..].starts_with(b"//") {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if b[i..].starts_with(b"/*") {
            i += 2;
            while i + 1 < b.len() && &b[i..i + 2] != b"*/" {
                i += 1;
            }
            i = (i + 2).min(b.len());
            continue;
        }
        let start = i;
        if matches!(b[i], b'\'' | b'"' | b'`') {
            let quote = b[i];
            i += 1;
            while i < b.len() {
                if b[i] == b'\\' {
                    i = (i + 2).min(b.len());
                } else if b[i] == quote {
                    i += 1;
                    break;
                } else {
                    i += 1;
                }
            }
            // Quoted object keys are real keys; other literals are opaque.
            let literal = &source[start..i];
            tokens.push((
                if literal == "\"timeout\"" || literal == "'timeout'" {
                    "timeout".into()
                } else {
                    "<literal>".into()
                },
                start,
            ));
        } else if b[i].is_ascii_alphanumeric() || matches!(b[i], b'_' | b'$') {
            i += 1;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || matches!(b[i], b'_' | b'$')) {
                i += 1;
            }
            tokens.push((source[start..i].into(), start));
        } else {
            let c = source[i..].chars().next().unwrap();
            i += c.len_utf8();
            tokens.push((c.to_string(), start));
        }
    }
    let mut found = Vec::new();
    for n in 0..tokens.len().saturating_sub(1) {
        if tokens[n].0 != "timeout" || tokens[n + 1].0 != ":" {
            continue;
        }
        let mut depth = 0;
        let mut rhs = String::new();
        for (t, _) in &tokens[n + 2..] {
            if depth == 0 && matches!(t.as_str(), "," | "}" | ";") {
                break;
            }
            match t.as_str() {
                "(" | "[" | "{" => depth += 1,
                ")" | "]" | "}" => depth -= 1,
                _ => (),
            }
            rhs.push_str(t);
        }
        found.push(TimeoutSite {
            line: source[..tokens[n].1]
                .bytes()
                .filter(|b| *b == b'\n')
                .count()
                + 1,
            rhs,
        });
    }
    found
}

/// Exact paths, expressions and counts make an exemption reviewable and stale
/// entries loud. These are proofs or budget ownership, never arbitrary names.
const TIMEOUT_EXCEPTIONS: &[(&str, &str, usize, &str)] = &[
    (
        "e2e/expect-grace.ts",
        "budget",
        1,
        "call-time assertion budget sampled by gracedPatience",
    ),
    (
        "e2e/playwright.config.ts",
        "loadGraceEnabled()?gracedBudget(BASE_TEST_TIMEOUT_MS):BASE_TEST_TIMEOUT_MS",
        1,
        "config kill switch",
    ),
    (
        "e2e/playwright.config.ts",
        "loadGraceEnabled()?gracedBudget(BASE_EXPECT_TIMEOUT_MS):BASE_EXPECT_TIMEOUT_MS",
        1,
        "config kill switch",
    ),
    (
        "e2e/specs/notification-contract.spec.ts",
        "GONE_TIMEOUT",
        2,
        "notice lifetime proof",
    ),
    (
        "e2e/specs/text-assertion-door.spec.ts",
        "FIDELITY_TIMEOUT_MS",
        7,
        "delegation fidelity proof",
    ),
    (
        "e2e/specs/attachment-fixture.ts",
        "test.info().timeout",
        1,
        "inherited test budget",
    ),
    (
        "e2e/specs/attachment-viewer.spec.ts",
        "test.info().timeout",
        1,
        "inherited test budget",
    ),
    (
        "e2e/specs/remote-image-viewer.spec.ts",
        "test.info().timeout",
        1,
        "inherited test budget",
    ),
    (
        "e2e/specs/untrusted-origin-cookie.spec.ts",
        "test.info().timeout",
        1,
        "inherited test budget",
    ),
    (
        "e2e/block-delivery-barrier.cjs",
        "boundMs",
        1,
        "validated remaining barrier patience",
    ),
    (
        "e2e/reporter-command.ts",
        "boundMs",
        1,
        "validated reporter process budget: graced suite allowance minus cleanup patience",
    ),
    (
        "e2e/specs/browser-launch-reporter.node.spec.ts",
        "boundMs",
        1,
        "readiness poll shares the fixture process bound sampled by gracedPatience",
    ),
    (
        "e2e/specs/support.ts",
        "0",
        1,
        "barrier owns the poll deadline",
    ),
    (
        "e2e/specs/support.ts",
        "options?.timeout??this.timeout",
        1,
        "text matcher delegates the explicit or effective assertion budget",
    ),
    (
        "e2e/specs/expect-grace.node.spec.ts",
        "71",
        1,
        "fixed configure override precedence proof",
    ),
    (
        "e2e/specs/expect-grace.node.spec.ts",
        "0",
        2,
        "explicit zero configure and poll override proofs",
    ),
    (
        "e2e/specs/expect-grace.node.spec.ts",
        "undefined",
        1,
        "undefined configure override restores dynamic sampling proof",
    ),
    (
        "e2e/specs/expect-grace.node.spec.ts",
        "PROOF_TIMEOUT_MS",
        1,
        "poll explicit deadline precedence proof",
    ),
    (
        "e2e/specs/expect-grace.spec.ts",
        "0",
        2,
        "positive and negated matcher explicit zero override proofs",
    ),
    (
        "e2e/specs/expect-grace.spec.ts",
        "PROOF_TIMEOUT_MS",
        3,
        "browser matcher and configured deadline precedence proofs",
    ),
    (
        "e2e/specs/engine.spec.ts",
        "remainingMs",
        1,
        "remaining monotonic engine observation budget",
    ),
    (
        "e2e/specs/load-grace.node.spec.ts",
        "SHORT_REQUEST_PROOF_MS",
        1,
        "request override proof: deliberately shorter context default",
    ),
];

fn grace_call(rhs: &str) -> bool {
    for name in [
        "gracedPatience",
        "gracedOperationBudget",
        "gracedRequestBudget",
    ] {
        if let Some(args) = rhs.strip_prefix(&format!("{name}(")) {
            let mut depth = 1;
            for (i, c) in args.char_indices() {
                if c == '(' {
                    depth += 1;
                }
                if c == ')' {
                    depth -= 1;
                }
                if depth == 0 {
                    return i + 1 == args.len();
                }
            }
        }
    }
    false
}

fn timeout_allowed(path: &str, rhs: &str) -> bool {
    grace_call(rhs)
        || TIMEOUT_EXCEPTIONS
            .iter()
            .any(|(p, r, _, _)| *p == path && *r == rhs)
}

#[test]
fn every_tracked_e2e_timeout_is_graced_or_a_reviewed_proof() {
    let mut paths = Vec::new();
    for extension in [".ts", ".js", ".cjs", ".mjs"] {
        paths.extend(tracked_e2e_files(extension));
    }
    assert!(
        paths.len() > 50,
        "the timeout census must read the real corpus"
    );
    let mut seen = vec![0; TIMEOUT_EXCEPTIONS.len()];
    let mut offenders = Vec::new();
    for path in paths {
        for site in timeout_sites(&read(&path)) {
            if !timeout_allowed(&path, &site.rhs) {
                offenders.push(format!("{path}:{}: timeout: {}", site.line, site.rhs));
            }
            for (i, (p, rhs, _, _)) in TIMEOUT_EXCEPTIONS.iter().enumerate() {
                if *p == path && *rhs == site.rhs {
                    seen[i] += 1;
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "ungraced patience / unreviewed proof:\n{}",
        offenders.join("\n")
    );
    for ((path, rhs, count, reason), found) in TIMEOUT_EXCEPTIONS.iter().zip(seen) {
        assert_eq!(
            *count, found,
            "stale or broadened timeout exception {path}: {rhs} ({reason})"
        );
    }
}

#[test]
fn assertion_grace_proofs_are_allowed_only_at_reviewed_paths_and_values() {
    for (path, expressions) in [
        ("e2e/expect-grace.ts", &["budget"][..]),
        (
            "e2e/specs/expect-grace.node.spec.ts",
            &["71", "0", "undefined", "PROOF_TIMEOUT_MS"][..],
        ),
        (
            "e2e/specs/expect-grace.spec.ts",
            &["0", "PROOF_TIMEOUT_MS"][..],
        ),
        (
            "e2e/specs/support.ts",
            &["options?.timeout??this.timeout"][..],
        ),
    ] {
        for rhs in expressions {
            assert!(timeout_allowed(path, rhs), "reviewed proof: {path}: {rhs}");
            assert!(!timeout_allowed("e2e/specs/unreviewed.spec.ts", rhs));
            assert!(!timeout_allowed(path, &format!("{rhs}+1")));
        }
        assert!(!timeout_allowed(path, "72"));
        assert!(!timeout_allowed(path, "ARBITRARY_WAIT_MS"));
    }
}

#[test]
fn timeout_fence_handles_formatting_and_rejects_named_bypasses() {
    let source = "// timeout: 1,\n/* timeout: 2, */\nconst text = `timeout: 3,`;\nconst opts = {\n 'timeout' /*why*/ :\n gracedOperationBudget(Math.max(BASE, OTHER), 2),\n};\n";
    let sites = timeout_sites(source);
    assert_eq!(sites.len(), 1);
    assert_eq!(sites[0].line, 5);
    assert_eq!(
        sites[0].rhs,
        "gracedOperationBudget(Math.max(BASE,OTHER),2)"
    );
    assert!(timeout_allowed("any.ts", &sites[0].rhs));
    for rhs in [
        "5000",
        "5_000",
        "5*1000",
        "0x1388",
        "WAIT_MS",
        "gracedPatience()+5000",
        "0",
    ] {
        let sites = timeout_sites(&format!("const options = {{ timeout : {rhs} }};"));
        assert_eq!(sites.len(), 1);
        assert!(!timeout_allowed("any.ts", &sites[0].rhs), "{rhs}");
    }
    assert!(timeout_allowed(
        "e2e/specs/notification-contract.spec.ts",
        "GONE_TIMEOUT"
    ));
    assert!(!timeout_allowed("another.ts", "GONE_TIMEOUT"));
    assert!(timeout_sites("interface Options { timeout?: number; } xhr.timeout = 50;").is_empty());
}

#[test]
fn reporter_timeout_exceptions_are_exact_and_nontransferable() {
    for path in [
        "e2e/reporter-command.ts",
        "e2e/specs/browser-launch-reporter.node.spec.ts",
    ] {
        assert!(timeout_allowed(path, "boundMs"), "{path}");
        for changed in ["anotherBound", "boundMs+5000", "60000", "0"] {
            assert!(!timeout_allowed(path, changed), "{path}: {changed}");
        }
    }
    assert!(!timeout_allowed("e2e/another-helper.ts", "boundMs"));
    assert!(!timeout_allowed("e2e/specs/another.spec.ts", "boundMs"));
}

#[test]
fn fixture_administration_never_inherits_the_request_default() {
    for (path, function, calls) in [
        ("e2e/specs/support.ts", "resetFixtureTokenPreferences", 1),
        ("e2e/specs/support.ts", "projectSlug", 1),
        ("e2e/specs/support.ts", "removeStrays", 1),
        ("e2e/specs/support.ts", "deleteStatus", 1),
        ("e2e/fixture-baseline.ts", "projectStories", 1),
        ("e2e/fixture-baseline.ts", "captureFixtureBaseline", 1),
    ] {
        let source = read(path);
        let body = source
            .split_once(&format!("function {function}("))
            .unwrap_or_else(|| panic!("missing {path}:{function}"))
            .1
            .split_once("\n}")
            .unwrap()
            .0;
        let sites = timeout_sites(body);
        assert_eq!(
            sites.len(),
            calls,
            "{path}:{function} must explicitly bound each request"
        );
        assert!(
            sites.iter().all(|s| s.rhs == "gracedRequestBudget()"),
            "{path}:{function} must sample request grace on entry"
        );
    }
}

#[test]
fn sqlite_busy_patience_comes_from_the_bounded_parent() {
    let source = read("e2e/block-delivery-barrier.cjs");
    for required in [
        "path, project, story, bound_ms = sys.argv[1:]",
        "timeout=int(bound_ms) / 1000",
        "storePath, project, story, String(boundMs)",
    ] {
        assert!(
            source.contains(required),
            "SQLite must share the read's budget: missing {required}"
        );
    }
}

#[test]
fn engine_remaining_time_is_owned_by_one_graced_monotonic_deadline() {
    let source = read("e2e/specs/engine.spec.ts");
    let body = source
        .split_once("async function observedWorkingRun(")
        .unwrap()
        .1
        .split_once("\n}")
        .unwrap()
        .0;
    for required in [
        "const patienceMs = gracedOperationBudget(REAL_ENGINE_TIMEOUT);",
        "const deadline = performance.now() + patienceMs;",
        "const remainingMs = Math.floor(deadline - performance.now());",
        "expect(remainingMs, diagnostic()).toBeGreaterThan(0);",
        "timeout: remainingMs,",
    ] {
        assert!(
            body.contains(required),
            "engine observation budget lost its owner: {required}"
        );
    }
    assert_eq!(
        body.matches("gracedOperationBudget(").count(),
        1,
        "polling must not reset its budget"
    );
}
