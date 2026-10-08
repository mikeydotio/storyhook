//! SH-846: direct integration entrances into production subprocess budgets.
//!
//! This is a lexical boundary census, not a call graph. It inventories known
//! constructors and daemon ticks in every tracked integration/support file,
//! including nested modules, keyed by enclosing function and occurrence count.
//! Aliases, CLI invocations, generated programs and new API families still need
//! manual review alongside src_bounds.json. A constructor is not itself a spawn;
//! its classification records what the caller subsequently invokes.
use std::collections::BTreeMap;
use std::path::Path;

use super::waits::code_only;

type Counts = BTreeMap<String, BTreeMap<String, usize>>;

fn function_ranges(code: &str) -> Vec<(usize, usize, String)> {
    let functions = regex::Regex::new(r"\bfn\s+([A-Za-z_][A-Za-z_0-9]*)").unwrap();
    functions
        .captures_iter(code)
        .filter_map(|capture| {
            let start = capture.get(0).unwrap().start();
            let tail = &code[capture.get(0).unwrap().end()..];
            let delimiter = tail.find(['{', ';'])?;
            if tail.as_bytes()[delimiter] != b'{' {
                return None;
            }
            let open = capture.get(0).unwrap().end() + delimiter;
            let mut depth = 0;
            for (offset, byte) in code.as_bytes()[open..].iter().enumerate() {
                match byte {
                    b'{' => depth += 1,
                    b'}' => {
                        depth -= 1;
                        if depth == 0 {
                            return Some((start, open + offset + 1, capture[1].to_string()));
                        }
                    }
                    _ => {}
                }
            }
            None
        })
        .collect()
}

fn boundaries(source: &str) -> BTreeMap<String, usize> {
    let code = code_only(source);
    let functions = function_ranges(&code);
    let calls = regex::Regex::new(r"\b(ShellDispatcher\s*::\s*new|LiveDispatchInspector\s*::\s*new|ShellVerificationActuator\s*::\s*(?:new|with_paths|with_paths_and_timing)|PythonRuntime\s*::\s*(?:at|installed)|CleanupService\s*::\s*new|ResourceService\s*::\s*new|(?:Repository|OriginObservation)\s*::\s*(?:resolve|resolve_for_fixture)|run_local|run_local_for_fixture|fixture_repository|reconcile_restart_tick|reconcile_tick|tick_closures)\s*\(").unwrap();
    let mut found = BTreeMap::new();
    for capture in calls.captures_iter(&code) {
        let offset = capture.get(0).unwrap().start();
        if code[..offset].trim_end().ends_with("fn") {
            continue;
        }
        let owner = functions
            .iter()
            .filter(|(start, end, _)| *start <= offset && offset < *end)
            .max_by_key(|(start, _, _)| *start)
            .map(|(_, _, name)| name.as_str())
            .unwrap_or("<module>");
        let call: String = capture[1].split_whitespace().collect();
        *found.entry(format!("{owner}: {call}")).or_insert(0) += 1;
    }
    found
}

fn inventory(sources: &BTreeMap<String, String>) -> Counts {
    sources
        .iter()
        .filter_map(|(path, source)| {
            let calls = boundaries(source);
            (!calls.is_empty()).then(|| (path.clone(), calls))
        })
        .collect()
}

#[derive(serde::Deserialize)]
struct Declaration {
    count: usize,
    kind: String,
    mechanism: String,
    reason: String,
}

#[test]
fn sh846_every_known_integration_subprocess_boundary_has_an_exact_classification() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut sources = super::tracked_test_files(root, "tests/*.rs");
    sources.extend(super::tracked_test_files(
        root,
        "crates/storyhook-test-support/src/*.rs",
    ));
    let declarations: BTreeMap<String, BTreeMap<String, Declaration>> = serde_json::from_str(
        &std::fs::read_to_string(root.join("tests/timing_assertions/integration_bounds.json"))
            .unwrap(),
    )
    .unwrap();
    let mut expected = Counts::new();
    for (path, entries) in declarations {
        for (boundary, entry) in entries {
            assert!(
                matches!(entry.kind.as_str(), "patience" | "proof" | "fixture"),
                "{path}: {boundary}: unknown class"
            );
            assert!(
                entry.count > 0
                    && !entry.mechanism.trim().is_empty()
                    && !entry.reason.trim().is_empty(),
                "{path}: {boundary}: incomplete review"
            );
            expected
                .entry(path.clone())
                .or_default()
                .insert(boundary, entry.count);
        }
    }
    assert_eq!(
        inventory(&sources),
        expected,
        "integration subprocess boundary changed; review its actual production budget and record explicit patience/proof/fixture ownership"
    );
}

#[test]
fn sh846_boundary_scanner_ignores_literals_and_uses_the_enclosing_function() {
    let source = r##"
        // ShellDispatcher::new(ignore, ignore)
        fn outer() {
            let text = r#"reconcile_tick(a, b)"#;
            struct Guard;
            impl Drop for Guard { fn drop(&mut self) { fixture_only(); } }
            storyhook::daemon::engine::reconcile_tick(a, b);
            ShellDispatcher :: new(script, env);
        }
        fn reconcile_tick(a: A, b: B) {}
    "##;
    assert_eq!(
        boundaries(source),
        BTreeMap::from([
            ("outer: reconcile_tick".into(), 1),
            ("outer: ShellDispatcher::new".into(), 1),
        ])
    );
}

#[test]
fn sh846_boundary_census_detects_added_removed_and_nested_calls() {
    let one = "fn fixture() { ResourceService::new(&ctx); }";
    let twice = "fn fixture() { ResourceService::new(&ctx); ResourceService::new(&ctx); }";
    let original = inventory(&BTreeMap::from([(
        "tests/nested/case.rs".into(),
        one.into(),
    )]));
    let added = inventory(&BTreeMap::from([(
        "tests/nested/case.rs".into(),
        twice.into(),
    )]));
    assert_ne!(
        original, added,
        "a second call must not inherit the first call's review"
    );
    assert_ne!(
        original,
        inventory(&BTreeMap::new()),
        "removal must retire stale review entries"
    );
    assert_eq!(
        original["tests/nested/case.rs"]["fixture: ResourceService::new"],
        1
    );
}
