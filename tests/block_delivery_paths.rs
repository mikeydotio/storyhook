//! New story mutation doors must account for transactional block edges.
use std::collections::BTreeSet;
use std::path::Path;

#[test]
fn every_event_writer_accounts_for_effective_block_changes() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut writers = BTreeSet::new();
    for entry in std::fs::read_dir(root.join("src/service")).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "rs") {
            continue;
        }
        let source = std::fs::read_to_string(&path).unwrap();
        if source
            .lines()
            .any(|line| !line.trim_start().starts_with("//") && line.contains("append_and_fold("))
        {
            writers.insert(path.file_stem().unwrap().to_str().unwrap().to_owned());
        }
    }
    let wrapped = [
        "config",
        "engine",
        "git",
        "history",
        "integrity",
        "pr_check",
        "relation",
        "story",
        "transfer",
        "verification",
    ];
    // These writers cannot change effective blocking. A new exception requires a reason.
    let exempt = [
        ("attachment", "attachment metadata only"),
        (
            "grouping",
            "labels only; structural grouping delegates to RelationService",
        ),
        ("mod", "defines append_and_fold, not a service transaction"),
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
        .chain(exempt.iter().map(|(name, _)| *name))
        .map(str::to_owned)
        .collect();
    assert_eq!(
        writers, expected,
        "review every new event writer for effective block edges"
    );
    for name in wrapped {
        let source = std::fs::read_to_string(root.join(format!("src/service/{name}.rs"))).unwrap();
        assert!(
            source.contains("write_stories("),
            "{name} must derive edges before commit"
        );
    }
    // The high-traffic mutation doors have no raw Ctx transaction escape hatch.
    for name in [
        "config", "history", "relation", "story", "transfer", "pr_check",
    ] {
        let source = std::fs::read_to_string(root.join(format!("src/service/{name}.rs"))).unwrap();
        let compact: String = source.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(
            !compact.contains("ctx.store().write("),
            "{name} bypasses transactional block derivation"
        );
    }
}
