//! Durable work context belongs in StoryHook, not in a tracked session note (SH-588).

use std::fs;
use std::path::Path;

#[test]
fn the_repository_keeps_root_handoff_notes_local() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    assert!(
        !root.join("HANDOFF.md").exists(),
        "durable handoff context belongs in story comments; HANDOFF.md must remain local"
    );

    let gitignore = fs::read_to_string(root.join(".gitignore")).expect("reading .gitignore");
    assert!(
        gitignore.lines().any(|line| line.trim() == "/HANDOFF.md"),
        "the root HANDOFF.md convention must be explicitly ignored"
    );
}
