//! SH-836: the subprocess bounds that `src/` holds real processes to.
//!
//! The SH-810 census (`waits.rs`) reads `tests/` and the shared test support.
//! Lib unit tests live in `src/`, and five of them went RED together in one
//! central gate because a fake `tmux` that answered at once missed the
//! production 3 s bound under load. The bound was named deep in production
//! code, where no scan of the test could see it. Council D1 on SH-836 put the
//! remedy on `Environment::subprocess_bound`, which a lib test's declared
//! policy governs, and asked for an exact census of every bound besides.
//! This module keeps that census, in `src_bounds.json`, three ways:
//!
//! - every production `run_captured*` bound that is not read through
//!   `subprocess_bound(...)` is classified: `delegated` (the caller's value)
//!   or `unrouted` (a constant, with the reason it is not routed);
//! - every production mention of a routed constant outside
//!   `subprocess_bound(...)`, its definition and `use` items is a bypass
//!   (`ref:NAME`), so a raw 3 s cannot reach a leaf unseen;
//! - every `run_captured*` bound in `src/` test code that is not graced by
//!   `load_grace` (`graced_now`, `graced_by`) or read through
//!   `subprocess_bound` is classified: `proof`, `fixture` or `delegated`.
//!
//! The bound argument's position is read from each signature in
//! `src/process.rs`. Test code is found on `waits::code_only`'s masked text,
//! which keeps byte offsets: an item is test code when a `#[cfg(...)]`
//! attribute on it compiles it only under test (`test`, or `all(...)` with
//! `test` among its arguments; `any(..., test)` and `not(test)` also compile
//! outside test). A `mod x;` declared under such an attribute makes its
//! whole file test code.
//!
//! Lexical, not a type checker, and said so: a bound reached through an alias
//! of a routed constant, or a helper that wraps a `run_captured*` call, is
//! only as visible as that helper's own call. The runtime half of the fence
//! is the panic an undeclared `Environment` raises when a lib test reads a
//! routed bound.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;
use std::path::{Path, PathBuf};

use super::waits::{code_only, expression_end};

/// The arguments of a parenthesized list, split at its top-level commas.
fn top_level_arguments(list: &str) -> Vec<&str> {
    let mut depth = 0_usize;
    let mut start = 0;
    let mut arguments = Vec::new();
    for (offset, byte) in list.bytes().enumerate() {
        match byte {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth = depth.saturating_sub(1),
            b',' if depth == 0 => {
                arguments.push(list[start..offset].trim());
                start = offset + 1;
            }
            _ => {}
        }
    }
    let last = list[start..].trim();
    if !last.is_empty() {
        arguments.push(last);
    }
    arguments
}

/// Whether a `cfg` predicate compiles its item only under test.
fn only_under_test(predicate: &str) -> bool {
    let predicate: String = predicate.split_whitespace().collect();
    if predicate == "test" {
        return true;
    }
    predicate
        .strip_prefix("all(")
        .and_then(|rest| rest.strip_suffix(')'))
        .is_some_and(|inner| top_level_arguments(inner).contains(&"test"))
}

/// The index just past the delimiter that closes the one opening at `open`.
fn past_closing(code: &[u8], open: usize) -> usize {
    let mut depth = 0_usize;
    for (index, byte) in code.iter().enumerate().skip(open) {
        match byte {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => {
                depth -= 1;
                if depth == 0 {
                    return index + 1;
                }
            }
            _ => {}
        }
    }
    code.len()
}

/// The end of the item that starts at `start`: its first top-level `;` or
/// `,`, the brace that closes its first top-level block, or the delimiter
/// that closes the list it sits in.
fn item_end(code: &[u8], start: usize) -> usize {
    let mut depth = 0_usize;
    let mut index = start;
    while index < code.len() {
        match code[index] {
            b'{' if depth == 0 => return past_closing(code, index),
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' if depth == 0 => return index,
            b')' | b']' | b'}' => depth -= 1,
            b';' | b',' if depth == 0 => return index + 1,
            _ => {}
        }
        index += 1;
    }
    code.len()
}

/// The first position at or after `from` that is not whitespace.
fn skip_whitespace(code: &str, from: usize) -> usize {
    code[from..]
        .find(|c: char| !c.is_whitespace())
        .map_or(code.len(), |offset| from + offset)
}

/// A `mod name;` declaration: the module's name.
fn declared_module(item: &str) -> Option<String> {
    let item: String = item.split_whitespace().collect::<Vec<_>>().join(" ");
    let item = item.strip_prefix("pub(crate) ").unwrap_or(&item);
    let item = item.strip_prefix("pub(super) ").unwrap_or(item);
    let item = item.strip_prefix("pub ").unwrap_or(item);
    let name = item.strip_prefix("mod ")?.strip_suffix(';')?.trim();
    name.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_')
        .then(|| name.to_string())
}

/// The quoted value of a `#[path = "..."]` attribute, read from the original
/// source because the masked text blanks string contents.
fn path_attribute(original: &str) -> Option<String> {
    let inner: String = original
        .strip_prefix("#[")?
        .strip_suffix(']')?
        .split_whitespace()
        .collect();
    let value = inner.strip_prefix("path=\"")?.strip_suffix('"')?;
    Some(value.to_string())
}

/// What one file compiles only under test.
#[derive(Debug, Default, PartialEq, Eq)]
struct TestCode {
    /// Byte ranges of test-only items, attribute included.
    regions: Vec<Range<usize>>,
    /// `mod name;` declarations among them, with any `#[path]` value.
    modules: Vec<(String, Option<String>)>,
}

/// The test-only items of one source file.
fn test_code(original: &str) -> TestCode {
    let masked = code_only(original);
    let code = masked.as_bytes();
    let mut found = TestCode::default();
    let mut search = 0;
    while let Some(offset) = masked[search..].find("#[cfg(") {
        let attribute = search + offset;
        let open = attribute + "#[cfg".len();
        let predicate_end = past_closing(code, open);
        let attribute_end = past_closing(code, attribute + 1);
        if !only_under_test(&masked[open + 1..predicate_end - 1]) {
            search = attribute_end;
            continue;
        }
        let mut start = skip_whitespace(&masked, attribute_end);
        let mut path = None;
        while masked[start..].starts_with("#[") {
            let end = past_closing(code, start + 1);
            path = path.or_else(|| path_attribute(&original[start..end]));
            start = skip_whitespace(&masked, end);
        }
        let end = item_end(code, start);
        if let Some(name) = declared_module(&masked[start..end]) {
            found.modules.push((name, path));
        }
        found.regions.push(attribute..end);
        search = end.max(attribute_end);
    }
    found
}

/// The file a `mod name;` in `declaring` loads, per the Rust reference: a
/// `#[path]` is relative to the declaring file's directory; otherwise the
/// module directory is that directory for `mod.rs`, `lib.rs` and `main.rs`,
/// and a directory named after the file for any other file.
fn module_file(root: &Path, declaring: &str, name: &str, path: Option<&str>) -> String {
    let declaring = Path::new(declaring);
    let directory = declaring.parent().unwrap_or(Path::new(""));
    if let Some(path) = path {
        return directory.join(path).to_string_lossy().into_owned();
    }
    let stem = declaring.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    let module_directory: PathBuf = if ["mod", "lib", "main"].contains(&stem) {
        directory.to_path_buf()
    } else {
        directory.join(stem)
    };
    let flat = module_directory.join(format!("{name}.rs"));
    if root.join(&flat).exists() {
        flat.to_string_lossy().into_owned()
    } else {
        module_directory
            .join(name)
            .join("mod.rs")
            .to_string_lossy()
            .into_owned()
    }
}

/// Every tracked `src/` file, and the byte ranges of each that compile only
/// under test: the whole file for a test-only module, otherwise its
/// test-only items.
struct SrcCorpus {
    sources: BTreeMap<String, String>,
    test_ranges: BTreeMap<String, Vec<Range<usize>>>,
}

impl SrcCorpus {
    fn read(root: &Path) -> Self {
        let sources = super::tracked_test_files(root, "src/*.rs");
        let mut test_files = BTreeSet::new();
        let mut test_ranges = BTreeMap::new();
        for (path, source) in &sources {
            let found = test_code(source);
            for (name, attribute) in &found.modules {
                test_files.insert(module_file(root, path, name, attribute.as_deref()));
            }
            test_ranges.insert(path.clone(), found.regions);
        }
        for file in &test_files {
            assert!(
                sources.contains_key(file),
                "a #[cfg(test)] module resolves to {file}, which is not a tracked src/ file"
            );
            let whole = 0..sources[file].len();
            test_ranges.insert(file.clone(), Vec::from([whole]));
        }
        for path in sources.keys() {
            let name = Path::new(path)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("");
            assert!(
                !(name == "tests.rs" || name.ends_with("_tests.rs")) || test_files.contains(path),
                "{path} is named as a test module but no #[cfg(test)] mod declares it, so this \
                 scan would read it as production"
            );
        }
        Self {
            sources,
            test_ranges,
        }
    }

    fn is_test(&self, path: &str, offset: usize) -> bool {
        self.test_ranges[path]
            .iter()
            .any(|range| range.contains(&offset))
    }
}

/// Where each `run_captured*` function in `process.rs` takes its bound: the
/// index of its `timeout: Duration` parameter, read from the signatures so a
/// new variant is covered without editing this scan.
fn bound_positions(process: &str) -> BTreeMap<String, usize> {
    let code = code_only(process);
    let signature =
        regex::Regex::new(r"pub\(crate\)\s+fn\s+(run_captured\w*)\s*(?:<[^>]*>)?\s*\(").unwrap();
    let mut positions = BTreeMap::new();
    for found in signature.captures_iter(&code) {
        let open = found.get(0).unwrap().end() - 1;
        let close = past_closing(code.as_bytes(), open) - 1;
        let parameters = top_level_arguments(&code[open + 1..close]);
        if let Some(index) = parameters
            .iter()
            .position(|parameter| parameter.starts_with("timeout:"))
        {
            positions.insert(found[1].to_string(), index);
        }
    }
    positions
}

/// One `run_captured*` call's bound, whitespace removed, keyed as
/// `name(bound`, with the byte offset of the call.
fn capture_bounds(source: &str, positions: &BTreeMap<String, usize>) -> Vec<(usize, String)> {
    let code = code_only(source);
    let call = regex::Regex::new(r"\b(run_captured\w*)\s*\(").unwrap();
    let mut found = Vec::new();
    for site in call.captures_iter(&code) {
        let whole = site.get(0).unwrap();
        if code[..whole.start()].trim_end().ends_with("fn") {
            continue;
        }
        let Some(&index) = positions.get(&site[1]) else {
            continue;
        };
        let mut argument = whole.end();
        for _ in 0..index {
            argument = expression_end(&code, argument) + 1;
        }
        let end = expression_end(&code, argument);
        let bound: String = code[argument..end].split_whitespace().collect();
        found.push((whole.start(), format!("{}({bound}", &site[1])));
    }
    found
}

/// Whether a bound is already governed: read through the declared policy,
/// or graced by `load_grace` in a test.
fn governed(key: &str) -> bool {
    ["subprocess_bound(", "graced_now(", "graced_by("]
        .iter()
        .any(|marker| key.contains(marker))
}

/// The constants production passes to `subprocess_bound(...)`, by their
/// last path segment.
fn routed_constants(code: &str) -> BTreeSet<String> {
    let routed =
        regex::Regex::new(r"subprocess_bound\(\s*(?:[A-Za-z_]\w*::)*([A-Z][A-Z0-9_]*)\s*\)")
            .unwrap();
    routed
        .captures_iter(code)
        .map(|found| found[1].to_string())
        .collect()
}

/// Every production mention of a routed constant outside
/// `subprocess_bound(...)`, its own definition, and `use` items.
fn bypasses(code: &str, routed: &BTreeSet<String>) -> Vec<(usize, String)> {
    let mut found = Vec::new();
    for name in routed {
        let mention = regex::Regex::new(&format!(r"\b{name}\b")).unwrap();
        for site in mention.find_iter(code) {
            let line_start = code[..site.start()].rfind('\n').map_or(0, |n| n + 1);
            let line = code[line_start..].lines().next().unwrap_or("").trim_start();
            let before = &code[..site.start()];
            let inside_routing = before
                .rfind("subprocess_bound(")
                .is_some_and(|open| !before[open..].contains(')'));
            let definition = line.contains(&format!("const {name}"));
            let import = line.starts_with("use ") || line.starts_with("pub use ");
            if !(inside_routing || definition || import) {
                found.push((site.start(), format!("ref:{name}")));
            }
        }
    }
    found
}

/// One file's findings, counted by key.
type Census = BTreeMap<String, BTreeMap<String, usize>>;

/// Production and test findings across `src/`: production capture bounds
/// and bypasses of routed constants, and test capture bounds, each ungoverned.
fn src_census(root: &Path) -> Census {
    let corpus = SrcCorpus::read(root);
    let positions = bound_positions(&corpus.sources["src/process.rs"]);
    assert!(
        positions.get("run_captured") == Some(&1) && positions.len() >= 10,
        "the run_captured* signatures were not read from src/process.rs: {positions:?}"
    );
    let mut production_code = String::new();
    for (path, source) in &corpus.sources {
        let code = code_only(source);
        let mut masked = code.into_bytes();
        for range in &corpus.test_ranges[path] {
            for byte in &mut masked[range.clone()] {
                if *byte != b'\n' {
                    *byte = b' ';
                }
            }
        }
        production_code.push_str(&String::from_utf8(masked).unwrap());
        production_code.push('\n');
    }
    let routed = routed_constants(&production_code);
    assert!(
        routed.contains("TMUX_TIMEOUT"),
        "no production site routes TMUX_TIMEOUT; this scan proved nothing: {routed:?}"
    );
    let mut census = Census::new();
    for (path, source) in &corpus.sources {
        let mut sites = capture_bounds(source, &positions);
        let code = code_only(source);
        sites.extend(
            bypasses(&code, &routed)
                .into_iter()
                .filter(|(offset, _)| !corpus.is_test(path, *offset)),
        );
        for (offset, key) in sites {
            if governed(&key) {
                continue;
            }
            let scope = if corpus.is_test(path, offset) {
                "test"
            } else {
                "production"
            };
            *census
                .entry(path.clone())
                .or_default()
                .entry(format!("{scope} {key}"))
                .or_default() += 1;
        }
    }
    census
}

#[test]
fn every_ungoverned_src_subprocess_bound_has_an_exact_classification() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let actual = src_census(root);
    let reviewed: BTreeMap<String, BTreeMap<String, serde_json::Value>> = serde_json::from_str(
        &std::fs::read_to_string(root.join("tests/timing_assertions/src_bounds.json")).unwrap(),
    )
    .unwrap();
    let expected: Census = reviewed
        .into_iter()
        .map(|(path, entries)| {
            let sites = entries
                .into_iter()
                .map(|(key, entry)| {
                    let kind = entry["kind"].as_str().expect("classification kind");
                    let allowed: &[&str] = if key.starts_with("test ") {
                        &["proof", "fixture", "delegated"]
                    } else {
                        &["delegated", "unrouted"]
                    };
                    assert!(allowed.contains(&kind), "{path}: {key}: {kind}");
                    assert!(
                        !entry["reason"]
                            .as_str()
                            .expect("classification reason")
                            .trim()
                            .is_empty(),
                        "{path}: {key}: empty reason"
                    );
                    let count = entry["count"].as_u64().expect("occurrence count") as usize;
                    assert!(count > 0);
                    (key, count)
                })
                .collect();
            (path, sites)
        })
        .collect();
    assert_eq!(
        actual, expected,
        "new, changed or stale subprocess bounds in src/ (SH-836): read a production bound \
         through Environment::subprocess_bound, grace a test's patience with load_grace, or \
         classify the site in tests/timing_assertions/src_bounds.json"
    );
}

#[test]
fn a_cfg_predicate_is_test_only_when_nothing_else_compiles_it() {
    for predicate in [
        "test",
        " test ",
        "all(test, unix)",
        "all(unix,test)",
        "all( test )",
    ] {
        assert!(only_under_test(predicate), "{predicate}");
    }
    for predicate in [
        "not(test)",
        "any(target_os = \"linux\", test)",
        "unix",
        "all(unix, not(test))",
    ] {
        assert!(!only_under_test(predicate), "{predicate}");
    }
}

#[test]
fn test_items_end_where_the_item_ends_and_never_swallow_production() {
    let attribute = "#[cfg(test)]";
    let source = format!(
        "{attribute}\nuse crate::a::{{b, c}};\nuse crate::process::run_captured;\n\
         {attribute}\n#[path = \"named_tests.rs\"]\nmod tests;\n\
         {attribute}\nconst WAIT: Duration = Duration::from_secs(1);\nfn production() {{}}\n\
         {attribute}\nthread_local! {{ static X: u8 = const {{ 0 }}; }}\n\
         #[cfg(any(unix, test))]\nfn also_production() {{}}\n\
         #[cfg(all(test, unix))]\nmod inline {{ fn a() {{ let c = '{{'; }} }}\nfn tail() {{}}\n"
    );
    let found = test_code(&source);
    let texts: Vec<&str> = found.regions.iter().map(|r| &source[r.clone()]).collect();
    assert_eq!(texts.len(), 5, "{texts:#?}");
    assert!(texts[0].ends_with("use crate::a::{b, c}"), "{}", texts[0]);
    assert!(texts[1].ends_with("mod tests;"), "{}", texts[1]);
    assert!(texts[2].ends_with("from_secs(1);"), "{}", texts[2]);
    assert!(texts[3].ends_with("}; }"), "{}", texts[3]);
    assert!(texts[4].ends_with("} }"), "{}", texts[4]);
    for text in &texts {
        for production in [
            "run_captured;",
            "fn production",
            "fn also_production",
            "fn tail",
        ] {
            assert!(
                !text.contains(production),
                "{production} swallowed by {text}"
            );
        }
    }
    assert_eq!(
        found.modules,
        [("tests".to_string(), Some("named_tests.rs".to_string()))]
    );
}

#[test]
fn module_files_follow_the_reference_rules() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    assert_eq!(
        module_file(
            root,
            "src/daemon/activity/hygiene.rs",
            "tests",
            Some("hygiene_tests.rs")
        ),
        "src/daemon/activity/hygiene_tests.rs"
    );
    assert_eq!(
        module_file(
            root,
            "src/daemon/activity/mod.rs",
            "isolation_tests",
            Some("tests.rs")
        ),
        "src/daemon/activity/tests.rs"
    );
    assert_eq!(
        module_file(root, "src/service/engine.rs", "restart_probe_tests", None),
        "src/service/engine/restart_probe_tests.rs"
    );
}

#[test]
fn bound_positions_are_read_from_each_signature() {
    let process = "pub(crate) fn run_captured(command: Command, timeout: Duration) -> R {}\n\
                   pub(crate) fn run_captured_with_input(\n    command: Command,\n    input: File,\n    timeout: Duration,\n) -> R {}\n\
                   pub(crate) fn run_captured_with_registration<G>(\n    command: Command,\n    timeout: Duration,\n    register: impl FnOnce(u32) -> Result<G, String>,\n) -> R {}\n\
                   pub(crate) fn run_captured_until(command: Command, deadline: impl Fn() -> D) -> R {}";
    let positions = bound_positions(process);
    assert_eq!(positions.get("run_captured"), Some(&1));
    assert_eq!(positions.get("run_captured_with_input"), Some(&2));
    assert_eq!(positions.get("run_captured_with_registration"), Some(&1));
    assert!(
        !positions.contains_key("run_captured_until"),
        "{positions:?}"
    );
}

#[test]
fn capture_bounds_find_calls_not_definitions_across_lines() {
    let positions = BTreeMap::from([
        ("run_captured".to_string(), 1),
        ("run_captured_with_input".to_string(), 2),
    ]);
    let source = "pub(crate) fn run_captured(command: Command, timeout: Duration) {}\n\
                  let a = run_captured(build(x, y), BOUND);\n\
                  let b = crate::process::run_captured_with_input(\n    cmd,\n    file,\n    graced_now(Duration::from_secs(5)),\n);\n\
                  // run_captured(ignored, IN_A_COMMENT);\n\
                  let c = \"run_captured(ignored, IN_A_STRING)\";";
    let keys: Vec<String> = capture_bounds(source, &positions)
        .into_iter()
        .map(|(_, key)| key)
        .collect();
    assert_eq!(
        keys,
        [
            "run_captured(BOUND",
            "run_captured_with_input(graced_now(Duration::from_secs(5))"
        ]
    );
    assert!(!governed(&keys[0]));
    assert!(governed(&keys[1]));
    assert!(governed("run_captured(env.subprocess_bound(TMUX_TIMEOUT)"));
    assert!(governed(
        "run_captured(load_grace::graced_by(BASE, reading)"
    ));
}

#[test]
fn a_routed_constant_named_outside_its_routing_is_a_bypass() {
    let code = "use crate::service::engine::TMUX_TIMEOUT;\n\
                pub const TMUX_TIMEOUT: Duration = TAILNET;\n\
                run_captured(c, env.subprocess_bound(crate::service::engine::TMUX_TIMEOUT));\n\
                let deadline = Instant::now() + TMUX_TIMEOUT;\n\
                census_through(c, TMUX_TIMEOUT_EXTRA);";
    let routed = routed_constants(code);
    assert_eq!(routed, BTreeSet::from(["TMUX_TIMEOUT".to_string()]));
    let found: Vec<String> = bypasses(code, &routed)
        .into_iter()
        .map(|(_, k)| k)
        .collect();
    assert_eq!(
        found,
        ["ref:TMUX_TIMEOUT"],
        "only the raw deadline is a bypass"
    );
}
