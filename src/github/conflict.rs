use super::diff::{ConflictField, FieldConflict};

/// Which side of a conflict wins.
///
/// **There is no `Skip`, and its absence is the fix for SH-152.** A third
/// variant meaning "decide later" is only honest if something later decides;
/// what happened instead was that the merge base advanced past the conflict, so
/// the next sync saw no disagreement and let the remote value overwrite the
/// local edit in silence. A conflict this program has not been told how to
/// resolve is not resolved at all — it stays a [`FieldConflict`], travels back
/// to the caller in the refusal, and waits for an answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    KeepLocal,
    KeepRemote,
}

/// A conflict together with its chosen resolution.
#[derive(Debug, Clone)]
pub struct ResolvedConflict {
    pub field: ConflictField,
    pub resolution: Resolution,
}

/// Resolve conflicts non-interactively, keeping all local or all remote.
///
/// The only way a conflict is ever resolved. There used to be a second —
/// `resolve_conflicts_interactive`, which drew a `dialoguer::Select` and, on
/// the error a missing terminal produces, chose `Skip` for the user. Since the
/// work runs in a daemon spawned with no stdin and its stderr on a log file,
/// that error was not the unlikely case; it was the only case. The menu is
/// gone, and with it the last prompt under `src/github/`.
pub fn resolve_conflicts_batch(
    conflicts: &[FieldConflict],
    keep: Resolution,
) -> Vec<ResolvedConflict> {
    conflicts
        .iter()
        .map(|c| ResolvedConflict {
            field: c.field,
            resolution: keep,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::super::diff::{ConflictField, FieldConflict};
    use super::*;

    fn sample_conflicts() -> Vec<FieldConflict> {
        vec![
            FieldConflict {
                field: ConflictField::Title,
                base_value: "Original".to_string(),
                local_value: "Local title".to_string(),
                remote_value: "Remote title".to_string(),
            },
            FieldConflict {
                field: ConflictField::State,
                base_value: "todo".to_string(),
                local_value: "in-progress".to_string(),
                remote_value: "done".to_string(),
            },
            FieldConflict {
                field: ConflictField::Priority,
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
        assert_eq!(resolutions[0].field, ConflictField::Title);
        assert_eq!(resolutions[1].field, ConflictField::State);
        assert_eq!(resolutions[2].field, ConflictField::Priority);
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

    /// There is no third answer. `batch_skip_resolves_all_as_skip` used to sit
    /// here and prove that a conflict could be resolved as "not resolved" —
    /// the state SH-152's data loss lived in — and it went with the variant.
    #[test]
    fn every_resolution_names_a_side() {
        for resolution in [Resolution::KeepLocal, Resolution::KeepRemote] {
            let resolutions = resolve_conflicts_batch(&sample_conflicts(), resolution);
            assert!(resolutions.iter().all(|r| r.resolution == resolution));
        }
    }

    #[test]
    fn batch_empty_conflicts_returns_empty() {
        let conflicts: Vec<FieldConflict> = vec![];
        let resolutions = resolve_conflicts_batch(&conflicts, Resolution::KeepLocal);
        assert!(resolutions.is_empty());
    }
}
