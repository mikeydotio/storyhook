//! Scripted [`Dispatcher`] for unit-level Full Auto reconciliation tests.
//!
//! This fake models decisions and observations, never tmux behavior. Shell and
//! browser suites remain responsible for the real [`ShellDispatcher`] path.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use storyhook::error::AppError;
use storyhook::lane_budget::WindowCensus;
use storyhook::service::engine::{
    DispatchOutcome, DispatchRequest, Dispatcher, UnclaimRequest, WindowProbe,
};

/// One answer the fake will consume, in exact call order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DispatcherStep {
    Dispatch(DispatchOutcome),
    DispatchFailure(String),
    Unclaim(DispatchOutcome),
    UnclaimFailure(String),
    /// A scripted answer from tmux: `alive` maps to [`WindowProbe::Alive`],
    /// otherwise to [`WindowProbe::Gone`] with a scripted reason.
    WindowAlive {
        window: String,
        alive: bool,
    },
    /// A scripted probe tmux could not answer (SH-626).
    WindowUnanswered {
        window: String,
        detail: String,
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
    Unclaim(UnclaimRequest),
    WindowAlive(String),
    KillWindow(String),
}

struct State {
    steps: VecDeque<DispatcherStep>,
    calls: Vec<DispatcherCall>,
    /// What `census()` answers, every pass, without consuming a step: the
    /// census runs on every fill and a scripted step would be eaten by it.
    /// An empty server by default — no manual sessions — so a test that says
    /// nothing about the budget sees the engine's own lanes alone.
    census: WindowCensus,
}

impl Default for State {
    fn default() -> Self {
        Self {
            steps: VecDeque::new(),
            calls: Vec::new(),
            census: WindowCensus::Counted {
                windows: Vec::new(),
            },
        }
    }
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
                ..State::default()
            })),
        }
    }

    /// What every later `census()` answers (SH-655).
    pub fn set_census(&self, census: WindowCensus) {
        self.state.lock().expect("fake dispatcher mutex").census = census;
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

    fn unclaim(&self, request: UnclaimRequest) -> Result<DispatchOutcome, AppError> {
        match self.next(DispatcherCall::Unclaim(request)) {
            DispatcherStep::Unclaim(outcome) => Ok(outcome),
            DispatcherStep::UnclaimFailure(detail) => Err(AppError::Storage(detail)),
            step => panic!("FakeDispatcher expected an unclaim step, got {step:?}"),
        }
    }

    fn probe_window(&self, window: &str) -> WindowProbe {
        match self.next(DispatcherCall::WindowAlive(window.to_string())) {
            DispatcherStep::WindowAlive {
                window: expected,
                alive,
            } => {
                assert_eq!(expected, window, "FakeDispatcher window probe target");
                if alive {
                    WindowProbe::Alive
                } else {
                    WindowProbe::Gone {
                        detail: format!("scripted: tmux reports `{window}` gone"),
                    }
                }
            }
            DispatcherStep::WindowUnanswered {
                window: expected,
                detail,
            } => {
                assert_eq!(expected, window, "FakeDispatcher window probe target");
                WindowProbe::Unanswered { detail }
            }
            step => panic!("FakeDispatcher expected a window probe step, got {step:?}"),
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

    fn census(&self) -> WindowCensus {
        self.state
            .lock()
            .expect("fake dispatcher mutex")
            .census
            .clone()
    }
}
