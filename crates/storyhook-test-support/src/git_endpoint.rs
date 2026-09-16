//! Substitute remote endpoints while retaining real Git protocol behavior.
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

/// Installs a Git executable adapter mapping explicit HTTPS URLs to local remotes.
/// Local config reads and URL-rewrite validation always reach unmodified Git.
/// Only network commands are redirected, after production routing has run.
pub fn install_git_endpoint(bin: &Path, mappings: &[(&str, &Path)]) {
    fs::create_dir_all(bin).expect("endpoint bin directory");
    let real_git = Command::new("sh")
        .args(["-c", "command -v git"])
        .output()
        .unwrap();
    assert!(real_git.status.success());
    let real_git = String::from_utf8(real_git.stdout)
        .unwrap()
        .trim()
        .to_owned();
    let mapping: std::collections::BTreeMap<_, _> = mappings.iter().copied().collect();
    let program = format!(
        r#"#!/usr/bin/env python3
import json, os, subprocess, sys
real = {real}
mapping = json.loads({mapping})
args = sys.argv[1:]
i = 0
while i < len(args) and args[i] in ['-C', '-c', '--git-dir', '--work-tree']:
    i += 2
if i < len(args) and args[i] in ['fetch', 'push', 'clone', 'ls-remote'] and '--get-url' not in args:
    for n in range(i + 1, len(args)):
        value = args[n]
        if value == 'origin':
            value = subprocess.check_output([real, *args[:i], 'remote', 'get-url', 'origin'], text=True).strip()
        if value in mapping:
            args[n] = mapping[value]
os.execv(real, [real, *args])
"#,
        real = serde_json::to_string(&real_git).unwrap(),
        mapping = serde_json::to_string(&serde_json::to_string(&mapping).unwrap()).unwrap(),
    );
    let path = bin.join("git");
    fs::write(&path, program).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}
