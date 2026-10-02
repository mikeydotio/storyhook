use super::*;
use command::Reply;
use std::cell::RefCell;

fn reply(text: &str) -> Result<Reply, AppError> {
    Ok(Reply {
        success: true,
        text: text.into(),
        diagnostic: String::new(),
    })
}

#[test]
fn absent_manager_and_absent_unit_are_distinct() {
    let dir = tempfile::tempdir().unwrap();
    let env = Environment::at(dir.path());
    assert!(
        matches!(select(&env, &|_| Err(AppError::Storage("no bus".into()))).unwrap(), Selection::Unavailable(reason) if reason.contains("no bus"))
    );
    assert!(matches!(
        select(&env, &|_| reply("245")).unwrap(),
        Selection::NotInstalled
    ));
}

#[test]
fn installed_unit_refusal_does_not_turn_into_a_fork() {
    let dir = tempfile::tempdir().unwrap();
    let env = Environment::at(dir.path());
    std::fs::create_dir_all(path(&env).parent().unwrap()).unwrap();
    std::fs::write(path(&env), "broken").unwrap();
    assert!(select(&env, &|_| reply("245")).is_err());
}

#[test]
fn start_refusal_and_control_timeout_keep_their_diagnostics() {
    let dir = tempfile::tempdir().unwrap();
    let env = Environment::at(dir.path());
    let run = |args: &[&str]| {
        if args[0] == "show" {
            reply(&format!(
                "LoadState=loaded\nFragmentPath={}\nDropInPaths=",
                path(&env).display()
            ))
        } else if args[0] == "start" {
            Ok(Reply {
                success: false,
                text: String::new(),
                diagnostic: "access denied".into(),
            })
        } else {
            reply("")
        }
    };
    let error = request_start(&env, &run).unwrap_err().to_string();
    assert!(error.contains("start --no-block") && error.contains("access denied"));
    let error = request_start(&env, &|_| {
        Err(AppError::Storage(
            "systemctl daemon-reload timed out".into(),
        ))
    })
    .unwrap_err()
    .to_string();
    assert!(error.contains("daemon-reload timed out"));
}

#[test]
fn manager_start_is_idempotent_and_rejects_overrides() {
    let dir = tempfile::tempdir().unwrap();
    let env = Environment::at(dir.path());
    let calls = RefCell::new(Vec::new());
    let run = |args: &[&str]| {
        calls.borrow_mut().push(args.join(" "));
        if args[0] == "show" {
            reply(&format!(
                "LoadState=loaded\nFragmentPath={}\nDropInPaths=\nActiveState=active\nUnitFileState=enabled",
                path(&env).display()
            ))
        } else {
            reply("")
        }
    };
    request_start(&env, &run).unwrap();
    assert!(
        calls
            .borrow()
            .last()
            .unwrap()
            .starts_with("start --no-block ")
    );
    assert!(
        !calls
            .borrow()
            .iter()
            .any(|call| call.starts_with("restart"))
    );
    assert!(request_start(&env, &|_| reply("LoadState=masked")).is_err());
    assert!(
        request_start(&env, &|_| reply(&format!(
            "LoadState=loaded\nFragmentPath={}\nDropInPaths=/override.conf",
            path(&env).display()
        )))
        .is_err()
    );
}

#[test]
fn generated_unit_cannot_name_another_store_or_runtime_root() {
    let dir = tempfile::tempdir().unwrap();
    let env = Environment::at(dir.path());
    let other = Environment::at(dir.path().join("other"));
    let execution = ExecutionPath::parse(Some(std::ffi::OsStr::new("/usr/bin"))).unwrap();
    let text = definition::Registration::new(&other, Path::new("/usr/bin/story"), &execution)
        .render()
        .unwrap();
    fs::create_dir_all(path(&env).parent().unwrap()).unwrap();
    fs::write(path(&env), text).unwrap();
    assert!(select(&env, &|_| reply("245")).is_err());
}

#[test]
#[cfg(unix)]
fn loaded_fragment_may_be_the_managers_symlink_to_the_same_definition() {
    let dir = tempfile::tempdir().unwrap();
    let env = Environment::at(dir.path());
    fs::create_dir_all(path(&env).parent().unwrap()).unwrap();
    fs::write(path(&env), "definition").unwrap();
    let alias = dir.path().join("manager-alias.service");
    std::os::unix::fs::symlink(path(&env), &alias).unwrap();
    let run = |args: &[&str]| {
        if args[0] == "show" {
            reply(&format!(
                "LoadState=loaded\nFragmentPath={}\nDropInPaths=",
                alias.display()
            ))
        } else {
            reply("")
        }
    };
    request_start(&env, &run).unwrap();
}

#[test]
fn installed_path_and_gc_guard_use_the_same_store_identity() {
    let dir = tempfile::tempdir().unwrap();
    let env = Environment::at(dir.path());
    let store = dir.path().join("other.db");
    let other = env.clone().with_store(
        crate::env::StoreLocation::resolve(
            Some(&store),
            &crate::env::StoreVars::default(),
            env.home(),
        )
        .unwrap(),
    );
    let execution = ExecutionPath::parse(Some(std::ffi::OsStr::new("/opt/bin:/usr/bin"))).unwrap();
    fs::create_dir_all(path(&other).parent().unwrap()).unwrap();
    fs::write(
        path(&other),
        definition(&other, Path::new("/opt/story"), &execution).unwrap(),
    )
    .unwrap();
    assert_eq!(registered_path(&other).unwrap(), Some(execution));
    assert_eq!(agent_serving(&env, &store), Some(path(&other)));
    assert!(report(&other).contains("missing"));
    fs::write(path(&other), "broken").unwrap();
    assert_eq!(agent_serving(&env, &store), Some(path(&other)));
    assert!(registered_path(&other).is_err());
}
