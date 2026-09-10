//! The merge-gate command a project's committed pointer names (SH-649).
//!
//! `.storyhook.toml` may carry
//!
//! ```toml
//! [verify]
//! gate = "make test"
//! ```
//!
//! and the verifier runs that command, in the speculative merge checkout, as
//! the merge gate. Absent — no pointer, no table, no key — the gate is
//! [`GateCommand::DEFAULT`], and that constant is the **only** place the
//! default lives: `scripts/verify-pr.sh` requires the argv on its command line
//! rather than carrying a second copy (the SH-136 rule).
//!
//! # Why an argv and not a shell string
//!
//! Every hop from the daemon to the process that runs the gate — `verify-pr.sh`,
//! `machine-lock.sh`, `merge-watch.sh --speculative-run … -- "$@"` — passes the
//! command as argv words and finally `exec`s them with no shell in between. A
//! value such as `make test && echo ok` would therefore reach `make` as the
//! literal arguments `&&`, `echo` and `ok`: never a syntax error, never what
//! its author meant. So the rule is a positive allowlist over the characters a
//! command and its arguments are made of, and a value outside it is refused
//! **by name** — the offending character and the word it sits in — rather
//! than quietly running something else (SH-357, one layer over: an argument
//! that lands nowhere is refused, not dropped).
//!
//! # Why the reader never fails open
//!
//! `pointer_hooks` and `pointer_plugin` (`super::project`) fail open on
//! purpose: a hook nobody can read is a hook that does not fire. A gate is the
//! opposite case. A verifier that ran the default over a typo would certify a
//! merge tree against a gate nobody chose, so an unreadable pointer surfaces
//! `read_pointer`'s own error, and an unknown key under `[verify]` — the SH-357
//! shape for a config key — is refused rather than silently defaulted. The
//! table stays `toml::Value` on [`super::project::ProjectPointer`] so that this
//! strictness is the verifier's alone: a typo here must never make the
//! repository unresolvable for `story list` (`ProjectPointer`'s own doc).

use std::path::Path;

use serde::Deserialize;

use crate::error::AppError;

/// The `[verify]` table, typed at the point of use — the `[github]` shape
/// (`super::pr_link`). `deny_unknown_fields` is what turns `gaet = "…"` into a
/// refusal instead of the default.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct VerifyTable {
    /// The merge-gate command, a plain argv in one string.
    gate: Option<String>,
}

/// A merge-gate command: an argv, parsed from one pointer string.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GateCommand {
    argv: Vec<String>,
}

/// Characters a plain-argv word may contain, beyond ASCII alphanumerics.
///
/// Every one of these is inert to `sh`: no expansion, no redirection, no
/// control operator. `-` and `=` carry flags and `VAR=value` assignments,
/// `.`/`/` carry paths, `:` carries `npm run test:ci`-style script names, `@`
/// and `+` appear in package and version specifiers, `,` in list arguments.
const WORD_PUNCTUATION: &[char] = &['_', '.', ':', '/', '=', '@', '+', ',', '-'];

impl GateCommand {
    /// What a pointer with no `[verify] gate` means.
    pub const DEFAULT: &'static str = "make test";

    /// Parses one pointer string under the plain-argv rule.
    ///
    /// Words are separated by one or more ASCII spaces; every character of
    /// every word is an ASCII alphanumeric or one of [`WORD_PUNCTUATION`]; the
    /// first word is a command, so it may not start with `-`. The `Err` names
    /// what was refused and why, and is meant to be shown to the person who
    /// wrote the value.
    pub fn parse(value: &str) -> Result<Self, String> {
        // Only the space separates words. Every other whitespace character
        // is refused below like any other character outside the allowlist,
        // so a tab or newline cannot split a word the author did not split.
        let words: Vec<&str> = value.split(' ').filter(|word| !word.is_empty()).collect();
        let Some(first) = words.first() else {
            return Err(format!(
                "`{value}` names no command; name a command and its arguments, \
                 for example `{}`",
                Self::DEFAULT
            ));
        };
        for word in &words {
            if let Some(offender) = word
                .chars()
                .find(|c| !(c.is_ascii_alphanumeric() || WORD_PUNCTUATION.contains(c)))
            {
                return Err(format!(
                    "`{value}` is not a plain argv: `{offender}` in `{word}` is not a \
                     character a command or argument is made of. Name a command and its \
                     arguments (`{}`, `make test-full`); storyhook runs it directly, \
                     never through a shell, so no quoting, expansion, redirection or \
                     chaining is possible.",
                    Self::DEFAULT
                ));
            }
        }
        if first.starts_with('-') {
            return Err(format!(
                "`{value}` starts with the flag `{first}`; the first word must be a command"
            ));
        }
        Ok(Self {
            argv: words.iter().map(|word| (*word).to_string()).collect(),
        })
    }

    /// The command and its arguments, one element per word.
    #[must_use]
    pub fn argv(&self) -> &[String] {
        &self.argv
    }

    /// The command as one line — the words joined by single spaces — for a
    /// comment or a diagnostic. Round-trips through [`Self::parse`].
    #[must_use]
    pub fn display(&self) -> String {
        self.argv.join(" ")
    }
}

/// The merge gate for the project whose pointer lives at `checkout`.
///
/// Reads `<checkout>/.storyhook.toml` and nothing above it: the caller hands
/// over a registered checkout, which is a repository top level. No pointer,
/// no `[verify]` table or no `gate` key is [`GateCommand::DEFAULT`]. Anything
/// else that is not a plain argv is refused naming `[verify].gate`, the file
/// and the value; a pointer that cannot be read at all propagates
/// `read_pointer`'s error, which names the file.
pub fn gate_command_for(checkout: &Path) -> Result<GateCommand, AppError> {
    let path = super::project::pointer_path(checkout);
    let Some(pointer) = super::project::read_pointer(checkout)? else {
        return Ok(default_gate());
    };
    let Some(table) = pointer.verify else {
        return Ok(default_gate());
    };
    let table: VerifyTable = table.try_into().map_err(|error| {
        AppError::Validation(format!(
            "the [verify] table in {} failed to parse: {error}. It takes one key, \
             `[verify].gate`, a plain command line such as `{}`.",
            path.display(),
            GateCommand::DEFAULT
        ))
    })?;
    let Some(gate) = table.gate else {
        return Ok(default_gate());
    };
    GateCommand::parse(&gate).map_err(|reason| {
        AppError::Validation(format!(
            "[verify].gate in {} refuses to load: {reason}",
            path.display()
        ))
    })
}

/// [`GateCommand::DEFAULT`], parsed. The constant is a plain argv by
/// construction, so this cannot fail; the `expect` is the assertion that it
/// stays one.
fn default_gate() -> GateCommand {
    GateCommand::parse(GateCommand::DEFAULT).expect("GateCommand::DEFAULT is a plain argv")
}
