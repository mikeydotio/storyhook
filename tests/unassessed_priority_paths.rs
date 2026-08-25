//! Source-derived fence around every production `StoryCreated` writer.
//!
//! SH-449 makes required type and priority events a service invariant. CLI,
//! REST, MCP, TUI, grouping, import and decompose all delegate to the two
//! service writers below; a new direct writer is the one way to bypass that
//! boundary. This scan makes such an addition fail until it is routed through
//! the invariant or documented as a compatibility-only path.

use std::path::Path;

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

fn relative(path: &str) -> String {
    path.rsplit_once("/src/")
        .map_or_else(|| path.to_string(), |(_, rest)| format!("src/{rest}"))
}

fn story_created_sites(files: &[(String, String)], allowed: &[(&str, &str)]) -> Vec<String> {
    const ALLOWED_PREFIX: &str = "src/store/";
    const MARKER: &str = "StoryEvent::StoryCreated {";

    let mut breaches = Vec::new();
    for (path, text) in files {
        let relative = relative(path);
        if relative.starts_with(ALLOWED_PREFIX) || allowed.iter().any(|(a, _)| *a == relative) {
            continue;
        }
        for (number, line) in text.lines().enumerate() {
            let code = line.trim_start();
            if code.starts_with("//") || code.starts_with('*') {
                continue;
            }
            if code.contains(MARKER) {
                breaches.push(format!("{relative}:{}: {}", number + 1, code.trim()));
            }
        }
    }
    breaches
}

/// Store code is separately exempt because conformance and migration fixtures
/// must construct legacy histories deliberately.
const ALLOWED: [(&str, &str); 5] = [
    (
        "src/domain.rs",
        "the enum, fold and domain fixtures; not a production creation door",
    ),
    (
        "src/service/story.rs",
        "creation_events is the shared live writer and always appends low/first-type defaults",
    ),
    (
        "src/service/transfer.rs",
        "import_events applies the same defaults; import-project normalizes legacy histories",
    ),
    (
        "src/storage.rs",
        "the compatibility rollback exporter writes a legacy tree, never a live current story",
    ),
    (
        "src/daemon/watch.rs",
        "test-only ChangeWatcher fixtures, not a production creation door",
    ),
];

#[test]
fn story_created_events_are_written_from_the_required_metadata_funnels() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    sources(&root, &mut files);
    assert!(files.len() > 20, "the source walk was unexpectedly empty");

    let breaches = story_created_sites(&files, &ALLOWED);
    assert!(
        breaches.is_empty(),
        "a new StoryCreated writer bypasses the required-metadata funnels. Route it through \
         service::story::creation_events or service::transfer::import_events, then document \
         it in ALLOWED.\n  {}\nThe allowlist today:\n  {}",
        breaches.join("\n  "),
        ALLOWED
            .iter()
            .map(|(file, why)| format!("{file} — {why}"))
            .collect::<Vec<_>>()
            .join("\n  ")
    );
}

#[test]
fn the_scan_detects_a_new_writer_and_ignores_comments_and_allowed_files() {
    let files = vec![
        (
            "/repo/src/service/new_door.rs".to_string(),
            "fn make() {\n StoryEvent::StoryCreated {\n  at: now,\n };\n}".to_string(),
        ),
        (
            "/repo/src/service/story.rs".to_string(),
            "// StoryEvent::StoryCreated {\nStoryEvent::StoryCreated {".to_string(),
        ),
    ];
    let allowed = [("src/service/story.rs", "the shared funnel")];

    let breaches = story_created_sites(&files, &allowed);
    assert_eq!(breaches.len(), 1, "{breaches:?}");
    assert!(breaches[0].starts_with("src/service/new_door.rs:2:"));
}
