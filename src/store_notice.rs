//! Notices a command collected on its way through the store, on their way back
//! to whoever ran it (SH-530).
//!
//! # Why this exists at all
//!
//! Since SH-114 a `story` command reaches the store only through
//! `/api/v1/invoke`, so [`crate::invoke::open_store`] runs inside the **daemon**
//! and its stderr goes to the daemon log. A warning printed there is a warning
//! the person typing the command never sees — which is the SH-306 shape exactly:
//! a check whose verdict nobody collects is operationally identical to one that
//! did not run.
//!
//! A degraded store must therefore be *reported over the wire*, not printed
//! locally. This module is the collection point: the store layer pushes, the
//! request handler drains into the envelope, and the client prints what it
//! receives.
//!
//! # Why a process-global rather than a thread-local
//!
//! A thread-local was tried first and was wrong twice over, both found by
//! running the thing rather than by reading it:
//!
//! - On the **daemon** side there was nothing to collect. The daemon opens its
//!   store once, at startup, so a notice pushed where the condition is detected
//!   fires for no request at all. That is why `rpc::degraded_notice` asks the
//!   store that actually served the request instead.
//! - On the **client** side `HttpInvoker::exchange` runs the HTTP exchange on a
//!   thread of its own, so a notice recorded as the response was decoded was
//!   recorded on a thread `main` never reads.
//!
//! The buffer is therefore process-wide. A `story` process answers one command,
//! so there is no cross-talk to scope against, and a global has no claim about
//! threads that a future refactor can quietly falsify.

use std::sync::{LazyLock, Mutex, PoisonError};

static NOTICES: LazyLock<Mutex<Vec<String>>> = LazyLock::new(|| Mutex::new(Vec::new()));

fn buffer() -> std::sync::MutexGuard<'static, Vec<String>> {
    NOTICES.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Records a notice for the command currently being answered.
///
/// Deduplicated: one condition reported twice is noise, and a store can be
/// opened more than once while answering a single command.
pub fn push(notice: String) {
    let mut list = buffer();
    if !list.contains(&notice) {
        list.push(notice);
    }
}

/// Takes everything recorded so far, leaving the buffer empty.
pub fn take() -> Vec<String> {
    std::mem::take(&mut *buffer())
}
