//! Legacy versus store for the transfer family: `export`, `import`,
//! `import-project` and `decompose`.
//!
//! `export` needs its own comparison rather than the shared [`Differential::step`]:
//! its answer is a single `RawJson` string with the whole document inside it, so
//! the harness's timestamp redaction — which replaces string *values* — cannot
//! see the timestamps buried in it. The document is parsed on both sides first,
//! then redacted, then compared.

mod differential_support;

use differential_support::{Differential, redact_timestamps};
use storyhook::cli::Invocation;
use storyhook::error::AppError;
use storyhook::output::Response;
use storyhook_test_support::scratch_dir;

/// The export document one leg produced, parsed and timestamp-redacted.
fn document(result: Result<Response, AppError>) -> serde_json::Value {
    match result {
        Ok(Response::RawJson(json)) => redact_timestamps(
            serde_json::from_str(&json).expect("an export document must be valid JSON"),
        ),
        other => panic!("expected an export document, got {other:?}"),
    }
}

/// Asserts the two legs export the same document.
fn assert_exports_agree(differential: &Differential, label: &str) {
    let legacy = document(differential.legacy_only(Invocation::Export));
    let store = document(differential.store_only(Invocation::Export));
    assert_eq!(
        legacy,
        store,
        "`{label}` diverged\n legacy: {}\n  store: {}",
        serde_json::to_string_pretty(&legacy).unwrap(),
        serde_json::to_string_pretty(&store).unwrap(),
    );
}

/// Writes `content` to a scratch file and hands back its path.
fn spec_file(name: &str, content: &str) -> (tempfile::TempDir, String) {
    let dir = scratch_dir();
    let path = dir.path().join(name);
    std::fs::write(&path, content).expect("writing the input document");
    let rendered = path.to_string_lossy().into_owned();
    (dir, rendered)
}

#[test]
fn an_empty_project_exports_identically() {
    let differential = Differential::new();
    assert_exports_agree(&differential, "export of an empty project");
}

#[test]
fn a_populated_project_exports_identically() {
    let differential = Differential::new();
    differential.add_member("ada-lovelace", Some("ada"));

    let parent = differential.step_id(
        "new parent",
        Invocation::New {
            title: "Parent".into(),
            state: None,
            story_type: Some("story".into()),
            description: Some("The umbrella".into()),
            priority: Some("high".into()),
            labels: Some(vec!["api".into(), "backend".into()]),
            assignee: Some("ada-lovelace".into()),
        },
    );
    let child = differential.step_id(
        "new child",
        Invocation::New {
            title: "Child".into(),
            state: None,
            story_type: None,
            description: None,
            priority: None,
            labels: None,
            assignee: None,
        },
    );
    differential.step(
        "relate",
        Invocation::Relate {
            a: parent.clone(),
            relation: "parent-of".into(),
            b: child.clone(),
            remove: false,
        },
    );
    differential.step(
        "comment",
        Invocation::Comment {
            id: parent.clone(),
            text: "Settled the shape.".into(),
        },
    );
    let doomed = differential.step_id(
        "new doomed",
        Invocation::New {
            title: "Doomed".into(),
            state: None,
            story_type: None,
            description: None,
            priority: None,
            labels: None,
            assignee: None,
        },
    );
    differential.step(
        "archive",
        Invocation::SetState {
            id: doomed,
            state: "done".into(),
            comment: None,
            if_state: None,
        },
    );

    assert_exports_agree(&differential, "export of a populated project");
}

#[test]
fn export_agrees_on_the_ordering_of_eleven_stories() {
    // Eleven, because ten is where lexicographic and numeric orderings part
    // company and the legacy exporter's order is lexicographic.
    let differential = Differential::new();
    for index in 1..=11 {
        differential.step(
            "new",
            Invocation::New {
                title: format!("Story {index}"),
                state: None,
                story_type: None,
                description: None,
                priority: None,
                labels: None,
                assignee: None,
            },
        );
    }
    assert_exports_agree(&differential, "export ordering");
}

#[test]
fn importing_a_batch_agrees_including_its_relationships() {
    let differential = Differential::new();
    let (_dir, path) = spec_file(
        "stories.json",
        r#"[
            {"title": "Parent", "priority": "high", "labels": ["b", "a", "a"]},
            {"title": "Child", "story_type": "story", "description": "  ",
             "relationships": [{"relation": "child-of", "ref_index": 0}]}
        ]"#,
    );
    differential.step("import", Invocation::Import { file: Some(path) });
    differential.show("after import", "SH-1");
    differential.show("after import", "SH-2");
    assert_exports_agree(&differential, "export after an import");
}

#[test]
fn importing_a_batch_with_an_unknown_type_agrees_on_the_rejection() {
    let differential = Differential::new();
    let (_dir, path) = spec_file(
        "stories.json",
        r#"[{"title": "Broken", "story_type": "nonsense"}]"#,
    );
    differential.step(
        "import unknown type",
        Invocation::Import { file: Some(path) },
    );
}

#[test]
fn importing_an_empty_batch_agrees() {
    let differential = Differential::new();
    let (_dir, path) = spec_file("stories.json", "[]");
    differential.step("import nothing", Invocation::Import { file: Some(path) });
}

#[test]
fn importing_from_a_file_that_does_not_exist_agrees_on_the_error() {
    let differential = Differential::new();
    differential.step(
        "import missing file",
        Invocation::Import {
            file: Some("/nonexistent/stories.json".into()),
        },
    );
}

#[test]
fn an_unparseable_priority_is_dropped_by_both_legs() {
    let differential = Differential::new();
    let (_dir, path) = spec_file(
        "stories.json",
        r#"[{"title": "Lenient", "priority": "urgent"}]"#,
    );
    differential.step("import lenient", Invocation::Import { file: Some(path) });
    differential.show("after lenient import", "SH-1");
}

#[test]
fn decomposing_a_spec_agrees_on_the_stories_and_the_summary() {
    let differential = Differential::new();
    let (_dir, path) = spec_file(
        "PLAN.md",
        "# Plan\n\n## Phase 1: Foundations\n\n- Set up the database\n- Wire the client\n\n\
         ## Phase 2: Features\n\n- Ship the dashboard\n",
    );
    differential.step(
        "decompose",
        Invocation::Decompose {
            file: Some(path),
            stdin: false,
            dry_run: false,
        },
    );
    assert_exports_agree(&differential, "export after a decompose");
}

#[test]
fn a_dry_run_decompose_agrees_and_writes_nothing() {
    let differential = Differential::new();
    let (_dir, path) = spec_file("PLAN.md", "# Plan\n\n- Do the thing\n");
    differential.step(
        "decompose --dry-run",
        Invocation::Decompose {
            file: Some(path),
            stdin: false,
            dry_run: true,
        },
    );
    assert_exports_agree(&differential, "export after a dry run");
}

#[test]
fn decompose_with_no_input_agrees_on_the_usage_error() {
    let differential = Differential::new();
    differential.step(
        "decompose with nothing",
        Invocation::Decompose {
            file: None,
            stdin: false,
            dry_run: false,
        },
    );
}

#[test]
fn import_project_diverges_on_a_project_that_already_has_stories() {
    // The legacy importer overwrites; the store refuses. Recorded as a
    // divergence rather than normalized away — an append-only store cannot
    // rewrite a story's history, and a restore that half-overwrites a live
    // project is how a tracker loses one.
    let differential = Differential::new();
    differential.step(
        "seed",
        Invocation::New {
            title: "In the way".into(),
            state: None,
            story_type: None,
            description: None,
            priority: None,
            labels: None,
            assignee: None,
        },
    );
    let document = match differential.store_only(Invocation::Export) {
        Ok(Response::RawJson(json)) => json,
        other => panic!("expected an export document, got {other:?}"),
    };
    let (_dir, path) = spec_file("export.json", &document);

    differential
        .legacy_only(Invocation::ImportProject { file: path.clone() })
        .expect("the legacy importer overwrites whatever it finds");
    let refused = differential
        .store_only(Invocation::ImportProject { file: path })
        .expect_err("the store refuses to overwrite");
    assert!(
        refused.to_string().contains("already holds stories"),
        "{refused}"
    );
}
