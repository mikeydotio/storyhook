//! SH-870: conflicts retain a durable diagnosis hold and release the queue.
use super::*;
use storyhook::daemon::lifecycle::CONTROL_DEADLINE;
use storyhook::daemon::verification::VerificationCancellation;
use storyhook::service::engine::WindowProbe;
use storyhook_test_support::{ChildGuard, STORY_COMMAND_DEADLINE};

#[test]
fn sh870_conflict_does_not_wait_or_starve_the_next_submission() {
    let f = ServiceFixture::new();
    f.github_checkout("https://github.com/acme/widgets");
    let held = submitted(&f, "conflict", Priority::High, PR_ONE);
    let next = submitted(&f, "independent", Priority::Low, PR_TWO);
    let actuator = SequencedActuator {
        outcomes: Mutex::new(VecDeque::from([
            VerificationOutcome::Conflict {
                detail: "base moved".into(),
            },
            VerificationOutcome::Certified {
                head: "a".repeat(40),
                tree: "b".repeat(40),
                detail: "certified".into(),
                gate: GateCommand::DEFAULT.into(),
            },
        ])),
        verified: Mutex::new(vec![]),
        notified: Mutex::new(vec![]),
        reaped: Mutex::new(vec![]),
    };
    let activity = VerificationActivity::new();
    let inflight = InFlight::new(f.env().clone());
    assert_eq!(
        tick_with_reconciliation(
            f.store(),
            f.env(),
            &actuator,
            &activity,
            &inflight,
            f.project(),
            |_| panic!("no repair wait")
        )
        .unwrap(),
        TickResult::Returned
    );
    assert!(activity.active_all().is_empty());
    assert_eq!(story_row(&f, &held).state, "verifying");
    assert_eq!(
        tick_with(f.store(), f.env(), &actuator, f.project()).unwrap(),
        TickResult::Completed
    );
    assert_eq!(story_row(&f, &next).state, "done");
    assert!(actuator.notified.lock().unwrap().is_empty());
}

#[test]
fn the_shell_probe_reads_a_real_story_window() {
    let fixture = ServiceFixture::new();
    let root = scratch_dir();
    let socket = root.path().join("tmux");
    let mut candidate = cleanup_candidate(&fixture, root.path());
    let lease = StoryCleanupLease {
        tmux: TmuxCleanupTarget {
            revivify: None,
            socket_path: socket.clone(),
        },
        ..candidate.cleanup_lease.take().unwrap()
    };
    let tmux = |args: &[&str]| {
        let mut command = Command::new("tmux");
        command
            .env_remove("TMUX")
            .env_remove("TMUX_PANE")
            .args(["-f", "/dev/null", "-S"])
            .arg(&socket)
            .args(args);
        let output = ChildGuard::spawn_with_output(&mut command)
            .expect("start fixture tmux client")
            .wait_with_output_within(load_grace::graced_now(STORY_COMMAND_DEADLINE), || {
                format!("fixture tmux {args:?} did not finish")
            });
        assert!(
            output.status.success(),
            "tmux {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    struct KillServer<'a>(&'a Path);
    impl Drop for KillServer<'_> {
        fn drop(&mut self) {
            let _ = Command::new("tmux")
                .env_remove("TMUX")
                .arg("-S")
                .arg(self.0)
                .arg("kill-server")
                .output();
        }
    }
    let actuator = ShellVerificationActuator::new(storyhook_test_support::subprocess_patience(
        fixture.env().clone(),
    ));
    let probe = || {
        actuator.probe_agent(
            &candidate,
            Some(&lease),
            &VerificationCancellation::default(),
        )
    };
    // A pane's answer changes as its process does. Each wait is the daemon's
    // control deadline, graced by machine contention (SH-806).
    let settles_to = |expected: &dyn Fn(&WindowProbe) -> bool| {
        let last = std::cell::RefCell::new(None);
        load_grace::wait_for(
            load_grace::Patience::new(CONTROL_DEADLINE),
            Duration::from_millis(25),
            || format!("the probe still answers {:?}", last.borrow()),
            || {
                let answer = probe();
                let settled = expected(&answer);
                *last.borrow_mut() = Some(answer);
                settled.then_some(())
            },
        );
    };

    assert!(matches!(
        actuator.probe_agent(&candidate, None, &VerificationCancellation::default()),
        WindowProbe::Unanswered { .. }
    ));
    assert!(
        matches!(probe(), WindowProbe::Gone { .. }),
        "no server has started on the lease's socket"
    );
    let window = candidate.story_id.clone();
    tmux(&[
        "new-session",
        "-d",
        "-s",
        "fixture",
        "-n",
        &window,
        "/bin/sleep 600",
    ]);
    let _server = KillServer(&socket);
    tmux(&["set-option", "-g", "remain-on-exit", "on"]);
    settles_to(
        &|answer| matches!(answer, WindowProbe::Unanswered { detail } if detail.contains("runs `sleep`")),
    );

    tmux(&[
        "respawn-pane",
        "-k",
        "-t",
        &format!("fixture:{window}"),
        "/usr/bin/true",
    ]);
    settles_to(&|answer| matches!(answer, WindowProbe::Gone { detail } if detail.contains("dead")));

    tmux(&["kill-server"]);
    settles_to(&|answer| matches!(answer, WindowProbe::Gone { .. }));
}
