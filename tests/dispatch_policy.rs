//! Policy inheritance must be field-specific, durable, and atomic.
use serde_json::{Value, json};
use storyhook_test_support::{TestEnv, scratch_dir};

#[test]
fn all_builtin_rows_and_provider_constraints_are_consistent() {
    use storyhook::domain::Complexity;
    use storyhook::service::dispatch_policy::{patch, show};
    use storyhook::store::EngineAgent;
    let fixture = storyhook_test_support::ServiceFixture::new();
    let document = show(fixture.store(), Some(fixture.project()), None).unwrap();
    assert_eq!(document.entries.len(), 6);
    for row in document.entries {
        let expected = match row.complexity {
            Complexity::Low => "medium",
            Complexity::Medium => "high",
            Complexity::High => "xhigh",
        };
        assert_eq!(row.effort, expected);
        assert_eq!(
            row.model,
            if row.agent == EngineAgent::Codex {
                "gpt-6-astra"
            } else {
                "fable"
            }
        );
        assert_eq!(row.model_source, "builtin");
        assert_eq!(row.effort_source, "builtin");
        for effort in ["low", "medium", "high", "xhigh", "max"] {
            patch(
                fixture.store(),
                None,
                row.agent,
                row.complexity,
                json!({"effort":effort}).as_object().unwrap(),
            )
            .unwrap();
        }
        for fields in [
            json!({"model":"haiku"}),
            json!({"effort":"extreme"}),
            json!({"effort":5}),
            json!({"surprise":null}),
        ] {
            assert!(
                patch(
                    fixture.store(),
                    None,
                    row.agent,
                    row.complexity,
                    fields.as_object().unwrap()
                )
                .is_err()
            );
        }
    }
    for effort in ["none", "ultra"] {
        assert!(
            patch(
                fixture.store(),
                None,
                EngineAgent::Claude,
                Complexity::High,
                json!({"effort":effort}).as_object().unwrap()
            )
            .is_err()
        );
        patch(
            fixture.store(),
            None,
            EngineAgent::Codex,
            Complexity::High,
            json!({"effort":effort}).as_object().unwrap(),
        )
        .unwrap();
    }
}

#[test]
fn project_overrides_do_not_change_installation_or_other_projects() {
    use storyhook::domain::Complexity;
    use storyhook::service::dispatch_policy::{patch, show};
    use storyhook::store::{EngineAgent, ReadOps, Store, WriteOps};
    let fixture = storyhook_test_support::ServiceFixture::new();
    let other = fixture
        .store()
        .write(|tx| {
            tx.create_project(&storyhook::store::NewProject {
                uuid: "policy-other".into(),
                slug: "policy-other".into(),
                name: "Other".into(),
                prefix: "OT".into(),
                created_at: storyhook_test_support::FIXTURE_NOW.into(),
            })
        })
        .unwrap();
    patch(
        fixture.store(),
        None,
        EngineAgent::Claude,
        Complexity::Low,
        json!({"model":"opus","effort":"low"}).as_object().unwrap(),
    )
    .unwrap();
    patch(
        fixture.store(),
        Some(fixture.project()),
        EngineAgent::Claude,
        Complexity::Low,
        json!({"effort":"high"}).as_object().unwrap(),
    )
    .unwrap();
    let peer = show(fixture.store(), Some(other), None)
        .unwrap()
        .entries
        .into_iter()
        .find(|r| r.agent == EngineAgent::Claude && r.complexity == Complexity::Low)
        .unwrap();
    assert_eq!(peer.effort, "low");
    assert_eq!(peer.model, "opus");
    assert_eq!(peer.effort_source, "installation");
    let reopened = storyhook::store::SqliteStore::open(fixture.store().path()).unwrap();
    let saved = reopened
        .read(|tx| {
            tx.dispatch_policy(
                Some(fixture.project()),
                EngineAgent::Claude,
                Complexity::Low,
            )
        })
        .unwrap();
    assert_eq!(saved.effort.as_deref(), Some("high"));
    assert!(saved.model.is_none());
}

#[test]
fn cli_policy_inherits_and_resets_each_field() {
    let env = TestEnv::isolated();
    let dir = scratch_dir();
    env.story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    env.story(dir.path())
        .args(["new", "Policy target", "--complexity", "low"])
        .assert()
        .success();
    let resolve = || -> Value {
        let output = env
            .story(dir.path())
            .args([
                "dispatch-policy",
                "resolve",
                "SH-1",
                "--agent",
                "codex",
                "--json",
            ])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        serde_json::from_slice::<Value>(&output).unwrap()["dispatch_policy"]["resolved"].clone()
    };
    assert_eq!(resolve()["model"], "gpt-6-astra");
    assert_eq!(resolve()["effort"], "medium");
    env.story(dir.path())
        .args([
            "dispatch-policy",
            "set",
            "--global",
            "--agent",
            "codex",
            "--complexity",
            "low",
            "--model",
            "gpt-5.6-sol",
        ])
        .assert()
        .success();
    env.story(dir.path())
        .args([
            "dispatch-policy",
            "set",
            "--agent",
            "codex",
            "--complexity",
            "low",
            "--effort",
            "high",
        ])
        .assert()
        .success();
    let resolved = resolve();
    assert_eq!(resolved["model"], "gpt-5.6-sol");
    assert_eq!(resolved["model_source"], "installation");
    assert_eq!(resolved["effort_source"], "project");
    env.story(dir.path())
        .args([
            "dispatch-policy",
            "set",
            "--agent",
            "codex",
            "--complexity",
            "low",
            "--model",
            "gpt-5.6-luna",
            "--effort",
            "low",
        ])
        .assert()
        .failure();
    assert_eq!(resolve(), resolved, "invalid model must not change effort");
    env.story(dir.path())
        .args([
            "dispatch-policy",
            "reset",
            "--agent",
            "codex",
            "--complexity",
            "low",
            "--effort",
        ])
        .assert()
        .success();
    assert_eq!(resolve()["effort"], json!("medium"));
}

#[test]
fn installation_policy_needs_no_project() {
    let env = TestEnv::isolated();
    let dir = scratch_dir();
    env.story(dir.path())
        .args([
            "dispatch-policy",
            "set",
            "--global",
            "--agent",
            "claude",
            "--complexity",
            "low",
            "--model",
            "opus",
        ])
        .assert()
        .success();
    let output = env
        .story(dir.path())
        .args(["dispatch-policy", "show", "--global", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let document: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(document["dispatch_policy"]["scope"], "installation");
    assert!(
        document["dispatch_policy"]["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["agent"] == "claude"
                && row["complexity"] == "low"
                && row["model"] == "opus")
    );
    env.story(dir.path())
        .args(["dispatch-policy", "show"])
        .assert()
        .failure();
}
