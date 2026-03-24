use crate::domain::{ImportRelationship, ImportStory};

pub fn decompose_spec(content: &str) -> Vec<ImportStory> {
    let mut stories: Vec<ImportStory> = Vec::new();
    // Track heading stack: (heading_level, story_index)
    let mut heading_stack: Vec<(usize, usize)> = Vec::new();

    for line in content.lines() {
        let trimmed = line.trim();

        if let Some(rest) = trimmed.strip_prefix("### ") {
            let title = rest.trim().to_string();
            if title.is_empty() {
                continue;
            }
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
                priority: None,
                labels: None,
                assignee: None,
                relationships,
            });
            // Update heading stack: pop anything >= level 3, push this
            heading_stack.retain(|(level, _)| *level < 3);
            heading_stack.push((3, index));
        } else if let Some(rest) = trimmed.strip_prefix("## ") {
            let title = rest.trim().to_string();
            if title.is_empty() {
                continue;
            }
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
                priority: None,
                labels: None,
                assignee: None,
                relationships,
            });
            // Update heading stack: pop anything >= level 2, push this
            heading_stack.retain(|(level, _)| *level < 2);
            heading_stack.push((2, index));
        } else if let Some(rest) = trimmed.strip_prefix("# ") {
            let title = rest.trim().to_string();
            if title.is_empty() {
                continue;
            }
            let index = stories.len();
            stories.push(ImportStory {
                title,
                priority: None,
                labels: None,
                assignee: None,
                relationships: None,
            });
            // Reset heading stack for new top-level heading
            heading_stack.clear();
            heading_stack.push((1, index));
        } else if let Some(rest) = trimmed.strip_prefix("- [ ] ") {
            let title = rest.trim().to_string();
            if title.is_empty() {
                continue;
            }
            // Parent is the most recent heading at any level
            let parent_index = heading_stack.last().map(|(_, idx)| *idx);
            let relationships = parent_index.map(|idx| {
                vec![ImportRelationship {
                    relation: "child-of".to_string(),
                    ref_index: Some(idx),
                    other_id: None,
                }]
            });
            stories.push(ImportStory {
                title,
                priority: None,
                labels: None,
                assignee: None,
                relationships,
            });
        } else if trimmed.starts_with("- [x] ") || trimmed.starts_with("- [X] ") {
            // Checked items are skipped
            continue;
        }
        // All other lines are ignored
    }

    stories
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
