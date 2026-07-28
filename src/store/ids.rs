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
    pub fn parse_id(prefix: &str, id: &str) -> Result<Self, StoreError> {
        let rest = id.strip_prefix(prefix).and_then(|r| r.strip_prefix('-'));
        let Some(rest) = rest else {
            return Err(StoreError::Validation(format!(
                "story id `{id}` does not belong to a project with prefix `{prefix}`"
            )));
        };
        // Reject `SH-007` and `SH-+1`: a number that does not render back to
        // the same text would make `to_id` and `parse_id` disagree.
        let number: i64 = rest.parse().map_err(|_| {
            StoreError::Validation(format!("story id `{id}` has a non-numeric story number"))
        })?;
        if number < 1 || number.to_string() != rest {
            return Err(StoreError::Validation(format!(
                "story id `{id}` has a malformed story number"
            )));
        }
        Ok(Self(number))
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

/// Whether a recorded checkout is a repository's main working tree or a linked
/// worktree.
///
/// Both resolve to the same project. That is the entire point: SH-46 exists
/// because a worktree used to resolve to a *different* tracker.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PathKind {
    /// The repository's main working tree.
    Main,
    /// A linked worktree created by `git worktree add`.
    Worktree,
}

impl PathKind {
    /// The value stored in the `project_paths.kind` column.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Main => "main",
            Self::Worktree => "worktree",
        }
    }

    /// Parses a stored `project_paths.kind` value.
    pub fn parse(raw: &str) -> Result<Self, StoreError> {
        match raw {
            "main" => Ok(Self::Main),
            "worktree" => Ok(Self::Worktree),
            other => Err(StoreError::Corrupt(format!(
                "project_paths.kind holds unknown value `{other}`"
            ))),
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
    fn path_kinds_round_trip_through_their_stored_form() {
        for kind in [PathKind::Main, PathKind::Worktree] {
            assert_eq!(PathKind::parse(kind.as_str()).unwrap(), kind);
        }
        assert!(PathKind::parse("submodule").is_err());
    }

    #[test]
    fn expected_seq_renders_for_conflict_messages() {
        assert_eq!(ExpectedSeq::Any.to_string(), "any");
        assert_eq!(ExpectedSeq::Exact(EventSeq::new(3)).to_string(), "seq 3");
    }
}
