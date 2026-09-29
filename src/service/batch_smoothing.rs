//! Automated smoothing of non-code conflicts between batch members (SH-834;
//! spec position B8, council decision D1 on SH-834): the project's
//! allowlist as its committed pointer states it.
//!
//! `.storyhook.toml` may carry
//!
//! ```toml
//! [batch]
//! smooth = ["docs/spec/", ".gitignore"]
//! ```
//!
//! The verifier reads it from the batch's **base** commit only, never from a
//! member: a member must not widen what its own batch may smooth. Absent, the
//! list is empty and nothing is smoothed. The table is its own, not a
//! `[verify]` key, because an older verifier refuses every unknown `[verify]`
//! key in the merge trees it inspects but ignores unknown tables.
//!
//! [`classify`] reads one conflicted merge the way council D1 admits it:
//! every conflicted path passes the deny floor and the text checks, and
//! every diff3 hunk Git wrote for it is insertion-only. It needs blobs, so
//! it reads them through a [`BlobSource`]; a blob it cannot read makes the
//! merge not smoothable, never an error, because a candidate the verifier
//! cannot judge stays out of the batch either way.

use serde::Deserialize;

use super::trial_merge::{BlobSource, ConflictShape, StageEntry};
use crate::domain::conflict_smoothing::{
    HunkRefusal, MAX_BLOB_BYTES, MAX_SMOOTHED_PATHS, SmoothPolicy, check_text, path_refusal,
    union_insertions,
};

/// Where a batch's base keeps its smoothing allowlist.
pub const POINTER: &str = ".storyhook.toml";

/// The informational record types a smoothable merge may carry. Git calls
/// them stable; any other, including one a later Git adds, is refused.
/// An add/add conflict is also `CONFLICT (contents)`; it is refused by its
/// missing stage 1.
const ADMITTED_RECORDS: &[&str] = &["Auto-merging", "CONFLICT (contents)"];

/// How one conflicted merge reads for smoothing (council D1 on SH-834).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Classification {
    /// Every conflicted path unites: what the verifier may resolve when the
    /// base's allowlist admits every path.
    UnionSmoothable(Vec<SmoothedFile>),
    /// Every conflicted path passes the floor and the text checks, but a
    /// hunk changes lines both sides share, so no rule resolves it: the
    /// `agent-candidate` measure. The text names the first such path.
    AgentCandidate(String),
    /// Anything else, and why.
    NotSmoothable(String),
}

/// One conflicted path the verifier can unite.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SmoothedFile {
    /// The path.
    pub path: String,
    /// Its blob in the merge base (stage 1).
    pub base: String,
    /// Its blob on the side merged onto (stage 2): the batch so far.
    pub ours: String,
    /// Its blob on the side merged in (stage 3): the member.
    pub theirs: String,
    /// The united content: its blob in the conflicted tree, with diff3
    /// markers, without its marker lines.
    pub resolved: String,
}

/// Classifies the conflicted merge `shape`, reading its blobs from `blobs`.
#[must_use]
pub fn classify(shape: &ConflictShape, blobs: &mut dyn BlobSource) -> Classification {
    if let Some(record) = shape
        .records
        .iter()
        .find(|record| !ADMITTED_RECORDS.contains(&record.kind.as_str()))
    {
        return Classification::NotSmoothable(format!(
            "{} is {}, not a content conflict",
            record.paths.join(", "),
            record.kind
        ));
    }
    let paths = shape.paths();
    if paths.is_empty() {
        return Classification::NotSmoothable("the merge names no conflicted path".into());
    }
    if paths.len() > MAX_SMOOTHED_PATHS {
        return Classification::NotSmoothable(format!(
            "{} conflicted paths, over the limit of {MAX_SMOOTHED_PATHS}",
            paths.len()
        ));
    }
    let mut files = Vec::with_capacity(paths.len());
    let mut modifies = None;
    for path in &paths {
        match classify_path(shape, path, blobs) {
            Ok(Ok(file)) => files.push(file),
            Ok(Err(HunkRefusal::Modifies)) => {
                modifies.get_or_insert_with(|| format!("a hunk of {path} changes shared lines"));
            }
            Ok(Err(refusal @ HunkRefusal::Malformed(_))) => {
                return Classification::NotSmoothable(format!("{path}: {refusal}"));
            }
            Err(why) => return Classification::NotSmoothable(why),
        }
    }
    match modifies {
        Some(why) => Classification::AgentCandidate(why),
        None => Classification::UnionSmoothable(files),
    }
}

/// Whether `policy` admits every path of `files`.
#[must_use]
pub fn admits_all(policy: &SmoothPolicy, files: &[SmoothedFile]) -> bool {
    !files.is_empty() && files.iter().all(|file| policy.admits(&file.path))
}

/// One path: `Err` when it can never be smoothed, else the hunk parser's
/// answer on its conflicted blob.
fn classify_path(
    shape: &ConflictShape,
    path: &str,
    blobs: &mut dyn BlobSource,
) -> Result<Result<SmoothedFile, HunkRefusal>, String> {
    if let Some(why) = path_refusal(path) {
        return Err(why);
    }
    let entries: Vec<&StageEntry> = shape.stages.iter().filter(|e| e.path == path).collect();
    let stage = |number: u8| entries.iter().find(|entry| entry.stage == number);
    let (Some(base), Some(ours), Some(theirs)) = (stage(1), stage(2), stage(3)) else {
        return Err(
            if stage(1).is_none() && stage(2).is_some() && stage(3).is_some() {
                format!("{path} was added on both sides (add/add)")
            } else {
                format!("{path} is missing on one side of the merge")
            },
        );
    };
    if entries.len() != 3 {
        return Err(format!("{path} has {} index entries, not 3", entries.len()));
    }
    if let Some(entry) = entries.iter().find(|entry| entry.mode != "100644") {
        return Err(format!(
            "{path} has mode {} at stage {}; only regular non-executable files are smoothed",
            entry.mode, entry.stage
        ));
    }
    for (side, entry) in [("base", base), ("ours", ours), ("theirs", theirs)] {
        let bytes = blobs
            .blob(&entry.oid)
            .map_err(|error| format!("{path} ({side}) could not be read: {error}"))?;
        check_text(&format!("{path} ({side})"), &bytes)?;
    }
    let conflicted = blobs
        .file(&shape.tree, path)
        .map_err(|error| format!("{path} (conflicted) could not be read: {error}"))?
        .ok_or_else(|| format!("the conflicted tree has no {path}"))?;
    if conflicted.len() > 3 * MAX_BLOB_BYTES || conflicted.contains(&0) {
        return Err(format!("{path} (conflicted) is too large or not text"));
    }
    let text = String::from_utf8(conflicted)
        .map_err(|_| format!("{path} (conflicted) is not UTF-8 text"))?;
    Ok(union_insertions(&text).map(|resolved| SmoothedFile {
        path: path.to_owned(),
        base: base.oid.clone(),
        ours: ours.oid.clone(),
        theirs: theirs.oid.clone(),
        resolved,
    }))
}

/// The `[batch]` table, typed at the point of use. `deny_unknown_fields`
/// turns `smoth = […]` into a refusal instead of an empty list.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BatchTable {
    /// The paths a batch may smooth.
    smooth: Option<Vec<String>>,
}

/// The smoothing allowlist in committed pointer bytes (`None`: the commit
/// has no `.storyhook.toml`). No pointer, no `[batch]` table or no `smooth`
/// key is the empty policy. The `Err` says what is wrong, for the person who
/// wrote the file; the caller smooths nothing and reports it.
pub fn policy_from_pointer(raw: Option<&[u8]>) -> Result<SmoothPolicy, String> {
    let Some(raw) = raw else {
        return Ok(SmoothPolicy::default());
    };
    let text = std::str::from_utf8(raw)
        .map_err(|error| format!("the committed .storyhook.toml is not UTF-8: {error}"))?;
    let pointer = toml::from_str::<super::project::ProjectPointer>(text)
        .map_err(|error| format!("the committed .storyhook.toml is not valid: {error}"))?;
    let Some(table) = pointer.batch else {
        return Ok(SmoothPolicy::default());
    };
    let table: BatchTable = table.try_into().map_err(|error| {
        format!(
            "the [batch] table in .storyhook.toml failed to parse: {error}. It takes one key, \
             `smooth`, a list of paths such as [\"docs/spec/\", \".gitignore\"]."
        )
    })?;
    SmoothPolicy::parse(&table.smooth.unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    const IDENTITY: &str = "schema = 1\nuuid = \"u\"\nprefix = \"SH\"\n";

    fn policy(tables: &str) -> Result<SmoothPolicy, String> {
        policy_from_pointer(Some(format!("{IDENTITY}{tables}").as_bytes()))
    }

    #[test]
    fn no_pointer_no_table_and_no_key_are_the_empty_policy() {
        assert_eq!(policy_from_pointer(None), Ok(SmoothPolicy::default()));
        assert_eq!(policy(""), Ok(SmoothPolicy::default()));
        assert_eq!(
            policy("\n[verify]\ngate = \"make test\"\n"),
            Ok(SmoothPolicy::default())
        );
        assert_eq!(policy("\n[batch]\n"), Ok(SmoothPolicy::default()));
        assert_eq!(
            policy("\n[batch]\nsmooth = []\n"),
            Ok(SmoothPolicy::default())
        );
    }

    #[test]
    fn a_configured_list_is_read_as_written() {
        let policy = policy("\n[batch]\nsmooth = [\"docs/spec/\", \".gitignore\"]\n").unwrap();
        assert_eq!(policy.entries(), ["docs/spec/", ".gitignore"]);
        assert!(policy.admits("docs/spec/a.md"));
        assert!(!policy.admits("src/a.rs"));
    }

    #[test]
    fn a_misspelled_key_a_wrong_type_or_a_bad_entry_is_refused_by_name() {
        let misspelled = policy("\n[batch]\nsmoth = [\"docs/\"]\n").unwrap_err();
        assert!(misspelled.contains("smoth"), "{misspelled}");
        let wrong_type = policy("\n[batch]\nsmooth = \"docs/\"\n").unwrap_err();
        assert!(wrong_type.contains("[batch]"), "{wrong_type}");
        let glob = policy("\n[batch]\nsmooth = [\"docs/*.md\"]\n").unwrap_err();
        assert!(glob.contains("docs/*.md"), "{glob}");
        let invalid = policy_from_pointer(Some(b"not toml = [")).unwrap_err();
        assert!(invalid.contains("not valid"), "{invalid}");
        let binary = policy_from_pointer(Some(b"\xff")).unwrap_err();
        assert!(binary.contains("UTF-8"), "{binary}");
    }

    use crate::error::AppError;
    use crate::service::trial_merge::{ConflictRecord, StageEntry};
    use std::collections::BTreeMap;

    /// Blobs by object id, and conflicted files by `<tree>:<path>`.
    #[derive(Default)]
    struct Blobs {
        blobs: BTreeMap<String, Vec<u8>>,
        files: BTreeMap<String, Vec<u8>>,
    }

    impl BlobSource for Blobs {
        fn blob(&mut self, oid: &str) -> Result<Vec<u8>, AppError> {
            self.blobs
                .get(oid)
                .cloned()
                .ok_or_else(|| AppError::Storage(format!("no blob {oid}")))
        }
        fn file(&mut self, treeish: &str, path: &str) -> Result<Option<Vec<u8>>, AppError> {
            Ok(self.files.get(&format!("{treeish}:{path}")).cloned())
        }
    }

    const TREE: &str = "7777777777777777777777777777777777777777";

    fn oid(seed: &str, stage: u8) -> String {
        let digit = char::from_digit(u32::from(stage), 10).unwrap();
        let mut id: String = seed
            .bytes()
            .map(|b| char::from_digit(u32::from(b) % 16, 16).unwrap())
            .collect();
        id.push(digit);
        format!("{id:0<40}")[..40].to_owned()
    }

    /// A conflicted path with the three sides and the conflicted text.
    fn conflicted(
        shape: &mut ConflictShape,
        blobs: &mut Blobs,
        path: &str,
        sides: [&str; 3],
        text: &str,
    ) {
        for (index, side) in sides.iter().enumerate() {
            let stage = u8::try_from(index + 1).unwrap();
            let id = oid(path, stage);
            blobs.blobs.insert(id.clone(), side.as_bytes().to_vec());
            shape.stages.push(StageEntry {
                mode: "100644".into(),
                oid: id,
                stage,
                path: path.into(),
            });
        }
        shape.records.push(ConflictRecord {
            kind: "CONFLICT (contents)".into(),
            paths: vec![path.into()],
        });
        blobs
            .files
            .insert(format!("{TREE}:{path}"), text.as_bytes().to_vec());
    }

    fn shape() -> ConflictShape {
        ConflictShape {
            tree: TREE.into(),
            ..ConflictShape::default()
        }
    }

    const INSERTION: [&str; 3] = ["a\nz\n", "a\nx\nz\n", "a\ny\nz\n"];
    const INSERTED: &str = "a\n<<<<<<< o\nx\n||||||| b\n=======\ny\n>>>>>>> t\nz\n";
    const MODIFIED: &str = "a\n<<<<<<< o\nx\n||||||| b\nold\n=======\ny\n>>>>>>> t\nz\n";

    #[test]
    fn insertion_only_hunks_in_text_files_are_union_smoothable() {
        let (mut shape, mut blobs) = (shape(), Blobs::default());
        conflicted(&mut shape, &mut blobs, "docs/a.md", INSERTION, INSERTED);
        conflicted(&mut shape, &mut blobs, "docs/b.md", INSERTION, INSERTED);
        let Classification::UnionSmoothable(files) = classify(&shape, &mut blobs) else {
            panic!("{:?}", classify(&shape, &mut blobs));
        };
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].path, "docs/a.md");
        assert_eq!(files[0].resolved, "a\nx\ny\nz\n");
        assert_eq!(files[0].base, oid("docs/a.md", 1));
        assert_eq!(files[0].ours, oid("docs/a.md", 2));
        assert_eq!(files[0].theirs, oid("docs/a.md", 3));
        let policy = SmoothPolicy::parse(&["docs/".into()]).unwrap();
        assert!(admits_all(&policy, &files));
        let narrow = SmoothPolicy::parse(&["docs/a.md".into()]).unwrap();
        assert!(!admits_all(&narrow, &files));
        assert!(!admits_all(&policy, &[]));
    }

    #[test]
    fn a_hunk_that_changes_shared_lines_is_an_agent_candidate() {
        let (mut shape, mut blobs) = (shape(), Blobs::default());
        conflicted(&mut shape, &mut blobs, "docs/a.md", INSERTION, INSERTED);
        conflicted(&mut shape, &mut blobs, "docs/b.md", INSERTION, MODIFIED);
        assert_eq!(
            classify(&shape, &mut blobs),
            Classification::AgentCandidate("a hunk of docs/b.md changes shared lines".into())
        );
    }

    #[test]
    fn any_path_that_can_never_be_smoothed_wins_over_an_agent_candidate_in_any_order() {
        for denied_first in [true, false] {
            let (mut shape, mut blobs) = (shape(), Blobs::default());
            if denied_first {
                conflicted(&mut shape, &mut blobs, "CLAUDE.md", INSERTION, INSERTED);
            }
            conflicted(&mut shape, &mut blobs, "docs/b.md", INSERTION, MODIFIED);
            if !denied_first {
                conflicted(&mut shape, &mut blobs, "CLAUDE.md", INSERTION, INSERTED);
            }
            let Classification::NotSmoothable(why) = classify(&shape, &mut blobs) else {
                panic!("denied_first={denied_first}");
            };
            assert!(why.contains("deny floor"), "{why}");
        }
    }

    #[test]
    fn a_conflict_type_other_than_contents_is_refused_even_on_a_smoothable_path() {
        let (mut shape, mut blobs) = (shape(), Blobs::default());
        conflicted(&mut shape, &mut blobs, "docs/a.md", INSERTION, INSERTED);
        for kind in [
            "CONFLICT (modify/delete)",
            "CONFLICT (rename/delete)",
            "CONFLICT (binary)",
            "CONFLICT (a type a later Git adds)",
        ] {
            let mut with = shape.clone();
            with.records.push(ConflictRecord {
                kind: kind.into(),
                paths: vec!["docs/a.md".into()],
            });
            let Classification::NotSmoothable(why) = classify(&with, &mut blobs) else {
                panic!("{kind} was smoothable");
            };
            assert!(why.contains(kind), "{why}");
        }
    }

    #[test]
    fn add_add_a_missing_side_an_extra_entry_and_an_executable_mode_are_refused() {
        let (base, mut blobs) = (shape(), Blobs::default());
        let mut full = base.clone();
        conflicted(&mut full, &mut blobs, "docs/a.md", INSERTION, INSERTED);

        let mut add_add = full.clone();
        add_add.stages.retain(|entry| entry.stage != 1);
        let why = format!("{:?}", classify(&add_add, &mut blobs));
        assert!(why.contains("add/add"), "{why}");

        let mut one_sided = full.clone();
        one_sided.stages.retain(|entry| entry.stage != 3);
        let why = format!("{:?}", classify(&one_sided, &mut blobs));
        assert!(why.contains("missing on one side"), "{why}");

        let mut doubled = full.clone();
        doubled.stages.push(doubled.stages[0].clone());
        let why = format!("{:?}", classify(&doubled, &mut blobs));
        assert!(why.contains("4 index entries"), "{why}");

        for mode in ["100755", "120000", "160000"] {
            let mut odd = full.clone();
            odd.stages[1].mode = mode.into();
            let why = format!("{:?}", classify(&odd, &mut blobs));
            assert!(why.contains(mode), "{why}");
        }
    }

    #[test]
    fn a_side_that_is_not_plain_text_or_already_holds_a_marker_is_refused() {
        for (label, sides) in [
            ("NUL", ["a\n", "a\0x\n", "a\ny\n"]),
            ("marker", ["a\n", "a\n=======\n", "a\ny\n"]),
        ] {
            let (mut shape, mut blobs) = (shape(), Blobs::default());
            conflicted(&mut shape, &mut blobs, "docs/a.md", sides, INSERTED);
            let why = format!("{:?}", classify(&shape, &mut blobs));
            assert!(why.contains(label), "{label}: {why}");
        }
        let (mut shape, mut blobs) = (shape(), Blobs::default());
        conflicted(&mut shape, &mut blobs, "docs/a.md", INSERTION, INSERTED);
        blobs
            .blobs
            .insert(oid("docs/a.md", 2), b"\xff\xfe".to_vec());
        let why = format!("{:?}", classify(&shape, &mut blobs));
        assert!(why.contains("UTF-8"), "{why}");
    }

    #[test]
    fn an_unreadable_blob_or_a_missing_conflicted_file_is_not_smoothable() {
        let (mut shape, mut blobs) = (shape(), Blobs::default());
        conflicted(&mut shape, &mut blobs, "docs/a.md", INSERTION, INSERTED);
        let mut unreadable = Blobs {
            blobs: BTreeMap::new(),
            files: blobs.files.clone(),
        };
        let why = format!("{:?}", classify(&shape, &mut unreadable));
        assert!(why.contains("could not be read"), "{why}");
        blobs.files.clear();
        let why = format!("{:?}", classify(&shape, &mut blobs));
        assert!(why.contains("has no docs/a.md"), "{why}");
    }

    #[test]
    fn malformed_markers_no_path_and_too_many_paths_are_not_smoothable() {
        let (mut shape_, mut blobs) = (shape(), Blobs::default());
        conflicted(
            &mut shape_,
            &mut blobs,
            "docs/a.md",
            INSERTION,
            "no markers\n",
        );
        let why = format!("{:?}", classify(&shape_, &mut blobs));
        assert!(why.contains("not diff3 hunks"), "{why}");

        assert!(matches!(
            classify(&shape(), &mut Blobs::default()),
            Classification::NotSmoothable(_)
        ));

        let (mut many, mut blobs) = (shape(), Blobs::default());
        for index in 0..=MAX_SMOOTHED_PATHS {
            conflicted(
                &mut many,
                &mut blobs,
                &format!("docs/{index}.md"),
                INSERTION,
                INSERTED,
            );
        }
        let why = format!("{:?}", classify(&many, &mut blobs));
        assert!(why.contains("over the limit"), "{why}");
    }
}
