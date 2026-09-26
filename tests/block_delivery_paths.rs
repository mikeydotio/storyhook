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
            // Unit-test modules call the helpers to test them, not to mutate.
            if path.extension().is_none_or(|e| e != "rs")
                || path.file_name().is_some_and(|name| name == "tests.rs")
            {
                continue;
            }
            let source = compact_code(&std::fs::read_to_string(&path).unwrap());
            if [
                "append_and_fold(",
                "append_restored_and_fold(",
                "append_and_fold_maintenance(",
                // SH-772: the landing door called only this one, so a scan that
                // knew three names never saw it.
                "append_state_transition(",
                "retract_closed_blocker_edges(",
                "refold_story(",
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
        "landing",
        "pr_check",
        "project",
        "project_recovery",
        "project_recovery/refusal",
        "project_recovery/resume",
        "project_recovery/test_return",
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
            "project_recovery/work_holds",
            "project_recovery/work",
            "super::work_holds::record(",
        ),
        (
            "project_recovery/holds",
            "project_recovery",
            "holds::record(",
        ),
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
            source.contains("write_stories(") || source.contains("derive_block_edges("),
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

/// Production code in `src/`, excluding the store's own internals and unit
/// test modules, as `(relative path, code without comments)`.
fn production_sources() -> Vec<(String, String)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut found = Vec::new();
    let mut pending = vec![root.clone()];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            let relative = path
                .strip_prefix(&root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            if path.is_dir() {
                if relative != "store" && !relative.ends_with("/tests") && relative != "tests" {
                    pending.push(path);
                }
                continue;
            }
            if path.extension().is_none_or(|e| e != "rs") || relative.ends_with("tests.rs") {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            let production = text.split("\n#[cfg(test)]\nmod tests {").next().unwrap();
            found.push((
                relative,
                storyhook_test_support::without_rust_comments(production),
            ));
        }
    }
    found
}

/// SH-772: only the derivation may declare a transaction derived, and only the
/// service funnel and the known replay doors write read-model rows directly.
/// Either rule broken would let a write change effective blocking unseen.
#[test]
fn derivation_is_declared_in_one_place_and_rows_are_written_through_known_doors() {
    let mut declarers = Vec::new();
    let mut row_writers = BTreeSet::new();
    for (path, code) in production_sources() {
        if code.contains("set_block_edge_derivation(") {
            declarers.push(path.clone());
        }
        if code.contains(".put_story(") || code.contains(".purge_story(") {
            row_writers.insert(path);
        }
    }
    assert_eq!(declarers, ["service/block_delivery.rs"]);
    assert_eq!(
        row_writers,
        [
            // The funnel and `refold_story`, both behind the backstop.
            "service/mod.rs",
            // Delete purges inside `write_stories`.
            "service/story.rs",
            // Replays into a project with no stories yet: nothing to resume.
            "service/migrate.rs",
            "service/transfer.rs",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect(),
        "a new direct row writer must derive block edges or say why it cannot change blocking"
    );
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
        ("nested/tests.rs", "append_and_fold(tx);"),
        ("indirect.rs", "super::story::append_state_transition(tx);"),
    ] {
        std::fs::write(scratch.path().join(file), source).unwrap();
    }
    assert_eq!(
        event_writers(scratch.path()),
        [
            "ordinary",
            "nested/restored",
            "nested/maintenance",
            "indirect"
        ]
        .into_iter()
        .map(str::to_owned)
        .collect()
    );
}

/// Top-level bash functions in `script`, as `(name, body)`: from `name() {` to
/// the first line that is exactly `}`.
fn bash_functions(script: &str) -> Vec<(String, String)> {
    let mut functions = Vec::new();
    let mut lines = script.lines();
    while let Some(line) = lines.next() {
        let Some(name) = line.strip_suffix("() {") else {
            continue;
        };
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_lowercase() || c == '_') {
            continue;
        }
        let body: Vec<&str> = lines.by_ref().take_while(|line| *line != "}").collect();
        functions.push((name.to_owned(), body.join("\n")));
    }
    functions
}

/// SH-772 (decision D5): a Resume with no acknowledged interrupt reaches the
/// story's REGISTERED session. That is safe only because every door that
/// writes a registration first reserves the workspace and revokes the story's
/// pending deliveries, so a Resume still pending can never reach a session
/// registered after it was enqueued. A new registration writer that skips
/// either step would let a stale prompt into a replacement session.
#[test]
fn every_session_registration_first_revokes_pending_deliveries_under_the_lock() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("plugins/story");
    let mut writers = BTreeSet::new();
    let mut scripts = vec![root.join("bin/story.sh")];
    for entry in std::fs::read_dir(root.join("lib")).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "sh") {
            scripts.push(path);
        }
    }
    for script in scripts {
        let source = std::fs::read_to_string(&script).unwrap();
        for (name, body) in bash_functions(&source) {
            for verb in ["register", "adopt"] {
                let call = format!("lib/agent_identity.py\" {verb}");
                let Some(at) = body.find(&call) else {
                    continue;
                };
                writers.insert(format!("{name}:{verb}"));
                let before = &body[..at];
                let revoked = before
                    .rfind("supersede_block_deliveries \"")
                    .unwrap_or_else(|| {
                        panic!("{name} {verb}s a session without revoking pending deliveries first")
                    });
                assert!(
                    before[..revoked].contains("reserve_story_workspace")
                        || before[..revoked].contains("reserve_dispatch_workspace"),
                    "{name} revokes pending deliveries without holding the workspace reservation"
                );
            }
        }
    }
    assert_eq!(
        writers,
        ["cmd_dispatch:register", "cmd_notify:adopt"]
            .into_iter()
            .map(str::to_owned)
            .collect(),
        "review every new session-registration writer against the SH-772 D5 contract"
    );
}

#[test]
fn the_function_scanner_reads_top_level_bodies() {
    let script = "helper() {\n  a\n}\n\ncmd_x() {\n  reserve_story_workspace\n  b() { c; }\n}\n";
    let functions = bash_functions(script);
    assert_eq!(
        functions,
        [
            ("helper".to_owned(), "  a".to_owned()),
            (
                "cmd_x".to_owned(),
                "  reserve_story_workspace\n  b() { c; }".to_owned()
            )
        ]
    );
}
