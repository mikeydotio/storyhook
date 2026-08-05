//! The type-slug invariant is only worth anything while `add_type` stays the
//! one place a caller-supplied type slug enters the store.
//!
//! SH-134 chose a validator at the call site over a funnel module, because
//! `service::state_set` — the funnel this codebase already has — exists to
//! arbitrate a refuse-vs-repair split over a whole *set*, and a type catalog
//! has no such rule to arbitrate. `validate_type_slug` guards one method, and
//! that is enough precisely while the list of methods writing a type catalog
//! stays the list below.
//!
//! So this is the cheaper half of what a funnel would have bought: not "every
//! write is validated", which was never true of the state funnel either, but
//! "no *new* writer appeared without anybody noticing". The day an eighth
//! `put_types` call site is added, this fails and its author has to decide
//! whether their slug came from a person — in which case it needs the check —
//! or from a document, which SH-134 deliberately left raw (see
//! `service::transfer` and `service::migrate`).
//!
//! The assertion is deliberately crude: it greps the source, in the style of
//! `state_set_funnel.rs::a_state_set_is_written_in_exactly_one_module` and
//! `invoker_seam.rs::the_legacy_write_path_is_gone`.

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

/// The files that may write a type catalog, and what each one's slugs are.
///
/// `src/store/` is exempt for the same reason `state_set_funnel.rs` exempts it:
/// that layer declares and implements the method, and its conformance suite
/// writes deliberately degenerate catalogs through it to prove what the storage
/// layer guarantees.
const ALLOWED: [(&str, &str); 4] = [
    (
        "src/service/config.rs",
        "add_type (validated), update_type (slug immutable) and remove_type (shrinks only)",
    ),
    (
        "src/service/project.rs",
        "default_types(), a compile-time constant pinned by its own unit test",
    ),
    (
        "src/service/migrate.rs",
        "a legacy tree's catalog, left raw by SH-134's D3",
    ),
    (
        "src/service/transfer.rs",
        "an export document's catalog, left raw by SH-134's D3",
    ),
];

#[test]
fn story_type_slugs_are_written_from_a_known_allowlist_of_files() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    sources(&root, &mut files);
    assert!(
        files.len() > 20,
        "expected the whole tree, got {}",
        files.len()
    );

    const ALLOWED_PREFIX: &str = "src/store/";

    let mut breaches = Vec::new();
    for (path, text) in &files {
        let relative = relative(path);
        if relative.starts_with(ALLOWED_PREFIX)
            || ALLOWED.iter().any(|(allowed, _)| *allowed == relative)
        {
            continue;
        }
        for (number, line) in text.lines().enumerate() {
            // Doc comments name the method to explain the rule; they cannot
            // call it.
            let code = line.trim_start();
            if code.starts_with("//") {
                continue;
            }
            if code.contains("put_types") {
                breaches.push(format!("{relative}:{}: {}", number + 1, code.trim()));
            }
        }
    }

    assert!(
        breaches.is_empty(),
        "a new writer of story types appeared. Decide whether its slugs come from a person — in \
         which case route it through `ConfigService::add_type`, which applies \
         `domain::validate_type_slug` — or from a document, which SH-134 leaves raw on purpose. \
         Then add it to this test's allowlist with the reason.\n  {}\nThe allowlist today:\n  {}",
        breaches.join("\n  "),
        ALLOWED
            .iter()
            .map(|(file, why)| format!("{file} — {why}"))
            .collect::<Vec<_>>()
            .join("\n  ")
    );
}
