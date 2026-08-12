//! Who wrote an event, and how much of that is attested (SH-246).
//!
//! # The two halves, and why they never merge
//!
//! Every append carries a [`Provenance`]: a **command**, derived by the daemon
//! from the arm it dispatched, and an optional **actor**, declared by the caller
//! in `$STORYHOOK_ACTOR`. They are stored in two columns and rendered
//! differently on purpose.
//!
//! A caller cannot misstate the command, because a caller is never asked for it
//! — it is read off the [`Invocation`](crate::cli::Invocation) the daemon is
//! already executing. A caller can put anything at all in the actor. Collapsing
//! them into one "actor, falling back to the verb" field — the shape this
//! started as — throws away the attested fact in precisely the case where a
//! declaration exists, and leaves a reader unable to tell which kind of claim
//! they are reading.
//!
//! # What this is not
//!
//! **Not a tamper-proof audit log, and it must never be described as one.** The
//! daemon is single-user and loopback-bound; anyone who can set
//! `$STORYHOOK_ACTOR` can already write to the store directly, so a signature
//! would protect against an adversary who does not exist here. It is a
//! diagnostic aid: it answers "what wrote this" for a *cooperating* caller,
//! which is the question that took a store dump and three source files to answer
//! the day SH-246 was filed.
//!
//! # Why an actor is refused rather than cleaned up
//!
//! The string is rendered into a terminal and kept in a trail people will later
//! reason from. A newline lets one entry forge extra lines; an ANSI escape lets
//! it rewrite what a reader sees. Silently stripping either would leave the
//! store holding something the caller did not say, which is the one failure an
//! audit trail cannot survive. So [`ActorLabel::parse`] refuses, loudly, naming
//! the variable — and the refusal happens at the CLI edge, before any write.

use std::fmt;

use serde::{Deserialize, Serialize};

/// The longest actor label that will be accepted, in bytes.
///
/// Generous enough for the `tool:role` shapes real callers use
/// (`story.sh:dispatch-rollback` is 27) and small enough that a runaway
/// variable cannot bloat every row of an append-only table that is never
/// pruned.
pub const MAX_ACTOR_LEN: usize = 128;

/// The environment variable a caller declares its identity in.
pub const ACTOR_VAR: &str = "STORYHOOK_ACTOR";

/// A caller-declared identity, validated.
///
/// Constructing one is the only way to get an actor into the store, so the
/// constraints below hold for every value that reaches a column — the
/// make-invalid-states-unrepresentable half of SH-246's fix.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ActorLabel(String);

impl ActorLabel {
    /// Parses a declared actor.
    ///
    /// `Ok(None)` for an empty or whitespace-only value: `STORYHOOK_ACTOR=` is
    /// what an exported-but-unset variable looks like in a shell script, and
    /// refusing it would break callers for no diagnostic gain. Every other
    /// rejection is an error naming [`ACTOR_VAR`], because the caller's next
    /// action is to go and fix that variable.
    ///
    /// # Errors
    ///
    /// If the value is longer than [`MAX_ACTOR_LEN`] bytes, or contains a
    /// control character — which includes newline, carriage return, tab, and
    /// the ESC that opens an ANSI sequence.
    pub fn parse(raw: &str) -> Result<Option<Self>, ActorError> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Ok(None);
        }
        if trimmed.len() > MAX_ACTOR_LEN {
            return Err(ActorError::TooLong {
                len: trimmed.len(),
                max: MAX_ACTOR_LEN,
            });
        }
        // `char::is_control` covers C0, DEL and C1 — so newline, carriage
        // return, tab and ESC are all refused by one question rather than by an
        // enumeration somebody has to keep complete.
        if let Some(bad) = trimmed.chars().find(|c| c.is_control()) {
            return Err(ActorError::ControlCharacter {
                codepoint: bad as u32,
            });
        }
        Ok(Some(Self(trimmed.to_string())))
    }

    /// A label read back out of storage, taken at face value.
    ///
    /// The validating constructor is [`parse`](Self::parse), and it is the only
    /// way a label gets *in*. This is the way one comes back out, and it does
    /// not re-check: the value already passed on the way in, and a read is the
    /// wrong place to start failing. Refusing here would mean a single
    /// hand-edited row could take away a story's whole history — punishing the
    /// person trying to diagnose the damage.
    ///
    /// It is not a hole in the constraint. Nothing reachable from a caller
    /// hands bytes to this: it is `pub(crate)` and its one caller is the SQLite
    /// row decoder.
    #[must_use]
    pub(crate) fn trusted(stored: String) -> Self {
        Self(stored)
    }

    /// The label as written.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ActorLabel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<ActorLabel> for String {
    fn from(label: ActorLabel) -> Self {
        label.0
    }
}

impl TryFrom<String> for ActorLabel {
    type Error = ActorError;

    /// The wire-boundary check. The envelope is a protocol the daemon must not
    /// assume well-formed, so a label that crossed a socket is parsed by the
    /// same rules as one read from the environment — a caller that skipped the
    /// CLI cannot skip the constraint.
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)?.ok_or(ActorError::Empty)
    }
}

/// Why a declared actor was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ActorError {
    /// Longer than [`MAX_ACTOR_LEN`].
    TooLong { len: usize, max: usize },
    /// Carries a control character, named by codepoint so the message is
    /// actionable about a byte the user cannot see.
    ControlCharacter { codepoint: u32 },
    /// Empty, reached only through the wire boundary — an empty *environment*
    /// value is `Ok(None)`, not an error, but an envelope that went to the
    /// trouble of carrying a field should not carry an empty one.
    Empty,
}

impl fmt::Display for ActorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLong { len, max } => write!(
                f,
                "${ACTOR_VAR} is {len} bytes; the limit is {max}. It labels who wrote a store \
                 event, so keep it short — `story.sh:dispatch-rollback` is the shape to aim for."
            ),
            Self::ControlCharacter { codepoint } => write!(
                f,
                "${ACTOR_VAR} contains a control character (U+{codepoint:04X}). An actor label is \
                 written into an audit trail and rendered into a terminal, so a newline or an \
                 escape sequence would let one entry rewrite what a reader sees. Storyhook \
                 refuses it rather than silently stripping it: the trail must say what you said."
            ),
            Self::Empty => write!(f, "${ACTOR_VAR} was carried as an empty value"),
        }
    }
}

impl std::error::Error for ActorError {}

/// Everything known about who performed one write.
///
/// Cheap to clone and carried on [`Ctx`](crate::service::Ctx) for the life of an
/// invocation, because every event a single command writes has the same
/// provenance by construction.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provenance {
    /// The verb the daemon dispatched. `None` only for a write that predates
    /// this field or that no command performed — a test fixture, a replay.
    pub command: Option<String>,
    /// What the caller said it was, if it said anything.
    pub actor: Option<ActorLabel>,
}

impl Provenance {
    /// Provenance for a write performed by `command`, with nothing declared.
    #[must_use]
    pub fn command(command: impl Into<String>) -> Self {
        Self {
            command: Some(command.into()),
            actor: None,
        }
    }

    /// The same, with a declared actor.
    #[must_use]
    pub fn with_actor(mut self, actor: Option<ActorLabel>) -> Self {
        self.actor = actor;
        self
    }

    /// Provenance for a write nothing can attribute — the value every
    /// pre-SH-246 row reads back as, and what a fixture writing events directly
    /// supplies. Renders as `(unrecorded)`.
    #[must_use]
    pub fn unrecorded() -> Self {
        Self::default()
    }

    /// Whether nothing at all is known about this write.
    #[must_use]
    pub fn is_unrecorded(&self) -> bool {
        self.command.is_none() && self.actor.is_none()
    }
}

impl fmt::Display for Provenance {
    /// One column of a rendered trail.
    ///
    /// The grammar is load-bearing and is documented for users in
    /// `story help log`: a bare word is the command the daemon derived, and
    /// **parentheses mean self-attested**. `(unrecorded)` is therefore
    /// consistent rather than a special case — it is the store admitting it was
    /// told nothing, not a claim that nobody acted.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (&self.command, &self.actor) {
            (Some(command), Some(actor)) => write!(f, "{command} ({actor})"),
            (Some(command), None) => f.write_str(command),
            (None, Some(actor)) => write!(f, "({actor})"),
            (None, None) => f.write_str("(unrecorded)"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_ordinary_label_parses() {
        let label = ActorLabel::parse("story.sh:dispatch-rollback")
            .expect("valid")
            .expect("present");
        assert_eq!(label.as_str(), "story.sh:dispatch-rollback");
    }

    #[test]
    fn surrounding_whitespace_is_trimmed_rather_than_refused() {
        // `STORYHOOK_ACTOR=" x "` is a shell quoting slip, not an attack, and
        // trimming is not silent alteration of meaning the way stripping an
        // escape would be.
        let label = ActorLabel::parse("  tui  ")
            .expect("valid")
            .expect("present");
        assert_eq!(label.as_str(), "tui");
    }

    #[test]
    fn an_empty_or_blank_label_is_no_declaration_rather_than_an_error() {
        assert_eq!(ActorLabel::parse("").expect("valid"), None);
        assert_eq!(ActorLabel::parse("   ").expect("valid"), None);
    }

    #[test]
    fn a_newline_is_refused() {
        let err = ActorLabel::parse("one\ntwo").expect_err("must refuse");
        assert!(matches!(err, ActorError::ControlCharacter { .. }));
        assert!(err.to_string().contains(ACTOR_VAR));
    }

    #[test]
    fn an_escape_sequence_is_refused() {
        let err = ActorLabel::parse("evil\u{1b}[2K").expect_err("must refuse");
        assert_eq!(err, ActorError::ControlCharacter { codepoint: 0x1b });
    }

    #[test]
    fn a_tab_and_a_carriage_return_are_refused_too() {
        // Both are control characters and both can misalign a rendered column.
        assert!(ActorLabel::parse("a\tb").is_err());
        assert!(ActorLabel::parse("a\rb").is_err());
    }

    #[test]
    fn an_over_long_label_is_refused_and_the_boundary_is_exact() {
        let at_limit = "x".repeat(MAX_ACTOR_LEN);
        assert!(
            ActorLabel::parse(&at_limit).is_ok(),
            "the limit itself fits"
        );

        let over = "x".repeat(MAX_ACTOR_LEN + 1);
        let err = ActorLabel::parse(&over).expect_err("must refuse");
        assert!(matches!(err, ActorError::TooLong { .. }));
        assert!(err.to_string().contains(ACTOR_VAR));
    }

    #[test]
    fn length_is_measured_in_bytes_after_trimming() {
        // Trimming happens first, so a padded label that fits once trimmed is
        // accepted rather than refused for whitespace it does not keep.
        let padded = format!("   {}   ", "x".repeat(MAX_ACTOR_LEN));
        assert!(ActorLabel::parse(&padded).is_ok());
    }

    #[test]
    fn a_multibyte_label_is_bounded_by_bytes_not_characters() {
        // The column stores bytes; a character bound would let a label four
        // times the intended size through.
        let wide = "é".repeat(MAX_ACTOR_LEN);
        assert!(
            ActorLabel::parse(&wide).is_err(),
            "{MAX_ACTOR_LEN} two-byte characters exceed a {MAX_ACTOR_LEN}-byte limit"
        );
    }

    #[test]
    fn the_wire_boundary_applies_the_same_rules() {
        // A caller that skipped the CLI cannot skip the constraint.
        assert!(ActorLabel::try_from("fine".to_string()).is_ok());
        assert!(ActorLabel::try_from("bad\u{1b}".to_string()).is_err());
        assert!(ActorLabel::try_from(String::new()).is_err());
    }

    #[test]
    fn provenance_renders_its_two_halves_distinguishably() {
        assert_eq!(Provenance::unrecorded().to_string(), "(unrecorded)");
        assert_eq!(Provenance::command("move").to_string(), "move");
        assert_eq!(
            Provenance::command("move")
                .with_actor(ActorLabel::parse("story.sh:dispatch").unwrap())
                .to_string(),
            "move (story.sh:dispatch)"
        );
    }

    #[test]
    fn an_unrecorded_provenance_never_renders_as_the_unset_dash() {
        // `-` means "unset, could be set later" elsewhere in this codebase.
        // Reusing it here would make a pre-cutover event read as "nobody did
        // this", which is the ambiguity SH-246 exists to remove.
        assert_ne!(Provenance::unrecorded().to_string(), "-");
        assert!(Provenance::unrecorded().is_unrecorded());
        assert!(!Provenance::command("move").is_unrecorded());
    }
}
