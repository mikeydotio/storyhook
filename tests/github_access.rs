//! Production origin resolution with real isolated Git checkouts.
use std::path::Path;
use std::process::Command;
use storyhook::github_access::Repository;

fn git(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn checkout(origin: &str) -> tempfile::TempDir {
    let root = tempfile::tempdir_in("/tmp").unwrap();
    git(root.path(), &["init", "--quiet"]);
    git(root.path(), &["remote", "add", "origin", origin]);
    root
}

#[test]
fn public_and_enterprise_origins_keep_their_destination() {
    for host in ["github.com", "github.example.com"] {
        for origin in [
            format!("https://{host}/Acme/Widgets.git"),
            format!("git@{host}:Acme/Widgets"),
            format!("ssh://git@{host}/Acme/Widgets.git"),
            format!("ssh://git@{host}:22/Acme/Widgets.git"),
        ] {
            let root = checkout(&origin);
            let resolved = Repository::resolve(root.path()).unwrap();
            assert_eq!(resolved.qualified(), format!("{host}/acme/widgets"));
            assert_eq!(
                resolved.transport_url(),
                format!("https://{host}/acme/widgets.git")
            );
        }
    }
}

#[test]
fn https_port_is_preserved_and_ssh_port_is_not_guessed() {
    let root = checkout("https://github.example.com:8443/acme/widgets.git");
    assert_eq!(
        Repository::resolve(root.path()).unwrap().identity().host,
        "github.example.com:8443"
    );
    git(
        root.path(),
        &[
            "remote",
            "set-url",
            "origin",
            "ssh://git@github.example.com:2222/acme/widgets",
        ],
    );
    assert!(
        Repository::resolve(root.path())
            .unwrap_err()
            .to_string()
            .contains("HTTPS")
    );
}

#[test]
fn changed_origin_is_read_again_instead_of_registered_or_cached_identity() {
    let root = checkout("https://github.com/old/widgets.git");
    assert_eq!(
        Repository::resolve(root.path()).unwrap().qualified(),
        "github.com/old/widgets"
    );
    git(
        root.path(),
        &[
            "remote",
            "set-url",
            "origin",
            "git@github.example.com:new/moved.git",
        ],
    );
    assert_eq!(
        Repository::resolve(root.path()).unwrap().qualified(),
        "github.example.com/new/moved"
    );
}

#[test]
fn unsupported_and_credential_bearing_origins_are_refused_without_secrets() {
    for origin in [
        "http://host/acme/widgets",
        "git://host/acme/widgets",
        "/tmp/local.git",
        "https://user:never-print-me@host/acme/widgets",
        "https://host/acme/widgets?secret=never-print-me",
        "https://host/acme/widgets/tree/main",
    ] {
        let root = checkout(origin);
        let error = Repository::resolve(root.path()).unwrap_err().to_string();
        assert!(!error.contains("never-print-me"), "{error}");
    }
}

#[test]
fn missing_checkout_missing_origin_and_multiple_origins_fail_closed() {
    let root = checkout("https://host/acme/widgets");
    assert!(Repository::resolve(&root.path().join("missing")).is_err());
    git(root.path(), &["remote", "remove", "origin"]);
    assert!(Repository::resolve(root.path()).is_err());
    git(
        root.path(),
        &[
            "config",
            "--add",
            "remote.origin.url",
            "https://host/acme/widgets",
        ],
    );
    git(
        root.path(),
        &[
            "config",
            "--add",
            "remote.origin.url",
            "https://other/acme/widgets",
        ],
    );
    assert!(Repository::resolve(root.path()).is_err());
}

#[test]
fn a_directory_inside_an_unrelated_checkout_is_not_a_checkout() {
    let root = checkout("https://host/acme/widgets");
    let child = root.path().join("missing-project");
    std::fs::create_dir(&child).unwrap();
    assert!(Repository::resolve(&child).is_err());
}

#[test]
fn raw_origin_is_not_changed_by_transport_rewrites() {
    let root = checkout("git@github.example.com:acme/widgets.git");
    git(
        root.path(),
        &[
            "config",
            "url.https://unrelated.invalid/.insteadOf",
            "git@github.example.com:",
        ],
    );
    assert_eq!(
        Repository::resolve(root.path()).unwrap().identity().host,
        "github.example.com"
    );
}

#[test]
fn linked_worktree_uses_its_actual_shared_origin() {
    let root = checkout("git@github.example.com:acme/widgets.git");
    git(
        root.path(),
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "--allow-empty",
            "-m",
            "fixture",
        ],
    );
    let lane = root.path().join("lane");
    git(
        root.path(),
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "lane",
            lane.to_str().unwrap(),
        ],
    );
    assert_eq!(
        Repository::resolve(&lane).unwrap().identity().host,
        "github.example.com"
    );
}
