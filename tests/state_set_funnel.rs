//! The required-state floor is only an invariant while every writer goes
//! through the funnel.
//!
//! `service::state_set` refuses a state set below the floor
//! ([`storyhook::domain::REQUIRED_STATES`]) and repairs one that arrived from
//! foreign data. Neither is worth anything if a service can simply call
//! `put_states` itself — which is exactly how the rule was lost once already:
//! the legacy `storage::save_states` validated every write, and
//! `TransferService::import_project` reached the store directly, so an export
//! document's catalog was installed unchecked.
//!
//! The assertion is deliberately crude: it greps the source, in the style of
//! `invoker_seam.rs::the_legacy_write_path_is_gone`. A build-graph check would
//! be more precise and could be satisfied by a re-export; this cannot.

use std::path::Path;

/// Every `.rs` file under `dir`, as (path, text).
fn sources(dir: &Path, into: &mut Vec<(String, String)>) {
    for entry in std::fs::read_dir(dir).expect("reading a source directory") {
        let path = entry.expect("a directory entry").path();
        if path.is_dir() {
            sources(&path, into);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            let text = std::fs::read_to_string(&path).expect("reading a source file");
            into.push((path.to_string_lossy().into_owned(), text));
        }
    }
}

/// `src/`-relative path, for a readable failure message.
fn relative(path: &str) -> String {
    path.rsplit_once("/src/")
        .map_or_else(|| path.to_string(), |(_, rest)| format!("src/{rest}"))
}

/// Outside the store itself, `put_states` may be named in exactly one file.
///
/// `src/store/` is exempt because that is the layer that *declares* and
/// *implements* the method, and because its conformance suite writes
/// deliberately degenerate catalogs through it — two-state sets, duplicated
/// slugs — to prove what the storage layer guarantees about a set no service
/// would ever assemble. That is why the floor is not enforced there: a product
/// rule inside `put_states` would delete those tests' subject matter.
#[test]
fn a_state_set_is_written_in_exactly_one_module() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    sources(&root, &mut files);
    assert!(
        files.len() > 20,
        "expected the whole tree, got {}",
        files.len()
    );

    const ALLOWED_PREFIX: &str = "src/store/";
    const FUNNEL: &str = "src/service/state_set.rs";

    let mut breaches = Vec::new();
    for (path, text) in &files {
        let relative = relative(path);
        if relative == FUNNEL || relative.starts_with(ALLOWED_PREFIX) {
            continue;
        }
        for (number, line) in text.lines().enumerate() {
            // Doc comments name the method to explain the rule; they cannot
            // call it.
            let code = line.trim_start();
            if code.starts_with("//") {
                continue;
            }
            if code.contains("put_states") {
                breaches.push(format!("{relative}:{}: {}", number + 1, code.trim()));
            }
        }
    }

    assert!(
        breaches.is_empty(),
        "every state set must be written through `{FUNNEL}`, which is what applies the \
         required-state floor. These call `put_states` directly:\n  {}",
        breaches.join("\n  ")
    );
}

/// The funnel is only a funnel while it is the thing that validates.
///
/// A future edit could keep every caller routed through `state_set` and quietly
/// drop the check inside it, which the grep above would not notice. This reads
/// the module and asserts both halves are still named.
#[test]
fn the_funnel_still_applies_the_floor() {
    let funnel = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/service/state_set.rs");
    let text = std::fs::read_to_string(&funnel).expect("reading the funnel");
    let code: String = text
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");

    assert!(
        code.contains("validate_required_states"),
        "`write_states` must refuse a set below the floor"
    );
    assert!(
        code.contains("with_required_states"),
        "`write_states_repairing` must repair a set below the floor"
    );
}
