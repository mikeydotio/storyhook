//! gh-backed PR observations exercise the production subprocess and JSON boundary.
#![cfg(feature = "github-pr")]

use std::os::unix::fs::PermissionsExt;
use std::process::Command;
use storyhook::github::client::GithubClient;
use storyhook::github_access::Repository;

#[test]
fn production_pr_observation_validates_identity_and_response_shape() {
    if let Ok(root) = std::env::var("SH734_GH_CLIENT_ROOT") {
        let repository = Repository::resolve(std::path::Path::new(&root)).unwrap();
        let result = GithubClient::new(repository).get_pull_request(7);
        if std::env::var("SH734_GH_CLIENT_VALID").unwrap() == "true" {
            let status = result.unwrap();
            assert!(status.merged);
            assert_eq!(status.state, "closed");
        } else {
            assert!(result.is_err());
        }
        return;
    }
    for host in ["github.com", "github.example.com"] {
        let good = serde_json::json!({"number":7,"html_url":format!("https://{host}/acme/widgets/pull/7"),"state":"closed","merged":true});
        let mut foreign = good.clone();
        foreign["html_url"] = "https://foreign/acme/widgets/pull/7".into();
        let mut renamed = good.clone();
        renamed["html_url"] = format!("https://{host}/acme/moved/pull/7").into();
        let mut wrong_number = good.clone();
        wrong_number["number"] = 8.into();
        let mut invalid_state = good.clone();
        invalid_state["state"] = "open".into();
        for (answer, valid) in [
            (good.to_string(), true),
            (foreign.to_string(), false),
            (renamed.to_string(), false),
            (wrong_number.to_string(), false),
            (invalid_state.to_string(), false),
            ("{}".into(), false),
            ("not json".into(), false),
        ] {
            let root = tempfile::tempdir_in("/tmp").unwrap();
            for args in [
                vec!["init", "--quiet"],
                vec![
                    "config",
                    "remote.origin.url",
                    &format!("https://{host}/acme/widgets.git"),
                ],
            ] {
                assert!(
                    Command::new("git")
                        .arg("-C")
                        .arg(root.path())
                        .args(args)
                        .status()
                        .unwrap()
                        .success()
                );
            }
            let bin = root.path().join("bin");
            std::fs::create_dir(&bin).unwrap();
            std::fs::write(root.path().join("response.json"), answer).unwrap();
            std::fs::write(
                bin.join("gh"),
                format!(
                    "#!/bin/sh\n[ \"$GH_HOST\" = '{host}' ] || exit 99\ncat '{}'\n",
                    root.path().join("response.json").display()
                ),
            )
            .unwrap();
            std::fs::set_permissions(bin.join("gh"), std::fs::Permissions::from_mode(0o755))
                .unwrap();
            let output = Command::new(std::env::current_exe().unwrap())
                .env_clear()
                .envs(storyhook_test_support::daemon_containment())
                .env("HOME", root.path())
                .env("TMPDIR", "/tmp")
                .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("SH734_GH_CLIENT_ROOT", root.path())
                .env("SH734_GH_CLIENT_VALID", valid.to_string())
                .args([
                    "--exact",
                    "production_pr_observation_validates_identity_and_response_shape",
                    "--nocapture",
                ])
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}
