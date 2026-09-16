//! Bootstrap the actual installer with controlled release endpoints.
use std::{fs, os::unix::fs::PermissionsExt, path::Path, process::Command};
use storyhook_test_support::{TestEnv, scratch_dir};

fn script(path: &Path, text: &str) {
    fs::write(path, text).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn bootstrap_requires_source_and_persists_enterprise_authority() {
    let dir = scratch_dir();
    let bin = dir.path().join("bin");
    fs::create_dir(&bin).unwrap();
    script(
        &bin.join("curl"),
        "#!/bin/sh\necho forbidden-direct-http >&2\nexit 91\n",
    );
    script(
        &bin.join("gh"),
        "#!/bin/sh\nprintf '%s|%s|%s\\n' \"$GH_HOST\" \"$GH_REPO\" \"$*\" >> \"$(dirname \"$0\")/calls\"\ncase \"$1 $2\" in\n'release view') printf '{\"tagName\":\"v99.0.0\"}' ;;\n'release download') while [ \"$1\" != --output ]; do shift; done; cp \"$(dirname \"$0\")/asset.tar.gz\" \"$2\" ;;\n*) exit 92;;\nesac\n",
    );
    let asset = dir.path().join("asset");
    fs::create_dir(&asset).unwrap();
    script(
        &asset.join("story"),
        "#!/bin/sh\ncase \"$*\" in\n--help) exit 0;;\n'plugin reinstall') echo called-reinstall; exit 2;;\n*) exit 93;;\nesac\n",
    );
    assert!(
        Command::new("tar")
            .arg("czf")
            .arg(bin.join("asset.tar.gz"))
            .arg("-C")
            .arg(&asset)
            .arg("story")
            .status()
            .unwrap()
            .success()
    );
    let install = dir.path().join("install");
    let run = |args: &[&str]| {
        let mut command = Command::new("sh");
        TestEnv::shared().apply(&mut command);
        command
            .current_dir(dir.path())
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("install.sh"))
            .args(args)
            .env("STORYHOOK_INSTALL_DIR", &install)
            .env_remove("STORYHOOK_VERSION")
            .env("GH_HOST", "wrong.example")
            .env("GH_REPO", "wrong.example/other/repo")
            .env(
                "PATH",
                format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
            )
            .output()
            .unwrap()
    };
    let missing = run(&[]);
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("--source"));
    assert!(!bin.join("calls").exists());
    for source in [
        "owner/repo",
        "https://host/owner/repo",
        "host/../repo",
        "user:secret@host/owner/repo",
    ] {
        let output = run(&["--source", source]);
        assert!(!output.status.success());
        assert!(!String::from_utf8_lossy(&output.stderr).contains("secret"));
        assert!(!bin.join("calls").exists());
    }
    let output = run(&["--source", "github.pie.apple.com/acme/storyhook"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("called-reinstall"));
    assert!(String::from_utf8_lossy(&output.stderr).contains("story plugin reinstall"));
    let metadata: serde_json::Value =
        serde_json::from_slice(&fs::read(install.join("story.source.json")).unwrap()).unwrap();
    assert_eq!(metadata["source"], "github.pie.apple.com/acme/storyhook");
    use sha2::{Digest, Sha256};
    assert_eq!(
        metadata["sha256"],
        format!(
            "{:x}",
            Sha256::digest(fs::read(install.join("story")).unwrap())
        )
    );
    let calls = fs::read_to_string(bin.join("calls")).unwrap();
    assert!(
        calls
            .lines()
            .all(|l| l.starts_with("github.pie.apple.com|github.pie.apple.com/acme/storyhook|"))
    );
    assert!(
        calls
            .lines()
            .all(|l| l.contains("--repo github.pie.apple.com/acme/storyhook"))
    );
    let before = fs::read(install.join("story")).unwrap();
    let before_metadata = fs::read(install.join("story.source.json")).unwrap();
    script(&asset.join("story"), "#!/bin/sh\nexit 1\n");
    assert!(
        Command::new("tar")
            .arg("czf")
            .arg(bin.join("asset.tar.gz"))
            .arg("-C")
            .arg(&asset)
            .arg("story")
            .status()
            .unwrap()
            .success()
    );
    let failed = run(&["--source", "github.com/other/storyhook"]);
    assert!(!failed.status.success());
    assert_eq!(fs::read(install.join("story")).unwrap(), before);
    assert_eq!(
        fs::read(install.join("story.source.json")).unwrap(),
        before_metadata
    );
}
