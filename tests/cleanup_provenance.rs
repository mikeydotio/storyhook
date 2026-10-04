//! Cleanup provenance survives wire round trips without weakening legacy leases.

use serde_json::{Value, json};
use storyhook::domain::TmuxCleanupTarget;

fn round_trip(value: Value) -> Result<Value, serde_json::Error> {
    serde_json::from_value::<TmuxCleanupTarget>(value).and_then(serde_json::to_value)
}

#[test]
fn old_cleanup_targets_keep_their_exact_wire_shape() {
    let old = json!({"socket_path": "/tmp/old.sock"});
    assert_eq!(round_trip(old.clone()).unwrap(), old);
}

#[test]
fn protected_cleanup_targets_retain_origin_separate_from_current_binding() {
    let value = json!({"socket_path": "/tmp/.rv-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb/s",
        "revivify": {"logical_socket": "/tmp/public.sock",
                     "origin_generation": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}});
    assert_eq!(round_trip(value.clone()).unwrap(), value);
}

#[test]
fn malformed_or_partial_provenance_is_not_silently_discarded() {
    for bad in [
        json!({}),
        json!({"logical_socket":"/tmp/public.sock"}),
        json!({"origin_generation":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}),
        json!({"logical_socket":"relative", "origin_generation":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}),
        json!({"logical_socket":"/tmp/public.sock", "origin_generation":"ABCDEFABCDEFABCDEFABCDEFABCDEFABCD"}),
        json!({"logical_socket":"/tmp/public.sock", "origin_generation":"../escape"}),
        json!({"logical_socket":"/tmp/public.sock", "origin_generation":17}),
        json!({"logical_socket":"/tmp/public.sock", "origin_generation":""}),
    ] {
        assert!(
            round_trip(json!({"socket_path":"/tmp/old.sock", "revivify":bad})).is_err(),
            "accepted {bad}"
        );
    }
}
