//! The `/api/v1/invoke` envelope.
//!
//! # What crosses, and what does not
//!
//! What crosses is everything that changes *what happens*: the command, the
//! directory it was run from, whether hooks are suppressed, and how deep inside
//! a hook the caller already is. What does not cross is `--json` and `--quiet`,
//! because they are rendering decisions and rendering stays in the process the
//! user is looking at.
//!
//! That split is the whole byte-compatibility argument. `Response` and
//! `AppError` are the values `main` has always rendered; if the wire carries
//! them faithfully and `output.rs` never moves, then output through a daemon is
//! identical to output without one **by construction**, not by testing. The
//! byte-comparison test exists to catch a break in that reasoning, not to
//! establish the result.
//!
//! # `hook_depth` travels
//!
//! A user's hook shells out to `story`, and that `story` reaches the daemon. If
//! depth were a property of the daemon's process rather than of the request, the
//! daemon would see every invocation as a fresh one and fire the hook that
//! spawned it — an unbounded loop through a long-lived process, which is worse
//! than the same loop in a CLI because nothing exits to break it.
//!
//! # Errors round-trip as variants
//!
//! An error crosses as [`WireError`] — its variant and its payload — and the
//! message and exit code are *recomputed* on the far side. A transported message
//! could disagree with the variant it claims to describe; a recomputed one
//! cannot.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::cli::Invocation;
use crate::error::{AppError, WireError};
use crate::output::Response;

/// One request to `/api/v1/invoke`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireRequest {
    /// The envelope version. A mismatch is refused rather than interpreted.
    pub protocol: u32,
    /// The `storyhook` version the client was built from.
    pub client_version: String,
    /// A caller-generated id, echoed back, so a log on either side can be
    /// matched to one on the other.
    pub request_id: String,
    /// The project to act on, when the client knows it. `None` means "resolve
    /// it from `cwd`", which is what the CLI does.
    pub project: Option<String>,
    /// The directory the command was run from. Root resolution, `hooks.toml`,
    /// and every git operation are relative to it.
    pub cwd: PathBuf,
    /// Suppress the project's event hooks, as `--no-hooks` does.
    pub no_hooks: bool,
    /// How deep inside an event hook the *client* is running.
    pub hook_depth: u32,
    /// What to do.
    pub invocation: Invocation,
}

impl WireRequest {
    /// A request carrying `invocation`, from `cwd`.
    pub fn new(invocation: Invocation, cwd: impl Into<PathBuf>) -> Self {
        Self {
            protocol: crate::daemon::lifecycle::PROTOCOL,
            client_version: env!("CARGO_PKG_VERSION").to_string(),
            request_id: uuid::Uuid::new_v4().simple().to_string(),
            project: None,
            cwd: cwd.into(),
            no_hooks: false,
            hook_depth: 0,
            invocation,
        }
    }

    /// Sets whether the project's event hooks are suppressed.
    #[must_use]
    pub fn no_hooks(mut self, no_hooks: bool) -> Self {
        self.no_hooks = no_hooks;
        self
    }

    /// Sets how deep inside an event hook the client is running.
    #[must_use]
    pub fn hook_depth(mut self, hook_depth: u32) -> Self {
        self.hook_depth = hook_depth;
        self
    }
}

/// What the daemon answered.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WireResponse {
    /// The envelope version the daemon speaks.
    pub protocol: u32,
    /// The `storyhook` version the daemon was built from.
    pub server_version: String,
    /// The request this answers.
    pub request_id: String,
    /// The answer.
    #[serde(flatten)]
    pub outcome: WireOutcome,
}

/// The answer half of a [`WireResponse`], tagged `ok` or `error` as the CLI's
/// own `--json` envelope is.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "lowercase")]
pub enum WireOutcome {
    /// The command succeeded; this is what it returned.
    Ok {
        /// The unrendered result.
        response: Response,
    },
    /// The command failed.
    Error {
        /// The failure, as a variant rather than as prose.
        error: WireError,
        /// The exit code the client should use.
        ///
        /// Carried for a reader that is not storyhook — a script, a log — and
        /// deliberately *not* what the client uses: it recomputes the code from
        /// the reconstructed variant, so a daemon that disagreed with itself
        /// could not talk a client into the wrong exit status.
        exit_code: i32,
    },
}

impl WireResponse {
    /// An answer carrying `result` for `request_id`.
    pub fn new(request_id: String, result: Result<Response, AppError>) -> Self {
        Self {
            protocol: crate::daemon::lifecycle::PROTOCOL,
            server_version: env!("CARGO_PKG_VERSION").to_string(),
            request_id,
            outcome: match result {
                Ok(response) => WireOutcome::Ok { response },
                Err(error) => WireOutcome::Error {
                    exit_code: error.exit_code(),
                    error: WireError::from(&error),
                },
            },
        }
    }

    /// The result this envelope carries, as the caller's own type.
    pub fn into_result(self) -> Result<Response, AppError> {
        match self.outcome {
            WireOutcome::Ok { response } => Ok(response),
            WireOutcome::Error { error, .. } => Err(AppError::from(error)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(result: Result<Response, AppError>) -> Result<Response, AppError> {
        let sent = WireResponse::new("req-1".to_string(), result);
        let json = serde_json::to_string(&sent).expect("encoding the envelope");
        let received: WireResponse = serde_json::from_str(&json).expect("decoding the envelope");
        assert_eq!(received.request_id, "req-1");
        assert_eq!(received.protocol, crate::daemon::lifecycle::PROTOCOL);
        received.into_result()
    }

    /// Compared as rendered bytes, in every rendering mode, because rendered
    /// bytes are what a caller sees — and because that is the comparison the
    /// byte-compatibility claim is actually about.
    #[test]
    fn a_successful_response_survives_the_hop() {
        let response = Response::Message("done".to_string());
        let received = round_trip(Ok(response.clone())).expect("a successful hop");
        for (json, quiet) in [(false, false), (true, false), (false, true), (true, true)] {
            assert_eq!(
                crate::output::render_response(&response, json, quiet),
                crate::output::render_response(&received, json, quiet),
                "json={json} quiet={quiet}"
            );
        }
    }

    /// The variant is what crosses, so the message and the exit code on the far
    /// side are recomputed from it — and therefore cannot disagree with it.
    #[test]
    fn an_error_survives_the_hop_as_a_variant_not_as_prose() {
        let sent = AppError::StateConflict("todo".to_string(), "done".to_string());
        let message = sent.to_string();
        let exit_code = sent.exit_code();
        let received = round_trip(Err(sent)).unwrap_err();
        assert!(matches!(received, AppError::StateConflict(ref e, ref a)
            if e == "todo" && a == "done"));
        assert_eq!(received.to_string(), message);
        assert_eq!(received.exit_code(), exit_code);
    }

    #[test]
    fn the_envelope_is_tagged_the_way_the_cli_tags_its_own() {
        let ok = serde_json::to_value(WireResponse::new(
            "r".to_string(),
            Ok(Response::Message("hi".to_string())),
        ))
        .unwrap();
        assert_eq!(ok["result"], "ok");
        assert!(ok["response"].is_object());

        let err = serde_json::to_value(WireResponse::new(
            "r".to_string(),
            Err(AppError::NotFound("SH-9".to_string())),
        ))
        .unwrap();
        assert_eq!(err["result"], "error");
        assert_eq!(err["error"]["kind"], "not_found");
        assert_eq!(err["exit_code"], 3);
    }

    #[test]
    fn a_request_survives_the_hop_by_value() {
        let request = WireRequest::new(
            Invocation::Show {
                id: "SH-1".to_string(),
            },
            "/tmp/repo",
        )
        .no_hooks(true)
        .hook_depth(2);
        let json = serde_json::to_string(&request).expect("encoding");
        let received: WireRequest = serde_json::from_str(&json).expect("decoding");
        assert_eq!(received, request);
    }

    /// The two rendering flags must not have a field to travel in: the moment
    /// one exists, some caller sets it, and rendering stops being the client's
    /// job — which is the property the whole byte-compatibility argument rests
    /// on.
    #[test]
    fn no_rendering_decision_can_cross_the_wire() {
        let json = serde_json::to_value(WireRequest::new(Invocation::Summary, "/tmp")).unwrap();
        let fields: Vec<&str> = json
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert!(!fields.contains(&"json"), "{fields:?}");
        assert!(!fields.contains(&"quiet"), "{fields:?}");
    }

    #[test]
    fn every_request_carries_a_distinct_id() {
        let a = WireRequest::new(Invocation::Summary, "/tmp");
        let b = WireRequest::new(Invocation::Summary, "/tmp");
        assert_ne!(a.request_id, b.request_id);
    }
}
