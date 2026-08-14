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

use std::path::Path;

/// Where an uncoupled `Store::snapshot` is legitimate, and why.
///
/// `src/daemon/backup.rs` is the whole of it: the daily schedule and `story
/// store backup` both take a copy that no write depends on, which is exactly
/// what `Store::snapshot` promises. `src/store/` is not listed because it is
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

#[test]
fn a_copy_a_write_depends_on_comes_from_write_with_snapshot() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut offenders = Vec::new();

    for (path, text) in tracked_sources(root) {
        if UNCOUPLED_BACKUP_SITES.contains(&path.as_str()) || path.starts_with("src/store/") {
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
