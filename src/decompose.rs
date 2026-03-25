use std::collections::HashMap;

use serde::Deserialize;

use crate::domain::{ImportRelationship, ImportStory};
use crate::error::AppError;

// ---------------------------------------------------------------------------
// YAML serde structs
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct YamlSpec {
    stories: Vec<YamlStory>,
}

#[derive(Deserialize)]
struct YamlStory {
    title: String,
    #[serde(default)]
    priority: Option<String>,
    #[serde(default)]
    labels: Option<Vec<String>>,
    #[serde(default)]
    assignee: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    children: Option<Vec<YamlStory>>,
}

// ---------------------------------------------------------------------------
// Format detection
// ---------------------------------------------------------------------------

pub enum SpecFormat {
    Markdown,
    Yaml,
}

pub fn detect_format(filename: Option<&str>, content: &str) -> SpecFormat {
    if let Some(name) = filename {
        let lower = name.to_ascii_lowercase();
        if lower.ends_with(".yaml") || lower.ends_with(".yml") {
            return SpecFormat::Yaml;
        }
        if lower.ends_with(".md") || lower.ends_with(".markdown") {
            return SpecFormat::Markdown;
        }
    }
    // Content sniff
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if trimmed.starts_with("stories:") {
            return SpecFormat::Yaml;
        }
        break;
    }
    SpecFormat::Markdown
}

pub fn decompose(filename: Option<&str>, content: &str) -> Result<Vec<ImportStory>, AppError> {
    match detect_format(filename, content) {
        SpecFormat::Yaml => decompose_yaml(content),
        SpecFormat::Markdown => Ok(decompose_spec(content)),
    }
}

// ---------------------------------------------------------------------------
// YAML parsing
// ---------------------------------------------------------------------------

pub fn decompose_yaml(content: &str) -> Result<Vec<ImportStory>, AppError> {
    let spec: YamlSpec = serde_yml::from_str(content)?;
    let mut output: Vec<ImportStory> = Vec::new();
    flatten_yaml(&spec.stories, None, &mut output);
    Ok(output)
}

fn flatten_yaml(
    yaml_stories: &[YamlStory],
    parent_index: Option<usize>,
    output: &mut Vec<ImportStory>,
) {
    for story in yaml_stories {
        let my_index = output.len();
        let relationships = parent_index.map(|idx| {
            vec![ImportRelationship {
                relation: "child-of".to_string(),
                ref_index: Some(idx),
                other_id: None,
            }]
        });
        output.push(ImportStory {
            title: story.title.clone(),
            priority: story.priority.clone(),
            labels: story.labels.clone(),
            assignee: story.assignee.clone(),
            description: story.description.clone(),
            relationships,
        });
        if let Some(ref children) = story.children {
            flatten_yaml(children, Some(my_index), output);
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers: priority and label extraction
// ---------------------------------------------------------------------------

fn extract_priority(text: &str) -> (String, Option<String>) {
    for (marker, value) in [
        ("[CRITICAL]", "critical"),
        ("[HIGH]", "high"),
        ("[MEDIUM]", "medium"),
        ("[LOW]", "low"),
        ("[NONE]", "none"),
    ] {
        if let Some(pos) = text.to_ascii_uppercase().find(marker) {
            let cleaned = format!("{}{}", &text[..pos], &text[pos + marker.len()..])
                .trim()
                .to_string();
            return (cleaned, Some(value.to_string()));
        }
    }
    (text.to_string(), None)
}

fn extract_labels(text: &str) -> (String, Vec<String>) {
    let mut labels = Vec::new();
    let mut cleaned = String::new();
    for token in text.split_whitespace() {
        if token.starts_with('#') && token.len() > 1 {
            let tag = &token[1..];
            if tag
                .starts_with(|c: char| c.is_ascii_alphabetic())
                && tag
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-')
            {
                labels.push(tag.to_string());
                continue;
            }
        }
        if !cleaned.is_empty() {
            cleaned.push(' ');
        }
        cleaned.push_str(token);
    }
    (cleaned, labels)
}

// ---------------------------------------------------------------------------
// Markdown parsing (enhanced)
// ---------------------------------------------------------------------------

pub fn decompose_spec(content: &str) -> Vec<ImportStory> {
    let mut stories: Vec<ImportStory> = Vec::new();
    // Track heading stack: (heading_level, story_index)
    let mut heading_stack: Vec<(usize, usize)> = Vec::new();
    // Track description lines per story index
    let mut descriptions: HashMap<usize, Vec<String>> = HashMap::new();
    // Index of the most recently created story (for description capture)
    let mut last_story_index: Option<usize> = None;

    for line in content.lines() {
        let trimmed = line.trim();

        if let Some(rest) = trimmed.strip_prefix("### ") {
            let title = rest.trim().to_string();
            if title.is_empty() {
                continue;
            }
            let (title, priority) = extract_priority(&title);
            let (title, labels) = extract_labels(&title);
            // Find parent: nearest ## heading
            let parent_index = heading_stack
                .iter()
                .rev()
                .find(|(level, _)| *level == 2)
                .map(|(_, idx)| *idx);
            let relationships = parent_index.map(|idx| {
                vec![ImportRelationship {
                    relation: "child-of".to_string(),
                    ref_index: Some(idx),
                    other_id: None,
                }]
            });
            let index = stories.len();
            stories.push(ImportStory {
                title,
                priority,
                labels: if labels.is_empty() {
                    None
                } else {
                    Some(labels)
                },
                assignee: None,
                description: None,
                relationships,
            });
            last_story_index = Some(index);
            // Update heading stack: pop anything >= level 3, push this
            heading_stack.retain(|(level, _)| *level < 3);
            heading_stack.push((3, index));
        } else if let Some(rest) = trimmed.strip_prefix("## ") {
            let title = rest.trim().to_string();
            if title.is_empty() {
                continue;
            }
            let (title, priority) = extract_priority(&title);
            let (title, labels) = extract_labels(&title);
            // Find parent: nearest # heading
            let parent_index = heading_stack
                .iter()
                .rev()
                .find(|(level, _)| *level == 1)
                .map(|(_, idx)| *idx);
            let relationships = parent_index.map(|idx| {
                vec![ImportRelationship {
                    relation: "child-of".to_string(),
                    ref_index: Some(idx),
                    other_id: None,
                }]
            });
            let index = stories.len();
            stories.push(ImportStory {
                title,
                priority,
                labels: if labels.is_empty() {
                    None
                } else {
                    Some(labels)
                },
                assignee: None,
                description: None,
                relationships,
            });
            last_story_index = Some(index);
            // Update heading stack: pop anything >= level 2, push this
            heading_stack.retain(|(level, _)| *level < 2);
            heading_stack.push((2, index));
        } else if let Some(rest) = trimmed.strip_prefix("# ") {
            let title = rest.trim().to_string();
            if title.is_empty() {
                continue;
            }
            let (title, priority) = extract_priority(&title);
            let (title, labels) = extract_labels(&title);
            let index = stories.len();
            stories.push(ImportStory {
                title,
                priority,
                labels: if labels.is_empty() {
                    None
                } else {
                    Some(labels)
                },
                assignee: None,
                description: None,
                relationships: None,
            });
            last_story_index = Some(index);
            // Reset heading stack for new top-level heading
            heading_stack.clear();
            heading_stack.push((1, index));
        } else if let Some(rest) = trimmed.strip_prefix("- [ ] ") {
            let title = rest.trim().to_string();
            if title.is_empty() {
                continue;
            }
            let (title, priority) = extract_priority(&title);
            let (title, labels) = extract_labels(&title);
            // Parent is the most recent heading at any level
            let parent_index = heading_stack.last().map(|(_, idx)| *idx);
            let relationships = parent_index.map(|idx| {
                vec![ImportRelationship {
                    relation: "child-of".to_string(),
                    ref_index: Some(idx),
                    other_id: None,
                }]
            });
            let index = stories.len();
            stories.push(ImportStory {
                title,
                priority,
                labels: if labels.is_empty() {
                    None
                } else {
                    Some(labels)
                },
                assignee: None,
                description: None,
                relationships,
            });
            last_story_index = Some(index);
        } else if trimmed.starts_with("- [x] ") || trimmed.starts_with("- [X] ") {
            // Checked items are skipped
            continue;
        } else if !trimmed.is_empty() {
            // Body text: accumulate as description for most recent story
            if let Some(idx) = last_story_index {
                descriptions.entry(idx).or_default().push(trimmed.to_string());
            }
        }
    }

    // Apply collected descriptions to stories
    for (idx, lines) in descriptions {
        if idx < stories.len() {
            let desc = lines.join("\n");
            if !desc.trim().is_empty() {
                stories[idx].description = Some(desc);
            }
        }
    }

    stories
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // Existing tests (regression)
    // -----------------------------------------------------------------------

    #[test]
    fn single_heading() {
        let stories = decompose_spec("# My Story");
        assert_eq!(stories.len(), 1);
        assert_eq!(stories[0].title, "My Story");
        assert!(stories[0].relationships.is_none());
    }

    #[test]
    fn nested_headings() {
        let input = "# Parent\n## Child";
        let stories = decompose_spec(input);
        assert_eq!(stories.len(), 2);
        assert_eq!(stories[0].title, "Parent");
        assert_eq!(stories[1].title, "Child");
        let rels = stories[1].relationships.as_ref().unwrap();
        assert_eq!(rels.len(), 1);
        assert_eq!(rels[0].relation, "child-of");
        assert_eq!(rels[0].ref_index, Some(0));
    }

    #[test]
    fn three_levels() {
        let input = "# Epic\n## Feature\n### Task";
        let stories = decompose_spec(input);
        assert_eq!(stories.len(), 3);
        // Feature is child of Epic
        let feature_rels = stories[1].relationships.as_ref().unwrap();
        assert_eq!(feature_rels[0].ref_index, Some(0));
        // Task is child of Feature
        let task_rels = stories[2].relationships.as_ref().unwrap();
        assert_eq!(task_rels[0].ref_index, Some(1));
    }

    #[test]
    fn checkbox_items() {
        let input = "# Epic\n- [ ] Task one\n- [ ] Task two";
        let stories = decompose_spec(input);
        assert_eq!(stories.len(), 3);
        assert_eq!(stories[1].title, "Task one");
        assert_eq!(stories[2].title, "Task two");
        // Both are children of the heading
        let rels1 = stories[1].relationships.as_ref().unwrap();
        assert_eq!(rels1[0].ref_index, Some(0));
    }

    #[test]
    fn checked_items_skipped() {
        let input = "# Epic\n- [x] Done task\n- [ ] Open task";
        let stories = decompose_spec(input);
        assert_eq!(stories.len(), 2);
        assert_eq!(stories[0].title, "Epic");
        assert_eq!(stories[1].title, "Open task");
    }

    #[test]
    fn empty_content() {
        let stories = decompose_spec("");
        assert!(stories.is_empty());
    }

    #[test]
    fn no_headings() {
        let stories = decompose_spec("Just some plain text\nAnother line");
        assert!(stories.is_empty());
    }

    // -----------------------------------------------------------------------
    // New YAML tests
    // -----------------------------------------------------------------------

    #[test]
    fn yaml_single_story() {
        let yaml = "stories:\n  - title: Build API\n";
        let stories = decompose_yaml(yaml).unwrap();
        assert_eq!(stories.len(), 1);
        assert_eq!(stories[0].title, "Build API");
        assert!(stories[0].relationships.is_none());
    }

    #[test]
    fn yaml_nested_children() {
        let yaml = r#"
stories:
  - title: Epic
    children:
      - title: Feature A
      - title: Feature B
"#;
        let stories = decompose_yaml(yaml).unwrap();
        assert_eq!(stories.len(), 3);
        assert_eq!(stories[0].title, "Epic");
        assert!(stories[0].relationships.is_none());
        assert_eq!(stories[1].title, "Feature A");
        let rels = stories[1].relationships.as_ref().unwrap();
        assert_eq!(rels[0].relation, "child-of");
        assert_eq!(rels[0].ref_index, Some(0));
        assert_eq!(stories[2].title, "Feature B");
        let rels2 = stories[2].relationships.as_ref().unwrap();
        assert_eq!(rels2[0].ref_index, Some(0));
    }

    #[test]
    fn yaml_deeply_nested() {
        let yaml = r#"
stories:
  - title: Level 0
    children:
      - title: Level 1
        children:
          - title: Level 2
            children:
              - title: Level 3
"#;
        let stories = decompose_yaml(yaml).unwrap();
        assert_eq!(stories.len(), 4);
        assert!(stories[0].relationships.is_none());
        assert_eq!(
            stories[1].relationships.as_ref().unwrap()[0].ref_index,
            Some(0)
        );
        assert_eq!(
            stories[2].relationships.as_ref().unwrap()[0].ref_index,
            Some(1)
        );
        assert_eq!(
            stories[3].relationships.as_ref().unwrap()[0].ref_index,
            Some(2)
        );
    }

    #[test]
    fn yaml_empty() {
        let yaml = "stories: []\n";
        let stories = decompose_yaml(yaml).unwrap();
        assert!(stories.is_empty());
    }

    // -----------------------------------------------------------------------
    // New markdown enhancement tests
    // -----------------------------------------------------------------------

    #[test]
    fn markdown_priority_extraction() {
        let input = "## [HIGH] Add auth";
        let stories = decompose_spec(input);
        assert_eq!(stories.len(), 1);
        assert_eq!(stories[0].title, "Add auth");
        assert_eq!(stories[0].priority.as_deref(), Some("high"));
    }

    #[test]
    fn markdown_label_extraction() {
        let input = "# Build API #backend #security";
        let stories = decompose_spec(input);
        assert_eq!(stories.len(), 1);
        assert_eq!(stories[0].title, "Build API");
        let labels = stories[0].labels.as_ref().unwrap();
        assert_eq!(labels, &["backend", "security"]);
    }

    #[test]
    fn markdown_description_capture() {
        let input = "# My Epic\nThis is the description.\nMore details here.";
        let stories = decompose_spec(input);
        assert_eq!(stories.len(), 1);
        let desc = stories[0].description.as_ref().unwrap();
        assert!(desc.contains("This is the description."));
        assert!(desc.contains("More details here."));
    }

    #[test]
    fn markdown_combined() {
        let input = "## [CRITICAL] Deploy service #infra #urgent\nNeeds to be done ASAP.";
        let stories = decompose_spec(input);
        assert_eq!(stories.len(), 1);
        assert_eq!(stories[0].title, "Deploy service");
        assert_eq!(stories[0].priority.as_deref(), Some("critical"));
        let labels = stories[0].labels.as_ref().unwrap();
        assert_eq!(labels, &["infra", "urgent"]);
        let desc = stories[0].description.as_ref().unwrap();
        assert!(desc.contains("Needs to be done ASAP."));
    }

    #[test]
    fn markdown_no_metadata_unchanged() {
        // Regression: plain headings and checkboxes still work as before
        let input = "# Parent\n## Child\n- [ ] Task";
        let stories = decompose_spec(input);
        assert_eq!(stories.len(), 3);
        assert_eq!(stories[0].title, "Parent");
        assert!(stories[0].priority.is_none());
        assert!(stories[0].labels.is_none());
        assert_eq!(stories[1].title, "Child");
        assert_eq!(stories[2].title, "Task");
    }

    // -----------------------------------------------------------------------
    // Format detection tests
    // -----------------------------------------------------------------------

    #[test]
    fn detect_format_yaml_extension() {
        assert!(matches!(
            detect_format(Some("spec.yaml"), ""),
            SpecFormat::Yaml
        ));
        assert!(matches!(
            detect_format(Some("spec.yml"), ""),
            SpecFormat::Yaml
        ));
    }

    #[test]
    fn detect_format_md_extension() {
        assert!(matches!(
            detect_format(Some("spec.md"), ""),
            SpecFormat::Markdown
        ));
        assert!(matches!(
            detect_format(Some("spec.markdown"), ""),
            SpecFormat::Markdown
        ));
    }

    #[test]
    fn detect_format_content_sniff() {
        assert!(matches!(
            detect_format(None, "stories:\n  - title: X"),
            SpecFormat::Yaml
        ));
        // First non-blank, non-# line is not "stories:" -> Markdown
        assert!(matches!(
            detect_format(None, "## Some heading\nSome body"),
            SpecFormat::Markdown
        ));
        // Unknown content defaults to Markdown
        assert!(matches!(
            detect_format(None, "Just some text"),
            SpecFormat::Markdown
        ));
    }
}
