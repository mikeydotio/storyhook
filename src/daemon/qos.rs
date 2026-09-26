//! What class of work a daemon thread is doing, in scheduler terms (SH-784).
//!
//! Every thread the daemon starts inherits its QoS class from whichever
//! thread spawned it — on macOS that is, ultimately, the main thread `serve`
//! runs on, unless something asks for a different class explicitly
//! (`man pthread_set_qos_class_self_np`). Left alone, that means every
//! background poller and every request-serving thread runs at the same
//! class, and nothing here chose it.
//!
//! [`WorkClass::Serving`] and [`WorkClass::Housekeeping`] make that choice
//! explicit rather than accidental, at the top of every thread the daemon
//! spawns: a request-serving thread requests the class a foreground command
//! deserves; background work stays at the class it already runs at. This is
//! deliberately narrower than the daemon's *ownership* (`lifecycle::DaemonOwner`,
//! `agent::plist`'s `ProcessType`): a thread can only ever *request* a class,
//! and the operating system decides whether that request has any effect,
//! bounded by the process's own ceiling. Measured on this machine (recorded
//! on SH-784): a process unclamped by any service manager stays at its
//! current class regardless of the request; a process launchd started with
//! `ProcessType = Interactive` actually rises from 31 to 37. Requesting the
//! class here is therefore always correct and never harmful — it does
//! nothing on a path that cannot honour it, and raises priority on the one
//! that can.

#[cfg(target_os = "macos")]
use libc::qos_class_t;
#[cfg(target_os = "macos")]
use libc::qos_class_t::{QOS_CLASS_DEFAULT, QOS_CLASS_USER_INITIATED};

/// The two kinds of work a daemon thread does, and the class each requests.
///
/// There is no third variant for "whatever the thread already is": every
/// thread the daemon spawns calls [`Self::enter`] with one of these two,
/// explicitly, at its own top — see the module doc for why that is safer
/// than leaving a thread's class to whatever it happened to inherit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WorkClass {
    /// Answering a request a client is waiting on: a connection thread, a
    /// dispatcher-pool worker, a nested-invoke lane. Requests
    /// `QOS_CLASS_USER_INITIATED` — the class SH-784's design settled on so a
    /// launchd-owned daemon (`ProcessType = Interactive`) outruns
    /// default-priority work on the same machine, including gate runs
    /// (SH-785 lowers those separately, so they never inherit this).
    Serving,
    /// Everything else the daemon does on its own initiative: pollers,
    /// verifier orchestration, backups, reconciliation. Requests
    /// `QOS_CLASS_DEFAULT` explicitly, rather than leaving it unstated —
    /// defensive, because a thread this variant is entered from can itself
    /// be spawned *from* an already-[`Serving`](Self::Serving) thread (a
    /// dispatch child's watcher, a reset's worker), which would otherwise
    /// inherit the elevated class it does not need.
    Housekeeping,
}

impl WorkClass {
    /// Requests this class for the calling thread, for the rest of its life
    /// (or until something else calls this again on the same thread).
    ///
    /// A request, never a promise: see the module doc for when the operating
    /// system actually changes anything. `libc::pthread_set_qos_class_self_np`
    /// is documented to fail only when handed an invalid class or priority
    /// offset, neither of which is possible with the fixed inputs here, so
    /// the result is intentionally discarded rather than threaded through a
    /// `Result` every call site would have to decide how to react to.
    #[cfg(target_os = "macos")]
    pub(crate) fn enter(self) {
        let class: qos_class_t = match self {
            WorkClass::Serving => QOS_CLASS_USER_INITIATED,
            WorkClass::Housekeeping => QOS_CLASS_DEFAULT,
        };
        // SAFETY: `pthread_set_qos_class_self_np` only ever inspects its two
        // by-value arguments and mutates this thread's own scheduling
        // metadata; both arguments here are fixed, valid constants.
        unsafe {
            libc::pthread_set_qos_class_self_np(class, 0);
        }
    }

    /// No-op off macOS: neither `QOS_CLASS_*` nor `pthread_set_qos_class_self_np`
    /// exist on Linux, and SH-787 tracks that platform's own mechanism
    /// (`nice`/`ionice`/cgroups) separately.
    #[cfg(not(target_os = "macos"))]
    pub(crate) fn enter(self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    fn requested_class() -> qos_class_t {
        let mut class: qos_class_t = QOS_CLASS_DEFAULT;
        let mut priority: libc::c_int = 0;
        // SAFETY: `pthread_get_qos_class_np` writes only through the two
        // pointers given, both to local stack variables sized for it.
        let rc = unsafe {
            libc::pthread_get_qos_class_np(libc::pthread_self(), &mut class, &mut priority)
        };
        assert_eq!(rc, 0, "pthread_get_qos_class_np failed");
        class
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn serving_requests_user_initiated() {
        std::thread::spawn(|| {
            WorkClass::Serving.enter();
            assert_eq!(requested_class() as u32, QOS_CLASS_USER_INITIATED as u32);
        })
        .join()
        .unwrap();
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn housekeeping_requests_default_even_after_serving() {
        std::thread::spawn(|| {
            // The defensive case the module doc names: a housekeeping thread
            // spawned from code that already raised the parent to `Serving`
            // must still end up at `Housekeeping`'s class, not inherit the
            // parent's.
            WorkClass::Serving.enter();
            WorkClass::Housekeeping.enter();
            assert_eq!(requested_class() as u32, QOS_CLASS_DEFAULT as u32);
        })
        .join()
        .unwrap();
    }

    #[test]
    #[cfg(not(target_os = "macos"))]
    fn enter_is_a_no_op_off_macos() {
        // Nothing to observe off macOS; this only proves the call compiles
        // and returns, on every platform this crate ships for.
        WorkClass::Serving.enter();
        WorkClass::Housekeeping.enter();
    }
}
