//! Production plugin mutations with isolated providers, homes, and daemons.

use super::*;
use serde_json::Value;

fn current(h: &Harness, provider: &str) -> PathBuf {
    h.home.join(format!(
        "data/storyhook/provider-installs/{provider}-operations/current.json"
    ))
}

fn record(h: &Harness, provider: &str) -> Value {
    let path = current(h, provider);
    serde_json::from_slice(&fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display())))
        .unwrap()
}

#[test]
fn operations_publish_terminal_evidence_and_archive_the_previous_attempt() {
    for provider in ["claude", "codex"] {
        let h = Harness::for_provider(provider);
        assert!(h.run(&["plugin", "install", provider]).status.success());
        let installed = record(&h, provider);
        assert_eq!(installed["version"], 1);
        assert_eq!(installed["verb"], "install");
        assert_eq!(installed["outcome"], "succeeded");
        assert_eq!(installed["actor"]["build"], "test");
        assert_eq!(installed["actor"]["override_set"], true);
        assert_eq!(installed["pid"], h.daemon.borrow().as_ref().unwrap().pid());
        assert!(
            installed["steps"]
                .as_array()
                .unwrap()
                .iter()
                .any(|s| s["action"].as_str().unwrap().contains("marketplace remove"))
        );
        assert!(
            installed["steps"]
                .as_array()
                .unwrap()
                .iter()
                .all(|s| s["completed_at"].is_string())
        );
        let reinstalled = h.run(&["plugin", "reinstall"]);
        assert!(reinstalled.status.success(), "{}", combined(&reinstalled));
        assert_eq!(record(&h, provider)["verb"], "reinstall");
        assert!(h.run(&["plugin", "uninstall", provider]).status.success());
        let removed = record(&h, provider);
        assert_eq!(removed["outcome"], "succeeded");
        assert_eq!(removed["verb"], "uninstall");
        let history = current(&h, provider).parent().unwrap().join("history");
        let archived: Vec<Value> = fs::read_dir(history)
            .unwrap()
            .map(|p| serde_json::from_slice(&fs::read(p.unwrap().path()).unwrap()).unwrap())
            .collect();
        assert!(archived.contains(&installed));
        let doctor = combined(&h.run(&["doctor", "install"]));
        assert!(!doctor.contains("INCOMPLETE PLUGIN OPERATION"), "{doctor}");
    }
}

#[test]
fn operation_evidence_failure_prevents_provider_mutation() {
    let h = Harness::for_provider("claude");
    let path = current(&h, "claude");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::create_dir(&path).unwrap();
    let output = h.run(&["plugin", "install", "claude"]);
    assert!(!output.status.success(), "{}", combined(&output));
    assert!(!h.provider_log("claude").contains("plugin "));
}

#[test]
fn failed_uninstall_keeps_the_install_receipt_and_names_the_phase() {
    let h = Harness::for_provider("claude");
    assert!(h.run(&["plugin", "install", "claude"]).status.success());
    let receipt = h.home.join("data/storyhook/provider-installs/claude");
    let before = fs::read(&receipt).unwrap();
    h.set_mode("claude", "uninstall-fail");
    let output = h.run(&["plugin", "uninstall", "claude"]);
    assert!(!output.status.success(), "{}", combined(&output));
    assert_eq!(fs::read(receipt).unwrap(), before);
    assert_eq!(record(&h, "claude")["outcome"], "failed");
    let doctor = combined(&h.run(&["doctor", "install"]));
    assert!(doctor.contains("FAILED PLUGIN OPERATION"), "{doctor}");
    assert!(
        doctor.contains("plugin uninstall story@storyhook"),
        "{doctor}"
    );
    let failed = record(&h, "claude");
    h.set_mode("claude", "");
    assert!(h.run(&["plugin", "install", "claude"]).status.success());
    assert!(!combined(&h.run(&["doctor", "install"])).contains("FAILED PLUGIN OPERATION"));
    let history = current(&h, "claude").parent().unwrap().join("history");
    assert!(fs::read_dir(history).unwrap().any(|p| {
        serde_json::from_slice::<Value>(&fs::read(p.unwrap().path()).unwrap()).unwrap() == failed
    }));
}

#[test]
fn killed_registration_leaves_evidence_without_an_uninstall_tombstone() {
    for verb in ["install", "uninstall"] {
        let h = Harness::for_provider("claude");
        assert!(h.run(&["plugin", "install", "claude"]).status.success());
        let receipt = h.home.join("data/storyhook/provider-installs/claude");
        let before = fs::read(&receipt).unwrap();
        // The provider removes its own fixture registration, then kills only
        // its parent: this harness's owned daemon. No timing race is needed.
        let fake = FAKE_CLAUDE.replace(
            "printf '{}\\n' > \"$HOME/.claude/plugins/known_marketplaces.json\"",
            "printf '{}\\n' > \"$HOME/.claude/plugins/known_marketplaces.json\"\nkill -KILL \"$PPID\"",
        );
        assert_ne!(fake, FAKE_CLAUDE);
        h.install_fake("claude", &fake);
        let output = h.run(&["plugin", verb, "claude"]);
        assert!(!output.status.success(), "{}", combined(&output));
        assert_eq!(fs::read(&receipt).unwrap(), before);
        assert!(
            !h.home.join("claude-installed").exists(),
            "{verb}: interrupted operation did not remove the plugin"
        );
        assert_eq!(
            h.registered_source("claude"),
            None,
            "{verb}: interrupted operation did not remove the marketplace"
        );
        let evidence = record(&h, "claude");
        assert_eq!(evidence["outcome"], "incomplete");
        assert_eq!(evidence["verb"], verb);
        let phase = evidence["steps"].as_array().unwrap().last().unwrap();
        assert!(
            phase["action"]
                .as_str()
                .unwrap()
                .contains("marketplace remove")
        );
        assert!(phase["completed_at"].is_null());
        // Doctor is a local read. Do not restart the killed daemon just to
        // inspect the evidence left by it.
        let mut command = Command::new(&h.story);
        command.args(["doctor", "install"]);
        h.configure_command(&mut command);
        let doctor = combined(&command.output().unwrap());
        assert!(doctor.contains("DEREGISTERED"), "{doctor}");
        assert!(doctor.contains("INCOMPLETE PLUGIN OPERATION"), "{doctor}");
        assert!(doctor.contains("marketplace remove storyhook"), "{doctor}");
        assert!(!doctor.contains("killed by"), "{doctor}");
    }
}

#[test]
fn malformed_operation_evidence_is_diagnostic_even_when_registered() {
    let h = Harness::for_provider("claude");
    assert!(h.run(&["plugin", "install", "claude"]).status.success());
    let installed = record(&h, "claude");
    let mut bodies = vec!["{".into(), "{}".into()];
    for (key, value) in [
        ("version", serde_json::json!(99)),
        ("home", serde_json::json!("/elsewhere")),
        ("verb", serde_json::json!("unknown")),
        ("completed_at", Value::Null),
    ] {
        let mut broken = installed.clone();
        broken[key] = value;
        bodies.push(serde_json::to_string(&broken).unwrap());
    }
    for body in bodies {
        let path = current(&h, "claude");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, &body).unwrap();
        let doctor = combined(&h.run(&["doctor", "install"]));
        assert!(doctor.contains("operation evidence"), "{doctor}");
        assert!(doctor.contains(&path.display().to_string()), "{doctor}");
        assert_eq!(fs::read_to_string(path).unwrap(), body);
    }
}

#[test]
fn evidence_retains_both_the_install_failure_and_the_failed_rollback() {
    for (provider, mode) in [("claude", "plugin-install-fail"), ("codex", "plugin-fail")] {
        let h = Harness::for_provider(provider);
        h.seed_previous_registration(provider);
        h.set_mode(provider, mode);
        let output = h.run(&["plugin", "install", provider]);
        let message = combined(&output);
        assert!(!output.status.success(), "{message}");
        assert!(message.contains("AND failed to re-register"), "{message}");
        let evidence = record(&h, provider);
        assert_eq!(evidence["outcome"], "failed");
        assert!(
            evidence["error"]
                .as_str()
                .unwrap()
                .contains("AND failed to re-register")
        );
        assert_eq!(
            evidence["steps"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|s| s["action"].as_str().unwrap().contains("marketplace remove"))
                .count(),
            2
        );
    }
}

#[test]
fn loss_of_evidence_storage_after_a_provider_call_stops_the_next_effect() {
    let h = Harness::for_provider("claude");
    let fake = FAKE_CLAUDE.replace(
        "rm -f \"$HOME/claude-installed\"",
        "rm -f \"$HOME/claude-installed\"\np=\"$HOME/data/storyhook/provider-installs/claude-operations/current.json\"\nmv \"$p\" \"$p.saved\"\nmkdir \"$p\"\necho 'provider failure after removal' >&2\nexit 19",
    );
    assert_ne!(fake, FAKE_CLAUDE);
    h.install_fake("claude", &fake);
    let output = h.run(&["plugin", "install", "claude"]);
    let message = combined(&output);
    assert!(!output.status.success(), "{message}");
    assert!(
        message.contains("provider failure after removal"),
        "{message}"
    );
    assert!(message.contains("operation evidence"), "{message}");
    assert!(!h.provider_log("claude").contains("marketplace remove"));
}

#[test]
fn a_provider_home_lock_refuses_mutations_even_with_another_data_directory() {
    use fs4::FileExt;
    let h = Harness::for_provider("claude");
    let lock_path = h
        .home
        .join(".local/state/storyhook/provider-locks/claude.lock");
    fs::create_dir_all(lock_path.parent().unwrap()).unwrap();
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&lock_path)
        .unwrap();
    lock.lock_exclusive().unwrap();
    let output = h.run(&["plugin", "install", "claude"]);
    assert!(!output.status.success(), "{}", combined(&output));
    assert!(
        combined(&output).contains("another plugin operation"),
        "{}",
        combined(&output)
    );
    assert!(!h.provider_log("claude").contains("plugin "));
    assert!(!current(&h, "claude").exists());
    drop(lock);
    assert!(h.run(&["plugin", "install", "claude"]).status.success());
}

#[test]
fn unexplained_loss_is_not_attributed_to_the_last_successful_install() {
    let h = Harness::for_provider("claude");
    assert!(h.run(&["plugin", "install", "claude"]).status.success());
    fs::write(h.home.join(".claude/plugins/known_marketplaces.json"), "{}").unwrap();
    let doctor = combined(&h.run(&["doctor", "install"]));
    assert!(doctor.contains("DEREGISTERED"), "{doctor}");
    assert!(doctor.contains("cause unknown"), "{doctor}");
    assert!(!doctor.contains("INCOMPLETE PLUGIN OPERATION"), "{doctor}");
}
