//! A tmux answer slower than the production bound (SH-836).
//!
//! Five engine lib tests went RED in one central gate because a fake tmux
//! that answered at once missed `TMUX_TIMEOUT` while the machine was loaded.
//! Load cannot be summoned here without failing other sessions' tests, so
//! each case injects the latency instead (the SH-806 D4 precedent): the
//! fixture answers after [`slow`], later than the production bound and
//! earlier than that bound graced at [`STATED_CONTENTION`]. Every production
//! path a fake tmux reaches is shown three ways: the production bound (a
//! proof) does not wait for the slow answer, declared patience does, and
//! patience never turns a wrong answer into the right verdict.

use super::restart_probe_tests::{adopted_lane, adopted_row, observer};
use super::*;

/// The contention the patient cases state, in runnable threads per core: it
/// grants `3 * TMUX_TIMEOUT` at idle, and the machine's own reading when that
/// is higher.
const STATED_CONTENTION: f64 = 3.0;

/// How long each fixture takes to answer: past the production bound, as a
/// starved spawn is, and well inside the graced one.
fn slow() -> Duration {
    TMUX_TIMEOUT * 3 / 2
}

/// A fixture that answers `answer` (a `printf` format) after [`slow`].
fn late(answer: &str) -> String {
    format!("sleep {}\nprintf '{answer}'", slow().as_secs_f64())
}

fn stated(root: &Path) -> Environment {
    Environment::at(root).with_subprocess_patience_under(STATED_CONTENTION)
}

fn production(root: &Path) -> Environment {
    Environment::at(root).with_subprocess_proof()
}

#[test]
fn a_slow_liveness_answer_is_waited_for_only_under_patience() {
    let root = storyhook_test_support::scratch_dir();
    let live = late(&format!(
        "{}\\tcodex\\t0\\t1789066115\\n",
        std::process::id()
    ));

    let WindowProbe::Unanswered { detail } =
        observer(root.path(), &live, production(root.path())).probe_window("@7")
    else {
        panic!("the production bound must not wait for a slow answer");
    };
    assert!(
        detail.contains("did not answer the liveness probe"),
        "{detail}"
    );

    assert_eq!(
        observer(root.path(), &live, stated(root.path())).probe_window("@7"),
        WindowProbe::Alive {
            last_output_at: Some(1_789_066_115)
        }
    );

    let dead = late(&format!(
        "{}\\tcodex\\t1\\t1789066115\\n",
        std::process::id()
    ));
    let WindowProbe::Gone { detail } =
        observer(root.path(), &dead, stated(root.path())).probe_window("@7")
    else {
        panic!("patience must not turn a dead pane into a live one");
    };
    assert!(detail.contains("pane_dead=1"), "{detail}");
}

#[test]
fn a_slow_adopted_identity_answer_is_waited_for_only_under_patience() {
    let root = storyhook_test_support::scratch_dir();
    // Only the identity query is slow; the activity query answers at once.
    let answering = |dead| {
        format!(
            "if [ \"$4\" = list-panes ]; then\n{}\nelse printf '1789066115\\n'; fi",
            late(&adopted_row(root.path(), dead))
        )
    };
    let lane = adopted_lane(root.path());

    let WindowProbe::Unanswered { detail } =
        observer(root.path(), &answering(0), production(root.path())).probe_lane(&lane, "%1")
    else {
        panic!("the production bound must not wait for a slow identity answer");
    };
    assert!(
        detail.contains("inspect leased tmux server") && detail.contains("timed out"),
        "{detail}"
    );

    assert_eq!(
        observer(root.path(), &answering(0), stated(root.path())).probe_lane(&lane, "%1"),
        WindowProbe::Alive {
            last_output_at: Some(1_789_066_115)
        }
    );

    let WindowProbe::Gone { detail } =
        observer(root.path(), &answering(1), stated(root.path())).probe_lane(&lane, "%1")
    else {
        panic!("patience must not turn a dead adopted pane into a live one");
    };
    assert!(detail.contains("pane process is dead"), "{detail}");
}

#[test]
fn a_slow_census_answer_is_waited_for_only_under_patience() {
    let root = storyhook_test_support::scratch_dir();
    let live = late("storyhook:SH-1\\tclaude\\t0\\n");

    let WindowCensus::Unanswered { detail } =
        observer(root.path(), &live, production(root.path())).census()
    else {
        panic!("the production bound must not wait for a slow census");
    };
    assert!(
        detail.contains("did not answer the window census"),
        "{detail}"
    );

    assert_eq!(
        observer(root.path(), &live, stated(root.path())).census(),
        WindowCensus::Counted {
            windows: vec!["storyhook:SH-1".to_string()]
        }
    );

    assert_eq!(
        observer(
            root.path(),
            &late("storyhook:SH-1\\tclaude\\t1\\n"),
            stated(root.path())
        )
        .census(),
        WindowCensus::Counted { windows: vec![] },
        "patience must not count a dead pane"
    );
}

#[test]
fn a_slow_kill_is_waited_for_only_under_patience() {
    // kill_window reads only the exit status, and every nonzero exit is
    // already a refusal whatever its speed, so there is no wrong answer here
    // that patience could hide.
    let root = storyhook_test_support::scratch_dir();
    let killed = format!("sleep {}", slow().as_secs_f64());

    let error = observer(root.path(), &killed, production(root.path()))
        .kill_window("@7")
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("did not answer while killing window"),
        "{error}"
    );

    observer(root.path(), &killed, stated(root.path()))
        .kill_window("@7")
        .unwrap();
}
