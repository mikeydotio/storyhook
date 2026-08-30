//! Scripted [`Dispatcher`] for unit-level Full Auto reconciliation tests.
//!
//! This fake models decisions and observations, never tmux behavior. Shell and
//! browser suites remain responsible for the real [`ShellDispatcher`] path.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use storyhook::error::AppError;
use storyhook::service::engine::{DispatchOutcome, DispatchRequest, Dispatcher};

/// One answer the fake will consume, in exact call order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DispatcherStep {
    Dispatch(DispatchOutcome),
    DispatchFailure(String),
    WindowAlive {
        window: String,
        alive: bool,
    },
    KillWindow {
        window: String,
        result: Result<(), String>,
    },
}

/// One call the engine made against the fake.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DispatcherCall {
    Dispatch(DispatchRequest),
    WindowAlive(String),
    KillWindow(String),
}

#[derive(Default)]
struct State {
    steps: VecDeque<DispatcherStep>,
    calls: Vec<DispatcherCall>,
}

/// A cloneable, thread-safe scripted dispatcher.
#[derive(Clone, Default)]
pub struct FakeDispatcher {
    state: Arc<Mutex<State>>,
}

impl FakeDispatcher {
    #[must_use]
    pub fn new(steps: impl IntoIterator<Item = DispatcherStep>) -> Self {
        Self {
            state: Arc::new(Mutex::new(State {
                steps: steps.into_iter().collect(),
                calls: Vec::new(),
            })),
        }
    }

    #[must_use]
    pub fn calls(&self) -> Vec<DispatcherCall> {
        self.state
            .lock()
            .expect("fake dispatcher mutex")
            .calls
            .clone()
    }

    fn next(&self, call: DispatcherCall) -> DispatcherStep {
        let mut state = self.state.lock().expect("fake dispatcher mutex");
        state.calls.push(call.clone());
        state
            .steps
            .pop_front()
            .unwrap_or_else(|| panic!("FakeDispatcher had no scripted answer for {call:?}"))
    }
}

impl Dispatcher for FakeDispatcher {
    fn dispatch(&self, request: DispatchRequest) -> Result<DispatchOutcome, AppError> {
        match self.next(DispatcherCall::Dispatch(request)) {
            DispatcherStep::Dispatch(outcome) => Ok(outcome),
            DispatcherStep::DispatchFailure(detail) => Err(AppError::Storage(detail)),
            step => panic!("FakeDispatcher expected a dispatch step, got {step:?}"),
        }
    }

    fn window_alive(&self, window: &str) -> bool {
        match self.next(DispatcherCall::WindowAlive(window.to_string())) {
            DispatcherStep::WindowAlive {
                window: expected,
                alive,
            } => {
                assert_eq!(expected, window, "FakeDispatcher window probe target");
                alive
            }
            step => panic!("FakeDispatcher expected a window-alive step, got {step:?}"),
        }
    }

    fn kill_window(&self, window: &str) -> Result<(), AppError> {
        match self.next(DispatcherCall::KillWindow(window.to_string())) {
            DispatcherStep::KillWindow {
                window: expected,
                result,
            } => {
                assert_eq!(expected, window, "FakeDispatcher kill target");
                result.map_err(AppError::Storage)
            }
            step => panic!("FakeDispatcher expected a kill-window step, got {step:?}"),
        }
    }
}
