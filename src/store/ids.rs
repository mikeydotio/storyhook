//! Identity newtypes for the store.
//!
//! Every one of these exists to make a *scoping mistake* a compile error rather
//! than a silent cross-project read. In a single global database where every
//! repository defaults to the prefix `SH`, a query that forgets its project
//! scope does not fail — it returns another project's story with the same
//! number. [`ProjectId`] is therefore a required argument of every read and
//! write in [`crate::store::ReadOps`] and [`crate::store::WriteOps`], and
//! [`StoryNo`] is deliberately *not* interchangeable with the `usize` a caller
//! might have parsed out of a CLI argument.

use serde::{Deserialize, Serialize};

use crate::store::StoreError;

/// The database identity of a project.
///
/// Assigned by [`crate::store::WriteOps::create_project`]; stable for the life
/// of the row. Distinct from the project's `uuid` (the value committed to a
/// repository's pointer file) and from its `slug` (the human-facing handle the
/// legacy registry called an id) — those are portable, this one is local.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ProjectId(i64);

impl ProjectId {
    /// Wraps a raw row id.
    ///
    /// Only the store's own engine should need this; callers receive
    /// `ProjectId` values from the store and pass them back unchanged.
    #[must_use]
    pub const fn new(raw: i64) -> Self {
        Self(raw)
    }

    /// The underlying row id.
    #[must_use]
    pub const fn get(self) -> i64 {
        self.0
    }
}

impl std::fmt::Display for ProjectId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A story's number within its project — the `1` in `SH-1`.
///
/// Numbers are allocated by [`crate::store::WriteOps::allocate_story_no`] from
/// a per-project counter held in the same transaction as the write that uses
/// it. Two checkouts of one repository therefore cannot mint the same number,
/// which is the corruption this rearchitecture exists to end.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct StoryNo(i64);

impl StoryNo {
    /// Wraps a raw story number.
    #[must_use]
    pub const fn new(raw: i64) -> Self {
        Self(raw)
    }

    /// The underlying number.
    #[must_use]
    pub const fn get(self) -> i64 {
        self.0
    }

    /// Renders the story's public id under a project prefix — `SH-1`.
    #[must_use]
    pub fn to_id(self, prefix: &str) -> String {
        format!("{prefix}-{}", self.0)
    }

    /// Parses a public story id (`SH-1`) under the project's `prefix`.
    ///
    /// Rejects an id belonging to a different prefix. That rejection is
    /// load-bearing: it is what stops a relation, or a snapshot, from naming a
    /// story in another project — the failure mode a shared database makes
    /// possible for the first time.
    ///
    /// Strict about the prefix's *case* as well as its letters, because a
    /// stored prefix is always canonical uppercase
    /// ([`crate::domain::prefix::validate`] uppercases before storing) and this
    /// is the parser data goes through. A user typing `sh-1` at a terminal is
    /// [`StoryRef::classify`]'s problem, not this one's.
    pub fn parse_id(prefix: &str, id: &str) -> Result<Self, StoreError> {
        let rest = id.strip_prefix(prefix).and_then(|r| r.strip_prefix('-'));
        let Some(rest) = rest else {
            return Err(StoreError::Validation(format!(
                "story id `{id}` does not belong to a project with prefix `{prefix}`"
            )));
        };
        story_number(rest)
            .map(Self)
            .map_err(|why| StoreError::Validation(format!("story id `{id}` has a {why} number")))
    }
}

/// Why a string is not a story number.
///
/// Two variants because the two messages predate this function and are worth
/// keeping: "that is not a number at all" and "that is a number written a way
/// this project cannot mint" are different mistakes.
enum NotANumber {
    NonNumeric,
    Malformed,
}

impl std::fmt::Display for NotANumber {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NonNumeric => write!(f, "non-numeric story"),
            Self::Malformed => write!(f, "malformed story"),
        }
    }
}

/// The number half of a story id, under the single rule that applies to it
/// wherever it appears.
///
/// `n >= 1`, and the number must render back to the text it came from — so
/// `007`, `+1`, `-1` and `01` are refused, because a number that does not
/// round-trip would make [`StoryNo::to_id`] and [`StoryNo::parse_id`] disagree.
///
/// It is a function rather than two copies because [`StoryNo::parse_id`] and
/// [`StoryRef::classify`] must not be *able* to disagree about what a story
/// number is: `5` and `SH-5` naming different things is precisely the defect
/// SH-118 exists to make unrepresentable.
fn story_number(text: &str) -> Result<i64, NotANumber> {
    let number: i64 = text.parse().map_err(|_| NotANumber::NonNumeric)?;
    if number < 1 || number.to_string() != text {
        return Err(NotANumber::Malformed);
    }
    Ok(number)
}

/// What a token in an id position turns out to be, once the selected project's
/// prefix is known.
///
/// The whole of SH-118's grammar, as three arms rather than a bool and a
/// convention. A caller that has one of these cannot forget the middle case,
/// which is the one that used to be answered — wrongly — with *not found*.
///
/// # Why the middle arm exists at all
///
/// Under one global store, `OTH-1` typed while scoped to `probe` is not a
/// missing story: it is a story that exists, in a project this command was not
/// pointed at. Reporting it as absent sends the reader looking for the wrong
/// thing, and the right answer — refuse, and name both — needs a way to say
/// "this names somewhere else" that is distinct from "I do not recognize this".
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StoryRef {
    /// A story in the selected project: `SH-5` under prefix `SH`, or a bare `5`.
    Here(StoryNo),
    /// A well-formed canonical id under a **different** prefix, carrying that
    /// prefix exactly as it was typed.
    ///
    /// Whether any project actually holds that prefix is not settled here, and
    /// deliberately: this type is pure, and the lookup costs a read that only a
    /// command already on its way to refusing should pay.
    Elsewhere { prefix: String },
    /// Anything else — `007`, `1x`, `SH-`, `nonsense`, and a prefix in the
    /// wrong case.
    Unrecognized,
}

impl StoryRef {
    /// Classifies one token typed in an id position, against the selected
    /// project's `prefix`.
    ///
    /// Pure, and total: every string is one of the three arms. The number rule
    /// is [`story_number`]'s, shared with [`StoryNo::parse_id`].
    ///
    /// # A prefix in the wrong case is `Unrecognized`, not `Elsewhere`
    ///
    /// `sh-1` under prefix `SH` could be read three ways and only one of them
    /// is honest. It is not `Here`: accepting it would add a leniency nobody
    /// asked for, and `parse_id` — the rule stored data goes through — refuses
    /// it. It is emphatically not `Elsewhere`, because a stored prefix is
    /// always uppercase, so the claimant lookup would find *the selected
    /// project itself* and produce a refusal naming one project twice. What is
    /// left is the truth: that is not an id this project recognizes, which is
    /// exactly what it has always been.
    #[must_use]
    pub fn classify(prefix: &str, token: &str) -> Self {
        if let Ok(number) = story_number(token) {
            return Self::Here(StoryNo(number));
        }
        let Some((left, rest)) = token.split_once('-') else {
            return Self::Unrecognized;
        };
        if left.eq_ignore_ascii_case(prefix) {
            return match StoryNo::parse_id(prefix, token) {
                Ok(no) => Self::Here(no),
                Err(_) => Self::Unrecognized,
            };
        }
        // A prefix shape this tool could have minted, and a number it could
        // have minted — which together is what makes the token a *claim about
        // another project* rather than a typo. `validate` is asked rather than
        // re-implemented, so the two cannot come to disagree about what a
        // prefix is.
        if crate::domain::prefix::validate(left).is_ok() && story_number(rest).is_ok() {
            return Self::Elsewhere {
                prefix: left.to_string(),
            };
        }
        Self::Unrecognized
    }
}

impl std::fmt::Display for StoryNo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A story's per-story event sequence number, starting at 1.
///
/// `EventSeq(0)` is the head of a story with no events, and is the value
/// [`ExpectedSeq::Exact`] takes to mean "this story must not exist yet".
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct EventSeq(i64);

impl EventSeq {
    /// The head of a story that has no events.
    pub const ZERO: Self = Self(0);

    /// Wraps a raw sequence number.
    #[must_use]
    pub const fn new(raw: i64) -> Self {
        Self(raw)
    }

    /// The underlying sequence number.
    #[must_use]
    pub const fn get(self) -> i64 {
        self.0
    }
}

impl std::fmt::Display for EventSeq {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A per-project monotonic sequence number spanning every story's events.
///
/// This is the change feed: [`crate::store::ReadOps::events_since`] pages
/// through it, which is how a daemon tells a client what has happened since it
/// last looked without diffing whole stories.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct GlobalSeq(i64);

impl GlobalSeq {
    /// The position before any event has been appended.
    pub const ZERO: Self = Self(0);

    /// Wraps a raw sequence number.
    #[must_use]
    pub const fn new(raw: i64) -> Self {
        Self(raw)
    }

    /// The underlying sequence number.
    #[must_use]
    pub const fn get(self) -> i64 {
        self.0
    }
}

impl std::fmt::Display for GlobalSeq {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// The compare-and-swap precondition of an append.
///
/// A writer that read a story, decided something, and now wants to write it
/// back passes [`ExpectedSeq::Exact`] with the head it read. If anything landed
/// in between, the append fails with [`StoreError::Conflict`] rather than
/// silently interleaving — the guarantee `--if-state` claims and the
/// per-directory file lock never actually provided.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExpectedSeq {
    /// Append regardless of the current head.
    Any,
    /// Append only if the story's head is exactly this sequence number.
    Exact(EventSeq),
}

impl std::fmt::Display for ExpectedSeq {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Any => write!(f, "any"),
            Self::Exact(seq) => write!(f, "seq {seq}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn story_ids_round_trip_through_their_prefix() {
        let no = StoryNo::new(42);
        assert_eq!(no.to_id("SH"), "SH-42");
        assert_eq!(StoryNo::parse_id("SH", "SH-42").unwrap(), no);
    }

    #[test]
    fn a_story_id_from_another_prefix_is_rejected() {
        let error = StoryNo::parse_id("SH", "OTHER-42").unwrap_err();
        assert!(
            error.to_string().contains("does not belong"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn a_prefix_that_is_a_prefix_of_another_does_not_match() {
        // `S` must not swallow `SH-1`: the separator is required.
        assert!(StoryNo::parse_id("S", "SH-1").is_err());
    }

    #[test]
    fn malformed_story_numbers_are_rejected() {
        for id in ["SH-0", "SH--1", "SH-007", "SH-", "SH-x", "SH-1x", "SH-+1"] {
            assert!(
                StoryNo::parse_id("SH", id).is_err(),
                "`{id}` should not parse"
            );
        }
    }

    #[test]
    fn expected_seq_renders_for_conflict_messages() {
        assert_eq!(ExpectedSeq::Any.to_string(), "any");
        assert_eq!(ExpectedSeq::Exact(EventSeq::new(3)).to_string(), "seq 3");
    }

    #[test]
    fn both_forms_of_the_same_story_classify_here() {
        assert_eq!(
            StoryRef::classify("SH", "5"),
            StoryRef::Here(StoryNo::new(5))
        );
        assert_eq!(
            StoryRef::classify("SH", "SH-5"),
            StoryRef::Here(StoryNo::new(5))
        );
    }

    #[test]
    fn the_bare_form_and_the_canonical_form_share_one_number_rule() {
        // The contract that makes `5` and `SH-5` the same story: whatever
        // `parse_id` accepts after the separator, the bare form accepts, and
        // whatever it refuses, the bare form refuses.
        for number in ["1", "5", "42", "1000000"] {
            assert_eq!(
                StoryRef::classify("SH", number),
                StoryRef::classify("SH", &format!("SH-{number}")),
                "`{number}` and `SH-{number}` must classify the same"
            );
        }
        for bad in ["0", "007", "-1", "+1", "1x", "", " 1", "1 ", "01"] {
            assert_eq!(
                StoryRef::classify("SH", bad),
                StoryRef::Unrecognized,
                "`{bad}` is not a story number"
            );
            assert_eq!(
                StoryRef::classify("SH", &format!("SH-{bad}")),
                StoryRef::Unrecognized,
                "`SH-{bad}` is not a story id either"
            );
        }
    }

    #[test]
    fn a_well_formed_id_under_another_prefix_names_somewhere_else() {
        assert_eq!(
            StoryRef::classify("SH", "OTH-1"),
            StoryRef::Elsewhere {
                prefix: "OTH".to_string()
            }
        );
        // The prefix travels exactly as typed, because the refusal quotes it.
        assert_eq!(
            StoryRef::classify("SH", "oth-1"),
            StoryRef::Elsewhere {
                prefix: "oth".to_string()
            }
        );
    }

    #[test]
    fn a_foreign_prefix_with_a_bad_number_is_merely_unrecognized() {
        // Symmetry with `SH-007`: the number rule decides first, so a token
        // nothing could have minted is a typo rather than a claim about
        // another project.
        for id in ["OTH-007", "OTH-0", "OTH-x", "OTH-"] {
            assert_eq!(
                StoryRef::classify("SH", id),
                StoryRef::Unrecognized,
                "`{id}` should not name another project"
            );
        }
    }

    #[test]
    fn our_own_prefix_in_the_wrong_case_is_unrecognized() {
        // Not `Here` — `parse_id` refuses it, and the two must agree. Not
        // `Elsewhere` — a stored prefix is uppercase, so the claimant lookup
        // would find the selected project itself and name it as the *other*
        // project.
        assert_eq!(StoryRef::classify("SH", "sh-1"), StoryRef::Unrecognized);
        assert_eq!(StoryRef::classify("SH", "Sh-1"), StoryRef::Unrecognized);
    }

    #[test]
    fn a_token_that_is_no_kind_of_id_is_unrecognized() {
        for token in ["nonsense", "-", "-1x", "a-b", "SH", "1-2-3", "--force"] {
            assert_eq!(
                StoryRef::classify("SH", token),
                StoryRef::Unrecognized,
                "`{token}` should be unrecognized"
            );
        }
    }

    #[test]
    fn the_first_hyphen_splits_the_token() {
        // `1-2-3` reads as prefix `1`, which is not a legal prefix, so it is
        // unrecognized rather than story 2 of project `1`. Pinned because the
        // alternative — splitting on the *last* hyphen — would make a project
        // whose prefix contains a digit ambiguous.
        assert_eq!(StoryRef::classify("SH", "1-2-3"), StoryRef::Unrecognized);
        assert_eq!(
            StoryRef::classify("SH", "AB-2-3"),
            StoryRef::Unrecognized,
            "the remainder after the first hyphen must be a whole number"
        );
    }
}
