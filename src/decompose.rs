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
            state: None,
            story_type: None,
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

/// Parse a wave heading like "Wave 1", "Wave 2 (parallel)", "Wave 3 (depends on Wave 2)".
/// Returns the wave number if the heading matches, None otherwise.
fn parse_wave_heading(title: &str) -> Option<usize> {
    let lower = title.to_ascii_lowercase();
    let rest = lower.strip_prefix("wave ")?;
    // The wave number is the first token after "wave "
    let num_str = rest.split(|c: char| !c.is_ascii_digit()).next()?;
    if num_str.is_empty() {
        return None;
    }
    num_str.parse::<usize>().ok()
}

pub fn decompose_spec(content: &str) -> Vec<ImportStory> {
    let mut stories: Vec<ImportStory> = Vec::new();
    // Track heading stack: (heading_level, story_index)
    let mut heading_stack: Vec<(usize, usize)> = Vec::new();
    // Track checkbox nesting: (indentation_level, story_index)
    let mut checkbox_stack: Vec<(usize, usize)> = Vec::new();
    // Track description lines per story index
    let mut descriptions: HashMap<usize, Vec<String>> = HashMap::new();
    // Index of the most recently created story (for description capture)
    let mut last_story_index: Option<usize> = None;
    // Wave-based format tracking
    let mut current_wave: Option<usize> = None;
    let mut wave_task_indices: HashMap<usize, Vec<usize>> = HashMap::new();

    for line in content.lines() {
        let trimmed = line.trim();

        if let Some(rest) = trimmed.strip_prefix("### ") {
            let title = rest.trim().to_string();
            if title.is_empty() {
                continue;
            }

            // Check if this is a wave heading (e.g. "Wave 1", "Wave 2 (depends on Wave 1)")
            if let Some(wave_num) = parse_wave_heading(&title) {
                current_wave = Some(wave_num);
                wave_task_indices.entry(wave_num).or_default();
                continue;
            }

            // Non-wave ### heading: reset wave tracking
            current_wave = None;

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
                state: None,
                story_type: None,
            });
            last_story_index = Some(index);
            // Update heading stack: pop anything >= level 3, push this
            heading_stack.retain(|(level, _)| *level < 3);
            heading_stack.push((3, index));
            checkbox_stack.clear();
        } else if let Some(rest) = trimmed.strip_prefix("## ") {
            // New ## heading resets wave tracking
            current_wave = None;
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
                state: None,
                story_type: None,
            });
            last_story_index = Some(index);
            // Update heading stack: pop anything >= level 2, push this
            heading_stack.retain(|(level, _)| *level < 2);
            heading_stack.push((2, index));
            checkbox_stack.clear();
        } else if let Some(rest) = trimmed.strip_prefix("# ") {
            // New # heading resets wave tracking
            current_wave = None;
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
                state: None,
                story_type: None,
            });
            last_story_index = Some(index);
            // Reset heading stack for new top-level heading
            heading_stack.clear();
            heading_stack.push((1, index));
            checkbox_stack.clear();
        } else if let Some(rest) = trimmed.strip_prefix("- [ ] ") {
            let title = rest.trim().to_string();
            if title.is_empty() {
                continue;
            }
            let (title, priority) = extract_priority(&title);
            let (title, labels) = extract_labels(&title);

            // Determine indentation level (number of leading spaces/tabs)
            let indent = line.len() - line.trim_start().len();

            // Find parent: check checkbox stack first for nested checklists,
            // then fall back to heading stack
            let parent_index = {
                // Pop checkbox entries at same or deeper indentation
                while let Some(&(prev_indent, _)) = checkbox_stack.last() {
                    if prev_indent >= indent {
                        checkbox_stack.pop();
                    } else {
                        break;
                    }
                }
                if let Some(&(_, parent_idx)) = checkbox_stack.last() {
                    // Nested under a previous checkbox
                    Some(parent_idx)
                } else {
                    // No checkbox parent — use the most recent heading
                    heading_stack.last().map(|(_, idx)| *idx)
                }
            };

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
                state: None,
                story_type: None,
            });
            last_story_index = Some(index);
            // Push onto checkbox stack so deeper items can become children
            checkbox_stack.push((indent, index));
            // Track task index for wave sequencing and add phase label
            if let Some(wave_num) = current_wave {
                wave_task_indices.entry(wave_num).or_default().push(index);
                let phase_label = format!("phase:{wave_num}");
                let labels = stories[index].labels.get_or_insert_with(Vec::new);
                if !labels.contains(&phase_label) {
                    labels.push(phase_label);
                }
            }
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

    // Add wave sequencing relationships
    let mut sorted_waves: Vec<usize> = wave_task_indices.keys().copied().collect();
    sorted_waves.sort();
    for window in sorted_waves.windows(2) {
        let prev_wave = window[0];
        let curr_wave = window[1];
        if let (Some(prev_tasks), Some(curr_tasks)) = (
            wave_task_indices.get(&prev_wave),
            wave_task_indices.get(&curr_wave),
        ) {
            for &curr_idx in curr_tasks {
                for &prev_idx in prev_tasks {
                    let rels = stories[curr_idx]
                        .relationships
                        .get_or_insert_with(Vec::new);
                    rels.push(ImportRelationship {
                        relation: "blocked-by".to_string(),
                        ref_index: Some(prev_idx),
                        other_id: None,
                    });
                }
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

    // -----------------------------------------------------------------------
    // Wave format tests
    // -----------------------------------------------------------------------

    #[test]
    fn wave_format_basic() {
        let input = "\
## Feature
### Wave 1 (parallel)
- [ ] Task A
- [ ] Task B
### Wave 2 (depends on Wave 1)
- [ ] Task C
- [ ] Task D";
        let stories = decompose_spec(input);
        // stories: [0]=Feature, [1]=Task A, [2]=Task B, [3]=Task C, [4]=Task D
        assert_eq!(stories.len(), 5);
        assert_eq!(stories[0].title, "Feature");
        assert_eq!(stories[1].title, "Task A");
        assert_eq!(stories[2].title, "Task B");
        assert_eq!(stories[3].title, "Task C");
        assert_eq!(stories[4].title, "Task D");

        // Wave 1 tasks should only have child-of relationship to Feature
        let rels_a = stories[1].relationships.as_ref().unwrap();
        assert_eq!(rels_a.len(), 1);
        assert_eq!(rels_a[0].relation, "child-of");

        let rels_b = stories[2].relationships.as_ref().unwrap();
        assert_eq!(rels_b.len(), 1);
        assert_eq!(rels_b[0].relation, "child-of");

        // Wave 2 tasks should have child-of + 2 follows relationships
        let rels_c = stories[3].relationships.as_ref().unwrap();
        assert_eq!(rels_c.len(), 3); // child-of + follows Task A + follows Task B
        assert_eq!(rels_c[0].relation, "child-of");
        assert_eq!(rels_c[0].ref_index, Some(0));
        let follows_c: Vec<_> = rels_c.iter().filter(|r| r.relation == "blocked-by").collect();
        assert_eq!(follows_c.len(), 2);
        let follow_indices: Vec<_> = follows_c.iter().map(|r| r.ref_index.unwrap()).collect();
        assert!(follow_indices.contains(&1)); // Task A
        assert!(follow_indices.contains(&2)); // Task B

        let rels_d = stories[4].relationships.as_ref().unwrap();
        assert_eq!(rels_d.len(), 3);
        let follows_d: Vec<_> = rels_d.iter().filter(|r| r.relation == "blocked-by").collect();
        assert_eq!(follows_d.len(), 2);
    }

    #[test]
    fn wave_format_parallel_within_wave() {
        let input = "\
### Wave 1 (parallel)
- [ ] Task A
- [ ] Task B
- [ ] Task C";
        let stories = decompose_spec(input);
        assert_eq!(stories.len(), 3);
        // No task should have a "blocked-by" relationship — they're all in the same wave
        for story in &stories {
            if let Some(rels) = &story.relationships {
                for rel in rels {
                    assert_ne!(rel.relation, "blocked-by");
                }
            }
        }
    }

    #[test]
    fn wave_format_three_waves() {
        let input = "\
### Wave 1
- [ ] Task A
### Wave 2
- [ ] Task B
### Wave 3
- [ ] Task C";
        let stories = decompose_spec(input);
        // stories: [0]=Task A, [1]=Task B, [2]=Task C
        assert_eq!(stories.len(), 3);

        // Task A (wave 1): no follows
        assert!(stories[0].relationships.is_none());

        // Task B (wave 2): follows Task A only
        let rels_b = stories[1].relationships.as_ref().unwrap();
        assert_eq!(rels_b.len(), 1);
        assert_eq!(rels_b[0].relation, "blocked-by");
        assert_eq!(rels_b[0].ref_index, Some(0));

        // Task C (wave 3): follows Task B only (not Task A directly)
        let rels_c = stories[2].relationships.as_ref().unwrap();
        assert_eq!(rels_c.len(), 1);
        assert_eq!(rels_c[0].relation, "blocked-by");
        assert_eq!(rels_c[0].ref_index, Some(1));
    }

    #[test]
    fn wave_format_with_parent() {
        let input = "\
## Feature Alpha
### Wave 1
- [ ] Setup database
### Wave 2
- [ ] Build API";
        let stories = decompose_spec(input);
        // stories: [0]=Feature Alpha, [1]=Setup database, [2]=Build API
        assert_eq!(stories.len(), 3);

        // Setup database: child-of Feature Alpha, no follows
        let rels_setup = stories[1].relationships.as_ref().unwrap();
        assert_eq!(rels_setup.len(), 1);
        assert_eq!(rels_setup[0].relation, "child-of");
        assert_eq!(rels_setup[0].ref_index, Some(0));

        // Build API: child-of Feature Alpha + follows Setup database
        let rels_api = stories[2].relationships.as_ref().unwrap();
        assert_eq!(rels_api.len(), 2);
        let child_of: Vec<_> = rels_api.iter().filter(|r| r.relation == "child-of").collect();
        assert_eq!(child_of.len(), 1);
        assert_eq!(child_of[0].ref_index, Some(0));
        let follows: Vec<_> = rels_api.iter().filter(|r| r.relation == "blocked-by").collect();
        assert_eq!(follows.len(), 1);
        assert_eq!(follows[0].ref_index, Some(1));
    }

    #[test]
    fn non_wave_headings_unchanged() {
        // Regression: ### headings that don't match "Wave N" still produce stories
        let input = "\
## Parent
### Regular Heading
- [ ] A task under regular heading";
        let stories = decompose_spec(input);
        // stories: [0]=Parent, [1]=Regular Heading, [2]=A task under regular heading
        assert_eq!(stories.len(), 3);
        assert_eq!(stories[0].title, "Parent");
        assert_eq!(stories[1].title, "Regular Heading");
        assert_eq!(stories[2].title, "A task under regular heading");

        // Regular Heading is child of Parent
        let rels_heading = stories[1].relationships.as_ref().unwrap();
        assert_eq!(rels_heading.len(), 1);
        assert_eq!(rels_heading[0].relation, "child-of");
        assert_eq!(rels_heading[0].ref_index, Some(0));

        // Task is child of Regular Heading (the most recent heading)
        let rels_task = stories[2].relationships.as_ref().unwrap();
        assert_eq!(rels_task.len(), 1);
        assert_eq!(rels_task[0].relation, "child-of");
        assert_eq!(rels_task[0].ref_index, Some(1));

        // No "blocked-by" relationships anywhere
        for story in &stories {
            if let Some(rels) = &story.relationships {
                for rel in rels {
                    assert_ne!(rel.relation, "blocked-by");
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // Nested checklist tests
    // -----------------------------------------------------------------------

    #[test]
    fn nested_checklist_parent_child() {
        let input = "\
## Phase 1
- [ ] Parent task
  - [ ] Subtask A
  - [ ] Subtask B";
        let stories = decompose_spec(input);
        // stories: [0]=Phase 1, [1]=Parent task, [2]=Subtask A, [3]=Subtask B
        assert_eq!(stories.len(), 4);
        assert_eq!(stories[0].title, "Phase 1");
        assert_eq!(stories[1].title, "Parent task");
        assert_eq!(stories[2].title, "Subtask A");
        assert_eq!(stories[3].title, "Subtask B");

        // Parent task is child of Phase 1
        let rels_parent = stories[1].relationships.as_ref().unwrap();
        assert_eq!(rels_parent.len(), 1);
        assert_eq!(rels_parent[0].relation, "child-of");
        assert_eq!(rels_parent[0].ref_index, Some(0));

        // Subtask A is child of Parent task (not Phase 1)
        let rels_a = stories[2].relationships.as_ref().unwrap();
        assert_eq!(rels_a.len(), 1);
        assert_eq!(rels_a[0].relation, "child-of");
        assert_eq!(rels_a[0].ref_index, Some(1));

        // Subtask B is child of Parent task (not Phase 1)
        let rels_b = stories[3].relationships.as_ref().unwrap();
        assert_eq!(rels_b.len(), 1);
        assert_eq!(rels_b[0].relation, "child-of");
        assert_eq!(rels_b[0].ref_index, Some(1));
    }

    #[test]
    fn nested_checklist_three_levels() {
        let input = "\
# Epic
- [ ] Top level task
  - [ ] Mid level task
    - [ ] Deep task";
        let stories = decompose_spec(input);
        // stories: [0]=Epic, [1]=Top level task, [2]=Mid level task, [3]=Deep task
        assert_eq!(stories.len(), 4);

        // Top level task is child of Epic
        let rels_top = stories[1].relationships.as_ref().unwrap();
        assert_eq!(rels_top[0].relation, "child-of");
        assert_eq!(rels_top[0].ref_index, Some(0));

        // Mid level task is child of Top level task
        let rels_mid = stories[2].relationships.as_ref().unwrap();
        assert_eq!(rels_mid[0].relation, "child-of");
        assert_eq!(rels_mid[0].ref_index, Some(1));

        // Deep task is child of Mid level task
        let rels_deep = stories[3].relationships.as_ref().unwrap();
        assert_eq!(rels_deep[0].relation, "child-of");
        assert_eq!(rels_deep[0].ref_index, Some(2));
    }

    #[test]
    fn nested_checklist_sibling_after_nested() {
        // After nested items, a sibling at the same level as the parent should
        // be a child of the heading, not the previous nested item
        let input = "\
## Feature
- [ ] Task A
  - [ ] Subtask A1
- [ ] Task B";
        let stories = decompose_spec(input);
        // stories: [0]=Feature, [1]=Task A, [2]=Subtask A1, [3]=Task B
        assert_eq!(stories.len(), 4);

        // Task A is child of Feature
        let rels_a = stories[1].relationships.as_ref().unwrap();
        assert_eq!(rels_a[0].relation, "child-of");
        assert_eq!(rels_a[0].ref_index, Some(0));

        // Subtask A1 is child of Task A
        let rels_a1 = stories[2].relationships.as_ref().unwrap();
        assert_eq!(rels_a1[0].relation, "child-of");
        assert_eq!(rels_a1[0].ref_index, Some(1));

        // Task B is child of Feature (same level as Task A, not child of Subtask A1)
        let rels_b = stories[3].relationships.as_ref().unwrap();
        assert_eq!(rels_b[0].relation, "child-of");
        assert_eq!(rels_b[0].ref_index, Some(0));
    }
}
