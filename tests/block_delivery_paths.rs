//! New story mutation doors must account for transactional block edges.
use std::collections::BTreeSet;
use std::path::Path;

fn compact_code(source: &str) -> String {
    storyhook_test_support::without_rust_comments(source)
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect()
}

fn event_writers(root: &Path) -> BTreeSet<String> {
    let mut writers = BTreeSet::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
                continue;
            }
            if path.extension().is_none_or(|e| e != "rs") {
                continue;
            }
            let source = compact_code(&std::fs::read_to_string(&path).unwrap());
            if [
                "append_and_fold(",
                "append_restored_and_fold(",
                "append_and_fold_maintenance(",
            ]
            .iter()
            .any(|helper| source.contains(helper))
            {
                writers.insert(
                    path.strip_prefix(root)
                        .unwrap()
                        .with_extension("")
                        .to_string_lossy()
                        .replace('\\', "/"),
                );
            }
        }
    }
    writers
}

#[test]
fn every_event_writer_accounts_for_effective_block_changes() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/service");
    let writers = event_writers(&root);
    let wrapped = [
        "config",
        "continuation/request",
        "engine",
        "engine/reset",
        "git",
        "history",
        "integrity",
        "pr_check",
        "relation",
        "reset",
        "story",
        "story_reset",
        "transfer",
        "verification",
        "verification/human",
    ];
    // Obviation evidence changes blocking inside the caller's complete transaction.
    let delegated = [
        (
            "project_recovery/decision_effects",
            "project_recovery",
            "decision_effects::apply(",
        ),
        (
            "project_recovery/decision_effects",
            "project_recovery/decision",
            "super::decision_effects::apply(",
        ),
        (
            "continuation/administrative",
            "continuation/request",
            "administrative::record(",
        ),
    ];
    // These writers cannot change effective blocking. A new exception requires a reason.
    let exempt = [
        ("attachment", "attachment metadata only"),
        (
            "grouping",
            "labels only; structural grouping delegates to RelationService",
        ),
        ("mod", "defines append helpers, not a service transaction"),
        (
            "pr_link",
            "PR-link metadata only; completion is in pr_check/verification",
        ),
        (
            "project",
            "prefix rename preserves numeric identity and graph; creation has no prior agents",
        ),
    ];
    let expected = wrapped
        .iter()
        .copied()
        .chain(delegated.iter().map(|(name, _, _)| *name))
        .chain(exempt.iter().map(|(name, _)| *name))
        .map(str::to_owned)
        .collect();
    assert_eq!(
        writers, expected,
        "review every new event writer for effective block edges"
    );
    for name in wrapped {
        let source =
            compact_code(&std::fs::read_to_string(root.join(format!("{name}.rs"))).unwrap());
        assert!(
            source.contains("write_stories("),
            "{name} must derive edges before commit"
        );
    }
    for (name, owner, call) in delegated {
        let source =
            compact_code(&std::fs::read_to_string(root.join(format!("{owner}.rs"))).unwrap());
        assert!(
            source.contains(call) && source.contains("write_stories("),
            "{owner} must own block derivation for {name}"
        );
    }
    // The high-traffic mutation doors have no raw Ctx transaction escape hatch.
    for name in [
        "config", "history", "relation", "story", "transfer", "pr_check",
    ] {
        let source =
            compact_code(&std::fs::read_to_string(root.join(format!("{name}.rs"))).unwrap());
        assert!(
            !source.contains("ctx.store().write("),
            "{name} bypasses transactional block derivation"
        );
    }
}

#[test]
fn event_writer_scan_covers_nested_modules_and_all_append_doors() {
    let scratch = storyhook_test_support::scratch_dir();
    std::fs::create_dir(scratch.path().join("nested")).unwrap();
    for (file, source) in [
        ("ordinary.rs", "append_and_fold (tx);"),
        (
            "nested/restored.rs",
            "super::append_restored_and_fold (tx);",
        ),
        (
            "nested/maintenance.rs",
            "append_and_fold_maintenance\n(tx);",
        ),
        (
            "comment.rs",
            "// append_and_fold(tx);\n/* append_restored_and_fold(tx); */",
        ),
    ] {
        std::fs::write(scratch.path().join(file), source).unwrap();
    }
    assert_eq!(
        event_writers(scratch.path()),
        ["ordinary", "nested/restored", "nested/maintenance"]
            .into_iter()
            .map(str::to_owned)
            .collect()
    );
}
