//! A copy that a write depends on must come from `Store::write_with_snapshot`.
//!
//! `Store::snapshot` promises only that its copy is *a* recent committed
//! state. `ProjectService::set_prefix` needed more than that and did not ask
//! for it: it called `snapshot` and then `write`, which is two critical
//! sections with room between them for any writer in the world, so the copy an
//! operator was told was "the state before this rewrite" could be missing work
//! that had already committed. Restoring it would have discarded that work
//! along with the rename (SH-297).
//!
//! `tests/service_project_set_prefix.rs` proves the ordering is right where it
//! is used today. This file is the cheaper, wider fence: it says nothing about
//! ordering and everything about *shape*, so it reaches a future author of the
//! next destructive verb — a project merge, a bulk purge — who would otherwise
//! reach for the same two calls in the same order and reintroduce the defect
//! somewhere the behavioural test never looks.
//!
//! Derived over `git ls-files` rather than a hand-maintained list, in the style
//! of `tests/dead_public_surface.rs` and `tests/store_isolation.rs`'s scans. A
//! written list of "the files allowed to do this" is precisely what stops being
//! true.

use std::path::{Component, Path, PathBuf};

#[path = "support/test_only_modules.rs"]
mod test_only_modules;

/// Where an uncoupled `Store::snapshot` is legitimate, and why.
///
/// In `src/daemon/backup.rs`, the daily schedule and `story
/// store backup` both take a copy that no write depends on, which is exactly
/// what `Store::snapshot` promises. Test-only module ancestry is derived from
/// declarations separately; fixture method names confer no production exemption.
/// `src/store/` is not listed because it is
/// the engine — the place `write_with_snapshot` is *implemented*, where the
/// call this scan is looking for is the correct one.
const UNCOUPLED_BACKUP_SITES: &[&str] = &["src/daemon/backup.rs"];

/// Every tracked `.rs` file under `src/`, as (path, contents).
fn tracked_sources(root: &Path) -> Vec<(String, String)> {
    let listed = std::process::Command::new("git")
        .current_dir(root)
        .args(["ls-files", "-z", "--", "src/*.rs"])
        .output()
        .expect("listing this repository's tracked Rust sources");
    assert!(
        listed.status.success(),
        "`git ls-files` failed, so this scan proved nothing: {}",
        String::from_utf8_lossy(&listed.stderr)
    );

    let files: Vec<(String, String)> = listed
        .stdout
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
        .map(|path| {
            let relative = std::str::from_utf8(path).expect("a UTF-8 path").to_string();
            let text = std::fs::read_to_string(root.join(&relative))
                .unwrap_or_else(|e| panic!("reading {relative}: {e}"));
            (relative, text)
        })
        .collect();
    assert!(
        !files.is_empty(),
        "no tracked sources under src/ — the scan would pass vacuously"
    );
    files
}

/// Directory ancestry alone does not prove test-only ownership: a production
/// module can load a file there through another declaration. Refuse that
/// ambiguity instead of letting the exclusion hide a production snapshot.
fn refuse_production_aliases(sources: &[(String, String)], test_roots: &[PathBuf]) {
    fn check_target(target: PathBuf, roots: &[PathBuf], declaring: &str) {
        assert!(
            !target.is_absolute(),
            "cannot establish production module path in {declaring}: absolute path"
        );
        let mut normalized = PathBuf::new();
        for component in target.components() {
            match component {
                Component::CurDir => {}
                Component::ParentDir => {
                    assert!(normalized.pop(), "module path escapes the source corpus");
                }
                other => normalized.push(other.as_os_str()),
            }
        }
        assert!(
            !roots.iter().any(|root| normalized.starts_with(root)),
            "production module in {declaring} aliases test-only source {}; \
             snapshot exclusion is ambiguous",
            normalized.display()
        );
    }

    fn visit(
        items: &[syn::Item],
        directory: &Path,
        path_directory: &Path,
        roots: &[PathBuf],
        declaring: &str,
    ) {
        for item in items {
            let syn::Item::Mod(module) = item else {
                continue;
            };
            if module.attrs.iter().any(|attribute| {
                matches!(&attribute.meta, syn::Meta::List(cfg)
                    if cfg.path.is_ident("cfg") && cfg.tokens.to_string() == "test")
            }) {
                continue;
            }
            assert!(
                !module
                    .attrs
                    .iter()
                    .any(|attr| attr.path().is_ident("cfg_attr")),
                "cannot establish production module path in {declaring}: conditional attributes"
            );
            let explicit = module.attrs.iter().find_map(|attribute| {
                if !attribute.path().is_ident("path") {
                    return None;
                }
                if let syn::Meta::NameValue(value) = &attribute.meta
                    && let syn::Expr::Lit(syn::ExprLit {
                        lit: syn::Lit::Str(path),
                        ..
                    }) = &value.value
                {
                    return Some(path.value());
                }
                panic!("cannot establish production module path in {declaring}");
            });
            let name = module.ident.to_string();
            if let Some((_, items)) = &module.content {
                let nested = directory.join(explicit.as_deref().unwrap_or(&name));
                visit(items, &nested, &nested, roots, declaring);
            } else if let Some(path) = explicit {
                check_target(path_directory.join(path), roots, declaring);
            } else {
                // Either legal Rust layout must remain production-visible.
                check_target(directory.join(format!("{name}.rs")), roots, declaring);
                check_target(directory.join(name).join("mod.rs"), roots, declaring);
            }
        }
    }

    if test_roots.is_empty() {
        return;
    }
    for (relative, source) in sources {
        let path = Path::new(relative);
        if test_roots.iter().any(|root| path.starts_with(root)) {
            continue;
        }
        let file = syn::parse_file(source).unwrap_or_else(|error| {
            panic!("cannot establish production module paths in {relative}: {error}")
        });
        let parent = path.parent().unwrap();
        let stem = path.file_stem().unwrap().to_str().unwrap();
        let directory = if matches!(stem, "lib" | "main" | "mod") {
            parent.to_path_buf()
        } else {
            parent.join(stem)
        };
        visit(&file.items, &directory, parent, test_roots, relative);
    }
}

fn uncoupled_snapshot_calls(sources: &[(String, String)]) -> Vec<String> {
    // Exclude only proven external test-module ancestry, after refusing
    // production aliases or module declarations whose reachability is unknown.
    let test_roots = test_only_modules::external_test_module_roots(sources);
    refuse_production_aliases(sources, &test_roots);
    let mut offenders = Vec::new();
    for (path, text) in sources {
        if UNCOUPLED_BACKUP_SITES.contains(&path.as_str())
            || path.starts_with("src/store/")
            || test_roots
                .iter()
                .any(|root| Path::new(path).starts_with(root))
        {
            continue;
        }
        for (index, line) in text.lines().enumerate() {
            // `.snapshot(` — the method call. A `.snapshot` field access
            // (`row.snapshot`, of which there are many) has no parenthesis and
            // is not this.
            if line.contains(".snapshot(") {
                offenders.push(format!("{path}:{}: {}", index + 1, line.trim()));
            }
        }
    }
    offenders
}

#[test]
fn a_copy_a_write_depends_on_comes_from_write_with_snapshot() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let offenders = uncoupled_snapshot_calls(&tracked_sources(root));
    assert!(
        offenders.is_empty(),
        "these call `Store::snapshot`, which promises only *a* recent copy. A copy \
         a write depends on — one an operator is told is the state before that write \
         — must come from `Store::write_with_snapshot`, which takes the copy and the \
         write in one critical section (SH-297). If the copy genuinely stands alone, \
         add the file to UNCOUPLED_BACKUP_SITES with the reason:\n  {}",
        offenders.join("\n  ")
    );
}

#[test]
fn test_module_ancestry_excludes_fixtures_without_hiding_production_snapshots() {
    let sources = [
        (
            "src/lib.rs",
            "#[cfg(test)] pub(crate) mod fixtures;\n\
             mod production;\n\
             #[cfg(any(test, unix))] mod shared;\n\
             #[cfg(test)] #[path = \"redirected.rs\"] mod checks;\n\
             fn after_tests() { store.snapshot(dir, label); }",
        ),
        (
            "src/fixtures.rs",
            "mod owner; fn fixture() { remote.snapshot(); }",
        ),
        ("src/fixtures/owner.rs", "pub(super) mod workflow;"),
        (
            "src/fixtures/owner/workflow.rs",
            "fn forward() { self.inner.snapshot(dir, label); }",
        ),
        ("src/production.rs", "mod tests;"),
        (
            "src/production/tests.rs",
            "fn production() { store.snapshot(dir, label); }",
        ),
        (
            "src/fixtures_extra.rs",
            "fn production() { store.snapshot(dir, label); }",
        ),
        (
            "src/shared.rs",
            "fn production() { store.snapshot(dir, label); }",
        ),
        (
            "src/redirected.rs",
            "fn unresolved() { store.snapshot(dir, label); }",
        ),
    ]
    .map(|(path, source)| (path.to_string(), source.to_string()));
    let offenders = uncoupled_snapshot_calls(&sources);
    let paths: Vec<_> = offenders
        .iter()
        .map(|offender| offender.split(':').next().unwrap())
        .collect();
    assert_eq!(
        paths,
        [
            "src/lib.rs",
            "src/production/tests.rs",
            "src/fixtures_extra.rs",
            "src/shared.rs",
            "src/redirected.rs",
        ],
        "only declared test ancestry may suppress a snapshot call: {offenders:?}"
    );
}

#[test]
fn production_aliases_cannot_hide_snapshot_calls_in_test_directories() {
    for (declaring, production) in [
        (
            "src/lib.rs",
            "#[path = \"tests/shared.rs\"] mod production;",
        ),
        (
            "src/lib.rs",
            "mod inline { #[path = \"../tests/shared.rs\"] mod production; }",
        ),
        ("src/main.rs", "mod tests;"),
    ] {
        let mut sources = [
            (
                "src/lib.rs".to_string(),
                "#[cfg(test)] mod tests;".to_string(),
            ),
            ("src/main.rs".to_string(), String::new()),
            ("src/tests.rs".to_string(), "mod shared;".to_string()),
            (
                "src/tests/shared.rs".to_string(),
                "fn production() { store.snapshot(dir, label); }".to_string(),
            ),
        ];
        sources
            .iter_mut()
            .find(|(path, _)| path == declaring)
            .unwrap()
            .1
            .push_str(production);
        let failure = std::panic::catch_unwind(|| uncoupled_snapshot_calls(&sources))
            .expect_err("a production snapshot must not disappear behind test ancestry");
        let message = failure
            .downcast_ref::<String>()
            .expect("classification diagnostic");
        assert!(message.contains("aliases test-only source"), "{message}");
    }
}

#[test]
#[should_panic(expected = "cannot establish production module paths")]
fn unparseable_production_declarations_cannot_certify_test_exclusions() {
    uncoupled_snapshot_calls(&[
        (
            "src/lib.rs".into(),
            "#[cfg(test)] mod tests; invalid rust!".into(),
        ),
        ("src/owner.rs".into(), "#[cfg(test)] mod fixtures;".into()),
        ("src/owner/fixtures.rs".into(), "fn fixture() {}".into()),
        (
            "src/tests.rs".into(),
            "fn production() { store.snapshot(dir, label); }".into(),
        ),
    ]);
}

/// The scan is only worth anything if it can see the shape it forbids.
///
/// SH-295 and SH-296 were both instruments that could not produce the event
/// they claimed to detect, and they stayed green with the defect reinstated.
/// This asserts the matcher fires on the defect's own text and stays quiet on
/// the field access that looks like it.
#[test]
fn the_scan_recognises_the_shape_it_forbids() {
    let defect = "        let backup_path = self.store.snapshot(backups_dir, \"set-prefix\")?;";
    assert!(
        defect.contains(".snapshot("),
        "the matcher no longer recognises the call SH-297 was filed for"
    );
    for innocent in [
        "        assert_eq!(row_a.snapshot.id, \"AGE-1\");",
        "            let snapshot = fold_story(&story.to_id(&prefix), &known, &states)?;",
    ] {
        assert!(
            !innocent.contains(".snapshot("),
            "the matcher fires on {innocent}, which is not a call to Store::snapshot"
        );
    }
}
