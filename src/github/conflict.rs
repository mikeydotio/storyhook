use dialoguer::Select;

use super::diff::FieldConflict;

/// How a conflict was resolved.
#[derive(Debug, Clone)]
pub enum Resolution {
    KeepLocal,
    KeepRemote,
    Skip,
}

/// A conflict together with its chosen resolution.
#[derive(Debug, Clone)]
pub struct ResolvedConflict {
    pub field: String,
    pub resolution: Resolution,
}

/// Present conflicts to the user interactively and collect resolutions.
///
/// For each conflict, displays the base, local, and remote values and prompts
/// the user to choose which side to keep (or skip for later resolution).
pub fn resolve_conflicts_interactive(
    story_id: &str,
    conflicts: &[FieldConflict],
) -> Vec<ResolvedConflict> {
    let mut resolutions = Vec::with_capacity(conflicts.len());

    for conflict in conflicts {
        println!();
        println!("Conflict on {}, field: {}", story_id, conflict.field);
        println!("  Base:   \"{}\"", conflict.base_value);
        println!("  Local:  \"{}\"", conflict.local_value);
        println!("  Remote: \"{}\"", conflict.remote_value);

        let options = &["Keep local", "Keep remote", "Skip (resolve later)"];
        let selection = Select::new()
            .with_prompt("How do you want to resolve this?")
            .items(options)
            .default(0)
            .interact()
            .unwrap_or(2); // default to Skip on error

        let resolution = match selection {
            0 => Resolution::KeepLocal,
            1 => Resolution::KeepRemote,
            _ => Resolution::Skip,
        };

        resolutions.push(ResolvedConflict {
            field: conflict.field.clone(),
            resolution,
        });
    }

    resolutions
}

/// Resolve conflicts non-interactively, keeping all local or all remote.
///
/// Useful for batch operations where user interaction is not available,
/// such as `--theirs` or `--ours` style resolution.
pub fn resolve_conflicts_batch(
    conflicts: &[FieldConflict],
    keep: Resolution,
) -> Vec<ResolvedConflict> {
    conflicts
        .iter()
        .map(|c| ResolvedConflict {
            field: c.field.clone(),
            resolution: keep.clone(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::super::diff::FieldConflict;
    use super::*;

    fn sample_conflicts() -> Vec<FieldConflict> {
        vec![
            FieldConflict {
                field: "title".to_string(),
                base_value: "Original".to_string(),
                local_value: "Local title".to_string(),
                remote_value: "Remote title".to_string(),
            },
            FieldConflict {
                field: "state".to_string(),
                base_value: "todo".to_string(),
                local_value: "in-progress".to_string(),
                remote_value: "done".to_string(),
            },
            FieldConflict {
                field: "priority".to_string(),
                base_value: "none".to_string(),
                local_value: "high".to_string(),
                remote_value: "low".to_string(),
            },
        ]
    }

    #[test]
    fn batch_keep_local_resolves_all_as_local() {
        let conflicts = sample_conflicts();
        let resolutions = resolve_conflicts_batch(&conflicts, Resolution::KeepLocal);

        assert_eq!(resolutions.len(), 3);
        for r in &resolutions {
            assert!(matches!(r.resolution, Resolution::KeepLocal));
        }
        assert_eq!(resolutions[0].field, "title");
        assert_eq!(resolutions[1].field, "state");
        assert_eq!(resolutions[2].field, "priority");
    }

    #[test]
    fn batch_keep_remote_resolves_all_as_remote() {
        let conflicts = sample_conflicts();
        let resolutions = resolve_conflicts_batch(&conflicts, Resolution::KeepRemote);

        assert_eq!(resolutions.len(), 3);
        for r in &resolutions {
            assert!(matches!(r.resolution, Resolution::KeepRemote));
        }
    }

    #[test]
    fn batch_skip_resolves_all_as_skip() {
        let conflicts = sample_conflicts();
        let resolutions = resolve_conflicts_batch(&conflicts, Resolution::Skip);

        assert_eq!(resolutions.len(), 3);
        for r in &resolutions {
            assert!(matches!(r.resolution, Resolution::Skip));
        }
    }

    #[test]
    fn batch_empty_conflicts_returns_empty() {
        let conflicts: Vec<FieldConflict> = vec![];
        let resolutions = resolve_conflicts_batch(&conflicts, Resolution::KeepLocal);
        assert!(resolutions.is_empty());
    }
}
