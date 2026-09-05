//! No `pub` item survives with zero call sites (SH-198).
//!
//! `get_timeline` sat on `GithubClient` with no caller anywhere in `src/` or
//! `tests/`, and rustc never warned. `dead_code` only fires for an item
//! unreachable from a crate root; `pub` makes everything reachable, which is
//! the right rule for a published library and the wrong one here — this
//! crate is published nowhere (`scripts/release.sh` ships binaries, never
//! `cargo publish`), so its `pub` surface has exactly the
//! consumers `src/` and `tests/` give it, and the compiler cannot see when
//! that count drops to zero. This file is the check that can.
//!
//! Derived over `git ls-files`, the same style `tests/store_isolation.rs`'s
//! `data_dir_harnesses` and
//! `nothing_outside_real_store_rs_re_infers_a_real_store_from_the_checkout`
//! scan with — a hand-maintained allowlist is exactly the thing that let ten
//! of these accumulate unnoticed.
//!
//! **Known blind spots, and why each fails safe (silence, never a false
//! alarm):**
//! - A name defined twice (a false match against another crate's identically
//!   named item, or a genuine shadow) is live if *either* copy has a caller —
//!   this scan does not attribute a reference to one definition over another.
//! - An identifier that appears only inside a string literal or `format!`
//!   template counts as a reference, same as real code would.
//! - `pub mod` is out of scope: modules are referenced by path
//!   (`crate::a::b`), which would make every leaf module name "used" via its
//!   parent path and turn the scan into noise.
//! - `pub(crate)` and `pub(super)` are out of scope on purpose — those are
//!   exactly what rustc's own `dead_code` lint already covers.

use std::collections::BTreeMap;
use std::path::Path;

/// One `pub` item definition: its name, and where it was found.
struct Definition {
    name: String,
    file: String,
    line: usize,
}

/// Every tracked `.rs` file's full text, keyed by its path relative to the
/// repository root.
fn tracked_rust_sources(root: &Path) -> BTreeMap<String, String> {
    let listed = std::process::Command::new("git")
        .current_dir(root)
        .args(["ls-files", "-z", "--", "*.rs"])
        .output()
        .expect("listing this repository's tracked Rust files");
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

/// The identifier a `pub fn` / `pub struct` / … line defines, if it defines
/// one of the kinds this scan fences.
///
/// Only a bare `pub` is matched — `pub(crate)` and `pub(super)` are already
/// within rustc's own `dead_code` reach, and matching them here would just
/// re-report what that lint already catches.
fn defined_name(line: &str) -> Option<&str> {
    let rest = line.trim_start();
    let rest = rest.strip_prefix("pub ")?;
    // Reject `pub(...)` — `strip_prefix("pub ")` above already requires a
    // space immediately after `pub`, so `pub(crate) fn f` never reaches here.
    let rest = rest
        .strip_prefix("const fn ")
        .or_else(|| rest.strip_prefix("async fn "))
        .or_else(|| rest.strip_prefix("fn "))
        .or_else(|| rest.strip_prefix("struct "))
        .or_else(|| rest.strip_prefix("enum "))
        .or_else(|| rest.strip_prefix("trait "))
        .or_else(|| rest.strip_prefix("type "))
        .or_else(|| rest.strip_prefix("const "))
        .or_else(|| rest.strip_prefix("static "))?;
    let end = rest
        .find(|c: char| !(c.is_alphanumeric() || c == '_'))
        .unwrap_or(rest.len());
    let name = &rest[..end];
    (!name.is_empty()).then_some(name)
}

/// Every `pub` `fn`/`struct`/`enum`/`trait`/`type`/`const`/`static` defined
/// across `sources`.
fn collect_definitions(sources: &BTreeMap<String, String>) -> Vec<Definition> {
    let mut definitions = Vec::new();
    for (file, text) in sources {
        for (idx, line) in text.lines().enumerate() {
            if let Some(name) = defined_name(line) {
                definitions.push(Definition {
                    name: name.to_string(),
                    file: file.clone(),
                    line: idx + 1,
                });
            }
        }
    }
    definitions
}

/// Whether `name` appears as a whole identifier anywhere on `line` other than
/// at `skip_col` (the definition's own occurrence, when `line` is its
/// definition line). Comment lines are excluded by the caller, not here.
fn references_on_line(line: &str, name: &str, skip: Option<usize>) -> bool {
    let bytes = line.as_bytes();
    let mut start = 0;
    while let Some(rel) = line[start..].find(name) {
        let at = start + rel;
        let before_ok =
            at == 0 || !(bytes[at - 1].is_ascii_alphanumeric() || bytes[at - 1] == b'_');
        let after = at + name.len();
        let after_ok =
            after >= bytes.len() || !(bytes[after].is_ascii_alphanumeric() || bytes[after] == b'_');
        if before_ok && after_ok && skip != Some(at) {
            return true;
        }
        start = at + name.len();
    }
    false
}

/// Whether `definition` has at least one call site outside its own
/// declaration, anywhere in `sources`.
fn has_a_reference(definition: &Definition, sources: &BTreeMap<String, String>) -> bool {
    for (file, text) in sources {
        for (idx, line) in text.lines().enumerate() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") || trimmed.starts_with('*') {
                continue;
            }
            let is_definition_line = file == &definition.file && idx + 1 == definition.line;
            if is_definition_line {
                // The definition line itself may reference the name more than
                // once only by coincidence (it never does, for the item
                // shapes this scan matches) — treat every occurrence on this
                // specific line as the declaration, not a call site.
                let bare = line.replace(&definition.name, "");
                if bare.len() != line.len() - definition.name.len() {
                    continue;
                }
            }
            if references_on_line(line, &definition.name, None) && !is_definition_line {
                return true;
            }
        }
    }
    false
}

/// A `pub` item with no call site anywhere in this repository's tracked Rust
/// sources is dead: rustc cannot see it because `pub` makes it reachable from
/// the crate root, and nothing outside this repository can reach the crate
/// root at all (`storyhook` is not published — see this file's module doc).
#[test]
fn every_pub_item_has_a_call_site() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let sources = tracked_rust_sources(root);
    let definitions = collect_definitions(&sources);

    // A scan that finds implausibly few definitions has a broken parser, not
    // a small codebase — this repository's `src/` and `tests/` together
    // declare thousands of `pub` items. Same self-check `data_dir_harnesses`
    // makes in `tests/store_isolation.rs`.
    assert!(
        definitions.len() > 500,
        "this scan is supposed to find every `pub` item declaration across \
         src/ and tests/, and it found {}. The parser is broken, not the \
         codebase.",
        definitions.len()
    );

    let orphans: Vec<String> = definitions
        .iter()
        .filter(|d| !has_a_reference(d, &sources))
        .map(|d| format!("{} ({}:{})", d.name, d.file, d.line))
        .collect();

    assert!(
        orphans.is_empty(),
        "{orphans:?} are `pub` with no call site anywhere in this repository's \
         tracked Rust sources. rustc cannot see this: a `pub` item reachable \
         from a lib crate's root counts as live to `dead_code`, and this \
         crate is published nowhere, so nothing outside src/ and tests/ can \
         ever call one. Delete it, or — if it genuinely has a caller this \
         scan's known blind spots miss (see this file's module doc) — extend \
         the scan rather than special-casing the name."
    );
}
