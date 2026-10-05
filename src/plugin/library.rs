//! The plugin's Python and shell library, as this binary carries it (SH-881).
//!
//! A helper that the binary runs from its own copy of `plugins/story/lib`
//! gets the whole directory, never a list of the files it is thought to
//! import. SH-825 added imports to `stop-dispatch-pane.py` that the hand-kept
//! dropped-cleanup list lacked, and every closed story's pane cleanup failed
//! until SH-881. The copy is the same set of files the installed plugin's
//! `lib/` holds, so a helper resolves its imports here exactly as it does
//! there and in the plugin's own tests.

use std::path::Path;

use crate::embedded::EmbeddedFile;
use crate::error::AppError;

/// The marketplace path of the library directory, `/`-terminated.
const LIBRARY: &str = "plugins/story/lib/";

/// Every file beneath `plugins/story/lib`, with its path relative to that
/// directory.
pub(crate) fn files() -> Vec<EmbeddedFile> {
    super::EMBEDDED_MARKETPLACE
        .iter()
        .filter_map(|file| {
            file.relative_path
                .strip_prefix(LIBRARY)
                .map(|relative_path| EmbeddedFile {
                    relative_path,
                    bytes: file.bytes,
                    executable: file.executable,
                })
        })
        .collect()
}

/// Writes the library beneath `root`, creating directories as needed.
pub(crate) fn project(root: &Path) -> Result<(), AppError> {
    crate::embedded::write(&files(), root)
}
