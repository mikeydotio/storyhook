//! Reading a legacy event log without understanding every event in it.

use std::path::Path;

use serde_json::value::RawValue;

use crate::domain::{StoryEvent, is_known_event_kind};

use super::LegacyError;

/// One event exactly as a legacy log holds it.
///
/// `payload` is the event's own JSON text, byte for byte, so an event this
/// binary cannot decode still survives a migration intact — that is what
/// [`crate::store::WriteOps::append_raw_events`] takes. `decoded` is `Some`
/// whenever the kind is one this binary understands; a *known* kind that fails
/// to decode is never retained as an unknown, it is a hard error, because a
/// `StoryCreated` with no title imported as an opaque blob is a story that
/// silently loses its title.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LegacyEvent {
    /// The `kind` discriminant.
    pub kind: String,
    /// The event's own RFC3339 timestamp.
    pub at: String,
    /// The payload's original JSON text.
    pub payload: String,
    /// The decoded event, when this binary knows the kind.
    pub decoded: Option<StoryEvent>,
}

impl LegacyEvent {
    /// Whether this binary could not decode the event.
    #[must_use]
    pub fn is_unknown(&self) -> bool {
        self.decoded.is_none()
    }
}

/// Parses one event's JSON text.
///
/// `path` and `ordinal` are carried only so a failure can name the file and the
/// position inside it — a corrupt log is worth refusing over, and a refusal
/// that does not say which line is worth very little.
pub(super) fn parse_event(
    path: &Path,
    ordinal: usize,
    text: &str,
) -> Result<LegacyEvent, LegacyError> {
    let malformed = |detail: String| LegacyError::Malformed {
        path: path.to_path_buf(),
        detail: format!("event {ordinal}: {detail}"),
    };

    let fields: serde_json::Map<String, serde_json::Value> = serde_json::from_str(text)
        .map_err(|error| malformed(format!("not a JSON object: {error}")))?;
    let kind = fields
        .get("kind")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| malformed("no string `kind` field".to_string()))?
        .to_string();
    let at = fields
        .get("at")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| malformed(format!("`{kind}` has no string `at` field")))?
        .to_string();

    let decoded = if is_known_event_kind(&kind) {
        Some(
            serde_json::from_str::<StoryEvent>(text)
                .map_err(|error| malformed(format!("`{kind}` is not a valid event: {error}")))?,
        )
    } else {
        None
    };

    Ok(LegacyEvent {
        kind,
        at,
        payload: text.to_string(),
        decoded,
    })
}

/// Every event in a JSONL log, oldest first.
///
/// Blank lines are skipped, matching the legacy reader. A *partial* final line
/// is not tolerated here, deliberately: `storage::load_open_snapshots_tolerant`
/// skips one so the TUI can reload while a write is in flight, but a migration
/// reads a tree once and keeps the result forever, and quietly dropping the
/// most recent event of a story is the least acceptable thing this program
/// could do.
pub(super) fn read_jsonl_log(path: &Path) -> Result<Vec<LegacyEvent>, LegacyError> {
    let text = super::read_to_string(path)?;
    let mut events = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        events.push(parse_event(path, index + 1, line)?);
    }
    Ok(events)
}

/// Every event in an archived story's `events_json` column, oldest first.
///
/// The column holds a JSON *array*, so each element's original text is
/// recovered with [`RawValue`] rather than re-serialized — an unknown event
/// round-trips through a migration byte for byte, key order included.
pub(super) fn read_event_array(path: &Path, json: &str) -> Result<Vec<LegacyEvent>, LegacyError> {
    let raw: Vec<&RawValue> =
        serde_json::from_str(json).map_err(|error| LegacyError::Malformed {
            path: path.to_path_buf(),
            detail: format!("stored events are not a JSON array: {error}"),
        })?;
    raw.into_iter()
        .enumerate()
        .map(|(index, element)| parse_event(path, index + 1, element.get()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const CREATED: &str =
        r#"{"kind":"StoryCreated","at":"2026-01-01T00:00:00Z","title":"t","state":"todo"}"#;

    #[test]
    fn a_known_event_decodes_and_keeps_its_bytes() {
        let event = parse_event(Path::new("x.jsonl"), 1, CREATED).unwrap();
        assert_eq!(event.kind, "StoryCreated");
        assert_eq!(event.at, "2026-01-01T00:00:00Z");
        assert_eq!(event.payload, CREATED, "the original text must survive");
        assert!(!event.is_unknown());
    }

    #[test]
    fn an_unknown_kind_is_retained_rather_than_refused() {
        let text = r#"{"kind":"StoryPinned","at":"2026-01-01T00:00:00Z","by":"ada"}"#;
        let event = parse_event(Path::new("x.jsonl"), 1, text).unwrap();
        assert!(
            event.is_unknown(),
            "an event kind a newer storyhook wrote must survive a reader that predates it (SH-54)"
        );
        assert_eq!(event.payload, text);
        assert_eq!(event.at, "2026-01-01T00:00:00Z");
    }

    #[test]
    fn a_corrupt_known_event_is_refused_by_name() {
        let text = r#"{"kind":"StoryCreated","at":"2026-01-01T00:00:00Z","state":"todo"}"#;
        let error = parse_event(Path::new("SH-1.jsonl"), 4, text).unwrap_err();
        let message = error.to_string();
        assert!(
            message.contains("SH-1.jsonl") && message.contains("event 4"),
            "a refusal must name the file and the position: {message}"
        );
        assert!(
            message.contains("title"),
            "and say what was missing: {message}"
        );
    }

    #[test]
    fn an_event_with_no_kind_or_no_timestamp_is_refused() {
        for text in [
            r#"{"at":"2026-01-01T00:00:00Z"}"#,
            r#"{"kind":"StoryPinned"}"#,
            r#"{"kind":42,"at":"2026-01-01T00:00:00Z"}"#,
            "not json at all",
            "[]",
        ] {
            assert!(
                parse_event(Path::new("SH-1.jsonl"), 1, text).is_err(),
                "an event storyhook cannot even index by kind and time is corrupt, not unknown: \
                 {text}"
            );
        }
    }

    #[test]
    fn an_event_array_keeps_each_elements_original_bytes() {
        let json = format!(
            r#"[{CREATED},{{"kind":"StoryPinned","at":"2026-01-02T00:00:00Z","z":1,"a":2}}]"#
        );
        let events = read_event_array(Path::new("archive.db"), &json).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].payload, CREATED);
        assert_eq!(
            events[1].payload, r#"{"kind":"StoryPinned","at":"2026-01-02T00:00:00Z","z":1,"a":2}"#,
            "an unknown payload must not be re-serialized — key order is part of `verbatim`"
        );
    }
}
