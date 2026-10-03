//! Isolated RV-10 records exercise the actual embedded discovery bridge.
use super::*;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fs;
use std::os::unix::{ffi::OsStrExt, fs::PermissionsExt};

#[cfg(test)]
/// Private activation records and explicitly owned disposable test servers.
pub(crate) struct Fixture {
    /// The fixture's disposable home and state root.
    pub(crate) root: tempfile::TempDir,
    /// Isolated selectors and declared subprocess patience.
    pub(crate) env: Environment,
    /// The logical public socket, which may name a foreign server.
    pub(crate) socket: PathBuf,
    /// The generation's never-reused private endpoint.
    pub(crate) endpoint: PathBuf,
    /// RV-10's canonical discovery record location.
    pub(crate) activation: PathBuf,
    /// The active record and default inspection answer.
    pub(crate) record: Value,
    servers: Vec<PathBuf>,
}

impl Fixture {
    /// Create a protected registration without starting a tmux server.
    pub(crate) fn new() -> Self {
        let root = storyhook_test_support::scratch_dir();
        let home = root.path().canonicalize().unwrap();
        let env = Environment::at(&home).with_subprocess_patience();
        let socket = home.join("logical");
        let generation = "a".repeat(32);
        let endpoint = home.join(format!(".rv-{generation}/s"));
        let executable = home.join("revivify");
        let state = home.join("snapshots");
        let activation = home
            .join(".local/state/tmux-revivify/activation")
            .join(format!(
                "{:x}.json",
                Sha256::digest(socket.as_os_str().as_bytes())
            ));
        fs::create_dir_all(activation.parent().unwrap()).unwrap();
        let record = json!({
            "version": 1, "active": true, "socket": socket, "executable": executable,
            "state_dir": state, "generation": generation, "endpoint": endpoint,
            "identity": {"host": "fixture", "boot": "fixture", "pid": 1, "start": "fixture"},
            "phase": "ready", "reservation_host": "fixture", "reservation_boot": "fixture",
            "history": [], "ownership_state": "reachable", "restore_ready": true,
            "generation_state_dir": state.join("generations").join(generation)
        });
        private_json(&activation, &record);
        private_json(&home.join("report.json"), &record);
        fs::write(&executable, r#"#!/usr/bin/env python3
import json, os, pathlib, sys
home = pathlib.Path(__file__).parent
(home / 'called.json').write_text(json.dumps({'argv': sys.argv[1:], 'environment': dict(os.environ)}))
print((home / 'report.json').read_text())
"#).unwrap();
        fs::set_permissions(executable, fs::Permissions::from_mode(0o700)).unwrap();
        Self {
            root,
            env,
            socket,
            endpoint,
            activation,
            record,
            servers: Vec::new(),
        }
    }

    /// One observation's graced test deadline.
    pub(crate) fn deadline(&self) -> Instant {
        Instant::now()
            + self
                .env
                .subprocess_bound(crate::service::engine::TMUX_TIMEOUT)
    }

    fn inspect(&self) -> Result<Target, AppError> {
        inspect(
            &self.env,
            Some(&self.socket),
            self.deadline(),
            &Cancellation::default(),
        )
    }

    fn call(&self) -> Value {
        serde_json::from_slice(&fs::read(self.root.path().join("called.json")).unwrap()).unwrap()
    }

    /// Start only an isolated test socket and retain it for exact teardown.
    pub(crate) fn start(&mut self, socket: &Path, window: &str) {
        fs::create_dir_all(socket.parent().unwrap()).unwrap();
        let mut command = Command::new("tmux");
        command
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap())
            .env("HOME", self.env.home())
            .args(["-f", "/dev/null", "-S"])
            .arg(socket)
            .args(["new-session", "-d", "-s", window, "-n", window, "-c"])
            .arg(self.env.home())
            .arg("sleep 600");
        self.servers.push(socket.to_owned());
        let output = crate::process::run_captured(
            command,
            self.env
                .subprocess_bound(crate::service::engine::TMUX_TIMEOUT),
        )
        .unwrap_or_else(|error| panic!("{}", error.detail()));
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        for socket in &self.servers {
            let mut command = Command::new("tmux");
            command.args(["-N", "-S"]).arg(socket).arg("kill-server");
            // A test may have already removed the listener to prove fail-closed
            // behavior. Never substitute an ambient socket during teardown.
            let _ = crate::process::run_captured(
                command,
                self.env
                    .subprocess_bound(crate::service::engine::TMUX_TIMEOUT),
            );
        }
    }
}

/// Publish a fixture-owned JSON record with RV-10's required permissions.
pub(crate) fn private_json(path: &Path, value: &Value) {
    fs::write(path, serde_json::to_vec(value).unwrap()).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}

#[test]
fn active_registration_pins_missing_logical_socket_by_inspection_only() {
    let fixture = Fixture::new();
    assert!(!fixture.socket.exists());
    let target = fixture.inspect().unwrap();
    assert!(target.protected);
    assert_eq!(target.socket, fixture.socket);
    assert_eq!(target.endpoint, fixture.endpoint);
    let mut command = Command::new("tmux");
    target.apply(&mut command, Some(&fixture.socket));
    assert_eq!(
        command.get_args().collect::<Vec<_>>(),
        ["-N".as_ref(), "-S".as_ref(), fixture.endpoint.as_os_str()]
    );
    let call = fixture.call();
    assert_eq!(
        call["argv"],
        json!(["server", "inspect", "--socket", fixture.socket, "--json"])
    );
    assert_eq!(call["environment"]["HOME"], json!(fixture.env.home()));
    for name in [
        "TMUX",
        "TMUX_PANE",
        "STORYHOOK_STORE_PATH",
        "GH_TOKEN",
        "GITHUB_TOKEN",
    ] {
        assert!(call["environment"].get(name).is_none(), "{name}: {call}");
    }
}

#[test]
fn absent_and_inactive_records_preserve_unmanaged_arguments() {
    let mut fixture = Fixture::new();
    fixture.record["active"] = json!(false);
    private_json(&fixture.activation, &fixture.record);
    for _ in 0..2 {
        let target = fixture.inspect().unwrap();
        assert!(!target.protected);
        let mut command = Command::new("tmux");
        target.apply(&mut command, None);
        assert_eq!(command.get_args().count(), 0);
        target.apply(&mut command, Some(&fixture.socket));
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            ["-S".as_ref(), fixture.socket.as_os_str()]
        );
        if fixture.activation.exists() {
            fs::remove_file(&fixture.activation).unwrap();
        }
    }
    assert!(!fixture.root.path().join("called.json").exists());
}

#[test]
fn protected_unknown_and_failed_readiness_are_errors() {
    let fixture = Fixture::new();
    for (key, value) in [
        ("ownership_state", json!("unknown")),
        ("restore_ready", json!(false)),
        ("phase", json!("failed")),
    ] {
        let mut record = fixture.record.clone();
        record[key] = value;
        private_json(&fixture.root.path().join("report.json"), &record);
        let error = fixture.inspect().unwrap_err().to_string();
        assert!(
            error.contains("revivify") && error.contains("restore-ready"),
            "{error}"
        );
    }
}

#[test]
fn expired_and_cancelled_inspection_never_runs_the_provider() {
    let fixture = Fixture::new();
    assert!(
        inspect(
            &fixture.env,
            Some(&fixture.socket),
            Instant::now(),
            &Cancellation::default()
        )
        .is_err()
    );
    let cancellation = Cancellation::default();
    cancellation.cancel();
    assert!(
        inspect(
            &fixture.env,
            Some(&fixture.socket),
            fixture.deadline(),
            &cancellation
        )
        .is_err()
    );
    assert!(!fixture.root.path().join("called.json").exists());
}

#[test]
fn native_resource_inventory_uses_private_endpoint_despite_foreign_public_server() {
    use crate::service::resources::tmux;
    let mut fixture = Fixture::new();
    fixture.start(&fixture.socket.clone(), "foreign");
    fixture.start(&fixture.endpoint.clone(), "SH-1");
    let (target, panes) = tmux::resolved_panes(
        &fixture.env,
        &fixture.socket,
        &std::collections::BTreeSet::from(["SH-1".into()]),
    )
    .unwrap();
    assert!(target.protected);
    assert_eq!(target.endpoint, fixture.endpoint);
    assert_eq!(panes.len(), 1);
    assert_eq!(panes[0].window_name, "SH-1");
    assert_eq!(panes[0].cwd, fixture.env.home());
    assert_eq!(fixture.call()["argv"][1], "inspect");
}

#[test]
fn protected_missing_endpoint_does_not_fall_back_to_public_resources() {
    use crate::service::engine::WindowProbe;
    use crate::service::resources::tmux;
    let mut fixture = Fixture::new();
    fixture.start(&fixture.socket.clone(), "SH-1");
    let names = std::collections::BTreeSet::from(["SH-1".into()]);
    let error = tmux::panes(&fixture.env, &fixture.socket, &names).unwrap_err();
    assert!(error.to_string().contains("has no endpoint"), "{error}");
    assert!(matches!(
        tmux::probe_story_panes(
            &fixture.env,
            &fixture.socket,
            &names,
            &Cancellation::default()
        ),
        WindowProbe::Unanswered { .. }
    ));
}
