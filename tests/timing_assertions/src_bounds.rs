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
//! whole file and its declared descendants test code, including declarations
//! nested inside inline modules. A file also loaded by a production module is
//! ambiguous and refused rather than silently excluded from production scans.
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
    // Generic return types can contain a top-level comma before the function
    // body (Result<T, E>). A function item ends at its body or semicolon.
    let prefix = String::from_utf8_lossy(&code[start..]);
    let function =
        regex::Regex::new(r"^(?:pub(?:\([^)]*\))?\s+)?(?:(?:async|unsafe|const)\s+)*fn\b")
            .unwrap()
            .is_match(&prefix);
    while index < code.len() {
        match code[index] {
            b'{' if depth == 0 => return past_closing(code, index),
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' if depth == 0 => return index,
            b')' | b']' | b'}' => depth -= 1,
            b';' if depth == 0 => return index + 1,
            b',' if depth == 0 && !function => return index + 1,
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

/// What one file compiles only under test.
#[derive(Debug, Default, PartialEq, Eq)]
struct TestCode {
    /// Byte ranges of test-only items, attribute included.
    regions: Vec<Range<usize>>,
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
        // Include adjacent attributes on either side of #[cfg(test)], but
        // never borrow attributes from another item.
        let mut region_start = attribute;
        while masked[..region_start].trim_end().ends_with(']') {
            let previous_end = masked[..region_start].trim_end().len();
            let Some(previous) = masked[..previous_end].rfind("#[") else {
                break;
            };
            if past_closing(code, previous + 1) != previous_end {
                break;
            }
            region_start = previous;
        }
        let mut start = skip_whitespace(&masked, attribute_end);
        while masked[start..].starts_with("#[") {
            let end = past_closing(code, start + 1);
            start = skip_whitespace(&masked, end);
        }
        let end = item_end(code, start);
        found.regions.push(region_start..end);
        search = end.max(attribute_end);
    }
    found
}

/// External module edges, including declarations inside inline modules. Keep
/// their test condition separate: an ordinary edge becomes test-only only
/// when its declaring file is itself reached through a test-only module.
fn module_edges(
    declaring: &str,
    source: &str,
    sources: &BTreeMap<String, String>,
) -> Vec<(String, bool)> {
    fn conditional_path(meta: &syn::Meta) -> bool {
        let syn::Meta::List(list) = meta else {
            return false;
        };
        if !list.path.is_ident("cfg_attr") {
            return false;
        }
        let arguments = list
            .parse_args_with(
                syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated,
            )
            .expect("cannot inspect conditional module attributes");
        arguments
            .iter()
            .skip(1)
            .any(|attribute| attribute.path().is_ident("path") || conditional_path(attribute))
    }

    fn normalized(path: PathBuf) -> String {
        let mut result = PathBuf::new();
        for component in path.components() {
            match component {
                std::path::Component::CurDir => {}
                std::path::Component::ParentDir => {
                    assert!(result.pop(), "module path escapes the source corpus");
                }
                other => result.push(other.as_os_str()),
            }
        }
        assert!(
            result.starts_with("src"),
            "module path escapes the source corpus"
        );
        result.to_string_lossy().into_owned()
    }

    fn visit(
        items: &[syn::Item],
        directory: &Path,
        path_directory: &Path,
        inherited_test: bool,
        sources: &BTreeMap<String, String>,
        edges: &mut Vec<(String, bool)>,
    ) {
        for item in items {
            let syn::Item::Mod(module) = item else {
                continue;
            };
            // A conditional redirect can add a production route into the
            // test closure. Inline redirects also change their children's
            // directory, so refuse both until every path can be resolved.
            assert!(
                !module
                    .attrs
                    .iter()
                    .any(|attribute| conditional_path(&attribute.meta)),
                "cannot establish module path for {}: conditional path attribute",
                module.ident
            );
            let test_only = inherited_test
                || module.attrs.iter().any(|attribute| {
                    matches!(&attribute.meta, syn::Meta::List(list)
                        if list.path.is_ident("cfg") && only_under_test(&list.tokens.to_string()))
                });
            let explicit_path = module.attrs.iter().find_map(|attribute| {
                let syn::Meta::NameValue(value) = &attribute.meta else {
                    return None;
                };
                if !value.path.is_ident("path") {
                    return None;
                }
                match &value.value {
                    syn::Expr::Lit(syn::ExprLit {
                        lit: syn::Lit::Str(path),
                        ..
                    }) => {
                        let value = path.value();
                        assert!(
                            !Path::new(&value).is_absolute(),
                            "cannot establish module path for {}: absolute path attribute",
                            module.ident
                        );
                        Some(value)
                    }
                    _ => None,
                }
            });
            let name = module.ident.to_string();
            if let Some((_, items)) = &module.content {
                // Inside an inline module, even #[path] is relative to the
                // enclosing module directory (including a non-mod.rs stem).
                let nested = PathBuf::from(normalized(
                    directory.join(explicit_path.as_deref().unwrap_or(&name)),
                ));
                visit(items, &nested, &nested, test_only, sources, edges);
            } else {
                let target = if let Some(path) = explicit_path {
                    normalized(path_directory.join(path))
                } else {
                    let flat = normalized(directory.join(format!("{name}.rs")));
                    if sources.contains_key(&flat) {
                        flat
                    } else {
                        normalized(directory.join(name).join("mod.rs"))
                    }
                };
                edges.push((target, test_only));
            }
        }
    }

    let file = syn::parse_file(source).unwrap_or_else(|error| {
        panic!("cannot inspect module declarations in {declaring}: {error}")
    });
    let path = Path::new(declaring);
    let parent = path.parent().unwrap_or(Path::new(""));
    let stem = path.file_stem().unwrap().to_str().unwrap();
    let directory = if ["mod", "lib", "main"].contains(&stem) {
        parent.to_path_buf()
    } else {
        parent.join(stem)
    };
    let mut edges = Vec::new();
    visit(&file.items, &directory, parent, false, sources, &mut edges);
    edges
}

/// Every tracked `src/` file, and the byte ranges of each that compile only
/// under test: the whole file for a test-only module, otherwise its
/// test-only items.
pub(super) struct SrcCorpus {
    sources: BTreeMap<String, String>,
    test_ranges: BTreeMap<String, Vec<Range<usize>>>,
}

impl SrcCorpus {
    pub(super) fn read(root: &Path) -> Self {
        Self::from_sources(root, super::tracked_test_files(root, "src/*.rs"))
    }

    pub(super) fn from_sources(_root: &Path, sources: BTreeMap<String, String>) -> Self {
        let mut test_files = BTreeSet::new();
        let mut test_ranges = BTreeMap::new();
        let mut edges = BTreeMap::new();
        for (path, source) in &sources {
            let found = test_code(source);
            let modules = module_edges(path, source, &sources);
            for (file, test_only) in &modules {
                if *test_only {
                    test_files.insert(file.clone());
                }
            }
            edges.insert(path.clone(), modules);
            test_ranges.insert(path.clone(), found.regions);
        }
        let mut pending: Vec<_> = test_files.iter().cloned().collect();
        while let Some(file) = pending.pop() {
            assert!(
                sources.contains_key(&file),
                "a test-only module resolves to {file}, which is not a tracked src/ file"
            );
            for (child, _) in &edges[&file] {
                if test_files.insert(child.clone()) {
                    pending.push(child.clone());
                }
            }
        }
        // A test edge does not make a file exclusively test-owned when another
        // module loads that same path in production. Every production route
        // into this closure crosses an ordinary edge from outside it; refuse
        // that ambiguity before masking any whole file as test-only.
        for (parent, modules) in &edges {
            if test_files.contains(parent) {
                continue;
            }
            for (child, test_only) in modules {
                assert!(
                    *test_only || !test_files.contains(child),
                    "{child} is reachable through both test-only and production modules \
                     (production declaration in {parent})"
                );
            }
        }
        for file in &test_files {
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

    /// Original test-only text; separating regions prevents tokens on either
    /// side of a production item from becoming a synthetic call.
    pub(super) fn test_sources(&self) -> BTreeMap<String, String> {
        self.sources
            .iter()
            .filter(|(path, _)| !self.test_ranges[*path].is_empty())
            .map(|(path, source)| {
                (
                    path.clone(),
                    self.test_ranges[path]
                        .iter()
                        .map(|range| &source[range.clone()])
                        .collect::<Vec<_>>()
                        .join("\n;\n"),
                )
            })
            .collect()
    }

    fn is_test(&self, path: &str, offset: usize) -> bool {
        self.test_ranges[path]
            .iter()
            .any(|range| range.contains(&offset))
    }
}

/// Where each `run_captured*` function in `process.rs` takes its bound: the
/// index of its `timeout: Duration` or `deadline: Instant` parameter, read from the signatures so a
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
        if let Some(index) = parameters.iter().position(|parameter| {
            let compact: String = parameter.split_whitespace().collect();
            compact == "timeout:Duration"
                || matches!(
                    compact.as_str(),
                    "deadline:Instant" | "deadline:std::time::Instant"
                )
        }) {
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

fn assert_exact_src_bounds() {
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
fn every_ungoverned_src_subprocess_bound_has_an_exact_classification() {
    assert_exact_src_bounds();
}

#[test]
fn sh846_routed_and_retained_capture_inventory_matches_exactly() {
    assert_exact_src_bounds();
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
}

#[test]
fn test_only_file_ownership_reaches_nested_modules_without_promoting_siblings() {
    let sources = BTreeMap::from([
        (
            "src/lib.rs".into(),
            "#[cfg(test)] mod tests; mod production; #[cfg(any(unix, test))] mod shared;".into(),
        ),
        ("src/tests.rs".into(), "mod owner;".into()),
        (
            "src/tests/owner.rs".into(),
            "pub(super) mod workflow;".into(),
        ),
        (
            "src/tests/owner/workflow.rs".into(),
            "fn proof() { run_captured_query_quiescent(command, deadline); }".into(),
        ),
        ("src/production.rs".into(), "fn production() {}".into()),
        ("src/shared.rs".into(), "fn shared() {}".into()),
    ]);
    let corpus = SrcCorpus::from_sources(Path::new("."), sources);
    let tests = corpus.test_sources();
    for path in [
        "src/tests.rs",
        "src/tests/owner.rs",
        "src/tests/owner/workflow.rs",
    ] {
        assert_eq!(tests[path], corpus.sources[path], "{path}");
        assert!(corpus.is_test(path, 0), "{path}");
    }
    for path in ["src/production.rs", "src/shared.rs"] {
        assert!(!tests.contains_key(path), "{path}");
        assert!(!corpus.is_test(path, 0), "{path}");
    }
    let production = corpus.sources["src/lib.rs"].find("mod production").unwrap();
    assert!(!corpus.is_test("src/lib.rs", production));
}

#[test]
#[should_panic(
    expected = "src/tests/shared.rs is reachable through both test-only and production modules"
)]
fn a_production_alias_to_a_test_descendant_is_not_silently_test_only() {
    SrcCorpus::from_sources(
        Path::new("."),
        BTreeMap::from([
            (
                "src/lib.rs".into(),
                "#[cfg(test)] mod tests; #[path = \"tests/shared.rs\"] mod production;".into(),
            ),
            ("src/tests.rs".into(), "mod shared;".into()),
            (
                "src/tests/shared.rs".into(),
                "fn production() { run_captured(command, deadline); }".into(),
            ),
        ]),
    );
}

#[test]
#[should_panic(
    expected = "cannot establish module path for production: conditional path attribute"
)]
fn a_conditional_production_alias_to_a_test_descendant_is_refused() {
    SrcCorpus::from_sources(
        Path::new("."),
        BTreeMap::from([
            (
                "src/lib.rs".into(),
                "#[cfg(test)] mod tests; \
                 #[cfg_attr(unix, path = \"tests/shared.rs\")] mod production;"
                    .into(),
            ),
            ("src/tests.rs".into(), "mod shared;".into()),
            ("src/tests/shared.rs".into(), "fn shared() {}".into()),
            ("src/production.rs".into(), "fn fallback() {}".into()),
        ]),
    );
}

#[test]
#[should_panic(expected = "cannot establish module path for production: absolute path attribute")]
fn an_absolute_production_alias_to_a_test_descendant_is_refused() {
    SrcCorpus::from_sources(
        Path::new("/checkout"),
        BTreeMap::from([
            (
                "src/lib.rs".into(),
                "#[cfg(test)] mod tests; \
                 #[path = \"/checkout/src/tests/shared.rs\"] mod production;"
                    .into(),
            ),
            ("src/tests.rs".into(), "mod shared;".into()),
            ("src/tests/shared.rs".into(), "fn shared() {}".into()),
        ]),
    );
}

#[test]
#[should_panic(expected = "cannot establish module path for production: absolute path attribute")]
fn an_absolute_inline_module_directory_is_refused() {
    module_edges(
        "src/lib.rs",
        "#[path = \"/checkout/src/tests\"] mod production { mod shared; }",
        &BTreeMap::new(),
    );
}

#[test]
fn module_parent_paths_cannot_escape_the_source_corpus() {
    for source in [
        "#[path = \"../outside.rs\"] mod production;",
        "#[path = \"../../outside.rs\"] mod production;",
        "#[path = \"../outside\"] mod production {}",
        "#[path = \"../../outside\"] mod production {}",
    ] {
        let failure =
            std::panic::catch_unwind(|| module_edges("src/lib.rs", source, &BTreeMap::new()))
                .expect_err("a module path outside src/ must be refused");
        let message = failure
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| failure.downcast_ref::<&str>().copied())
            .expect("the refusal must explain the path ambiguity");
        assert!(
            message.contains("module path escapes the source corpus"),
            "{message}"
        );
    }
}

#[test]
fn test_module_edges_follow_inline_paths_and_directory_modules() {
    let sources = BTreeMap::from([
        (
            "src/feature.rs".into(),
            "#[cfg(test)] #[path = \"special\"] mod checks { mod ordinary; \
             #[path = \"selected/../renamed.rs\"] mod leaf; } \
             mod production { #[cfg(test)] mod child; mod sibling; }"
                .into(),
        ),
        (
            "src/feature/special/ordinary/mod.rs".into(),
            "mod descendant;".into(),
        ),
        (
            "src/feature/special/ordinary/descendant.rs".into(),
            "fn proof() {}".into(),
        ),
        (
            "src/feature/special/renamed.rs".into(),
            "fn proof() {}".into(),
        ),
        (
            "src/feature/production/child.rs".into(),
            "fn proof() {}".into(),
        ),
        (
            "src/feature/production/sibling.rs".into(),
            "fn production() {}".into(),
        ),
    ]);
    let corpus = SrcCorpus::from_sources(Path::new("."), sources);
    for path in [
        "src/feature/special/ordinary/mod.rs",
        "src/feature/special/ordinary/descendant.rs",
        "src/feature/special/renamed.rs",
        "src/feature/production/child.rs",
    ] {
        assert!(corpus.is_test(path, 0), "{path}");
    }
    assert!(!corpus.is_test("src/feature/production/sibling.rs", 0));
}

#[test]
fn explicit_test_module_paths_propagate_from_the_loaded_file() {
    let sources = BTreeMap::from([
        (
            "src/feature.rs".into(),
            "#[path = \"alternate.rs\"] #[cfg(all(unix, test))] mod checks;".into(),
        ),
        ("src/alternate.rs".into(), "mod child;".into()),
        ("src/alternate/child.rs".into(), "fn proof() {}".into()),
    ]);
    let corpus = SrcCorpus::from_sources(Path::new("."), sources);
    assert!(corpus.is_test("src/alternate.rs", 0));
    assert!(corpus.is_test("src/alternate/child.rs", 0));
}

#[test]
#[should_panic(expected = "not a tracked src/ file")]
fn a_missing_descendant_of_a_test_only_module_is_not_silently_ignored() {
    SrcCorpus::from_sources(
        Path::new("."),
        BTreeMap::from([
            ("src/lib.rs".into(), "#[cfg(test)] mod tests;".into()),
            ("src/tests.rs".into(), "mod missing;".into()),
        ]),
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
fn sh871_absolute_deadline_capture_boundaries_are_not_invisible() {
    let signatures = "pub(crate) fn run_captured_absolute(cmd: Command, deadline: Instant) {}\n\
        pub(crate) fn run_captured_input(cmd: Command, input: File, deadline: std::time::Instant) {}\n\
        pub(crate) fn run_captured_callback(cmd: Command, deadline: impl Fn() -> D) {}";
    let positions = bound_positions(signatures);
    assert_eq!(positions.get("run_captured_absolute"), Some(&1));
    assert_eq!(positions.get("run_captured_input"), Some(&2));
    assert!(!positions.contains_key("run_captured_callback"));
    let calls = "run_captured_absolute(cmd, owner.deadline);\n\
        run_captured_input(cmd, file, Instant::now() + Duration::from_secs(30));";
    let keys: Vec<_> = capture_bounds(calls, &positions)
        .into_iter()
        .map(|(_, key)| key)
        .collect();
    assert_eq!(
        keys,
        [
            "run_captured_absolute(owner.deadline",
            "run_captured_input(Instant::now()+Duration::from_secs(30)"
        ]
    );
    assert!(
        keys.iter().all(|key| !governed(key)),
        "renewed absolute deadlines must require explicit census review"
    );
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
