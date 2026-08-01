//! [`StoreLocation`] — which store this invocation is about.
//!
//! # A store's identity determines its daemon's identity
//!
//! Daemon runtime state used to be keyed on the state home while the store was
//! keyed on the data home. The two move independently and nothing reconciled
//! them, so a client could be pointed at one store and served — for reads *and*
//! writes — by a daemon holding another, with no diagnostic and exit 0
//! (SH-123).
//!
//! What this type gives the rest of the program is a **canonical path**, which
//! [`Environment`](super::Environment) derives the daemon's portfile, pidfile,
//! spawn lock and log from. One store therefore has exactly one daemon by
//! construction, and "client and daemon disagree about the store" is
//! unrepresentable rather than detected.
//!
//! That is deliberately not a check. A check would leave the shared state home
//! in place and fix the encounter point; the origin is the sharing itself.
//!
//! Design of record: `docs/spec/store-isolation.md`.

use std::path::{Component, Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::error::AppError;

/// The store database's file name inside a data directory.
const STORE_FILE: &str = "store.db";

/// How many hex characters of the path digest name a store's state directory.
///
/// Eight bytes of SHA-256. Long enough that a collision between two paths on
/// one machine is not a thing that happens, short enough to stay readable in a
/// diagnostic — and a collision would be *detectable* rather than silent,
/// because the portfile inside carries the store path in full.
const KEY_HEX: usize = 16;

/// How the store was chosen.
///
/// Carried rather than discarded because the two places that have to explain a
/// store to somebody who did not choose it — the test-build refusal and
/// `story store new` — need to name the lever the user actually pulled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoreOrigin {
    /// `--store-path`.
    Flag,
    /// `$STORYHOOK_STORE_PATH`.
    StorePathVar,
    /// `$STORYHOOK_DATA_DIR`, plus `store.db`.
    DataDirVar,
    /// Nothing named one: `$XDG_DATA_HOME/storyhook/store.db`, else
    /// `~/.local/share/storyhook/store.db`.
    XdgDefault,
}

impl StoreOrigin {
    /// How to name this choice back to whoever made it.
    pub fn describe(self) -> &'static str {
        match self {
            StoreOrigin::Flag => "--store-path",
            StoreOrigin::StorePathVar => "$STORYHOOK_STORE_PATH",
            StoreOrigin::DataDirVar => "$STORYHOOK_DATA_DIR",
            StoreOrigin::XdgDefault => "the default location",
        }
    }
}

/// The environment variables [`StoreLocation::resolve`] reads.
///
/// Lifted into a value rather than read inside the resolver, so the whole
/// resolution order is testable without mutating the process environment —
/// which parallel tests inside one binary cannot do safely.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StoreVars {
    /// `$STORYHOOK_STORE_PATH` — the store file itself.
    pub store_path: Option<PathBuf>,
    /// `$STORYHOOK_DATA_DIR` — the directory the store file sits in.
    pub data_dir: Option<PathBuf>,
    /// `$XDG_DATA_HOME`, which the default location hangs off.
    pub xdg_data_home: Option<PathBuf>,
}

/// Which store this invocation is about, and how that was decided.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoreLocation {
    path: PathBuf,
    default_path: PathBuf,
    origin: StoreOrigin,
}

impl StoreLocation {
    /// Resolves the store from a flag, the environment, and the home directory.
    ///
    /// In order: `--store-path`, `$STORYHOOK_STORE_PATH`, `$STORYHOOK_DATA_DIR`
    /// plus `store.db`, then the default location. The first three name a store
    /// explicitly, and the difference between them is only which lever an error
    /// message should name.
    ///
    /// Every answer is canonicalized, including the default, so that two
    /// spellings of one path can never become two daemons on one SQLite file.
    pub fn resolve(flag: Option<&Path>, vars: &StoreVars, home: &Path) -> Result<Self, AppError> {
        let default_path = canonical_ish(&default_path_for(vars.xdg_data_home.as_deref(), home))?;
        let named = match (flag, vars.store_path.as_deref(), vars.data_dir.as_deref()) {
            (Some(path), _, _) => Some((path.to_path_buf(), StoreOrigin::Flag)),
            (None, Some(path), _) => Some((path.to_path_buf(), StoreOrigin::StorePathVar)),
            (None, None, Some(dir)) => Some((dir.join(STORE_FILE), StoreOrigin::DataDirVar)),
            (None, None, None) => None,
        };
        let (path, origin) = match named {
            Some((path, origin)) => (canonical_ish(&path)?, origin),
            None => (default_path.clone(), StoreOrigin::XdgDefault),
        };
        Ok(StoreLocation {
            path,
            default_path,
            origin,
        })
    }

    /// The default store for `home`, for [`Environment::at`](super::Environment::at).
    ///
    /// Infallible, unlike [`Self::resolve`]: the path it builds is absolute and
    /// free of `..`, so the only way [`canonical_ish`] could fail is if every
    /// ancestor including the filesystem root refused to canonicalize. If that
    /// ever happens the literal path is the best answer available, and the
    /// fixture asking for it is about to fail for a better reason.
    pub fn for_home(home: &Path) -> Self {
        let literal = default_path_for(None, home);
        let path = canonical_ish(&literal).unwrap_or(literal);
        StoreLocation {
            default_path: path.clone(),
            path,
            origin: StoreOrigin::XdgDefault,
        }
    }

    /// The store's database file, canonicalized.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Where the store would have been had nothing named one.
    pub fn default_path(&self) -> &Path {
        &self.default_path
    }

    /// Which lever chose this store.
    pub fn origin(&self) -> StoreOrigin {
        self.origin
    }

    /// Whether this is the store a machine uses when nothing names one.
    ///
    /// Not the same question as "did anything name it": pointing
    /// `$STORYHOOK_DATA_DIR` at the place the store already lives selects the
    /// default store, and it should keep the default store's port and the
    /// default store's backups.
    pub fn is_default(&self) -> bool {
        self.path == self.default_path
    }

    /// The name of this store's state directory.
    ///
    /// A digest rather than an escaped path, because a directory name must not
    /// depend on the path's length or on characters a filesystem may refuse.
    pub fn key(&self) -> String {
        let digest = Sha256::digest(self.path.as_os_str().as_encoded_bytes());
        digest
            .iter()
            .flat_map(|byte| [HEX[(byte >> 4) as usize], HEX[(byte & 0xf) as usize]])
            .take(KEY_HEX)
            .collect()
    }
}

/// Lowercase hex digits, indexed by nibble.
const HEX: [char; 16] = [
    '0', '1', '2', '3', '4', '5', '6', '7', '8', '9', 'a', 'b', 'c', 'd', 'e', 'f',
];

/// Where the store lives when nothing names one, canonicalized.
///
/// Split out from [`StoreLocation::resolve`] because `story store new` needs
/// this one path in order to refuse it, and must not resolve — let alone
/// create — a whole [`Environment`](super::Environment) to find it out.
pub(super) fn default_store_path_for(
    xdg_data_home: Option<&Path>,
    home: &Path,
) -> Result<PathBuf, AppError> {
    canonical_ish(&default_path_for(xdg_data_home, home))
}

/// Where the store lives when nothing names one.
fn default_path_for(xdg_data_home: Option<&Path>, home: &Path) -> PathBuf {
    xdg_data_home
        .map(|xdg| xdg.join("storyhook"))
        .unwrap_or_else(|| home.join(".local/share/storyhook"))
        .join(STORE_FILE)
}

/// Canonicalizes a path that does not exist yet.
///
/// **This is load-bearing.** Two spellings of one path must not produce two
/// daemons on one SQLite file, and the spellings a user actually types are a
/// symlinked directory, a `..`, a trailing slash and a relative path.
/// [`std::fs::canonicalize`] answers all of them and requires the file to
/// exist — which it does not before `story store new`, and does not on a
/// machine whose store has never been created.
///
/// So: canonicalize the deepest ancestor that *does* exist and rejoin the rest.
/// A `..` left in the rejoined remainder is refused rather than resolved
/// lexically, because a lexical answer would be wrong exactly when it mattered
/// — `a/b/../c` is `a/c` only if `a/b` is not a symlink, and the remainder is
/// the part of the path we could not look at.
///
/// **The answer must not change when the store file appears**, which is the
/// invariant `the_same_path_resolves_the_same_before_and_after_it_exists` holds
/// this function to. A store is resolved once before it exists and forever
/// after, and a path that keys one way on Monday and another on Tuesday is two
/// daemons on one database — the exact failure this module exists to prevent,
/// re-entering through the mechanism itself.
pub fn canonical_ish(path: &Path) -> Result<PathBuf, AppError> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|e| {
                AppError::Storage(format!(
                    "failed to resolve `{}` against the current directory: {e}",
                    path.display()
                ))
            })?
            .join(path)
    };

    for ancestor in absolute.ancestors() {
        let Ok(base) = std::fs::canonicalize(ancestor) else {
            continue;
        };
        let remainder = absolute
            .strip_prefix(ancestor)
            .expect("an ancestor is a prefix of its path");
        // The whole path existed, so `base` is already the final answer.
        // Returning `base.join("")` instead would append a separator, and
        // `/store.db/` is both a different string and a path `exists()` answers
        // `false` for — which is how one store became two daemons the moment its
        // file was created.
        if remainder.as_os_str().is_empty() {
            return Ok(base);
        }
        if remainder
            .components()
            .any(|component| component == Component::ParentDir)
        {
            return Err(AppError::Usage(format!(
                "`{}` cannot be resolved: it walks up through `{}`, which does not exist, so \
                 there is no way to know what `..` refers to. Name the store with a path that \
                 exists, or create the directory first.",
                path.display(),
                remainder.display(),
            )));
        }
        return Ok(base.join(remainder));
    }

    Err(AppError::Storage(format!(
        "failed to resolve `{}`: no part of it could be read",
        path.display()
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix("storyhook-store-location-")
            .tempdir_in("/private/tmp")
            .expect("a scratch directory")
    }

    fn vars(dir: Option<&Path>) -> StoreVars {
        StoreVars {
            data_dir: dir.map(Path::to_path_buf),
            ..StoreVars::default()
        }
    }

    #[test]
    fn nothing_named_means_the_xdg_default() {
        let dir = scratch();
        let home = dir.path();
        let located = StoreLocation::resolve(None, &StoreVars::default(), home).expect("resolving");
        assert_eq!(located.origin(), StoreOrigin::XdgDefault);
        assert!(located.is_default());
        assert!(located.path().ends_with(".local/share/storyhook/store.db"));
    }

    #[test]
    fn xdg_data_home_moves_the_default() {
        let dir = scratch();
        let xdg = dir.path().join("xdg");
        std::fs::create_dir_all(&xdg).unwrap();
        let located = StoreLocation::resolve(
            None,
            &StoreVars {
                xdg_data_home: Some(xdg.clone()),
                ..StoreVars::default()
            },
            dir.path(),
        )
        .expect("resolving");
        assert!(located.is_default());
        assert!(
            located
                .path()
                .starts_with(std::fs::canonicalize(&xdg).unwrap())
        );
    }

    /// The order is the contract, and each level has to beat the one below it.
    #[test]
    fn the_flag_outranks_the_variables_and_both_outrank_the_data_dir() {
        let dir = scratch();
        let flag = dir.path().join("by-flag.db");
        let by_var = dir.path().join("by-var.db");
        let data_dir = dir.path().join("data");
        let all = StoreVars {
            store_path: Some(by_var.clone()),
            data_dir: Some(data_dir.clone()),
            xdg_data_home: None,
        };

        let with_flag = StoreLocation::resolve(Some(&flag), &all, dir.path()).expect("resolving");
        assert_eq!(with_flag.origin(), StoreOrigin::Flag);
        assert!(with_flag.path().ends_with("by-flag.db"));

        let without_flag = StoreLocation::resolve(None, &all, dir.path()).expect("resolving");
        assert_eq!(without_flag.origin(), StoreOrigin::StorePathVar);
        assert!(without_flag.path().ends_with("by-var.db"));

        let data_only =
            StoreLocation::resolve(None, &vars(Some(&data_dir)), dir.path()).expect("resolving");
        assert_eq!(data_only.origin(), StoreOrigin::DataDirVar);
        assert!(data_only.path().ends_with("data/store.db"));
    }

    /// A named store that happens to *be* the default one is still the default
    /// one: it keeps the stable port and the existing backups.
    #[test]
    fn naming_the_default_location_still_counts_as_the_default_store() {
        let dir = scratch();
        let data_dir = dir.path().join(".local/share/storyhook");
        std::fs::create_dir_all(&data_dir).unwrap();
        let located =
            StoreLocation::resolve(None, &vars(Some(&data_dir)), dir.path()).expect("resolving");
        assert_eq!(located.origin(), StoreOrigin::DataDirVar);
        assert!(
            located.is_default(),
            "{} should be the default store",
            located.path().display()
        );
    }

    /// The property the whole mechanism rests on: one store, one key.
    #[test]
    fn every_spelling_of_one_path_produces_one_key() {
        let dir = scratch();
        let stores = dir.path().join("stores");
        std::fs::create_dir_all(&stores).unwrap();
        let link = dir.path().join("link");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&stores, &link).expect("symlinking");

        let plain = stores.join("one.db");
        let spellings = [
            plain.clone(),
            stores.join(".").join("..").join("stores").join("one.db"),
            link.join("one.db"),
            PathBuf::from(format!("{}/", stores.display())).join("one.db"),
        ];

        let expected = StoreLocation::resolve(Some(&plain), &StoreVars::default(), dir.path())
            .expect("resolving")
            .key();
        for spelling in spellings {
            let located =
                StoreLocation::resolve(Some(&spelling), &StoreVars::default(), dir.path())
                    .expect("resolving");
            assert_eq!(
                located.key(),
                expected,
                "`{}` is the same store and must key the same",
                spelling.display()
            );
        }
    }

    #[test]
    fn two_stores_do_not_share_a_key() {
        let dir = scratch();
        let one = StoreLocation::resolve(
            Some(&dir.path().join("one.db")),
            &StoreVars::default(),
            dir.path(),
        )
        .expect("resolving");
        let two = StoreLocation::resolve(
            Some(&dir.path().join("two.db")),
            &StoreVars::default(),
            dir.path(),
        )
        .expect("resolving");
        assert_ne!(one.key(), two.key());
    }

    #[test]
    fn a_key_is_sixteen_lowercase_hex_characters() {
        let dir = scratch();
        let located = StoreLocation::for_home(dir.path());
        let key = located.key();
        assert_eq!(key.len(), KEY_HEX);
        assert!(
            key.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase())
        );
    }

    /// The digest is a wire format in the sense that matters: a daemon started
    /// by yesterday's build and a client from today's must agree on where the
    /// portfile is.
    #[test]
    fn the_key_is_the_first_sixteen_hex_of_the_paths_sha256() {
        let located = StoreLocation::resolve(
            Some(Path::new("/private/tmp")),
            &StoreVars::default(),
            Path::new("/private/tmp"),
        )
        .expect("resolving");
        let expected: String = format!("{:x}", Sha256::digest(b"/private/tmp"))
            .chars()
            .take(KEY_HEX)
            .collect();
        assert_eq!(located.key(), expected);
    }

    #[test]
    fn a_relative_store_path_is_resolved_against_the_working_directory() {
        let located = StoreLocation::resolve(
            Some(Path::new("relative-store.db")),
            &StoreVars::default(),
            Path::new("/private/tmp"),
        )
        .expect("resolving");
        assert!(located.path().is_absolute());
        assert_eq!(
            located.path().parent(),
            Some(
                std::fs::canonicalize(std::env::current_dir().unwrap())
                    .unwrap()
                    .as_path()
            )
        );
    }

    /// Resolving `..` lexically is wrong exactly when it matters, so it is
    /// refused instead — loudly, and naming what to do about it.
    #[test]
    fn a_dot_dot_below_a_directory_that_does_not_exist_is_refused() {
        let dir = scratch();
        let path = dir.path().join("nope").join("..").join("store.db");
        let error = StoreLocation::resolve(Some(&path), &StoreVars::default(), dir.path())
            .expect_err("a `..` under a missing directory has no answer");
        let rendered = error.to_string();
        assert!(rendered.contains(".."), "{rendered}");
    }

    /// A store is resolved once before its file exists and forever after. If
    /// the two answers differ by so much as a separator they are two keys, two
    /// state directories and two daemons on one database — which is the failure
    /// the whole module exists to prevent, arriving through the mechanism
    /// itself. It got in once: `Path::join("")` appends a separator.
    #[test]
    fn the_same_path_resolves_the_same_before_and_after_it_exists() {
        let dir = scratch();
        let path = dir.path().join("appears.db");

        let before = StoreLocation::resolve(Some(&path), &StoreVars::default(), dir.path())
            .expect("resolving before it exists");
        std::fs::write(&path, b"not really a database").expect("creating the file");
        let after = StoreLocation::resolve(Some(&path), &StoreVars::default(), dir.path())
            .expect("resolving after it exists");

        assert_eq!(before.path(), after.path());
        assert_eq!(before.key(), after.key());
        assert!(
            after.path().exists(),
            "{} must still name the file it resolved",
            after.path().display()
        );
    }

    /// Same invariant, for the directory-shaped lever: `$STORYHOOK_DATA_DIR`
    /// exists long before `store.db` inside it does.
    #[test]
    fn a_data_dir_resolves_the_same_before_and_after_its_store_exists() {
        let dir = scratch();
        let data = dir.path().join("data");
        std::fs::create_dir_all(&data).unwrap();

        let before =
            StoreLocation::resolve(None, &vars(Some(&data)), dir.path()).expect("resolving");
        std::fs::write(data.join("store.db"), b"not really a database").unwrap();
        let after =
            StoreLocation::resolve(None, &vars(Some(&data)), dir.path()).expect("resolving");

        assert_eq!(before.path(), after.path());
        assert_eq!(before.key(), after.key());
    }

    #[test]
    fn a_store_that_does_not_exist_yet_still_resolves() {
        let dir = scratch();
        let path = dir.path().join("not").join("made").join("yet.db");
        let located = StoreLocation::resolve(Some(&path), &StoreVars::default(), dir.path())
            .expect("a store that has not been created yet must still have a path");
        assert!(located.path().ends_with("not/made/yet.db"));
        assert!(located.path().is_absolute());
    }
}
