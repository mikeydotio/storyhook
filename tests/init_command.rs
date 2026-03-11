use assert_cmd::Command;
use tempfile::tempdir;

#[test]
fn init_creates_storyhook_layout() {
    let dir = tempdir().unwrap();
    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir.path())
        .arg("init")
        .assert()
        .success();

    assert!(dir.path().join(".storyhook/project.toml").exists());
    assert!(dir.path().join(".storyhook/states.toml").exists());
    assert!(dir.path().join(".storyhook/open/stories").exists());
    assert!(dir.path().join(".storyhook/archive/archive.db").exists());
}
