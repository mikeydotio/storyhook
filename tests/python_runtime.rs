//! SH-858: the gate's Python runtime cannot depend on the daemon client's PATH.

use std::path::Path;
use std::process::Command;

#[path = "support/foreign_repo.rs"]
mod foreign_repo;

use foreign_repo::{ForeignRepo, output, success};

#[test]
fn runtime_selection_and_infrastructure_refusals() {
    let output = Command::new("python3")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/tests/test_python_runtime.py"))
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .output()
        .expect("run Python runtime regressions");
    assert!(output.status.success(), "{output:?}");
}

#[test]
fn rust_spawned_python_inherits_the_selected_runtime() {
    if std::env::var_os("SH858_RUNTIME_CHILD").is_some() {
        success(output(Command::new("python3").args([
            "-c",
            "import os,sys; assert os.path.samefile(sys.executable, os.environ['STORYHOOK_PYTHON'])",
        ])));
        success(output(&mut storyhook::env::git_env::command(Path::new(
            ".",
        ))));
        let mut provider = Command::new("python3");
        storyhook::env::spawn_env::apply_plugin_cli_allowlist(&mut provider);
        success(output(provider.args([
            "-c",
            "import os,sys; assert os.path.samefile(sys.executable, os.environ['STORYHOOK_PYTHON'])",
        ])));
        return;
    }
    let root = storyhook_test_support::scratch_dir();
    let poison = root.path().join("python3");
    std::fs::write(&poison, "#!/bin/sh\nexit 91\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&poison, std::fs::Permissions::from_mode(0o755)).unwrap();
    let git = root.path().join("git");
    std::fs::write(
        &git,
        "#!/usr/bin/env python3\nimport os,sys\nassert os.path.samefile(sys.executable, os.environ['STORYHOOK_PYTHON'])\n",
    )
    .unwrap();
    std::fs::set_permissions(&git, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!(
        "{}:{}",
        root.path().display(),
        std::env::var("PATH").unwrap()
    );
    success(output(
        Command::new("/bin/bash")
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/python-runtime.sh"))
            .arg("--")
            .arg(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "rust_spawned_python_inherits_the_selected_runtime",
            ])
            .env("PATH", path)
            .env("SH858_RUNTIME_CHILD", "1"),
    ));
}

#[test]
fn foreign_gate_and_portable_writers_use_the_pinned_interpreter() {
    let fixture = ForeignRepo::new();
    let journal = fixture.root.path().join("journal");
    std::fs::write(&journal, "").unwrap();
    // Apple Python comes first on the incoming PATH, as in the reported gate.
    let path = format!("/usr/bin:/bin:{}", std::env::var("PATH").unwrap());
    let body = r#"
python3 -c 'import os,sys; assert sys.version_info >= (3,11); assert os.path.samefile(sys.executable, os.environ["STORYHOOK_PYTHON"])'
"$STORYHOOK_GATE_PROGRESS_WRITER" leg start runtime
"$STORYHOOK_GATE_RECEIPT" preflight
"$STORYHOOK_GATE_PROGRESS_WRITER" leg pass runtime
"$STORYHOOK_GATE_RECEIPT" postlude gate
"#;
    success(output(
        fixture
            .speculative_run(body, &[])
            .env("PATH", path)
            .env_remove("STORYHOOK_PYTHON")
            .env("STORYHOOK_GATE_PROGRESS", &journal),
    ));
    assert!(
        std::fs::read_to_string(journal)
            .unwrap()
            .contains("release gate/runtime")
    );
    assert_eq!(success(fixture.preflight()), fixture.tree);
    fixture.assert_restored();
}

#[test]
fn unsupported_runtime_is_infrastructure_but_real_test_failure_stays_red() {
    let fixture = ForeignRepo::new();
    let marker = fixture.root.path().join("gate-ran");
    let mut command = fixture.command("verify-pr.sh", &fixture.repo);
    command.args([
        "--run-gate",
        "1",
        &fixture.tree,
        &fixture.base,
        &fixture.head,
        fixture.poller.to_str().unwrap(),
        "--",
        "bash",
        "-c",
        "touch \"$1\"; printf 'test sh858_control ... FAILED\\n'; exit 7",
        "gate",
        marker.to_str().unwrap(),
    ]);
    command.env(
        "STORYHOOK_PYTHON",
        fixture.root.path().join("missing-python"),
    );
    let refused: serde_json::Value = serde_json::from_str(&success(output(&mut command))).unwrap();
    assert_eq!(refused["result"], "infrastructure-failure", "{refused}");
    assert_eq!(refused["disposition"], "permanent");
    assert!(!marker.exists());
    assert!(
        !fixture
            .repo
            .join(".git/storyhook/verifier-lifecycle")
            .exists()
    );
    assert!(
        !fixture
            .repo
            .join(".git/storyhook/verification-executions")
            .exists()
    );
    assert_eq!(fixture.preflight().status.code(), Some(1));
    fixture.assert_restored();

    command.env_remove("STORYHOOK_PYTHON");
    let red: serde_json::Value = serde_json::from_str(&success(output(&mut command))).unwrap();
    assert_eq!(red["result"], "tests-failed", "{red}");
    assert_eq!(red["exit_status"], 7);
    assert!(marker.exists());
    assert_eq!(fixture.preflight().status.code(), Some(1));
    fixture.assert_restored();
}
