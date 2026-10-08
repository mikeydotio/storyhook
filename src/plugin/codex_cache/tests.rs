use super::*;
use std::cell::Cell;
use std::os::unix::fs::PermissionsExt;
use std::sync::mpsc;

fn key(home: &Path) -> Key {
    Key::from_context(
        home,
        home.to_path_buf(),
        Some(home.join("bin").into_os_string()),
        [const { None }; 3],
    )
}

fn plugin(home: &Path, version: &str) -> PathBuf {
    let root = home
        .join(".codex/plugins/cache/storyhook/story")
        .join(version);
    fs::create_dir_all(root.join(".codex-plugin")).unwrap();
    fs::create_dir_all(root.join("bin")).unwrap();
    fs::write(root.join(".codex-plugin/plugin.json"), "{}").unwrap();
    fs::write(root.join("bin/story.sh"), "fixture").unwrap();
    root
}

fn installed(home: &Path) -> Option<PathBuf> {
    super::super::codex_installed_plugin_root_from(home, br#"{"installed":[{"pluginId":"story@storyhook","name":"story","marketplaceName":"storyhook","version":"current","installed":true,"enabled":true}]}"#)
}

fn patience() -> Duration {
    storyhook_test_support::load_grace::graced_now(storyhook_test_support::STORY_COMMAND_DEADLINE)
}

#[test]
fn successful_probe_is_shared_until_success_ttl_expires() {
    let home = storyhook_test_support::scratch_dir();
    let root = plugin(home.path(), "current");
    let cache = Cache::default();
    let now = Cell::new(Instant::now());
    let calls = Cell::new(0);
    let resolve = || {
        cache
            .resolve(
                key(home.path()),
                || now.get(),
                || true,
                || {
                    calls.set(calls.get() + 1);
                    Ok(Some(root.clone()))
                },
            )
            .unwrap()
    };
    for _ in 0..3 {
        assert_eq!(resolve(), Some(root.clone()));
    }
    assert_eq!(calls.get(), 1);
    now.set(now.get() + SUCCESS_TTL);
    assert_eq!(resolve(), Some(root));
    assert_eq!(calls.get(), 2);
}

#[test]
fn absent_and_failed_probes_keep_short_ttl_and_error_kind_from_completion() {
    for failure in [false, true] {
        let home = storyhook_test_support::scratch_dir();
        let cache = Cache::default();
        let now = Cell::new(Instant::now());
        let calls = Cell::new(0);
        let resolve = || {
            cache.resolve(
                key(home.path()),
                || now.get(),
                || true,
                || {
                    calls.set(calls.get() + 1);
                    now.set(now.get() + WAIT_BOUND);
                    if failure {
                        Err(AppError::Validation("registry fixture failure".into()))
                    } else {
                        Ok(None)
                    }
                },
            )
        };
        for _ in 0..2 {
            match resolve() {
                Err(AppError::Validation(detail)) if failure => {
                    assert_eq!(detail, "registry fixture failure")
                }
                Ok(None) if !failure => {}
                result => panic!("cached result changed type: {result:?}"),
            }
        }
        assert_eq!(
            calls.get(),
            1,
            "TTL must begin after the slow probe completes"
        );
        now.set(now.get() + NEGATIVE_TTL);
        let _ = resolve();
        assert_eq!(calls.get(), 2);
    }
}

#[test]
fn cache_keys_separate_home_path_cwd_and_xdg_contexts() {
    let home = storyhook_test_support::scratch_dir();
    let other = storyhook_test_support::scratch_dir();
    let base = key(home.path());
    let mut path = base.clone();
    path.path = Some("another-provider-path".into());
    let mut cwd = base.clone();
    cwd.cwd = other.path().to_path_buf();
    let mut xdg = base.clone();
    xdg.xdg[0] = Some(other.path().as_os_str().to_owned());
    let cache = Cache::default();
    let calls = Cell::new(0);
    for context in [base, key(other.path()), path, cwd, xdg] {
        for _ in 0..2 {
            assert_eq!(
                cache
                    .resolve(
                        context.clone(),
                        Instant::now,
                        || true,
                        || {
                            calls.set(calls.get() + 1);
                            Ok(None)
                        }
                    )
                    .unwrap(),
                None
            );
        }
    }
    assert_eq!(calls.get(), 5);
}

#[test]
fn config_and_provider_replacements_force_new_authoritative_probes() {
    let home = storyhook_test_support::scratch_dir();
    fs::create_dir_all(home.path().join("bin")).unwrap();
    fs::create_dir_all(home.path().join(".codex")).unwrap();
    let provider = home.path().join("bin/codex");
    fs::write(&provider, "fixture").unwrap();
    fs::set_permissions(&provider, fs::Permissions::from_mode(0o700)).unwrap();
    let config = home.path().join(".codex/config.toml");
    fs::write(&config, "first").unwrap();
    let cache = Cache::default();
    let calls = Cell::new(0);
    let resolve = || {
        cache
            .resolve(
                key(home.path()),
                Instant::now,
                || true,
                || {
                    calls.set(calls.get() + 1);
                    Ok(None)
                },
            )
            .unwrap()
    };
    resolve();
    resolve();
    assert_eq!(calls.get(), 1);
    fs::write(config, "changed configuration").unwrap();
    resolve();
    assert_eq!(calls.get(), 2);
    fs::write(provider, "replacement executable fixture").unwrap();
    resolve();
    assert_eq!(calls.get(), 3);
}

#[test]
fn manifest_and_helper_changes_refresh_without_selecting_a_stale_version() {
    let home = storyhook_test_support::scratch_dir();
    let _stale = plugin(home.path(), "old");
    let root = plugin(home.path(), "current");
    let cache = Cache::default();
    let calls = Cell::new(0);
    let resolve = || {
        cache
            .resolve(
                key(home.path()),
                Instant::now,
                || true,
                || {
                    calls.set(calls.get() + 1);
                    Ok(installed(home.path()))
                },
            )
            .unwrap()
    };
    assert_eq!(resolve(), Some(root.clone()));
    assert_eq!(resolve(), Some(root.clone()));
    assert_eq!(calls.get(), 1);
    fs::write(root.join("bin/story.sh"), "changed helper bytes").unwrap();
    assert_eq!(resolve(), Some(root.clone()));
    assert_eq!(calls.get(), 2);
    fs::remove_file(root.join("bin/story.sh")).unwrap();
    assert_eq!(resolve(), Some(root.clone()));
    assert_eq!(calls.get(), 3);
    fs::remove_file(root.join(".codex-plugin/plugin.json")).unwrap();
    assert_eq!(
        resolve(),
        None,
        "never choose the surviving stale cache directory"
    );
    assert_eq!(calls.get(), 4);
}

#[test]
fn concurrent_same_key_coalesces_while_other_home_can_finish() {
    let home = storyhook_test_support::scratch_dir();
    let other = storyhook_test_support::scratch_dir();
    let context = key(home.path());
    let cache = Arc::new(Cache::default());
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let owner_cache = Arc::clone(&cache);
    let owner_key = context.clone();
    let bound = patience();
    let owner = std::thread::spawn(move || {
        owner_cache.resolve(
            owner_key,
            Instant::now,
            || true,
            || {
                started_tx.send(()).unwrap();
                release_rx.recv_timeout(bound).unwrap();
                Ok(None)
            },
        )
    });
    started_rx.recv_timeout(bound).unwrap();
    let follower_cache = Arc::clone(&cache);
    let follower_key = context.clone();
    let follower = std::thread::spawn(move || {
        follower_cache.resolve(
            follower_key,
            Instant::now,
            || true,
            || panic!("same-key follower started a duplicate registry probe"),
        )
    });
    let deadline = Instant::now() + bound;
    while Arc::strong_count(lock(&cache.entries).get(&context).unwrap()) < 3 {
        assert!(
            Instant::now() < deadline,
            "follower did not reach the occupied entry"
        );
        std::thread::yield_now();
    }
    assert_eq!(
        cache
            .resolve(key(other.path()), Instant::now, || true, || Ok(None))
            .unwrap(),
        None
    );
    release_tx.send(()).unwrap();
    assert_eq!(owner.join().unwrap().unwrap(), None);
    assert_eq!(follower.join().unwrap().unwrap(), None);
}

#[test]
fn invalidation_retires_in_flight_answers_and_completed_answers() {
    let home = storyhook_test_support::scratch_dir();
    let root = plugin(home.path(), "current");
    let cache = Cache::default();
    let result = cache.resolve(
        key(home.path()),
        Instant::now,
        || true,
        || {
            cache.invalidate(home.path());
            Ok(Some(root.clone()))
        },
    );
    assert!(
        matches!(result, Err(AppError::Storage(message)) if message.contains("context changed"))
    );
    assert_eq!(
        cache
            .resolve(
                key(home.path()),
                Instant::now,
                || true,
                || Ok(Some(root.clone()))
            )
            .unwrap(),
        Some(root)
    );
    cache.invalidate(home.path());
    assert_eq!(
        cache
            .resolve(key(home.path()), Instant::now, || true, || Ok(None))
            .unwrap(),
        None
    );
}

#[test]
fn context_change_during_probe_is_not_published() {
    let home = storyhook_test_support::scratch_dir();
    let root = plugin(home.path(), "current");
    let cache = Cache::default();
    let unchanged = Cell::new(true);
    let result = cache.resolve(
        key(home.path()),
        Instant::now,
        || unchanged.get(),
        || {
            unchanged.set(false);
            Ok(None)
        },
    );
    assert!(
        matches!(result, Err(AppError::Storage(message)) if message.contains("context changed"))
    );
    unchanged.set(true);
    assert_eq!(
        cache
            .resolve(
                key(home.path()),
                Instant::now,
                || unchanged.get(),
                || Ok(Some(root.clone()))
            )
            .unwrap(),
        Some(root)
    );
}

#[test]
fn panicking_probe_releases_its_entry_for_a_later_caller() {
    let home = storyhook_test_support::scratch_dir();
    let cache = Cache::default();
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            cache.resolve(
                key(home.path()),
                Instant::now,
                || true,
                || panic!("probe fixture panic"),
            )
        }))
        .is_err()
    );
    assert_eq!(
        cache
            .resolve(key(home.path()), Instant::now, || true, || Ok(None))
            .unwrap(),
        None
    );
}

#[test]
fn cache_capacity_never_evicts_active_entries_or_grows_without_bound() {
    let home = storyhook_test_support::scratch_dir();
    let cache = Cache::default();
    let held: Vec<_> = (0..MAX_ENTRIES)
        .map(|index| {
            cache
                .entry(key(&home.path().join(index.to_string())))
                .unwrap()
        })
        .collect();
    assert!(cache.entry(key(&home.path().join("overflow"))).is_err());
    assert_eq!(lock(&cache.entries).len(), MAX_ENTRIES);
    drop(held);
    assert!(cache.entry(key(&home.path().join("replacement"))).is_ok());
    assert_eq!(lock(&cache.entries).len(), MAX_ENTRIES);
}

#[test]
fn manifest_lost_during_publication_cannot_be_a_positive_cache_hit() {
    for replacement_directory in [false, true] {
        let home = storyhook_test_support::scratch_dir();
        let root = plugin(home.path(), "current");
        let manifest = root.join(".codex-plugin/plugin.json");
        let cache = Cache::default();
        let first = cache
            .resolve(
                key(home.path()),
                Instant::now,
                || true,
                || {
                    let answer = installed(home.path());
                    fs::remove_file(&manifest).unwrap();
                    if replacement_directory {
                        fs::create_dir(&manifest).unwrap();
                    }
                    Ok(answer)
                },
            )
            .unwrap();
        assert_eq!(first, Some(root));
        let calls = Cell::new(0);
        let next = cache
            .resolve(
                key(home.path()),
                Instant::now,
                || true,
                || {
                    calls.set(calls.get() + 1);
                    Ok(installed(home.path()))
                },
            )
            .unwrap();
        assert_eq!(next, None);
        assert_eq!(
            calls.get(),
            1,
            "missing/nonregular publication stamp must force an authoritative retry"
        );
    }
}

#[test]
fn mutation_guard_invalidates_on_entry_and_on_drop() {
    let home = storyhook_test_support::scratch_dir();
    let root = plugin(home.path(), "current");
    let context = key(home.path());
    assert_eq!(
        cache()
            .resolve(
                context.clone(),
                Instant::now,
                || true,
                || Ok(Some(root.clone()))
            )
            .unwrap(),
        Some(root.clone())
    );
    let guard = Mutation::begin(home.path().to_path_buf());
    assert_eq!(
        cache()
            .resolve(context.clone(), Instant::now, || true, || Ok(None))
            .unwrap(),
        None
    );
    drop(guard);
    assert_eq!(
        cache()
            .resolve(context, Instant::now, || true, || Ok(Some(root.clone())))
            .unwrap(),
        Some(root)
    );
    cache().invalidate(home.path());
}
