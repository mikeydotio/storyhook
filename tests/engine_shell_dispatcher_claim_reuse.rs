use std::path::Path;

use storyhook::env::Environment;
use storyhook::service::engine::{
    DispatchOutcomeState, DispatchRequest, Dispatcher, ShellDispatcher,
};
use storyhook::store::EngineAgent;
use storyhook_test_support::scratch_dir;

fn write_claim_guard_fixture(path: &Path) {
    std::fs::write(
        path,
        r#"
for argument in "$@"; do
  if [ "$argument" = "--force" ]; then
    printf '{"ok":true,"reused_claim":true,"argv":"%s"}\n' "$*"
    exit 0
  fi
done
printf '%s\n' '{"ok":false,"reason":"resume-available","display":"the engine-owned claim needs --force before the helper can reuse it"}'
exit 17
"#,
    )
    .expect("write claim-guard fixture");
}

#[test]
fn full_auto_shell_dispatcher_signals_reuse_of_the_engine_owned_claim() {
    let root = scratch_dir();
    let home = root.path().join("home");
    std::fs::create_dir(&home).unwrap();
    let script = root.path().join("story.sh");
    write_claim_guard_fixture(&script);

    let outcome = ShellDispatcher::new(&script, Environment::at(home))
        .dispatch(DispatchRequest {
            project: "alpha".to_string(),
            story: "ALPHA-7".to_string(),
            agent: EngineAgent::Codex,
        })
        .unwrap();

    assert_eq!(
        outcome.state,
        DispatchOutcomeState::Ok,
        "Full Auto must tell the helper to reuse the claim the engine already owns; helper answered {}",
        outcome.payload
    );
    assert_eq!(outcome.payload["reused_claim"], true);
}
