//! `story attachment add|list|remove|save` — the SH-315 epic's storage-and-
//! CLI foundation child (SH-387).
//!
//! See `docs/spec/story-attachments.md` for the design of record.

use assert_cmd::Command;
use predicates::prelude::*;
use storyhook_test_support::{TestEnv, scratch_dir};

/// Every `story` invocation in this file runs in the shared test
/// environment's private HOME/XDG directories, so nothing here can reach the
/// developer's own storyhook state.
fn story(dir: &std::path::Path) -> Command {
    TestEnv::shared().story(dir)
}

/// The smallest byte sequence [`storyhook::domain::media_type::MediaType::sniff`]
/// recognises as a PNG — the signature alone, no real image data behind it.
/// `add`/`sniff` only ever look at the header, so this is a genuine input for
/// this suite, not a shortcut around it.
const PNG_BYTES: &[u8] = &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 1, 2, 3, 4];

fn write_png(dir: &std::path::Path, name: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, PNG_BYTES).expect("writing a fixture png");
    path
}

// ============================================================
// The happy path: add, list, save, remove
// ============================================================

#[test]
fn add_records_the_attachment_and_show_lists_it() {
    let dir = TestEnv::shared().project().build();
    story(dir.path())
        .args(["new", "with a screenshot"])
        .assert()
        .success();
    let png = write_png(dir.path(), "shot.png");

    story(dir.path())
        .args(["attachment", "add", "SH-1", png.to_str().unwrap()])
        .assert()
        .success();

    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("attachments:")
                .and(predicate::str::contains("shot.png"))
                .and(predicate::str::contains("image/png"))
                .and(predicate::str::contains(format!(
                    "{} bytes",
                    PNG_BYTES.len()
                ))),
        );
}

#[test]
fn add_accepts_a_name_override() {
    let dir = TestEnv::shared().project().build();
    story(dir.path())
        .args(["new", "with a named attachment"])
        .assert()
        .success();
    let png = write_png(dir.path(), "shot.png");

    story(dir.path())
        .args([
            "attachment",
            "add",
            "SH-1",
            png.to_str().unwrap(),
            "--name",
            "before/after diagram",
        ])
        .assert()
        .success();

    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("before/after diagram"));
}

#[test]
fn save_writes_back_byte_identical_bytes() {
    let dir = TestEnv::shared().project().build();
    story(dir.path())
        .args(["new", "roundtrip"])
        .assert()
        .success();
    let png = write_png(dir.path(), "shot.png");
    story(dir.path())
        .args(["attachment", "add", "SH-1", png.to_str().unwrap()])
        .assert()
        .success();

    let out_dir = scratch_dir();
    let restored = out_dir.path().join("restored.png");
    story(dir.path())
        .args([
            "attachment",
            "save",
            "SH-1",
            "1",
            restored.to_str().unwrap(),
        ])
        .assert()
        .success();

    let bytes = std::fs::read(&restored).expect("reading the restored file");
    assert_eq!(bytes, PNG_BYTES, "saved bytes must match what was attached");
}

#[test]
fn remove_deletes_the_attachment_and_a_second_removal_is_not_found() {
    let dir = TestEnv::shared().project().build();
    story(dir.path())
        .args(["new", "removable"])
        .assert()
        .success();
    let png = write_png(dir.path(), "shot.png");
    story(dir.path())
        .args(["attachment", "add", "SH-1", png.to_str().unwrap()])
        .assert()
        .success();

    story(dir.path())
        .args(["attachment", "remove", "SH-1", "1"])
        .assert()
        .success();

    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("attachments:").not());

    story(dir.path())
        .args(["attachment", "remove", "SH-1", "1"])
        .assert()
        .failure()
        .code(3)
        .stderr(predicate::str::contains("no attachment"));
}

#[test]
fn ids_never_reuse_once_an_attachment_is_removed() {
    let dir = TestEnv::shared().project().build();
    story(dir.path())
        .args(["new", "sequential ids"])
        .assert()
        .success();
    let png = write_png(dir.path(), "shot.png");
    story(dir.path())
        .args(["attachment", "add", "SH-1", png.to_str().unwrap()])
        .assert()
        .success();
    story(dir.path())
        .args(["attachment", "remove", "SH-1", "1"])
        .assert()
        .success();

    story(dir.path())
        .args(["attachment", "add", "SH-1", png.to_str().unwrap()])
        .assert()
        .success();

    // The second attachment must be id 2, not a reused 1 — save against the
    // stale id 1 must fail now that nothing holds it.
    story(dir.path())
        .args(["attachment", "remove", "SH-1", "1"])
        .assert()
        .failure();
    story(dir.path())
        .args(["attachment", "remove", "SH-1", "2"])
        .assert()
        .success();
}

// ============================================================
// Refusals
// ============================================================

#[test]
fn add_refuses_a_file_that_is_not_an_image() {
    let dir = TestEnv::shared().project().build();
    story(dir.path())
        .args(["new", "wrong format"])
        .assert()
        .success();
    let path = dir.path().join("notes.txt");
    std::fs::write(&path, b"just some plain text, not an image at all").unwrap();

    story(dir.path())
        .args(["attachment", "add", "SH-1", path.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not a supported image"));
}

#[test]
fn add_refuses_html_wearing_a_png_extension() {
    let dir = TestEnv::shared().project().build();
    story(dir.path())
        .args(["new", "disguised html"])
        .assert()
        .success();
    let path = dir.path().join("shot.png");
    std::fs::write(&path, b"<html><body>not a real png</body></html>").unwrap();

    story(dir.path())
        .args(["attachment", "add", "SH-1", path.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not a supported image"));
}

#[test]
fn add_refuses_an_svg() {
    let dir = TestEnv::shared().project().build();
    story(dir.path())
        .args(["new", "svg refused"])
        .assert()
        .success();
    let path = dir.path().join("shot.svg");
    std::fs::write(&path, b"<svg xmlns='http://www.w3.org/2000/svg'></svg>").unwrap();

    story(dir.path())
        .args(["attachment", "add", "SH-1", path.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not a supported image"));
}

#[test]
fn add_refuses_a_file_over_the_size_cap() {
    let dir = TestEnv::shared().project().build();
    story(dir.path())
        .args(["new", "too big"])
        .assert()
        .success();
    let path = dir.path().join("huge.png");
    let mut bytes = PNG_BYTES.to_vec();
    bytes.resize(10 * 1024 * 1024 + 1, 0);
    std::fs::write(&path, &bytes).unwrap();

    story(dir.path())
        .args(["attachment", "add", "SH-1", path.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicate::str::contains("over the"));
}

#[test]
fn add_refuses_a_missing_source_path() {
    let dir = TestEnv::shared().project().build();
    story(dir.path())
        .args(["new", "missing source"])
        .assert()
        .success();

    story(dir.path())
        .args(["attachment", "add", "SH-1", "does-not-exist.png"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("does-not-exist.png"));
}

#[test]
fn attachment_commands_refuse_a_story_that_does_not_exist() {
    let dir = TestEnv::shared().project().build();
    story(dir.path())
        .args(["attachment", "list", "SH-9"])
        .assert()
        .failure()
        .code(3);
}

#[test]
fn add_and_remove_refuse_a_closed_story() {
    let dir = TestEnv::shared().project().build();
    story(dir.path())
        .args(["new", "will be closed"])
        .assert()
        .success();
    let png = write_png(dir.path(), "shot.png");
    story(dir.path())
        .args(["attachment", "add", "SH-1", png.to_str().unwrap()])
        .assert()
        .success();
    story(dir.path())
        .args(["move", "SH-1", "done"])
        .assert()
        .success();

    story(dir.path())
        .args(["attachment", "add", "SH-1", png.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicate::str::contains("closed"));

    story(dir.path())
        .args(["attachment", "remove", "SH-1", "1"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("closed"));

    // Read-only access still works on a closed story, exactly as `story
    // show` does.
    story(dir.path())
        .args(["attachment", "list", "SH-1"])
        .assert()
        .success();
}

// ============================================================
// Grammar
// ============================================================

#[test]
fn a_trailing_word_on_any_form_is_refused() {
    let dir = TestEnv::shared().project().build();
    story(dir.path())
        .args(["new", "grammar"])
        .assert()
        .success();
    let png = write_png(dir.path(), "shot.png");
    story(dir.path())
        .args(["attachment", "add", "SH-1", png.to_str().unwrap()])
        .assert()
        .success();

    story(dir.path())
        .args(["attachment", "list", "SH-1", "extra"])
        .assert()
        .failure()
        .code(2);
    story(dir.path())
        .args(["attachment", "remove", "SH-1", "1", "extra"])
        .assert()
        .failure()
        .code(2);
}

#[test]
fn remove_refuses_a_non_numeric_attachment_id() {
    let dir = TestEnv::shared().project().build();
    story(dir.path()).args(["new", "bad id"]).assert().success();

    story(dir.path())
        .args(["attachment", "remove", "SH-1", "not-a-number"])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("positive integer"));
}

// ============================================================
// story doctor
// ============================================================

#[test]
fn doctor_reports_nothing_wrong_with_a_healthy_attachment() {
    let dir = TestEnv::shared().project().build();
    story(dir.path())
        .args(["new", "healthy"])
        .assert()
        .success();
    let png = write_png(dir.path(), "shot.png");
    story(dir.path())
        .args(["attachment", "add", "SH-1", png.to_str().unwrap()])
        .assert()
        .success();

    story(dir.path()).args(["doctor"]).assert().success();
}

// ============================================================
// story export / import-project
// ============================================================

#[test]
fn export_and_restore_carries_the_attachment_bytes() {
    let dir = TestEnv::shared().project().build();
    story(dir.path())
        .args(["new", "exported"])
        .assert()
        .success();
    let png = write_png(dir.path(), "shot.png");
    story(dir.path())
        .args(["attachment", "add", "SH-1", png.to_str().unwrap()])
        .assert()
        .success();

    let output = story(dir.path()).args(["export"]).assert().success();
    let export_json = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    let export_dir = scratch_dir();
    let export_file = export_dir.path().join("export.json");
    std::fs::write(&export_file, &export_json).expect("writing the export document");

    let restore_dir = TestEnv::shared().project().build();
    // `import-project` restores into an *empty* project, so the one this
    // builder already created has to go before the restore can proceed —
    // exactly as `story_export.rs`'s own roundtrip test does.
    std::fs::remove_file(restore_dir.path().join(".storyhook.toml")).unwrap();
    story(restore_dir.path())
        .args(["import-project", export_file.to_str().unwrap()])
        .assert()
        .success();

    let out_dir = scratch_dir();
    let restored = out_dir.path().join("restored.png");
    story(restore_dir.path())
        .args([
            "attachment",
            "save",
            "SH-1",
            "1",
            restored.to_str().unwrap(),
        ])
        .assert()
        .success();
    let bytes = std::fs::read(&restored).expect("reading the restored file");
    assert_eq!(bytes, PNG_BYTES);
}
