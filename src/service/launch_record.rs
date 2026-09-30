//! The settings a person chose for a story's last confirmed launch (SH-850).
//!
//! `story.sh` writes [`LAUNCH_RECORD_FILE`] beside the cleanup marker, in the
//! story worktree's private Git directory, once a dispatch's handoff is
//! confirmed. After a reboot has erased the pane's environment it is the only
//! record of how the lost agent was launched, so the dashboard's Resume offers
//! it again. It is not identity: nothing selects, authorizes or removes a
//! resource by it, and a record that cannot be read costs only the prefill.
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The record's file name. `LAUNCH_RECORD_FILE` in `plugins/story/bin/story.sh`
/// spells the same name; the helper writes what this module reads.
pub const LAUNCH_RECORD_FILE: &str = "storyhook-launch-v1.json";

/// How the launch ran its session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Autonomy {
    /// A person answers the agent's plan and questions.
    Attended,
    /// The autonomous charter (`--auto`).
    Auto,
    /// A Full Auto engine lane (`--auto --full-auto`).
    FullAuto,
}

/// One confirmed launch's settings. Selectors hold only what was chosen
/// explicitly: `None` is the provider's (or the dispatch policy's) default,
/// which a resume re-resolves exactly as the launch did.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchRecord {
    /// Always 1 for this file name.
    pub version: u32,
    /// The project the launch belonged to.
    pub project_slug: String,
    /// The story the launch dispatched.
    pub story_id: String,
    /// `claude` or `codex`.
    pub provider: String,
    /// The explicitly chosen model.
    pub model: Option<String>,
    /// The explicitly chosen reasoning effort.
    pub effort: Option<String>,
    /// The explicitly chosen speed tier: `standard` or `fast`.
    pub speed: Option<String>,
    /// How the session ran.
    pub autonomy: Autonomy,
    /// RFC3339, when the launch was confirmed.
    pub recorded_at: String,
}

/// Reads the launch record of the story worktree at `worktree`.
///
/// `Ok(None)` when the worktree has no private Git directory of its own (it
/// is missing, or a main checkout) or no record was ever written. A record
/// that exists but cannot be read, does not parse, or names another project,
/// story, provider or speed is an `Err` naming why: it must not be offered.
pub fn read(worktree: &Path, project: &str, story: &str) -> Result<Option<LaunchRecord>, String> {
    let Some(private) = private_git_dir(worktree)? else {
        return Ok(None);
    };
    let path = private.join(LAUNCH_RECORD_FILE);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("cannot read {}: {error}", path.display())),
    };
    let record: LaunchRecord = serde_json::from_str(&text)
        .map_err(|error| format!("{} is not a launch record: {error}", path.display()))?;
    if record.version != 1 {
        return Err(format!(
            "{} has unsupported version {}",
            path.display(),
            record.version
        ));
    }
    if record.project_slug != project || record.story_id != story {
        return Err(format!(
            "{} records {}/{}, not {project}/{story}",
            path.display(),
            record.project_slug,
            record.story_id
        ));
    }
    if !matches!(record.provider.as_str(), "claude" | "codex") {
        return Err(format!(
            "{} names unknown provider `{}`",
            path.display(),
            record.provider
        ));
    }
    if record
        .speed
        .as_deref()
        .is_some_and(|speed| !matches!(speed, "standard" | "fast"))
    {
        return Err(format!("{} names an unknown speed", path.display()));
    }
    Ok(Some(record))
}

/// The private Git directory a linked worktree's `.git` file points at, or
/// `None` when `worktree` has no `.git` file (missing, or a main checkout).
fn private_git_dir(worktree: &Path) -> Result<Option<PathBuf>, String> {
    let gitfile = worktree.join(".git");
    let text = match std::fs::symlink_metadata(&gitfile) {
        Ok(meta) if meta.file_type().is_file() => std::fs::read_to_string(&gitfile)
            .map_err(|error| format!("cannot read {}: {error}", gitfile.display()))?,
        Ok(_) => return Ok(None),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("cannot inspect {}: {error}", gitfile.display())),
    };
    let target = text
        .lines()
        .next()
        .and_then(|line| line.strip_prefix("gitdir:"))
        .map(str::trim)
        .filter(|target| !target.is_empty())
        .ok_or_else(|| format!("{} names no gitdir", gitfile.display()))?;
    Ok(Some(worktree.join(target)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A linked-worktree layout: `<root>/wt/.git` points at `<root>/admin`.
    fn layout(gitdir_line: &str) -> (tempfile::TempDir, PathBuf, PathBuf) {
        let root = storyhook_test_support::scratch_dir();
        let worktree = root.path().join("wt");
        let admin = root.path().join("admin");
        std::fs::create_dir_all(&worktree).unwrap();
        std::fs::create_dir_all(&admin).unwrap();
        std::fs::write(
            worktree.join(".git"),
            gitdir_line.replace("ADMIN", admin.to_str().unwrap()),
        )
        .unwrap();
        (root, worktree, admin)
    }

    fn record(provider: &str, autonomy: &str) -> String {
        format!(
            r#"{{"version":1,"project_slug":"proj","story_id":"SH-1","provider":"{provider}","model":"opus","effort":null,"speed":"fast","autonomy":"{autonomy}","recorded_at":"2026-09-29T00:00:00Z"}}"#
        )
    }

    #[test]
    fn a_confirmed_launch_is_read_back_exactly() {
        let (_root, worktree, admin) = layout("gitdir: ADMIN\n");
        std::fs::write(admin.join(LAUNCH_RECORD_FILE), record("codex", "full-auto")).unwrap();
        let read = read(&worktree, "proj", "SH-1").unwrap().unwrap();
        assert_eq!(read.provider, "codex");
        assert_eq!(read.model.as_deref(), Some("opus"));
        assert_eq!(read.effort, None);
        assert_eq!(read.speed.as_deref(), Some("fast"));
        assert_eq!(read.autonomy, Autonomy::FullAuto);
    }

    #[test]
    fn a_relative_gitdir_resolves_against_the_worktree() {
        let (_root, worktree, admin) = layout("gitdir: ../admin\n");
        std::fs::write(admin.join(LAUNCH_RECORD_FILE), record("claude", "attended")).unwrap();
        assert_eq!(
            read(&worktree, "proj", "SH-1").unwrap().unwrap().autonomy,
            Autonomy::Attended
        );
    }

    #[test]
    fn no_worktree_no_gitfile_or_no_record_is_no_record() {
        let root = storyhook_test_support::scratch_dir();
        assert_eq!(read(&root.path().join("missing"), "proj", "SH-1"), Ok(None));
        std::fs::create_dir_all(root.path().join("main/.git")).unwrap();
        assert_eq!(read(&root.path().join("main"), "proj", "SH-1"), Ok(None));
        let (_root, worktree, _admin) = layout("gitdir: ADMIN\n");
        assert_eq!(read(&worktree, "proj", "SH-1"), Ok(None));
    }

    #[test]
    fn a_record_that_cannot_be_trusted_is_refused_with_its_reason() {
        for (label, text, story) in [
            ("malformed", "{".to_string(), "SH-1"),
            ("foreign story", record("claude", "auto"), "SH-2"),
            ("unknown provider", record("gemini", "auto"), "SH-1"),
            ("unknown autonomy", record("claude", "sometimes"), "SH-1"),
            (
                "unknown field",
                record("claude", "auto").replace("\"version\":1", "\"version\":1,\"extra\":1"),
                "SH-1",
            ),
            (
                "unknown speed",
                record("claude", "auto").replace("\"fast\"", "\"ludicrous\""),
                "SH-1",
            ),
            (
                "future version",
                record("claude", "auto").replace("\"version\":1", "\"version\":2"),
                "SH-1",
            ),
        ] {
            let (_root, worktree, admin) = layout("gitdir: ADMIN\n");
            std::fs::write(admin.join(LAUNCH_RECORD_FILE), text).unwrap();
            let error = read(&worktree, "proj", story).expect_err(label);
            assert!(error.contains(LAUNCH_RECORD_FILE), "{label}: {error}");
        }
        let (_root, worktree, _admin) = layout("nonsense\n");
        assert!(
            read(&worktree, "proj", "SH-1").is_err(),
            "a gitfile naming no gitdir"
        );
    }
}
