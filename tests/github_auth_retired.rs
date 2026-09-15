//! SH-734: GitHub credentials belong to gh, never the StoryHook protocol.
use storyhook::cli::parse_invocation;

#[test]
fn standalone_github_auth_is_not_a_command() {
    for action in ["login", "status", "logout"] {
        let args = ["github-auth".to_owned(), action.to_owned()];
        assert!(parse_invocation(&args).is_err(), "{action} must be retired");
    }
}

#[test]
fn requests_never_serialize_github_credentials() {
    let wire = storyhook::api::wire::WireRequest::new(storyhook::cli::Invocation::Summary, "/tmp");
    let mut legacy = serde_json::to_value(wire).unwrap();
    legacy["github_token"] = serde_json::json!("ghp_legacy_must_not_survive");
    let decoded: storyhook::api::wire::WireRequest = serde_json::from_value(legacy).unwrap();
    assert!(
        !serde_json::to_string(&decoded)
            .unwrap()
            .contains("ghp_legacy")
    );
    assert!(!format!("{decoded:?}").contains("ghp_legacy"));
}
