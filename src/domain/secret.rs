//! Credentials that travel, and the one rule that keeps them from leaking.
//!
//! # Why a type rather than a `String`
//!
//! Since SH-153 a GitHub Personal Access Token is read by the **client** and
//! carried to the daemon in the request envelope, because the daemon's own
//! environment belongs to whichever process happened to start it. That is the
//! right channel — `/api/v1/*` is answered on loopback only and is bearer-gated
//! — but it moves a secret into a value that gets `Debug`-printed, serialized,
//! compared and passed around.
//!
//! **The danger is that `Debug` is structural.** `InvokeRequest`, `WireRequest`
//! and `Invocation` all *derive* it, so each of them prints whatever its fields
//! print, and `assert_eq!(received, request)` in a wire test renders both sides
//! into output a developer pastes into an issue. A `String` field would be a
//! credential in every one of those. [`GithubToken`] is the same bytes with the
//! printing removed.
//!
//! # This module is deliberately not behind `github-pr`
//!
//! `src/github` is gated (`lib.rs:11`) and this is not: the envelope has to
//! carry the field in **every** build, while only a `github-pr` build has
//! anything (`pr-check`, `github-auth`) to spend it on. A gated type here
//! would make `--no-default-features` fail to compile the wire.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::AppError;

/// The environment variable a client reads its GitHub credential from.
///
/// Named here rather than at the read site so that the structural test in
/// `tests/invoker_seam.rs` has one place to point at: after SH-153 exactly one
/// file under `src/` may name this string, and it is the one the *client* runs.
pub const GITHUB_TOKEN_VAR: &str = "STORYHOOK_GITHUB_TOKEN";

/// What to tell a caller who has no GitHub credential.
///
/// Says *caller* rather than naming a process, because the whole point of
/// SH-153 is that the shell being talked about is the one the command was typed
/// into — not the daemon's, which is what this text used to mean by accident.
pub const NO_TOKEN: &str = "STORYHOOK_GITHUB_TOKEN environment variable is not set.\n\
     Create a GitHub Personal Access Token at https://github.com/settings/tokens\n\
     and export it: export STORYHOOK_GITHUB_TOKEN=ghp_...";

/// What to tell a caller whose credential is present but blank.
///
/// `std::env::var` answers `Ok("")` for `export STORYHOOK_GITHUB_TOKEN=`, which
/// is a different mistake from not setting it at all and used to produce a
/// mystifying `401` from GitHub several network calls later.
pub const EMPTY_TOKEN: &str = "STORYHOOK_GITHUB_TOKEN is set but empty.\n\
     Create a GitHub Personal Access Token at https://github.com/settings/tokens\n\
     and export it: export STORYHOOK_GITHUB_TOKEN=ghp_...";

/// A GitHub Personal Access Token, carried but never printed.
///
/// # The contract, in four parts
///
/// * **[`Debug`] is written by hand and prints `GithubToken(<redacted>)`.** It
///   must never be derived. This is the load-bearing half of the type: the
///   containers it travels in derive their own `Debug`, so this impl is what
///   makes `{request:?}` harmless anywhere in the program.
/// * **There is no [`Display`](fmt::Display), and there must not be.** A
///   `Display` impl is how a secret ends up inside a `format!` in an error
///   message a year from now.
/// * **[`Serialize`] and [`Deserialize`] are transparent**, and the asymmetry
///   with `Debug` *is* the rule: serializing is transport to a loopback,
///   bearer-gated, mode-0600 peer, while printing is diagnostics to a sink
///   nobody has chosen.
/// * **The bytes come out through [`expose`](Self::expose) and nowhere else**,
///   a name chosen so that `grep expose` finds every place the secret is in the
///   clear. Today that is one call site: the `Authorization` header.
///
/// # No zeroization on drop, deliberately
///
/// It would be theatre here, and stating that is cheaper than having somebody
/// add it later believing it helps. By the time this value is dropped its bytes
/// have already been copied into a `serde_json` buffer, an HTTP request body and
/// a `ureq` header; `String`'s own buffer may have been reallocated and the old
/// allocation left behind by any push that grew it. Wiping one of several copies
/// buys nothing, and `zeroize` would be a dependency for a property this design
/// cannot deliver. What does the work is that the value is short-lived and never
/// written down.
#[derive(Clone, Eq)]
pub struct GithubToken(String);

impl GithubToken {
    /// A token from a caller-supplied string, refusing a blank one.
    ///
    /// Validation lives in the constructor rather than at the read site so that
    /// **an empty `GithubToken` is unrepresentable** — including one arriving
    /// off the wire, since [`Deserialize`] comes through here too. A hand-built
    /// request carrying `""` is refused by the envelope rather than turning
    /// into a puzzling `401` after two network round trips.
    ///
    /// # Errors
    ///
    /// [`AppError::GithubAuth`] if `raw` is empty or only whitespace.
    pub fn new(raw: impl Into<String>) -> Result<Self, AppError> {
        let raw = raw.into();
        if raw.trim().is_empty() {
            return Err(AppError::GithubAuth(EMPTY_TOKEN.to_string()));
        }
        Ok(Self(raw))
    }

    /// This token's bytes, in the clear.
    ///
    /// Every call site is a place the credential is exposed, which is what the
    /// name is for: it makes them greppable, and it makes adding one an obvious
    /// thing to notice in review.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

/// Prints nothing. See the type's contract — this impl is why the containers
/// that derive `Debug` are safe to print.
impl fmt::Debug for GithubToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("GithubToken(<redacted>)")
    }
}

/// Compared in constant time, for the same reason the daemon's own bearer check
/// is (`api::rpc::token_ok`): the defence costs one loop and the alternative is
/// having to argue that the timing channel is unreachable.
impl PartialEq for GithubToken {
    fn eq(&self, other: &Self) -> bool {
        crate::api::rpc::constant_time_eq(self.0.as_bytes(), other.0.as_bytes())
    }
}

impl Serialize for GithubToken {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for GithubToken {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::new(raw).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The assertion the whole type exists for. Checked on the newtype **and**
    /// on a struct that derives `Debug` around it, because the derived case is
    /// the one that actually occurs: nobody prints a bare token, and everybody
    /// prints the request that holds one.
    #[test]
    fn a_token_never_prints_itself() {
        let token = GithubToken::new("ghp_a_very_secret_value").expect("a usable token");
        let printed = format!("{token:?}");
        assert!(
            !printed.contains("ghp_a_very_secret_value"),
            "the value must not appear: {printed}"
        );
        assert!(
            printed.contains("redacted"),
            "and it must say so: {printed}"
        );

        #[derive(Debug)]
        struct Envelope {
            #[allow(dead_code)]
            token: Option<GithubToken>,
        }
        let printed = format!("{:?}", Envelope { token: Some(token) });
        assert!(
            !printed.contains("ghp_a_very_secret_value"),
            "a derived Debug around it must be safe too — this is the case that \
             occurs in practice: {printed}"
        );
    }

    /// Transport must still work. The point of the type is that printing and
    /// serializing are *different* decisions, not that both are refused.
    #[test]
    fn a_token_serializes_as_its_bare_value() {
        let token = GithubToken::new("ghp_wire").expect("a usable token");
        let json = serde_json::to_string(&token).expect("serializing");
        assert_eq!(json, "\"ghp_wire\"");
        let back: GithubToken = serde_json::from_str(&json).expect("deserializing");
        assert_eq!(back.expose(), "ghp_wire");
    }

    /// `export STORYHOOK_GITHUB_TOKEN=` yields `Ok("")`, which is the trap
    /// `tests/error_contract.rs` already warns about by name. It must be a
    /// refusal that says what is wrong, not a `401` two network calls later.
    #[test]
    fn a_blank_token_is_refused_at_construction() {
        for blank in ["", "   ", "\t\n"] {
            let error = GithubToken::new(blank).expect_err("a blank token is unusable");
            assert_eq!(error.exit_code(), 6, "it is still an auth failure");
            assert!(
                format!("{error}").contains("set but empty"),
                "and it must distinguish itself from an unset one"
            );
        }
    }

    /// The refusal has to survive the wire too, or a hand-built request could
    /// put a state past the constructor that the constructor exists to prevent.
    #[test]
    fn a_blank_token_cannot_arrive_off_the_wire_either() {
        let error =
            serde_json::from_str::<GithubToken>("\"\"").expect_err("an empty token is not a token");
        assert!(
            format!("{error}").contains("set but empty"),
            "the constructor's reason must survive deserialization: {error}"
        );
    }

    /// Equality is constant-time, and still correct — the property that makes
    /// the constant-time routing worth having rather than merely present.
    #[test]
    fn equality_still_answers_correctly() {
        let a = GithubToken::new("ghp_one").expect("usable");
        let b = GithubToken::new("ghp_one").expect("usable");
        let c = GithubToken::new("ghp_two").expect("usable");
        let longer = GithubToken::new("ghp_one_but_longer").expect("usable");
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_ne!(a, longer);
    }
}
