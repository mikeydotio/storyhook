//! A cleanup child must outlive neither exclusion nor its identity.
use super::*;
use std::io::Write;
use std::process::Stdio;

#[test]
fn orphaned_child_retains_exclusion_until_it_exits() {
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let owner = WorkspaceLock::at(root.path(), "SH-1").unwrap();
    let mut command = Command::new("sh");
    command.args(["-c", "read answer"]).stdin(Stdio::piped());
    owner.command(&mut command);
    let mut child = command.spawn().unwrap();
    drop(owner);
    assert!(WorkspaceLock::at(root.path(), "SH-1").is_err());
    writeln!(child.stdin.take().unwrap(), "finish").unwrap();
    assert!(child.wait().unwrap().success());
    assert!(WorkspaceLock::at(root.path(), "SH-1").is_ok());
}
