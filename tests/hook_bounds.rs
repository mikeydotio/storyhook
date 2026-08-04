//! An event hook cannot hold storyhook for longer than the hook's own timeout.
//!
//! # Why this file exists
//!
//! `hooks.settings.timeout_seconds` is the promise event hooks make: a hook gets
//! that long, and then storyhook carries on. Two of the three pipes
//! `src/event_hooks.rs` opened were outside that promise, and the reason is one
//! sentence: **it drove three concurrent pipes sequentially** — write the
//! payload to stdin, then wait, then read stderr. That is the classic pipe
//! deadlock [`std::process::Child::wait_with_output`] exists to prevent, and it
//! has three reachable forms:
//!
//! * a hook that **fails while a descendant holds its stderr** — the failure
//!   branch blocks waiting for an end-of-file only that descendant can deliver.
//!   `child.kill()` kills `sh`, not what `sh` backgrounded;
//! * a hook whose descendant holds its **stdin** — the payload write blocks
//!   *before* `wait_timeout` is reached, so `timeout_seconds` is not merely
//!   exceeded, it is never consulted;
//! * a merely **verbose** hook, with no descendant at all — it blocks writing
//!   stderr into a buffer nobody drains while storyhook blocks writing stdin
//!   into a buffer the hook has not reached.
//!
//! The third is the one that says what the defect really was. SH-141 was filed
//! about a lifetime mismatch between a descriptor's holder and its owner, and
//! that describes the first two exactly — but a well-behaved hook that talks and
//! then reads its input wedges the store with no descendant and no misbehaviour.
//! The grandchild is an instance; the sequential driver was the defect.
//!
//! None of the three is bounded by anything storyhook controls: `sleep 86400 &`
//! makes the first a day, and the third has no bound at all.
//!
//! The fix is not a new deadline. Both pipes became unlinked temporary files, so
//! the wedge is **unrepresentable** rather than bounded — a regular file has no
//! end-of-file rendezvous, so no descendant can make storyhook wait for one.
//!
//! # Why it is worth its own file
//!
//! Hooks run inside the daemon's request handler (`src/service/mod.rs`, served
//! by `src/api/rest.rs`), so a wedge here stops **every** client of that store,
//! not one command — and since SH-144 the client's own bound is measured on the
//! daemon's published boundaries, which a wedged handler never reaches. This is
//! the innermost of the three bounds SH-143, SH-144 and SH-141 install, and the
//! only one below which there is nothing left to bound.
//!
//! # How these tests work
//!
//! Every call is made on a worker thread with a channel deadline, so a
//! regression fails in seconds with a name instead of stalling the suite — the
//! failure mode being retired, not reproduced. The hooks are fired through
//! `event_hooks::fire_hook` directly: the wedge is in that function, it needs no
//! store, no daemon and no project, and a test that reaches it through the CLI
//! would spend its wall clock proving things other files already prove.
//!
//! What the diagnostic *says*, and that it reaches the only place a human can
//! read it, is pinned by
//! `tests/event_hooks.rs::a_failing_hooks_reason_reaches_the_daemon_log`. That
//! test is the guard against closing this wedge by discarding hook output — the
//! one fix that would pass every timing assertion in this file while making
//! every failing hook silent. This paragraph used to claim the wording was
//! pinned at the CLI level when nothing anywhere asserted it; the claim is now
//! true rather than merely written down.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use storyhook::event_hooks::{self, HookDef, HookEventType, HooksConfig, HooksSettings};

/// How long a test waits for a call that is supposed to give up on its own.
///
/// Comfortably beyond any bound the hook itself sets, and far short of the
/// grandchild lifetimes below: what is being distinguished is a bounded wait
/// from an unbounded one, not one bounded wait from another.
const PATIENCE: Duration = Duration::from_secs(30);

/// How long the grandchildren in this file live.
///
/// Long enough that a wedged `fire_hook` cannot finish inside [`PATIENCE`] by
/// accident, short enough that the strays a *failing* run leaves behind are gone
/// before anybody looks. A regression here leaks one `sleep` per test.
const GRANDCHILD_LIFETIME_SECS: u64 = 300;

/// Fires one hook with `command` and returns how long the call took.
///
/// Panics with the hook's command rather than hanging, because a hang here is
/// indistinguishable from the suite being slow and arrives with no name
/// attached.
fn fire_bounded(command: &str, payload: &str, timeout_seconds: u64) -> Duration {
    let config = HooksConfig {
        settings: HooksSettings {
            timeout_seconds,
            enabled: true,
        },
        on_create: Some(HookDef {
            command: command.to_string(),
            timeout_seconds: None,
        }),
        ..HooksConfig::default()
    };
    let root = std::env::temp_dir();
    let payload = payload.to_string();
    let label = command.to_string();

    let (tx, rx) = mpsc::channel();
    // Not joined on timeout: the thread is blocked in a `read` or a `write` that
    // nothing here can interrupt, and leaving it is the price of reporting the
    // failure at all. The process ends at the end of the run.
    std::thread::spawn(move || {
        let started = Instant::now();
        event_hooks::fire_hook(&root, &config, HookEventType::Create, &payload, 0);
        let _ = tx.send(started.elapsed());
    });

    rx.recv_timeout(PATIENCE).unwrap_or_else(|_| {
        panic!(
            "`fire_hook` has not returned after {PATIENCE:?} for the hook `{label}`.\n\
             The hook's own timeout is {timeout_seconds}s, so the wait is bounded by \
             something other than the promise storyhook made — a grandchild holding a \
             pipe end is the way that happens."
        )
    })
}

/// A payload far larger than any pipe buffer, so a write that nobody drains
/// must block.
///
/// 1 MiB against a buffer that is 64 KiB on Linux and grows to at most 64 KiB on
/// macOS. Deliberately not "a little over": the number that matters is that no
/// platform's buffer swallows it whole, and a margin costs nothing.
///
/// This is not a synthetic size. A hook payload carries `comment_text`
/// verbatim (`src/service/story.rs`), and this repository routinely comments
/// council verdicts of several kilobytes onto a story; a pasted log or diff
/// reaches a megabyte without trying.
fn oversized_payload() -> String {
    format!(
        r#"{{"event_type":"create","comment_text":"{}"}}"#,
        "x".repeat(1024 * 1024)
    )
}

/// The defect, on the stderr pipe: a hook that fails while leaving a grandchild
/// behind held the caller for as long as the grandchild lived.
///
/// `sh` exits non-zero at once, so the timeout never fires and `child.kill()` is
/// never reached; the `sleep` holds the stderr write end, and the failure branch
/// blocks reading it.
#[test]
fn a_grandchild_holding_stderr_does_not_hold_the_caller() {
    let elapsed = fire_bounded(
        &format!("sleep {GRANDCHILD_LIFETIME_SECS} & exit 1"),
        r#"{"event_type":"create"}"#,
        10,
    );
    assert!(
        elapsed < Duration::from_secs(20),
        "a failing hook with a live grandchild took {elapsed:?}; \
         the hook's timeout is 10s"
    );
}

/// The same defect on the stdin pipe, and worse: it blocks *before* the timeout
/// is consulted, so the hook's own `timeout_seconds` is not merely exceeded — it
/// is never reached.
///
/// # Why the hook reconnects stdin explicitly
///
/// This test used to read `sleep N & exit 0`, and it was a **false negative** —
/// green against the unfixed code, pinning nothing. POSIX assigns `/dev/null` to
/// an asynchronous list's stdin *before* explicit redirections, so the textbook
/// `cmd &` cannot hold the read end on a shell that applies the rule.
///
/// That rule is not something to rely on in either direction. Measured across
/// five shells with a 1 MiB write, no two agree: **zsh blocks on `sleep 5 &
/// exit 0` even when invoked as `sh`**; `set -m` defeats the rule on bash, sh
/// and ksh, because POSIX conditions it on job control being *disabled*; and
/// dash and ksh diverge from bash in opposite directions on the dup cases. Nor
/// is the shell a property of the platform — `src/event_hooks.rs` runs
/// `Command::new("sh")`, a `PATH` lookup.
///
/// `exec 3<&0` is used because it blocked on four of the five shells measured,
/// which is as close to shell-independent as a repro of this gets. The rule the
/// fix implements does not depend on any of it: a file cannot block.
#[test]
fn a_grandchild_holding_stdin_does_not_hold_the_caller() {
    let elapsed = fire_bounded(
        &format!("exec 3<&0; sleep {GRANDCHILD_LIFETIME_SECS} & exit 0"),
        &oversized_payload(),
        10,
    );
    assert!(
        elapsed < Duration::from_secs(20),
        "a hook that never read its oversized payload took {elapsed:?}; \
         the hook's timeout is 10s"
    );
}

/// The third wedge, and the one that shows the grandchild was never the defect:
/// a merely **verbose** hook, with no descendant anywhere.
///
/// `fire_hook` used to drive three concurrent pipes sequentially — write the
/// payload, then wait, then read stderr. So a hook that writes more than one
/// pipe buffer to stderr and *then* reads its payload deadlocks against
/// storyhook: the hook is blocked writing stderr into a buffer nobody is
/// draining, and storyhook is blocked writing stdin into a buffer the hook has
/// not reached yet. Neither ever moves and `timeout_seconds` is never consulted,
/// because `wait_timeout` has not been reached either.
///
/// Measured at 200 KiB: no return in 25 s. The same hook at 32 KiB — under one
/// buffer — returns in 12.9 ms, which is what identifies the mechanism exactly.
///
/// It is simultaneously the repro and a went-too-far guard: a fix that bounded
/// the write by truncating it would return promptly and fail the second
/// assertion.
#[test]
fn a_chatty_hook_that_also_reads_its_payload_is_not_deadlocked() {
    let dir = storyhook_test_support::scratch_dir();
    let sink = dir.path().join("payload.json");
    let payload = oversized_payload();
    let elapsed = fire_bounded(
        &format!(
            "head -c 204800 /dev/zero | tr '\\0' 'z' >&2; cat > {}; exit 0",
            sink.display()
        ),
        &payload,
        10,
    );
    assert!(
        elapsed < Duration::from_secs(20),
        "a verbose hook that reads its payload took {elapsed:?}; \
         the hook's timeout is 10s, and it has no grandchild at all"
    );
    let received = std::fs::read_to_string(&sink).expect("the hook must have written the payload");
    assert_eq!(
        received.len(),
        payload.len(),
        "the hook received {} of {} bytes",
        received.len(),
        payload.len()
    );
}

/// A hook may background work that outlives it, **and that work must be able to
/// finish**.
///
/// # Why this asserts a marker file rather than `kill -0`
///
/// The council settled 3–0 that a hook's descendants are left alone, and the
/// obvious guard — assert the backgrounded pid is still alive — is green under
/// *every* pipe design while the property it names is violated. A backgrounded
/// job that writes more than one pipe buffer to the stderr it inherited blocks
/// there forever: alive, scheduled, satisfying `kill -0`, and permanently not
/// working. The test would pass and the promise would be broken.
///
/// So the job has to actually *do* something storyhook can observe. Under any
/// pipe design the marker never appears; under files it appears in milliseconds.
/// That makes "left alone" mean "left alone and able to work", which is the
/// property.
///
/// # The `&&` is the whole test
///
/// It was `;` first, and that made this test green against the *unfixed* code —
/// a fresh instance of the exact fault it was written to correct. With `;` the
/// `touch` runs whatever happened to the write before it, so the marker appeared
/// even when `SIGPIPE` had just killed the write against a closed pipe. With
/// `&&` the marker is evidence that the write **completed**, which is the
/// property being pinned. Verified in both directions: red against pipes, green
/// against files.
#[test]
fn a_hook_may_background_work_that_outlives_it() {
    let dir = storyhook_test_support::scratch_dir();
    let marker = dir.path().join("the-job-finished");
    let elapsed = fire_bounded(
        &format!(
            "sh -c 'head -c 131072 /dev/zero | tr \"\\0\" \"z\" >&2 && touch {}' & exit 0",
            marker.display()
        ),
        r#"{"event_type":"create"}"#,
        10,
    );
    assert!(
        elapsed < Duration::from_secs(10),
        "firing took {elapsed:?}; it should not wait for backgrounded work"
    );

    let deadline = Instant::now() + Duration::from_secs(5);
    while !marker.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        marker.exists(),
        "the backgrounded job never finished. It is probably still alive and \
         blocked writing 128 KiB into a stderr pipe nobody drains — which is why \
         this test asserts the work completed rather than that the process exists"
    );
}

// The *unlinked* choice is pinned by `event_hooks`'s own
// `a_scratch_file_has_no_name`, which asserts the link count is zero. That was a
// test here first, comparing the temp directory's entry count across a fire —
// and it was racy by construction: the other tests in this file create scratch
// directories concurrently under `--test-threads=4`, so the count moves for
// reasons that have nothing to do with the property. A link count is the
// property itself, and nothing else can perturb it.

/// A hook that reads its payload gets all of it, however large.
///
/// The guard against a fix that goes too far: bounding the write must not mean
/// truncating it, and a hook that does its job is entitled to the whole
/// document. Fails in the *other* direction from the two above, which is why it
/// is here rather than assumed.
#[test]
fn a_hook_that_reads_its_payload_receives_every_byte() {
    let dir = storyhook_test_support::scratch_dir();
    let sink = dir.path().join("payload.json");
    let payload = oversized_payload();
    let elapsed = fire_bounded(&format!("cat > {}", sink.display()), &payload, 30);

    let received = std::fs::read_to_string(&sink).expect("the hook must have written the payload");
    assert_eq!(
        received.len(),
        payload.len(),
        "the hook received {} of {} bytes in {elapsed:?}",
        received.len(),
        payload.len()
    );
    assert_eq!(received, payload, "the payload arrived altered");
}

/// A hook that fails and leaves nothing behind costs no wall clock at all.
///
/// The third guard, and the one that catches a bound implemented as a sleep:
/// the overwhelming majority of failing hooks have no grandchild, and they must
/// not start paying for the ones that do.
///
/// What the diagnostic *says* is pinned at the CLI level in
/// `tests/event_hooks.rs`, where the message reaches a capturable stderr.
#[test]
fn a_failing_hook_with_nothing_behind_it_returns_at_once() {
    let elapsed = fire_bounded(
        "echo 'the hook is unhappy' >&2; exit 3",
        r#"{"event_type":"create"}"#,
        10,
    );
    assert!(
        elapsed < Duration::from_secs(2),
        "a failing hook with nothing behind it took {elapsed:?}; \
         a bound that waits out its own deadline on every failure is a bound in \
         the wrong place"
    );
}
