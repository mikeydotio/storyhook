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
use crate::domain::provenance::ActorLabel;
use crate::error::{AppError, WireError};
use crate::output::Response;

/// How a caller named the project it wants to act on.
///
/// The two sources — `--project <slug>` and `$STORYHOOK_PROJECT` — are collapsed
/// into one value **by the client**, which is the only process that can see both:
/// a daemon's environment is its own, and `hook_depth` already travels on the
/// wire for the same reason. Precedence is therefore applied exactly once, in
/// [`ProjectSelector::resolve`], rather than re-decided by whoever reads it next.
///
/// # Why the source is carried and not just the slug
///
/// A slug that names no project has to be refused, and the refusal is only
/// useful if it says *where the slug came from*. A bad `--project` is a mistake
/// in one command. A bad `$STORYHOOK_PROJECT` is a mistake in a shell, and it
/// will repeat on every command run there until somebody changes it — so the
/// remedy a reader needs is different, and a bare `Option<String>` cannot tell
/// them which one they have.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "via", rename_all = "snake_case")]
pub enum ProjectSelector {
    /// `--project <slug>`, about this invocation.
    Flag {
        /// The slug the caller typed.
        slug: String,
    },
    /// `$STORYHOOK_PROJECT`, about this shell.
    Environment {
        /// The slug the variable held.
        slug: String,
    },
}

impl ProjectSelector {
    /// The precedence rule, as a pure function: the flag beats the variable.
    ///
    /// Separated from `main` so it can be tested without a process. An empty
    /// value from either source is treated as absent, for the same reason
    /// [`crate::env`] ignores an empty path variable: an `export` unset the
    /// careless way leaves an empty string behind, and reading that as "the
    /// project named `""`" would refuse every command in the shell.
    #[must_use]
    pub fn resolve(flag: Option<&str>, environment: Option<&str>) -> Option<Self> {
        fn named(value: Option<&str>) -> Option<&str> {
            value.map(str::trim).filter(|slug| !slug.is_empty())
        }
        if let Some(slug) = named(flag) {
            return Some(Self::Flag {
                slug: slug.to_string(),
            });
        }
        named(environment).map(|slug| Self::Environment {
            slug: slug.to_string(),
        })
    }

    /// The project slug this selector names.
    #[must_use]
    pub fn slug(&self) -> &str {
        match self {
            Self::Flag { slug } | Self::Environment { slug } => slug,
        }
    }

    /// How the caller spelled it, for a refusal that has to name the source.
    #[must_use]
    pub fn source(&self) -> &'static str {
        match self {
            Self::Flag { .. } => "--project",
            Self::Environment { .. } => "$STORYHOOK_PROJECT",
        }
    }
}

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
    /// The project to act on, and how the caller named it.
    ///
    /// `None` means "resolve it from `cwd`". A `Some` is **binding**: the daemon
    /// either resolves that slug or refuses, and never falls back to the
    /// directory — see [`ProjectSelector`].
    #[serde(default)]
    pub project: Option<ProjectSelector>,
    /// The directory the command was run from. Root resolution, `hooks.toml`,
    /// and every git operation are relative to it.
    pub cwd: PathBuf,
    /// Suppress the project's event hooks, as `--no-hooks` does.
    pub no_hooks: bool,
    /// How deep inside an event hook the *client* is running.
    pub hook_depth: u32,
    /// The client's standard input, when the command reads it.
    ///
    /// `story import` with no file and `story decompose --stdin` read a document
    /// the user is piping in. The daemon cannot: it has no connection to that
    /// terminal. So the client reads it and it travels here.
    #[serde(default)]
    pub stdin: Option<String>,
    /// The client's GitHub credential, when the command spends one.
    ///
    /// Carried for the same reason [`stdin`](Self::stdin) is, and it is the
    /// only field here that is a secret: see
    /// [`GithubToken`](crate::domain::secret::GithubToken) for why it prints as
    /// `<redacted>` and what that protects. Absent from the serialized envelope
    /// entirely when there is none, rather than present and null.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github_token: Option<crate::domain::secret::GithubToken>,
    /// Who the client says it is, from its own `$STORYHOOK_ACTOR` (SH-246).
    ///
    /// Carried for the same reason [`stdin`](Self::stdin) is: the daemon
    /// outlives the client and holds whatever environment the process that
    /// happened to start it had, so a daemon reading this variable itself would
    /// label every write on the machine with one stale value.
    ///
    /// Typed as [`ActorLabel`], so the bound and the control-character refusal
    /// are applied again on deserialization — a client that skipped storyhook's
    /// own CLI does not get to skip the constraint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<ActorLabel>,
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
            stdin: None,
            github_token: None,
            actor: None,
            invocation,
        }
    }

    /// Supplies the client's standard input.
    #[must_use]
    pub fn stdin(mut self, stdin: Option<String>) -> Self {
        self.stdin = stdin;
        self
    }

    /// Supplies the client's GitHub credential.
    #[must_use]
    pub fn github_token(
        mut self,
        github_token: Option<crate::domain::secret::GithubToken>,
    ) -> Self {
        self.github_token = github_token;
        self
    }

    /// Supplies who the client says it is (SH-246).
    #[must_use]
    pub fn actor(mut self, actor: Option<ActorLabel>) -> Self {
        self.actor = actor;
        self
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

    /// Names the project this request acts on.
    #[must_use]
    pub fn project(mut self, project: Option<ProjectSelector>) -> Self {
        self.project = project;
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
    /// Conditions the daemon met while answering, for the client to print
    /// (SH-530).
    ///
    /// `default` + `skip_serializing_if` so the field is invisible on every
    /// ordinary response and absent envelopes still decode — the SH-372 rule
    /// that an absent field states nothing rather than asserting a negative.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notices: Vec<String>,
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
        Self::with_notices(request_id, result, Vec::new())
    }

    /// [`Self::new`], carrying notices the daemon collected while answering.
    pub fn with_notices(
        request_id: String,
        result: Result<Response, AppError>,
        notices: Vec<String>,
    ) -> Self {
        Self {
            protocol: crate::daemon::lifecycle::PROTOCOL,
            server_version: env!("CARGO_PKG_VERSION").to_string(),
            notices,
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
    use crate::domain::secret::GithubToken;

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

    /// A credential the client read has to arrive intact, or `pr-check` is
    /// back to reading the daemon's environment (SH-153, originally about
    /// the now-retired sync engine but load-bearing for every caller of this
    /// field since).
    #[test]
    fn a_request_carries_its_token_across_the_hop() {
        let token =
            crate::domain::secret::GithubToken::new("ghp_across_the_hop").expect("a usable token");
        let request =
            WireRequest::new(Invocation::Summary, "/tmp/repo").github_token(Some(token.clone()));
        let json = serde_json::to_string(&request).expect("encoding");
        let received: WireRequest = serde_json::from_str(&json).expect("decoding");
        assert_eq!(received, request);
        assert_eq!(
            received.github_token.as_ref().map(GithubToken::expose),
            Some("ghp_across_the_hop"),
            "the value has to survive, not merely the field"
        );
    }

    /// **A request that carries no credential has no credential field at all**,
    /// rather than one spelled `null`.
    ///
    /// Worth asserting rather than assuming: `story list` is the overwhelming
    /// majority of traffic, and the smallest envelope is the one that cannot
    /// leak anything. It is also what keeps a hand-written client, or an older
    /// one, from having to know the field exists.
    #[test]
    fn a_request_without_a_token_has_no_token_field() {
        let json = serde_json::to_value(WireRequest::new(Invocation::Summary, "/tmp")).unwrap();
        assert!(
            !json
                .as_object()
                .expect("an object")
                .contains_key("github_token"),
            "an absent credential must not appear on the wire at all: {json}"
        );
    }

    /// And an envelope holding one must not print it. This is the assertion
    /// that would fail if `GithubToken`'s hand-written `Debug` were ever
    /// replaced by a derive — the containers derive theirs, so they print
    /// whatever their fields print.
    #[test]
    fn a_request_holding_a_token_does_not_print_it() {
        let request = WireRequest::new(Invocation::Summary, "/tmp").github_token(Some(
            crate::domain::secret::GithubToken::new("ghp_must_not_appear").expect("usable"),
        ));
        let printed = format!("{request:?}");
        assert!(
            !printed.contains("ghp_must_not_appear"),
            "a whole request is Debug-printed by wire tests on failure: {printed}"
        );
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

    /// The precedence rule, tested without a process — which is the whole
    /// reason it is a pure function rather than a few lines inside `main`.
    #[test]
    fn the_flag_beats_the_variable_and_either_beats_nothing() {
        assert_eq!(ProjectSelector::resolve(None, None), None);
        assert_eq!(
            ProjectSelector::resolve(Some("a"), Some("b")),
            Some(ProjectSelector::Flag {
                slug: "a".to_string()
            })
        );
        assert_eq!(
            ProjectSelector::resolve(None, Some("b")),
            Some(ProjectSelector::Environment {
                slug: "b".to_string()
            })
        );
    }

    /// An `export` unset the careless way leaves an empty string behind, and
    /// reading that as "the project named `""`" would refuse every command in
    /// the shell — the same trap `crate::env` avoids for an empty path variable.
    #[test]
    fn an_empty_or_blank_value_is_absence_rather_than_a_project_named_nothing() {
        assert_eq!(ProjectSelector::resolve(Some(""), None), None);
        assert_eq!(ProjectSelector::resolve(Some("   "), None), None);
        assert_eq!(ProjectSelector::resolve(None, Some("")), None);
        // …and an empty flag must not shadow a usable variable.
        assert_eq!(
            ProjectSelector::resolve(Some(""), Some("b")),
            Some(ProjectSelector::Environment {
                slug: "b".to_string()
            })
        );
    }

    /// The source is what makes a refusal actionable, so it must survive the
    /// hop. A selector that arrived as a variable and came back as a flag would
    /// send a reader looking in the wrong place for a slug they cannot see.
    #[test]
    fn a_selector_keeps_its_source_across_the_wire() {
        for selector in [
            ProjectSelector::Flag {
                slug: "widgets".to_string(),
            },
            ProjectSelector::Environment {
                slug: "widgets".to_string(),
            },
        ] {
            let request =
                WireRequest::new(Invocation::Summary, "/tmp").project(Some(selector.clone()));
            let json = serde_json::to_string(&request).expect("encoding");
            let received: WireRequest = serde_json::from_str(&json).expect("decoding");
            assert_eq!(received.project, Some(selector.clone()));
            assert_eq!(
                received.project.as_ref().map(ProjectSelector::source),
                Some(selector.source())
            );
        }
    }

    /// A request that names no project must not grow one by default. `None` is
    /// "resolve from the directory"; there is no default project anywhere in
    /// this design, and a serde default that invented one would be exactly that.
    #[test]
    fn a_request_names_no_project_unless_the_caller_named_one() {
        assert_eq!(WireRequest::new(Invocation::Summary, "/tmp").project, None);
        let bare = serde_json::json!({
            "protocol": crate::daemon::lifecycle::PROTOCOL,
            "client_version": env!("CARGO_PKG_VERSION"),
            "request_id": "r",
            "cwd": "/tmp",
            "no_hooks": false,
            "hook_depth": 0,
            "invocation": serde_json::to_value(Invocation::Summary).unwrap(),
        });
        let decoded: WireRequest =
            serde_json::from_value(bare).expect("a request with no project field decodes");
        assert_eq!(decoded.project, None);
    }

    #[test]
    fn every_request_carries_a_distinct_id() {
        let a = WireRequest::new(Invocation::Summary, "/tmp");
        let b = WireRequest::new(Invocation::Summary, "/tmp");
        assert_ne!(a.request_id, b.request_id);
    }
}
