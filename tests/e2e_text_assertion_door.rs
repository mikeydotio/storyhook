//! A wiring fence around the browser suite's `expect` (SH-622).
//!
//! `e2e/specs/support.ts` exports an `expect` whose `toHaveText` and
//! `toContainText` refuse an assertion that rides an `aria-hidden` glyph --
//! text `textContent` carries and the accessible name excludes. SH-620 put a
//! decorative emoji inside every dashboard control that has one, and four
//! specs asserting a control's own words through `toHaveText` read the
//! decoration too; the sweep that updated the specs whose subject IS an icon
//! could not see them, and nothing static can, since a third of the suite's
//! text assertions target a bare local variable. The rule therefore runs at
//! assertion time, on the elements the locator actually resolved to, and
//! `e2e/specs/text-assertion-door.spec.ts` is what executes it.
//!
//! **This is a wiring fence, and calling it anything more would be the
//! SH-360 mistake.** It proves three lexical facts, in the Rust suite, on
//! every merge: only `support.ts` takes Playwright's own `expect`; the
//! `expect` it exports is the guarded one, carrying both text matchers; and
//! every spec that asserts text takes its `expect` from `support.ts`. Only a
//! browser can prove the door refuses -- and a spec that bypassed the door
//! by importing `@playwright/test` directly would pass every browser test
//! while the fence sat inert, which is exactly the shape SH-531 closed for
//! `story_binary()`: the correct door has to be the only door.
//!
//! Derived over `git ls-files`, never a hand-kept list (SH-136, SH-198,
//! SH-258, SH-260/276, SH-360), with the parsing primitives unit-tested in
//! both directions and their offenders assembled at run time, so no literal
//! in this file can trip the scan it exists to run.

use std::path::{Path, PathBuf};

/// The one file allowed to import Playwright's own `expect`.
const DOOR: &str = "e2e/specs/support.ts";

/// Where the door's `expect` comes from.
const PLAYWRIGHT: &str = "@playwright/test";

/// Below this many text-asserting specs, the pathspec or the matcher scan has
/// drifted and the fence is clearing a tree it never read. Measured at 78
/// when this was written.
const TEXT_ASSERTING_SPEC_FLOOR: usize = 50;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Every tracked TypeScript file under `e2e/`, paired with its contents.
/// Git's default pathspec `*` crosses directory boundaries, so this reaches
/// `e2e/specs/` as well as the config, the launch probe and the reporters.
fn all_e2e_sources(root: &Path) -> Vec<(String, String)> {
    let listed = std::process::Command::new("git")
        .current_dir(root)
        .args(["ls-files", "-z", "--", "e2e/*.ts"])
        .output()
        .expect("listing this repository's tracked browser-suite sources");
    assert!(
        listed.status.success(),
        "`git ls-files` failed, so this scan proved nothing: {}",
        String::from_utf8_lossy(&listed.stderr)
    );

    listed
        .stdout
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
        .map(|path| {
            let relative = std::str::from_utf8(path).expect("a UTF-8 path").to_string();
            let text = std::fs::read_to_string(root.join(&relative))
                .unwrap_or_else(|e| panic!("reading {relative}: {e}"));
            (relative, text)
        })
        .collect()
}

/// Blanks JavaScript comments while preserving line numbers -- the same
/// string-unaware stripper `tests/dashboard_dom_removal.rs` carries, copied
/// rather than shared because each `tests/*.rs` file compiles as its own
/// crate. The real-file assertions below are the positive control that it
/// still sees the code it polices.
fn strip_comments(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let chars: Vec<char> = source.chars().collect();
    let mut index = 0;
    let mut in_block = false;
    let mut in_line = false;

    while index < chars.len() {
        let current = chars[index];
        let next = chars.get(index + 1).copied().unwrap_or('\0');
        if current == '\n' {
            in_line = false;
            out.push('\n');
            index += 1;
            continue;
        }
        if in_block {
            if current == '*' && next == '/' {
                in_block = false;
                out.push(' ');
                out.push(' ');
                index += 2;
            } else {
                out.push(' ');
                index += 1;
            }
            continue;
        }
        if in_line {
            out.push(' ');
            index += 1;
            continue;
        }
        if current == '/' && next == '*' {
            in_block = true;
            out.push(' ');
            out.push(' ');
            index += 2;
            continue;
        }
        if current == '/' && next == '/' {
            in_line = true;
            out.push(' ');
            out.push(' ');
            index += 2;
            continue;
        }
        out.push(current);
        index += 1;
    }

    out
}

/// One `import` statement, as this scan reads it.
#[derive(Debug, PartialEq)]
struct Import {
    /// The module specifier, unquoted.
    from: String,
    /// `import type { … }` -- erased at run time, so it can carry no value.
    type_only: bool,
    /// The named bindings, each trimmed: `expect`, `expect as baseExpect`,
    /// `type Locator`.
    named: Vec<String>,
    /// `import * as NAME` -- the whole module under one binding.
    namespace: Option<String>,
}

/// Every `import … from "…"` statement in comment-stripped source. An
/// `import` that is not at the start of a statement (`export { x } from`
/// re-exports are not imports; the word inside a string is not one either)
/// is ignored, and each statement runs to its own `;`.
fn imports(stripped: &str) -> Vec<Import> {
    let mut found = Vec::new();
    for (start, _) in stripped.match_indices("import") {
        let preceded_by_boundary = stripped[..start]
            .chars()
            .next_back()
            .is_none_or(|c| c.is_whitespace() || c == ';' || c == '}');
        if !preceded_by_boundary {
            continue;
        }
        let rest = &stripped[start + "import".len()..];
        if !rest.starts_with(|c: char| c.is_whitespace() || c == '{' || c == '*') {
            continue;
        }
        let statement = &rest[..rest.find(';').unwrap_or(rest.len())];
        let Some((bindings, source)) = statement.rsplit_once(" from ") else {
            continue;
        };
        let from = source
            .trim()
            .trim_matches(|c| c == '"' || c == '\'')
            .to_string();
        let bindings = bindings.trim();
        let (type_only, bindings) = match bindings.strip_prefix("type ") {
            Some(rest) => (true, rest.trim()),
            None => (false, bindings),
        };
        let namespace = bindings
            .strip_prefix("* as ")
            .map(|name| name.trim().to_string());
        let named = match (bindings.find('{'), bindings.rfind('}')) {
            (Some(open), Some(close)) if open < close => bindings[open + 1..close]
                .split(',')
                .map(|s| s.split_whitespace().collect::<Vec<_>>().join(" "))
                .filter(|s| !s.is_empty())
                .collect(),
            _ => Vec::new(),
        };
        found.push(Import {
            from,
            type_only,
            named,
            namespace,
        });
    }
    found
}

/// Whether a named binding is Playwright's `expect` as a run-time value,
/// under any local name. `type expect` would be a type-only inline binding
/// and can carry no value; nothing in the tree writes it, and it is skipped
/// for the same reason `import type` is.
fn binds_expect(named: &str) -> bool {
    let mut words = named.split(' ');
    match (words.next(), words.next()) {
        (Some("type"), _) => false,
        (Some("expect"), None) => true,
        (Some("expect"), Some("as")) => true,
        _ => false,
    }
}

/// How a file reaches Playwright's own `expect` value, if it does: the named
/// binding, or a namespace binding it then dereferences as `NAME.expect`.
fn playwright_expect_bindings(stripped: &str) -> Vec<String> {
    let mut bindings = Vec::new();
    for import in imports(stripped) {
        if import.from != PLAYWRIGHT || import.type_only {
            continue;
        }
        bindings.extend(import.named.iter().filter(|n| binds_expect(n)).cloned());
        if let Some(name) = import.namespace
            && stripped.contains(&format!("{name}.expect"))
        {
            bindings.push(format!("* as {name} (used as {name}.expect)"));
        }
    }
    bindings
}

/// Whether a spec's `expect` comes from the door.
fn takes_expect_from_support(stripped: &str) -> bool {
    imports(stripped)
        .iter()
        .any(|i| i.from == "./support" && !i.type_only && i.named.iter().any(|n| n == "expect"))
}

/// Whether a file asserts text at all.
fn asserts_text(stripped: &str) -> bool {
    stripped.contains(".toHaveText(") || stripped.contains(".toContainText(")
}

/// The argument block of `baseExpect.extend({ … })` in the door, brace-matched
/// from its opening `{`, so a matcher defined elsewhere in the file and never
/// registered cannot satisfy the shape.
fn door_matchers(stripped: &str) -> Option<&str> {
    let signature = "export const expect = baseExpect.extend(";
    let start = stripped.find(signature)? + signature.len();
    let open = start + stripped[start..].find('{')?;
    let mut depth = 0usize;
    for (offset, c) in stripped[open..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&stripped[open..=open + offset]);
                }
            }
            _ => {}
        }
    }
    None
}

#[test]
fn only_the_shared_helper_takes_playwrights_own_expect() {
    let root = repo_root();
    let sources = all_e2e_sources(&root);
    assert!(
        sources.iter().any(|(path, _)| path == DOOR),
        "{DOOR} is not among the tracked `e2e/*.ts` files, so this scan never read the door"
    );

    let bypasses: Vec<String> = sources
        .iter()
        .filter(|(path, _)| path != DOOR)
        .flat_map(|(path, text)| {
            playwright_expect_bindings(&strip_comments(text))
                .into_iter()
                .map(move |binding| format!("  {path}: `{binding}` from {PLAYWRIGHT:?}"))
        })
        .collect();
    assert!(
        bypasses.is_empty(),
        "these files take Playwright's own `expect`, which walks past SH-622's door \
         (`toHaveText`/`toContainText` refusing an aria-hidden glyph). Import `expect` \
         from ./support instead:\n{}",
        bypasses.join("\n")
    );
}

#[test]
fn the_door_exports_the_guarded_expect_and_never_the_bare_one() {
    let stripped = strip_comments(&std::fs::read_to_string(repo_root().join(DOOR)).unwrap());

    let own = playwright_expect_bindings(&stripped);
    assert_eq!(
        own,
        vec!["expect as baseExpect".to_string()],
        "{DOOR} must take Playwright's `expect` under the name `baseExpect` and no other, \
         so the bare one can never be re-exported by accident"
    );
    assert!(
        !stripped.contains("export { expect }") && !stripped.contains("export {expect}"),
        "{DOOR} re-exports Playwright's bare `expect`, which is the pre-SH-622 shape: \
         every spec's text assertion would walk past the door"
    );

    let matchers = door_matchers(&stripped).unwrap_or_else(|| {
        panic!("{DOOR} no longer defines `export const expect = baseExpect.extend({{ … }})`")
    });
    for matcher in ["toHaveText", "toContainText"] {
        assert!(
            matchers.contains(&format!("async {matcher}(")),
            "{DOOR}'s guarded `expect` no longer registers `{matcher}`; a text assertion \
             through that matcher would reach Playwright's own, unguarded"
        );
    }
}

#[test]
fn every_spec_that_asserts_text_takes_expect_from_the_door() {
    let root = repo_root();
    let specs: Vec<(String, String)> = all_e2e_sources(&root)
        .into_iter()
        .filter(|(path, _)| path.ends_with(".spec.ts"))
        .map(|(path, text)| (path, strip_comments(&text)))
        .collect();

    let asserting: Vec<&(String, String)> = specs
        .iter()
        .filter(|(_, text)| asserts_text(text))
        .collect();
    assert!(
        asserting.len() >= TEXT_ASSERTING_SPEC_FLOOR,
        "only {} tracked specs assert text, below the floor of {TEXT_ASSERTING_SPEC_FLOOR}: \
         either the suite shrank dramatically or the pathspec/matcher scan drifted, in \
         which case this fence is clearing a tree it never read",
        asserting.len()
    );

    let outside: Vec<&String> = asserting
        .iter()
        .filter(|(_, text)| !takes_expect_from_support(text))
        .map(|(path, _)| path)
        .collect();
    assert!(
        outside.is_empty(),
        "these specs assert text with an `expect` that did not come from ./support, \
         so SH-622's door never sees the assertion:\n  {}",
        outside
            .iter()
            .map(|p| p.as_str())
            .collect::<Vec<_>>()
            .join("\n  ")
    );
}

// Unit tests for the parsing primitives, including both-direction mutation
// checks -- this project's own standard for a new assertion. Every offender
// is assembled at run time so it never sits in this file as a literal the
// real scans could read.

fn offending_import(bindings: &str) -> String {
    format!("import {} from \"{}\";\n", bindings, PLAYWRIGHT)
}

#[test]
fn the_scan_can_still_see_a_bypass() {
    let bare = offending_import("{ test, expect }");
    assert_eq!(
        playwright_expect_bindings(&bare),
        vec!["expect".to_string()]
    );

    let aliased = offending_import("{ expect as e }");
    assert_eq!(
        playwright_expect_bindings(&aliased),
        vec!["expect as e".to_string()]
    );

    let namespaced = format!(
        "import * as pw from \"{}\";\nawait pw.expect(x).toHaveText(\"y\");\n",
        PLAYWRIGHT
    );
    assert_eq!(playwright_expect_bindings(&namespaced).len(), 1);

    let multiline = format!(
        "import {{\n  test,\n  expect,\n}} from \"{}\";\n",
        PLAYWRIGHT
    );
    assert_eq!(
        playwright_expect_bindings(&multiline),
        vec!["expect".to_string()]
    );
}

#[test]
fn the_scan_does_not_flag_what_is_not_a_bypass() {
    let type_only = format!(
        "import type {{ Locator, expect }} from \"{}\";\n",
        PLAYWRIGHT
    );
    assert!(playwright_expect_bindings(&type_only).is_empty());

    let other_names = offending_import("{ test, devices }");
    assert!(playwright_expect_bindings(&other_names).is_empty());

    let namespace_unused = format!(
        "import * as pw from \"{}\";\npw.defineConfig({{}});\n",
        PLAYWRIGHT
    );
    assert!(playwright_expect_bindings(&namespace_unused).is_empty());

    let from_support = "import { test, expect } from \"./support\";\n";
    assert!(playwright_expect_bindings(from_support).is_empty());
    assert!(takes_expect_from_support(from_support));

    let commented = format!("// {}", offending_import("{ expect }"));
    assert!(playwright_expect_bindings(&strip_comments(&commented)).is_empty());

    let block_commented = format!("/*\n{}*/\n", offending_import("{ expect }"));
    assert!(playwright_expect_bindings(&strip_comments(&block_commented)).is_empty());

    // The word inside a string literal is not at a statement boundary, so it
    // is not read as an import -- the same boundary rule that keeps
    // `export { x } from "…"` re-exports out of the import list.
    let inside_a_string = format!("const s = \"{}\";\n", offending_import("{ expect }").trim());
    assert!(playwright_expect_bindings(&inside_a_string).is_empty());
}

#[test]
fn imports_reads_the_shapes_the_suite_actually_writes() {
    let parsed = imports(
        "import { expect as baseExpect, test as base } from \"@playwright/test\";\n\
         import type {\n  Page,\n} from \"@playwright/test\";\n\
         import { contention } from \"../load-grace\";\n\
         export { something } from \"./elsewhere\";\n",
    );
    assert_eq!(
        parsed,
        vec![
            Import {
                from: PLAYWRIGHT.into(),
                type_only: false,
                named: vec!["expect as baseExpect".into(), "test as base".into()],
                namespace: None,
            },
            Import {
                from: PLAYWRIGHT.into(),
                type_only: true,
                named: vec!["Page".into()],
                namespace: None,
            },
            Import {
                from: "../load-grace".into(),
                type_only: false,
                named: vec!["contention".into()],
                namespace: None,
            },
        ]
    );
}

#[test]
fn door_matchers_is_brace_matched_and_absent_when_the_signature_is() {
    let door = "export const expect = baseExpect.extend({\n  async toHaveText(a) { return { pass: true }; },\n});\nasync function toContainText() {}\n";
    let block = door_matchers(door).expect("the signature is present");
    assert!(block.contains("async toHaveText("));
    assert!(
        !block.contains("toContainText"),
        "a matcher defined outside the extend() block must not satisfy the shape"
    );
    assert!(
        door_matchers("export { expect };").is_none(),
        "the pre-SH-622 shape has no door, and must read as none rather than as an empty block"
    );
}

#[test]
fn takes_expect_from_support_reads_only_a_value_import_of_expect() {
    assert!(takes_expect_from_support(
        "import { test, expect } from \"./support\";"
    ));
    assert!(takes_expect_from_support(
        "import { test } from \"./support\";\nimport { expect, openProject } from \"./support\";"
    ));
    assert!(!takes_expect_from_support(
        "import { test } from \"./support\";"
    ));
    assert!(!takes_expect_from_support(
        "import type { expect } from \"./support\";"
    ));
    assert!(!takes_expect_from_support(
        "import { expect } from \"./somewhere-else\";"
    ));
}

#[test]
fn asserts_text_sees_both_matchers_and_nothing_else() {
    assert!(asserts_text("await expect(x).toHaveText(\"a\");"));
    assert!(asserts_text("await expect(x).toContainText(\"a\");"));
    assert!(!asserts_text(
        "await expect(x).toHaveAccessibleName(\"a\");"
    ));
    assert!(!asserts_text("const t = await x.textContent();"));
}

#[test]
fn the_real_door_and_a_real_spec_read_as_expected() {
    // Positive control on the real tree: the scan must see the door's own
    // shape and at least one real spec's shape, or every assertion above is
    // passing over an empty read.
    let root = repo_root();
    let door = strip_comments(&std::fs::read_to_string(root.join(DOOR)).unwrap());
    assert!(door_matchers(&door).is_some());
    let spec = strip_comments(
        &std::fs::read_to_string(root.join("e2e/specs/text-assertion-door.spec.ts")).unwrap(),
    );
    assert!(asserts_text(&spec));
    assert!(takes_expect_from_support(&spec));
    assert!(playwright_expect_bindings(&spec).is_empty());
}
