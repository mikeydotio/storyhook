//! Explicit in-process patience for integration fixtures that link the non-test lib.
use std::time::Duration;
use storyhook::env::Environment;

/// The routed production families in the SH-846 census: tmux (3), submission
/// Git (5), identity/capabilities/hygiene (10), workspace/GitHub reads (30),
/// view/notification/cleanup (45), resource Git (60), GitHub operations (120),
/// dispatch (180), and continuation resume (225). Only an exact bound match receives its own graced value.
const BASE_SECONDS: [u64; 9] = [3, 5, 10, 30, 45, 60, 120, 180, 225];

/// Configure only this Environment, before invoking an effect exactly once.
/// No process-global switch, automatic fixture default, or retry is involved.
/// Reads not routed through Environment retain their separately declared policy.
pub fn subprocess_patience(mut env: Environment) -> Environment {
    let reading = crate::load_grace::contention();
    for seconds in BASE_SECONDS {
        let base = Duration::from_secs(seconds);
        let graced = crate::load_grace::graced_by(base, reading);
        let patience = Duration::from_millis(graced.as_nanos().div_ceil(1_000_000) as u64);
        env = env
            .with_test_subprocess_patience(base, patience)
            .expect("the shared load grace supplies a bounded fixture allowance");
    }
    if reading.is_some_and(|ratio| ratio > 1.0) {
        eprintln!(
            "integration subprocess patience (SH-846): contention={reading:?}; exact per-bound allowances, release policy unchanged"
        );
    }
    env
}
