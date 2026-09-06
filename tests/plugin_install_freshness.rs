//! Installation must verify the enabled provider payload, even when the
//! external provider reports success and the expected version.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;

use storyhook_test_support::{
    ChildGuard, STORY_COMMAND_DEADLINE, daemon_containment, scratch_dir, story_binary,
};

/// A provider that reports successful installation but retains an old helper
/// at the current version's enabled cache path. Only Codex is replaced; the
/// installer, release materialization, and files under HOME are real.
const STALE_CODEX: &str = r#"#!/bin/sh
set -eu
printf '%s\n' "$*" >> "$HOME/codex-invocations"
version="$(cat "$HOME/expected-version")"
case "$*" in
  --version) echo 'codex-cli 1.2.3' ;;
  'plugin marketplace remove storyhook --json')
    echo '{"marketplaceName":"storyhook","installedRoot":null}' ;;
  'plugin remove story@storyhook --json')
    echo '{"pluginId":"story@storyhook","marketplaceName":"storyhook"}' ;;
  'plugin marketplace add '*)
    printf '{"marketplaceName":"storyhook","installedRoot":"%s","alreadyAdded":false}\n' "$4" ;;
  'plugin add story@storyhook --json')
    printf '{"pluginId":"story@storyhook","marketplaceName":"storyhook","version":"%s","installedPath":"%s/.codex/plugins/cache/storyhook/story/%s"}\n' "$version" "$HOME" "$version" ;;
  'plugin list --json')
    printf '{"installed":[{"pluginId":"story@storyhook","name":"story","marketplaceName":"storyhook","version":"%s","installed":true,"enabled":true}]}\n' "$version" ;;
  'execpolicy check '*)
    echo '{"matchedRules":[{"prefixRuleMatch":{"decision":"allow"}}],"decision":"allow"}' ;;
  *) echo "unexpected codex invocation: $*" >&2; exit 64 ;;
esac
"#;

#[test]
fn successful_provider_response_cannot_hide_stale_enabled_helper() {
    let temp = scratch_dir();
    let home = temp.path().join("home");
    let bin = temp.path().join("bin");
    let version = env!("CARGO_PKG_VERSION");
    let installed = home
        .join(".codex/plugins/cache/storyhook/story")
        .join(version);
    fs::create_dir_all(installed.join("bin")).unwrap();
    fs::create_dir_all(installed.join(".codex-plugin")).unwrap();
    fs::create_dir_all(&bin).unwrap();
    fs::write(home.join("expected-version"), version).unwrap();
    fs::write(
        installed.join(".codex-plugin/plugin.json"),
        format!(r#"{{"name":"story","version":"{version}"}}"#),
    )
    .unwrap();
    let helper = installed.join("bin/story.sh");
    let stale = "#!/bin/sh\nprintf '%s\\n' 'stale catalog: gpt-5.5'\n";
    fs::write(&helper, stale).unwrap();
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o755)).unwrap();
    let codex = bin.join("codex");
    fs::write(&codex, STALE_CODEX).unwrap();
    fs::set_permissions(&codex, fs::Permissions::from_mode(0o755)).unwrap();

    let mut command = Command::new(story_binary());
    command
        .args(["plugin", "install", "codex"])
        .current_dir(temp.path())
        .env_clear()
        .env("HOME", &home)
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .env("TMPDIR", temp.path())
        .env("XDG_DATA_HOME", home.join("data"))
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_STATE_HOME", home.join("state"))
        .env("STORYHOOK_DATA_DIR", home.join("data/storyhook"))
        .envs(daemon_containment());
    let output = ChildGuard::spawn_with_output(&mut command)
        .expect("running isolated production plugin installer")
        .wait_with_output_within(STORY_COMMAND_DEADLINE, || {
            "isolated plugin installer did not finish".into()
        });
    let diagnostic = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let calls = fs::read_to_string(home.join("codex-invocations"))
        .unwrap_or_else(|error| panic!("provider was not invoked: {error}\n{diagnostic}"));
    assert!(
        calls.contains("plugin add story@storyhook --json"),
        "{calls}"
    );
    assert!(
        !diagnostic.contains("unexpected codex invocation"),
        "{diagnostic}"
    );
    let installed_bytes = fs::read(&helper).unwrap();
    assert!(
        !output.status.success() || installed_bytes != stale.as_bytes(),
        "installer reported success while the enabled helper still contains the stale catalog:\n{diagnostic}"
    );
    if !output.status.success() {
        assert!(
            diagnostic.contains("plugin")
                && (diagnostic.contains("stale")
                    || diagnostic.contains("match")
                    || diagnostic.contains("payload")
                    || diagnostic.contains("verify")),
            "failure must identify the installed payload problem: {diagnostic}"
        );
    }
}
