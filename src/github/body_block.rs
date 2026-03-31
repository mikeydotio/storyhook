use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Metadata block embedded in a GitHub Issue body as a fenced `storyhook` code block.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct StoryhookBlock {
    pub story_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub awaiting: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub relationships: Vec<BlockRelationship>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sync_version: Option<u32>,
}

/// A single relationship entry represented as a one-entry map, e.g. `{"relates-to": "SH-10"}`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct BlockRelationship {
    #[serde(flatten)]
    pub entries: BTreeMap<String, String>,
}

const FENCE_OPEN: &str = "```storyhook";
const FENCE_CLOSE: &str = "```";

/// Extract the **last** `storyhook` fenced code block from an issue body.
///
/// Returns the body text with the block (and its preceding `---` separator, if any)
/// removed, together with the parsed [`StoryhookBlock`].
///
/// Returns `None` if no valid block is found or if the YAML inside is malformed.
pub fn extract_block(body: &str) -> Option<(String, StoryhookBlock)> {
    // Find the last occurrence of the opening fence.
    let fence_start = body.rfind(FENCE_OPEN)?;

    // The YAML content starts on the line after the opening fence.
    let after_open = fence_start + FENCE_OPEN.len();
    let yaml_start = body[after_open..].find('\n').map(|i| after_open + i + 1)?;

    // Find the closing fence after the YAML content.
    let yaml_region = &body[yaml_start..];
    let close_offset = yaml_region.find(FENCE_CLOSE)?;
    let yaml_text = &yaml_region[..close_offset];
    let block_end = yaml_start + close_offset + FENCE_CLOSE.len();

    // Parse the YAML.
    let block: StoryhookBlock = serde_yml::from_str(yaml_text).ok()?;

    // Build the remaining body text, stripping the block and an optional `---` separator.
    let before_block = &body[..fence_start];
    let after_block = &body[block_end..];

    // Strip a preceding `---` separator (with optional surrounding whitespace/newlines).
    let trimmed_before = before_block.trim_end();
    let cleaned_before = if trimmed_before.ends_with("---") {
        trimmed_before.trim_end_matches("---").trim_end()
    } else {
        trimmed_before
    };

    let mut result = String::from(cleaned_before);
    let after_trimmed = after_block.trim_start_matches('\n');
    if !after_trimmed.is_empty() {
        if !result.is_empty() {
            result.push('\n');
        }
        result.push_str(after_trimmed);
    }

    Some((result, block))
}

/// Render a [`StoryhookBlock`] onto a body string.
///
/// If the body already contains a `storyhook` block it is replaced; otherwise the block
/// is appended after a `---` separator.
pub fn render_block(body_text: &str, block: &StoryhookBlock) -> String {
    // If there is an existing block, strip it first.
    let base = if body_text.contains(FENCE_OPEN) {
        match extract_block(body_text) {
            Some((cleaned, _)) => cleaned,
            None => body_text.to_string(),
        }
    } else {
        body_text.to_string()
    };

    let yaml = serde_yml::to_string(block).expect("StoryhookBlock should always serialize");
    // serde_yml may emit a leading `---\n`; strip it for a cleaner output.
    let yaml = yaml.strip_prefix("---\n").unwrap_or(&yaml);
    let yaml = yaml.trim_end();

    format!("{}\n\n---\n\n```storyhook\n{}\n```\n", base.trim_end(), yaml)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_block() -> StoryhookBlock {
        StoryhookBlock {
            story_id: "SH-42".into(),
            priority: Some("high".into()),
            awaiting: Some("code review from @alice".into()),
            relationships: vec![
                BlockRelationship {
                    entries: BTreeMap::from([("relates-to".into(), "SH-10".into())]),
                },
                BlockRelationship {
                    entries: BTreeMap::from([("obviates".into(), "SH-15".into())]),
                },
            ],
            sync_version: Some(1),
        }
    }

    fn sample_body() -> String {
        [
            "Human-written issue description here.",
            "",
            "---",
            "",
            "```storyhook",
            "story_id: SH-42",
            "priority: high",
            "awaiting: \"code review from @alice\"",
            "relationships:",
            "  - relates-to: SH-10",
            "  - obviates: SH-15",
            "sync_version: 1",
            "```",
            "",
        ]
        .join("\n")
    }

    #[test]
    fn parse_valid_block() {
        let body = sample_body();
        let (text, block) = extract_block(&body).expect("should parse");
        assert_eq!(text, "Human-written issue description here.");
        assert_eq!(block.story_id, "SH-42");
        assert_eq!(block.priority.as_deref(), Some("high"));
        assert_eq!(block.awaiting.as_deref(), Some("code review from @alice"));
        assert_eq!(block.relationships.len(), 2);
        assert_eq!(block.sync_version, Some(1));
    }

    #[test]
    fn parse_no_block() {
        let body = "Just a plain issue description.\nNo storyhook block here.";
        assert!(extract_block(body).is_none());
    }

    #[test]
    fn parse_empty_block() {
        let body = "Description.\n\n---\n\n```storyhook\nstory_id: SH-1\n```\n";
        let (text, block) = extract_block(body).expect("should parse minimal block");
        assert_eq!(text, "Description.");
        assert_eq!(block.story_id, "SH-1");
        assert!(block.priority.is_none());
        assert!(block.awaiting.is_none());
        assert!(block.relationships.is_empty());
        assert!(block.sync_version.is_none());
    }

    #[test]
    fn render_onto_clean_body() {
        let body = "My issue description.";
        let block = sample_block();
        let rendered = render_block(body, &block);

        assert!(rendered.starts_with("My issue description."));
        assert!(rendered.contains("---"));
        assert!(rendered.contains("```storyhook"));
        assert!(rendered.contains("story_id: SH-42"));
        assert!(rendered.contains("priority: high"));
    }

    #[test]
    fn render_replaces_existing_block() {
        let body = sample_body();
        let new_block = StoryhookBlock {
            story_id: "SH-99".into(),
            priority: Some("low".into()),
            awaiting: None,
            relationships: vec![],
            sync_version: Some(2),
        };
        let rendered = render_block(&body, &new_block);

        // Old block data should be gone.
        assert!(!rendered.contains("SH-42"));
        // New block data should be present.
        assert!(rendered.contains("SH-99"));
        assert!(rendered.contains("priority: low"));
        // Human text preserved.
        assert!(rendered.contains("Human-written issue description here."));
    }

    #[test]
    fn roundtrip() {
        let original_text = "Some description text.";
        let block = sample_block();
        let rendered = render_block(original_text, &block);
        let (extracted_text, extracted_block) =
            extract_block(&rendered).expect("should roundtrip");
        assert_eq!(extracted_text, original_text);
        assert_eq!(extracted_block.story_id, block.story_id);
        assert_eq!(extracted_block.priority, block.priority);
        assert_eq!(extracted_block.awaiting, block.awaiting);
        assert_eq!(extracted_block.relationships.len(), block.relationships.len());
        assert_eq!(extracted_block.sync_version, block.sync_version);
    }

    #[test]
    fn block_with_multiple_relationships() {
        let body = [
            "Description.",
            "",
            "---",
            "",
            "```storyhook",
            "story_id: SH-5",
            "relationships:",
            "  - blocks: SH-1",
            "  - relates-to: SH-2",
            "  - obviates: SH-3",
            "  - depends-on: SH-4",
            "```",
        ]
        .join("\n");
        let (_, block) = extract_block(&body).expect("should parse");
        assert_eq!(block.relationships.len(), 4);
        assert_eq!(
            block.relationships[0].entries.get("blocks"),
            Some(&"SH-1".to_string())
        );
        assert_eq!(
            block.relationships[3].entries.get("depends-on"),
            Some(&"SH-4".to_string())
        );
    }

    #[test]
    fn malformed_yaml_returns_none() {
        let body = [
            "Description.",
            "",
            "```storyhook",
            "this is: [not: valid: yaml: {{{",
            "```",
        ]
        .join("\n");
        assert!(extract_block(&body).is_none());
    }

    #[test]
    fn block_at_start_of_body() {
        let body = "```storyhook\nstory_id: SH-1\n```\n";
        let (text, block) = extract_block(body).expect("should parse block at start");
        assert_eq!(text, "");
        assert_eq!(block.story_id, "SH-1");
    }

    #[test]
    fn block_at_end_without_trailing_newline() {
        let body = "Description.\n\n---\n\n```storyhook\nstory_id: SH-1\n```";
        let (text, block) = extract_block(body).expect("should parse block at end");
        assert_eq!(text, "Description.");
        assert_eq!(block.story_id, "SH-1");
    }

    #[test]
    fn extra_whitespace_around_block() {
        let body =
            "Description.\n\n  ---  \n\n  ```storyhook\nstory_id: SH-1\n```  \n\n";
        let result = extract_block(body);
        // The opening fence must start at the beginning of a line conceptually,
        // but rfind will find it even with leading whitespace. The key requirement
        // is that it doesn't panic and handles gracefully.
        if let Some((text, block)) = result {
            assert_eq!(block.story_id, "SH-1");
            assert!(!text.contains("```storyhook"));
        }
    }
}
