//! `checkout_path` answers *where repo-side work runs*, never *which project this is*.
//!
//! SH-120's third acceptance criterion was written as "a grep confirms no code
//! path other than dispatch reads `checkout_path`". That was **already false**
//! when SH-120 landed in the backlog and is falser now: eleven production sites
//! read the column today, every one of them deliberate, and this story adds a
//! twelfth. None of them is resolution.
//!
//! The criterion was reaching for a real invariant and named the wrong
//! enforcement for it. The invariant is epic SH-112's:
//!
//! > Nothing about the filesystem is ever *required* to answer "which project
//! > is this?"
//!
//! So the rule enforced here is:
//!
//! > `checkout_path` is read only to **report** it, or to **choose a working
//! > directory** for repo-side work. It is never read to decide which project a
//! > directory belongs to, and the set of files that name it is written down.
//!
//! Two tests, because that rule has a structural half and a behavioural half and
//! **neither implies the other**. A frozen file list cannot notice an
//! already-listed function quietly repurposed into a resolver; a behavioural
//! probe cannot notice a brand-new reader in a module it never exercises.
//!
//! The structural half is deliberately a source grep, in the shape
//! `tests/state_set_funnel.rs` and `tests/invoker_seam.rs` already use, and for
//! the reason `state_set_funnel.rs` gives: a build-graph check would be more
//! precise and could be satisfied by a re-export, where a grep cannot.

use std::collections::BTreeSet;
use std::path::Path;

/// Every file under `src/` that may name `checkout_path`, and why.
///
/// **The reason column is the point.** The criterion this replaces wanted "one
/// reader"; what is actually wanted is *no reader nobody argued for*. A new
/// entry here is cheap — one line — but it cannot be added without writing down
/// what the new site reads the column *for*, which is exactly the review this
/// test exists to force.
///
/// `src/store/` is excluded wholesale below: it declares the trait method and
/// implements it, so naming the column is its job.
const ALLOWED: &[(&str, &str)] = &[
    (
        "src/invoke.rs",
        "`project list` and `project show` report it; `project show` is dispatch's lookup",
    ),
    (
        "src/api/rest.rs",
        "a web write needs a working directory to fire the project's hooks in",
    ),
    (
        "src/service/catalog.rs",
        "doctor's orphan audit, the catalog listing, and the unregistered-origin probe",
    ),
    (
        "src/service/migrate.rs",
        "a scan, once, to refuse a second migration of a tree that already moved",
    ),
    (
        "src/service/project.rs",
        "adopt_checkout fills a gap, forget_checkout is doctor --fix's half, delete_plan lists it",
    ),
    (
        "src/service/git_links.rs",
        "link and unlink read the previous value to report the path they displaced; link also \
         reads it to decide the pointer-file plan, but resolves nothing by it (SH-167)",
    ),
    (
        "src/service/engine.rs",
        "engine start refuses repo-side work when the already-selected project has no checkout",
    ),
];

/// Walks `src/`, skipping the store layer, and returns every `.rs` file whose
/// non-comment source names `checkout_path`.
///
/// Comment lines are skipped so that *discussing* the column — which several
/// headers do at length, including this story's own — never trips the guard. A
/// doc comment is not a call site.
fn files_naming_checkout_path() -> BTreeSet<String> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut found = BTreeSet::new();
    let mut stack = vec![root.join("src")];
    let mut scanned = 0usize;
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("reading a source directory") {
            let path = entry.expect("reading a directory entry").path();
            if path.is_dir() {
                // The store layer declares `checkout_path` and implements it.
                if path.ends_with("store") {
                    continue;
                }
                stack.push(path);
                continue;
            }
            if path.extension().is_none_or(|ext| ext != "rs") {
                continue;
            }
            scanned += 1;
            let source = std::fs::read_to_string(&path).expect("reading a source file");
            let names_it = source.lines().any(|line| {
                let trimmed = line.trim_start();
                !trimmed.starts_with("//") && trimmed.contains("checkout_path")
            });
            if names_it {
                found.insert(relative(root, &path));
            }
        }
    }
    // A walker that silently found nothing would make this test vacuous, and it
    // would pass. `state_set_funnel.rs` guards itself the same way.
    assert!(
        scanned > 20,
        "the source walk found only {scanned} files under src/, which means it is broken \
         rather than that the tree shrank"
    );
    found
}

/// `path` relative to the crate root, with forward slashes.
fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .expect("a path under the crate root")
        .to_string_lossy()
        .replace('\\', "/")
}

/// The structural half: the set of files naming the column is exactly the
/// written-down list.
#[test]
fn checkout_path_is_read_only_where_it_is_written_down() {
    let found = files_naming_checkout_path();
    let allowed: BTreeSet<String> = ALLOWED
        .iter()
        .map(|(file, _)| (*file).to_string())
        .collect();

    let unexpected: Vec<&String> = found.difference(&allowed).collect();
    assert!(
        unexpected.is_empty(),
        "these files name `checkout_path` and are not on the allowlist in \
         tests/checkout_path_readers.rs: {unexpected:?}\n\n\
         `checkout_path` may be read to REPORT a path, or to CHOOSE A WORKING DIRECTORY for \
         repo-side work. It may never be read to decide which project a directory belongs to — \
         that is epic SH-112's invariant, and `--project`, $STORYHOOK_PROJECT, the committed \
         pointer file and the registered origin are the whole resolver list.\n\n\
         If the new site is legitimate, add it to ALLOWED with a one-line reason saying what it \
         reads the column FOR. Writing that sentence is the review this test exists to force."
    );

    let departed: Vec<&String> = allowed.difference(&found).collect();
    assert!(
        departed.is_empty(),
        "these files are on the allowlist but no longer name `checkout_path`: {departed:?}\n\n\
         A reader that went away is good news, but the list has to shrink with it or it stops \
         describing the tree and starts describing history."
    );
}

/// Every allowlist entry carries a reason, and no file is listed twice.
///
/// Cheap, and it is what keeps the reason column from decaying into `""` under
/// time pressure — which would leave the test above passing while the review it
/// exists to force silently stopped happening.
#[test]
fn every_allowlist_entry_says_what_it_reads_the_column_for() {
    let mut seen = BTreeSet::new();
    for (file, reason) in ALLOWED {
        assert!(
            reason.len() > 20,
            "the allowlist entry for {file} needs a real reason, got {reason:?}"
        );
        assert!(
            seen.insert(*file),
            "{file} is on the checkout_path allowlist twice"
        );
    }
}
