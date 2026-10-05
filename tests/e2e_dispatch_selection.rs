//! SH-812: real-dispatch checks use Playwright's rootDir-relative file names.

use std::process::Command;

#[test]
fn only_real_dispatch_files_trigger_the_postcheck() {
    let library = concat!(env!("CARGO_MANIFEST_DIR"), "/scripts/e2e-selection.sh");
    for (file, expected) in [
        ("dispatch.spec.ts", "1"),
        ("engine.spec.ts", "1"),
        ("story-context-menu-dispatch.spec.ts", "0"),
        ("engine-other.spec.ts", "0"),
        ("dispatch.spec.ts.backup", "0"),
    ] {
        let output = Command::new("/bin/bash")
            .args([
                "-c",
                "set -euo pipefail; . \"$1\"; printf '%s\\n' \"$2\" | e2e_selection_real_dispatch",
                "dispatch-selection",
                library,
                &format!("chromium\t{file}\t2"),
            ])
            .output()
            .expect("run real-dispatch selection");
        assert!(output.status.success(), "{output:?}");
        assert_eq!(String::from_utf8(output.stdout).unwrap().trim(), expected);
    }
}
