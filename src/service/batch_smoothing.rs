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

use serde::Deserialize;

use crate::domain::conflict_smoothing::SmoothPolicy;

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
}
