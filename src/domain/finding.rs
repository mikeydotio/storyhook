//! What `story doctor` found, as data rather than as a sentence.
//!
//! # The loss this exists to stop
//!
//! Every check `story doctor` runs already holds its answer structured — a
//! [`Divergence`] with four named fields, a relation and the story at its far
//! end, a slug the catalog cannot address. All of it was flattened into one
//! newline-joined string on its way into `AppError::Integrity`, and a caller
//! wanting any of it back had to write a regex. SH-243 did exactly that, over
//! 1.68MB and 109 findings, to answer a question the producer could have
//! answered directly.
//!
//! So a finding travels as a [`Finding`]: a [`FindingCode`] a script matches
//! on, the story it concerns, the story a repair belongs *on*, the sentence a
//! human reads, and — where the producer held more than a sentence — a
//! [`FindingData`] carrying it.
//!
//! # The sentence is a field, not a rendering
//!
//! [`Finding::message`] holds the exact line `story doctor` prints, moved
//! verbatim from the `format!` that used to be the only copy. The rendered
//! report is those lines joined, so there is **one** string per finding and
//! one place it lives: structure and prose cannot drift apart, because there is
//! nothing to drift from. This is the same reasoning
//! [`crate::error::WireError`] uses when it declines to transport a message it
//! can recompute — a second copy of a value is a second thing that can be
//! wrong.
//!
//! # `subject` and `remedy` are different questions
//!
//! `subject` is what is wrong. `remedy` is where the fix goes, and SH-225 is
//! the story of the two being conflated: a missing inverse relation is
//! reported against the end that *has* its half, while the repair has to be
//! written to the end that *lacks* it. An operator working from the finding
//! alone reopened the wrong story for a week. `remedy` is that second story,
//! carried as data instead of buried in prose an operator has to read
//! carefully.
//!
//! `remedy` is a **pointer, not a promise** — it names where a repair would
//! land, never that `story doctor --fix` will succeed in landing one. Whether
//! the destination is closed, and so out of reach, is knowable only inside
//! [`crate::service::IntegrityService::repair`], which says so in its own output.

use serde::{Deserialize, Serialize};

/// Which check produced a [`Finding`].
///
/// A Rust enum rather than a free string, because nothing can hand this build
/// a code it has never heard of: a client and a daemon from different builds
/// never talk (`daemon::lifecycle`'s `is_this_binary` compares version,
/// executable path *and* executable mtime; `api::rpc`'s `compatible` refuses a
/// mismatch independently), and findings are computed per run rather than
/// stored. So the forward-compatibility that argues for a string elsewhere
/// buys nothing here, while an enum makes the renderer's match exhaustive —
/// a check added without a rendering stops the build.
///
/// `rename_all = "snake_case"` means the wire form is a plain lower-case
/// string, so a `jq` consumer cannot tell this from a stringly-typed field.
/// The type safety is entirely on this side of the wire, which is where it was
/// wanted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingCode {
    /// The project does not define the states SH-125 requires.
    RequiredStates,
    /// A type whose slug no command can address (SH-134).
    UnaddressableType,
    /// A story with more than one parent.
    MultipleParents,
    /// A relation pointing at a story that does not exist.
    DanglingRelation,
    /// A relation whose inverse the other end's history does not claim.
    MissingInverseRelation,
    /// The mutual-relation spelling of [`Self::MissingInverseRelation`].
    MissingReciprocalRelation,
    /// A parent/child cycle.
    ParentChildCycle,
    /// A story typed with a slug the catalog does not define.
    UnknownType,
    /// A label carrying a comma, or blank (SH-164).
    MalformedLabels,
    /// A story with events and no read-model row.
    MissingRow,
    /// A read-model row with no events behind it.
    ExtraRow,
    /// A story whose events will not fold at all.
    FoldFailure,
    /// A read-model row that disagrees with its own events — the finding
    /// SH-243 had to regex out of a 1.68MB string, and the reason this module
    /// exists.
    ReadModelDivergence,
    /// An event of a kind this build *knows* and could not decode: a torn
    /// payload, which is damage. The kind it has *never heard of* is a newer
    /// storyhook's data and is not a finding at all — it rides the advice
    /// channel, per SH-185.
    UndecodableEvent,
    /// A story the events do not corroborate, whose story-level checks were
    /// therefore not run (SH-286).
    ///
    /// The disclosure half of a suppression. The doctor's story-level checks
    /// read the `stories` table, which is a cache of a fold of the events; when
    /// a story's row is missing, or present but unattested by its own history,
    /// nothing that row says is evidence and nothing its absence implies is
    /// damage. So those checks are skipped and this is minted in their place —
    /// **one per unattested story**, so the report cannot get quieter without
    /// saying why. See [`crate::domain::compute_integrity_issues`] for the
    /// three rules and `crate::store::ReadModelDiff::unattested` for the set.
    UnexaminedStory,
    /// An integrity fault raised as prose by a caller that has no structure to
    /// offer — a story missing a required field mid-fold, a refused migration,
    /// a store invariant.
    ///
    /// Its existence is what lets `From<String>` be **total**: every
    /// `AppError::Integrity`, from any raise site, carries at least one
    /// finding, so an integrity error with an empty `findings` array is
    /// unrepresentable rather than merely unusual.
    Unstructured,
}

/// What a finding carries beyond its sentence.
///
/// Only the codes whose producer held more than prose have a variant here;
/// `data` is `None` for the rest rather than an empty object, because "there
/// was nothing more to say" and "there was something and it is empty" are
/// different facts.
///
/// Externally tagged, the serde default: the payload names its own shape, so a
/// consumer never has to consult [`Finding::code`] to know how to read it, and
/// deserialization cannot be ambiguous the way an untagged enum's is once two
/// variants come to share a field set.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingData {
    /// A read-model field and the two values that disagree about it.
    Divergence {
        /// Which field of the story's row.
        field: String,
        /// What the read model holds.
        persisted: String,
        /// What a fold of the story's own events produces instead.
        rebuilt: String,
    },
    /// One event, named by its position in a story's history.
    Event {
        /// The event's sequence number within its story.
        seq: i64,
        /// Its kind discriminant, as stored.
        event_kind: String,
    },
    /// An edge, as the claiming end records it.
    Relation {
        /// The relation's name.
        relation: String,
        /// The story at the far end.
        other: String,
    },
    /// A type slug.
    Type {
        /// The slug, exactly as stored — including whatever makes it
        /// unaddressable.
        slug: String,
    },
    /// The labels a story carries, unrepaired.
    Labels {
        /// Every label, in stored order.
        labels: Vec<String>,
    },
    /// Why something could not be done, when the reason is prose from a layer
    /// below.
    Reason {
        /// The underlying explanation.
        reason: String,
    },
}

/// One thing `story doctor` found wrong with a project.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    /// Which check produced this.
    pub code: FindingCode,
    /// The story this is about. `None` for a finding that is a property of the
    /// *project* — the required-state floor, an unaddressable type — where a
    /// story id would be an invention.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    /// The story a repair has to be written to, when that is not
    /// [`subject`](Self::subject). See this module's own documentation: a
    /// pointer, never a promise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remedy: Option<String>,
    /// The exact line `story doctor` prints for this finding.
    pub message: String,
    /// What the producer held beyond the sentence.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<FindingData>,
}

impl Finding {
    /// A finding with nothing but a code and its sentence.
    #[must_use]
    pub fn new(code: FindingCode, message: impl Into<String>) -> Self {
        Self {
            code,
            subject: None,
            remedy: None,
            message: message.into(),
            data: None,
        }
    }

    /// The same finding, about `subject`.
    #[must_use]
    pub fn about(mut self, subject: impl Into<String>) -> Self {
        self.subject = Some(subject.into());
        self
    }

    /// The same finding, repaired on `remedy`.
    #[must_use]
    pub fn repaired_on(mut self, remedy: impl Into<String>) -> Self {
        self.remedy = Some(remedy.into());
        self
    }

    /// The same finding, carrying `data`.
    #[must_use]
    pub fn carrying(mut self, data: FindingData) -> Self {
        self.data = Some(data);
        self
    }

    /// An integrity fault a caller could only describe in prose.
    ///
    /// The one constructor that is not about `story doctor`, and the reason
    /// `IntegrityDetail: From<String>` can be total — see
    /// [`FindingCode::Unstructured`].
    #[must_use]
    pub fn unstructured(message: impl Into<String>) -> Self {
        Self::new(FindingCode::Unstructured, message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The wire form of a code is a plain snake_case string, so structuring
    /// this side of the wire costs a `jq` consumer nothing.
    #[test]
    fn a_code_travels_as_a_plain_snake_case_string() {
        let rendered = serde_json::to_value(FindingCode::ReadModelDivergence).expect("serializing");
        assert_eq!(rendered, serde_json::json!("read_model_divergence"));
    }

    /// Absent is absent: a finding with no story and no payload must not emit
    /// `null`s a consumer then has to filter.
    #[test]
    fn the_optional_halves_are_omitted_rather_than_nulled() {
        let rendered = serde_json::to_value(Finding::new(FindingCode::RequiredStates, "missing"))
            .expect("serializing");
        let object = rendered.as_object().expect("an object");
        assert_eq!(
            object.keys().map(String::as_str).collect::<Vec<_>>(),
            vec!["code", "message"]
        );
    }

    /// The four values SH-243 had to regex out of a sentence, reachable
    /// without one. This is the story's acceptance criterion in miniature.
    #[test]
    fn a_divergence_carries_the_four_values_sh243_had_to_parse() {
        let finding = Finding::new(FindingCode::ReadModelDivergence, "story 41: draft is …")
            .about("SH-41")
            .carrying(FindingData::Divergence {
                field: "draft".to_string(),
                persisted: "true".to_string(),
                rebuilt: "false".to_string(),
            });
        let rendered = serde_json::to_value(&finding).expect("serializing");

        assert_eq!(rendered["subject"], serde_json::json!("SH-41"));
        assert_eq!(rendered["data"]["divergence"]["field"], "draft");
        assert_eq!(rendered["data"]["divergence"]["persisted"], "true");
        assert_eq!(rendered["data"]["divergence"]["rebuilt"], "false");
    }

    /// Every variant survives a round trip intact — the property
    /// `tests/wire_envelope.rs` relies on for the whole error envelope.
    #[test]
    fn every_data_variant_round_trips() {
        for data in [
            FindingData::Divergence {
                field: "f".to_string(),
                persisted: "p".to_string(),
                rebuilt: "r".to_string(),
            },
            FindingData::Event {
                seq: 7,
                event_kind: "StoryVibed".to_string(),
            },
            FindingData::Relation {
                relation: "blocks".to_string(),
                other: "SH-2".to_string(),
            },
            FindingData::Type {
                slug: "in review".to_string(),
            },
            FindingData::Labels {
                labels: vec!["web,sse".to_string()],
            },
            FindingData::Reason {
                reason: "unknown state".to_string(),
            },
        ] {
            let json = serde_json::to_string(&data).expect("serializing");
            let back: FindingData = serde_json::from_str(&json).expect("deserializing");
            assert_eq!(
                data, back,
                "a finding payload must survive the wire: {json}"
            );
        }
    }

    /// `remedy` is a separate question from `subject`, and SH-225 is the cost
    /// of answering them with one value.
    #[test]
    fn a_remedy_can_name_a_story_the_subject_does_not() {
        let finding = Finding::new(FindingCode::MissingInverseRelation, "SH-1: …")
            .about("SH-1")
            .repaired_on("SH-2");
        assert_eq!(finding.subject.as_deref(), Some("SH-1"));
        assert_eq!(finding.remedy.as_deref(), Some("SH-2"));
    }
}
