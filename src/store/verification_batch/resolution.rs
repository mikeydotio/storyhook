//! The automated resolution a batch member's merge carries (SH-834; spec
//! B8, council decision D1 on SH-834).
//!
//! Only the last member of a batch may carry one: its merge commit is the
//! batch tip, so every shorter prefix of the merge chain, which a
//! bisection gates, holds none.

use serde::{Deserialize, Serialize};

/// How the verifier resolved the conflicts of one member's merge.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchResolution {
    /// The versioned strategy, such as `union-insertions/1`.
    pub strategy: String,
    /// The earlier members whose merges changed a resolved path, by story
    /// id, in queue order.
    pub conflicted_with: Vec<String>,
    /// The tree Git wrote for the conflicted merge, with its markers: the
    /// auto-merge the resolution differs from only at `files`, and there
    /// only by the removed marker lines.
    pub auto_merge_tree: String,
    /// Every resolved path, in Git's order.
    pub files: Vec<ResolvedFile>,
}

/// One resolved path and the blobs that decide it, so the resolution can
/// be recomputed exactly.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedFile {
    /// The path.
    pub path: String,
    /// Its blob in the merge base.
    pub base: String,
    /// Its blob in the batch it was merged onto.
    pub ours: String,
    /// Its blob in the member.
    pub theirs: String,
    /// Its blob in the merge commit.
    pub resolved: String,
}

impl BatchResolution {
    /// Why the resolution is malformed, if it is: a strategy, at least one
    /// file, each path once, and full object ids throughout.
    pub(super) fn refusal(&self, pinned: impl Fn(&str) -> bool) -> Option<&'static str> {
        if self.strategy.is_empty() {
            return Some("a resolution names its strategy");
        }
        if self.files.is_empty() {
            return Some("a resolution resolves at least one path");
        }
        let mut paths: Vec<&str> = self.files.iter().map(|file| file.path.as_str()).collect();
        paths.sort_unstable();
        paths.dedup();
        if paths.len() != self.files.len() || paths.iter().any(|path| path.is_empty()) {
            return Some("a resolution resolves each path once");
        }
        let oids = self.files.iter().flat_map(|file| {
            [
                file.base.as_str(),
                file.ours.as_str(),
                file.theirs.as_str(),
                file.resolved.as_str(),
            ]
        });
        if !pinned(&self.auto_merge_tree) || oids.into_iter().any(|oid| !pinned(oid)) {
            return Some("a resolution's trees and blobs are full object ids");
        }
        None
    }
}
