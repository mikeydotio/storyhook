//! The story-id prefix: one validator, one derivation, no second opinion.
//!
//! # Why this exists
//!
//! Until SH-117 **nothing validated a prefix anywhere**. Three sites read one
//! and applied [`DEFAULT_PREFIX`](crate::service::project::DEFAULT_PREFIX)
//! through an `unwrap_or_else`, so `story project init --prefix 'hello world'`
//! was accepted and minted `hello world-1` as a story id — an id
//! [`looks_like_story_id`](crate::cli) cannot recognize, in a project whose ids
//! can never be typed back at the CLI that made them.
//!
//! A prefix is the one field in a project that is **not reversible**. A name is
//! a display string with `rename_project` behind it; a prefix is baked into
//! every id ever minted and into every commit message, comment and document
//! that ever quoted one. So it is checked before it is stored, and it is checked
//! in exactly one place — the CLI parser, the questionnaire and
//! `POST /api/repos` all call [`validate`], because a browser and a terminal
//! that disagree about what a prefix is would disagree about what a project is.
//!
//! # The grammar, and where it comes from
//!
//! `^[A-Z][A-Z0-9]{0,9}$`, after uppercasing. The shape is not arbitrary: the
//! CLI recognizes a story id by splitting on the first `-` and requiring the
//! left half to be ASCII alphanumeric (`looks_like_story_id`, `src/cli.rs`), so
//! a prefix containing a hyphen, a space or any non-ASCII character produces
//! ids that parse as something else or as nothing at all. Leading with a letter
//! keeps `12-3` from being minted, which reads as a range rather than an id.
//!
//! # Derivation is a suggestion, never a silent default
//!
//! [`derive`] exists to fill the questionnaire's bracketed default and the
//! dashboard's pre-filled input. It is never applied on the user's behalf
//! without being shown — that silent `SH` is exactly what SH-109 was filed
//! about. Its one guarantee is that whatever it returns passes [`validate`], so
//! a caller may offer it without checking.

use crate::error::AppError;
use crate::service::project::DEFAULT_PREFIX;

/// The longest prefix a project may carry.
const MAX_LEN: usize = 10;

/// The most name-parts [`derive`] will take an initial from.
const MAX_INITIALS: usize = 4;

/// The most characters [`derive`] takes from a single-part name.
const SINGLE_PART_LEN: usize = 3;

/// Checks a user-supplied prefix and returns its canonical (uppercased) form.
///
/// The one gate every prefix passes through, whoever supplied it. Accepts
/// `^[A-Z][A-Z0-9]{0,9}$` after ASCII-uppercasing, so `sh` and `SH` are the same
/// answer and `hello world` is not an answer at all.
///
/// # Errors
///
/// [`AppError::Validation`] — exit 2 — naming the offending value and the rule.
pub fn validate(raw: &str) -> Result<String, AppError> {
    let canonical = raw.to_ascii_uppercase();
    let refuse = |because: &str| {
        Err(AppError::Validation(format!(
            "invalid story-id prefix `{raw}`: {because}. A prefix is 1–{MAX_LEN} characters, \
             starts with a letter and holds only letters and digits — it becomes the first \
             half of every story id this project ever mints, as in `SH-1`."
        )))
    };

    let mut chars = canonical.chars();
    let Some(first) = chars.next() else {
        return refuse("it is empty");
    };
    if !first.is_ascii_alphabetic() {
        return refuse("it does not start with a letter");
    }
    if canonical.chars().count() > MAX_LEN {
        return refuse("it is too long");
    }
    if let Some(bad) = chars.find(|c| !c.is_ascii_alphanumeric()) {
        return refuse(&format!("`{bad}` is neither a letter nor a digit"));
    }
    Ok(canonical)
}

/// Suggests a prefix from a directory's basename.
///
/// Two or more parts take an initial from each of the first
/// [`MAX_INITIALS`](self) (`my-cool-app` → `MCA`); one part takes its first
/// [`SINGLE_PART_LEN`](self) alphanumerics (`storyhook` → `STO`). A basename
/// with nothing usable in it — or one whose usable characters produce something
/// [`validate`] would refuse, such as a leading digit — falls back to
/// [`DEFAULT_PREFIX`].
///
/// **The return value always passes [`validate`].** That is the contract that
/// lets a caller print it as a default without checking it first.
#[must_use]
pub fn derive(basename: &str) -> String {
    let parts: Vec<&str> = basename
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|part| !part.is_empty())
        .collect();

    let candidate: String = match parts.as_slice() {
        [] => String::new(),
        [single] => single.chars().take(SINGLE_PART_LEN).collect(),
        many => many
            .iter()
            .take(MAX_INITIALS)
            .filter_map(|part| part.chars().next())
            .collect(),
    };

    // Validated rather than assumed: `123-456` splits into two usable parts
    // whose initials are digits, which is a legal basename and an illegal
    // prefix. Asking the gate is what keeps the contract above true for inputs
    // nobody enumerated.
    validate(&candidate).unwrap_or_else(|_| DEFAULT_PREFIX.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_prefix_survives_unchanged() {
        assert_eq!(validate("SH").unwrap(), "SH");
        assert_eq!(validate("API").unwrap(), "API");
    }

    #[test]
    fn case_is_folded_up_rather_than_refused() {
        assert_eq!(validate("sh").unwrap(), "SH");
        assert_eq!(validate("Api").unwrap(), "API");
    }

    #[test]
    fn digits_are_legal_after_the_first_character() {
        assert_eq!(validate("A1").unwrap(), "A1");
        assert_eq!(validate("sh2").unwrap(), "SH2");
    }

    #[test]
    fn one_character_and_ten_characters_are_both_legal() {
        assert_eq!(validate("A").unwrap(), "A");
        assert_eq!(validate("ABCDEFGHIJ").unwrap(), "ABCDEFGHIJ");
    }

    #[test]
    fn eleven_characters_is_too_long() {
        let err = validate("ABCDEFGHIJK").unwrap_err();
        assert!(matches!(err, AppError::Validation(_)));
        assert!(err.to_string().contains("too long"), "{err}");
    }

    #[test]
    fn an_empty_prefix_is_refused() {
        let err = validate("").unwrap_err();
        assert!(err.to_string().contains("is empty"), "{err}");
    }

    #[test]
    fn whitespace_only_is_refused_rather_than_trimmed() {
        // Deliberately not trimmed: a caller who passed `" "` passed a value,
        // and silently turning it into a different one is how `--prefix` came
        // to accept `hello world` in the first place.
        assert!(validate("   ").is_err());
    }

    #[test]
    fn the_defect_this_module_was_filed_about_is_refused() {
        let err = validate("hello world").unwrap_err();
        assert!(matches!(err, AppError::Validation(_)));
        assert_eq!(err.exit_code(), 2);
        assert!(err.to_string().contains("hello world"), "{err}");
    }

    #[test]
    fn a_hyphen_is_refused_because_the_id_grammar_splits_on_it() {
        let err = validate("SH-X").unwrap_err();
        assert!(err.to_string().contains('-'), "{err}");
    }

    #[test]
    fn a_leading_digit_is_refused() {
        let err = validate("1SH").unwrap_err();
        assert!(err.to_string().contains("start with a letter"), "{err}");
    }

    #[test]
    fn non_ascii_is_refused_at_every_position() {
        assert!(validate("Ω").is_err());
        assert!(validate("SHΩ").is_err());
        assert!(validate("café").is_err());
    }

    #[test]
    fn punctuation_and_control_characters_are_refused() {
        for bad in ["S H", "S.H", "S/H", "S_H", "S\tH", "S\nH", "S\0H"] {
            assert!(validate(bad).is_err(), "accepted `{bad}`");
        }
    }

    #[test]
    fn several_parts_take_one_initial_each() {
        assert_eq!(derive("my-cool-app"), "MCA");
        assert_eq!(derive("story_hook"), "SH");
        assert_eq!(derive("open.grid.scad"), "OGS");
    }

    #[test]
    fn no_more_than_four_initials_are_taken() {
        assert_eq!(derive("a-b-c-d-e-f"), "ABCD");
    }

    #[test]
    fn a_single_part_takes_its_first_three_characters() {
        assert_eq!(derive("storyhook"), "STO");
        assert_eq!(derive("ab"), "AB");
        assert_eq!(derive("x"), "X");
    }

    #[test]
    fn a_basename_with_nothing_usable_falls_back_to_the_default() {
        assert_eq!(derive(""), DEFAULT_PREFIX);
        assert_eq!(derive("---"), DEFAULT_PREFIX);
        assert_eq!(derive("   "), DEFAULT_PREFIX);
        assert_eq!(derive("日本語"), DEFAULT_PREFIX);
    }

    #[test]
    fn a_derivation_that_would_not_validate_falls_back_rather_than_shipping() {
        // Both parts start with a digit, so the initials are `12` — a legal
        // basename and an illegal prefix.
        assert_eq!(derive("123-456"), DEFAULT_PREFIX);
        assert_eq!(derive("2024"), DEFAULT_PREFIX);
    }

    #[test]
    fn every_derivation_passes_the_validator() {
        // The module's one contract, asserted over the shapes a real directory
        // name takes rather than over the four examples above.
        for basename in [
            "",
            "-",
            "a",
            "ab",
            "abc",
            "abcdefghijklmnop",
            "a-b",
            "a-b-c-d-e-f-g-h",
            "123",
            "1-2-3",
            "9lives",
            "my-cool-app",
            "My_Cool_App",
            "日本語",
            "café-au-lait",
            ".hidden",
            "with space",
            "trailing-",
            "-leading",
            "dots.and-dashes_mixed",
        ] {
            let derived = derive(basename);
            assert!(
                validate(&derived).is_ok(),
                "derive({basename:?}) produced `{derived}`, which the validator refuses"
            );
        }
    }

    #[test]
    fn derivation_is_already_canonical() {
        // Not merely valid: equal to its own validated form, so a caller may
        // print it and store it without a round trip through `validate`.
        for basename in ["my-cool-app", "storyhook", "My_Cool_App", "CAFE"] {
            let derived = derive(basename);
            assert_eq!(validate(&derived).unwrap(), derived);
        }
    }
}
