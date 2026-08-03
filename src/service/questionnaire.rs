//! The `story project new` questionnaire — the interactive half of one verb.
//!
//! # The rule, stated once
//!
//! **Absence is the trigger.** A bare `story project new` at a terminal asks;
//! the same command with *any* verb-local switch on it never asks and fails
//! loudly on a missing required field. That is the one rule a script author and
//! a human can both predict without reading anything, and it is the rule
//! `main.rs::confirm()` already applies — so the program has one rule about
//! prompting rather than two.
//!
//! # Why this is a pure function over two streams
//!
//! The whole architecture rests on *the daemon never prompts*. So this takes
//! [`BufRead`] and [`Write`] rather than reaching for `std::io::stdin`, and
//! everything it needs to know about the world arrives in [`Surroundings`],
//! computed by the one process that has a terminal. Two consequences, both
//! deliberate:
//!
//! * every answer branch is unit-tested against the real function with
//!   `Cursor`s for streams — no mocks, no trait objects, no test-only code path;
//! * the file never names stdin, so it needs no exemption from the interactive
//!   seam test that `tests/invoker_seam.rs` enforces.
//!
//! # It can only produce invocations the flags can also produce
//!
//! Every answer maps to exactly one switch: `--name`, `--prefix`,
//! `--no-attach`, `--no-agents-md`. That is a constraint rather than a
//! coincidence — an interactive answer with no flag behind it would be an
//! operation a script could never perform, and the two forms would stop being
//! the same command.
//!
//! **This is why the origin is reported rather than asked about.** The council
//! sketched a sixth question offering to register the repository's origin, and
//! it cannot be built as specified: there is no `--no-origin` switch to carry a
//! "no", and the note it was to print — naming the project that already holds
//! the URL — needs a store read the client has not been able to make since
//! SH-114 collapsed the transports. So the origin appears in the summary, where
//! it tells the user what is about to be claimed, and
//! `story project link|unlink origin` remains the verb for changing it. Recorded
//! as a deviation on the story.

use std::io::{BufRead, Write};
use std::path::PathBuf;

use crate::cli::{Attach, NewProjectSpec};
use crate::domain::prefix;
use crate::error::AppError;

/// How many times an unusable answer is re-asked before the run is abandoned.
///
/// Bounded because this loop reads a stream: a terminal supplies a person, but
/// the same code reads a here-doc that repeats one wrong line forever.
const MAX_RETRIES: usize = 3;

/// What the client could see about the directory it ran in.
///
/// Gathered by the caller rather than read here, which is what keeps this
/// module a pure function over its two streams — and therefore what makes every
/// branch below reachable from a unit test.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Surroundings {
    /// The directory `--attach` would default to: the client's own.
    pub dir: PathBuf,
    /// The origin git reports there, as the user would recognize it, if any.
    pub origin: Option<String>,
    /// Whether that directory already has an `AGENTS.md`.
    ///
    /// When it does, the question is not asked at all: `init` has never
    /// overwritten one, so the only honest answer is the one nobody was given a
    /// choice about.
    pub agents_md_exists: bool,
}

impl Surroundings {
    /// The basename the display name defaults to.
    fn basename(&self) -> String {
        self.dir
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.dir.display().to_string())
    }
}

/// The outcome of asking.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Answered {
    /// Everything the verb needs, exactly as a switch-bearing command line
    /// would have supplied it.
    Create(Box<NewProjectSpec>),
    /// The user said no, or the stream ended. **Nothing has been written**, and
    /// this is not an error: a cancellation is a decision, and exiting non-zero
    /// on one makes `story project new || echo failed` lie.
    Cancelled,
}

/// Asks the six-part question and returns what to create.
///
/// Prompts are written to `out` — stderr in production, so that a caller who
/// redirects stdout still sees them — and answers are read from `input`. An
/// empty answer takes the bracketed default.
///
/// # Errors
///
/// [`AppError::Validation`] when an answer cannot be used after
/// [`MAX_RETRIES`](self) attempts, or when a stream fails. A stream that simply
/// *ends* is a cancellation rather than an error.
pub fn ask(
    surroundings: &Surroundings,
    input: &mut impl BufRead,
    out: &mut impl Write,
) -> Result<Answered, AppError> {
    let mut session = Session { input, out };

    let default_name = surroundings.basename();
    let Some(name) = session.line(&format!("Project name [{default_name}]: "))? else {
        return session.cancel();
    };
    let name = if name.is_empty() { default_name } else { name };

    let Some(prefix) = session.prefix(&prefix::derive(&name))? else {
        return session.cancel();
    };

    let Some(attach) = session.yes_no(
        &format!("Attach this checkout ({})?", surroundings.dir.display()),
        true,
    )?
    else {
        return session.cancel();
    };

    // Never asked when the file is already there: `init` has never overwritten
    // one, so offering the choice would be offering something that cannot
    // happen.
    let agents_md = if attach && !surroundings.agents_md_exists {
        match session.yes_no("Generate AGENTS.md?", true)? {
            Some(answer) => answer,
            None => return session.cancel(),
        }
    } else {
        false
    };

    let spec = NewProjectSpec {
        attach: if attach { Attach::Cwd } else { Attach::Nothing },
        prefix,
        name: Some(name),
        no_agents_md: !agents_md,
    };

    session.summary(&spec, surroundings, attach)?;
    match session.yes_no("Create this project?", true)? {
        Some(true) => Ok(Answered::Create(Box::new(spec))),
        Some(false) | None => session.cancel(),
    }
}

/// The two streams, and the small vocabulary of things asked over them.
struct Session<'a, R: BufRead, W: Write> {
    input: &'a mut R,
    out: &'a mut W,
}

impl<R: BufRead, W: Write> Session<'_, R, W> {
    /// Prints `prompt` and reads one line; `None` means the stream ended.
    fn line(&mut self, prompt: &str) -> Result<Option<String>, AppError> {
        write!(self.out, "{prompt}").map_err(write_failed)?;
        self.out.flush().map_err(write_failed)?;
        let mut answer = String::new();
        let read = self
            .input
            .read_line(&mut answer)
            .map_err(|e| AppError::Validation(format!("failed to read an answer: {e}")))?;
        if read == 0 {
            return Ok(None);
        }
        Ok(Some(answer.trim().to_string()))
    }

    /// The prefix question, which is the only one with a validator behind it.
    ///
    /// Re-asked rather than rejected outright, because a prefix is the one
    /// answer here that cannot be changed afterwards and a typo caught now is
    /// free.
    fn prefix(&mut self, default: &str) -> Result<Option<String>, AppError> {
        let prompt = format!("Story-id prefix [{default}]: ");
        let mut last = String::new();
        for _ in 0..=MAX_RETRIES {
            let Some(answer) = self.line(&prompt)? else {
                return Ok(None);
            };
            let candidate = if answer.is_empty() {
                default.to_string()
            } else {
                answer
            };
            match prefix::validate(&candidate) {
                Ok(prefix) => return Ok(Some(prefix)),
                Err(error) => {
                    writeln!(self.out, "{error}").map_err(write_failed)?;
                    last = candidate;
                }
            }
        }
        Err(AppError::Validation(format!(
            "giving up after {MAX_RETRIES} more tries; `{last}` is still not a usable story-id \
             prefix. Nothing was created."
        )))
    }

    /// A `[Y/n]` question. `None` means the stream ended.
    fn yes_no(&mut self, question: &str, default: bool) -> Result<Option<bool>, AppError> {
        let prompt = format!("{question} [{}] ", if default { "Y/n" } else { "y/N" });
        for _ in 0..=MAX_RETRIES {
            let Some(answer) = self.line(&prompt)? else {
                return Ok(None);
            };
            match answer.to_ascii_lowercase().as_str() {
                "" => return Ok(Some(default)),
                "y" | "yes" => return Ok(Some(true)),
                "n" | "no" => return Ok(Some(false)),
                _ => writeln!(self.out, "Please answer y or n.").map_err(write_failed)?,
            }
        }
        Err(AppError::Validation(format!(
            "giving up after {MAX_RETRIES} more tries; `{question}` needs a yes or a no. Nothing \
             was created."
        )))
    }

    /// What is about to happen, before the last question rather than after it.
    ///
    /// The origin line is here rather than in a question of its own — see the
    /// module header. It says what is about to be claimed, which is the part a
    /// user cannot reconstruct.
    fn summary(
        &mut self,
        spec: &NewProjectSpec,
        surroundings: &Surroundings,
        attach: bool,
    ) -> Result<(), AppError> {
        writeln!(self.out).map_err(write_failed)?;
        writeln!(
            self.out,
            "  name       {}",
            spec.name.as_deref().unwrap_or("—")
        )
        .map_err(write_failed)?;
        writeln!(
            self.out,
            "  prefix     {} (as in {}-1)",
            spec.prefix, spec.prefix
        )
        .map_err(write_failed)?;
        if attach {
            writeln!(self.out, "  checkout   {}", surroundings.dir.display())
                .map_err(write_failed)?;
            if let Some(origin) = &surroundings.origin {
                writeln!(self.out, "  origin     {origin}").map_err(write_failed)?;
            }
            if !spec.no_agents_md {
                writeln!(self.out, "  generates  AGENTS.md").map_err(write_failed)?;
            }
        } else {
            writeln!(
                self.out,
                "  checkout   none — reachable only by `--project <slug>`"
            )
            .map_err(write_failed)?;
        }
        writeln!(self.out).map_err(write_failed)?;
        Ok(())
    }

    /// The one cancellation message, so both routes to it say the same thing.
    fn cancel(&mut self) -> Result<Answered, AppError> {
        writeln!(self.out, "cancelled; nothing was created").map_err(write_failed)?;
        Ok(Answered::Cancelled)
    }
}

fn write_failed(error: std::io::Error) -> AppError {
    AppError::Validation(format!("failed to write a prompt: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn surroundings() -> Surroundings {
        Surroundings {
            dir: PathBuf::from("/work/my-cool-app"),
            origin: None,
            agents_md_exists: false,
        }
    }

    /// Runs the real function over fake streams. Zero mocks: the only thing
    /// substituted is where the bytes come from.
    fn run(answers: &str, surroundings: &Surroundings) -> (Result<Answered, AppError>, String) {
        let mut input = Cursor::new(answers.to_string().into_bytes());
        let mut out: Vec<u8> = Vec::new();
        let outcome = ask(surroundings, &mut input, &mut out);
        (outcome, String::from_utf8(out).expect("prompts are utf-8"))
    }

    fn created(answers: &str, surroundings: &Surroundings) -> NewProjectSpec {
        match run(answers, surroundings).0 {
            Ok(Answered::Create(spec)) => *spec,
            other => panic!("expected a creation, got {other:?}"),
        }
    }

    #[test]
    fn every_default_taken_produces_the_directorys_own_name_and_a_derived_prefix() {
        let spec = created("\n\n\n\n\n", &surroundings());
        assert_eq!(spec.name.as_deref(), Some("my-cool-app"));
        assert_eq!(spec.prefix, "MCA");
        assert_eq!(spec.attach, Attach::Cwd);
        assert!(!spec.no_agents_md);
    }

    #[test]
    fn the_prefix_default_is_derived_from_the_answered_name_not_the_directory() {
        // The name is asked first for exactly this reason: somebody who renames
        // the project should be offered a prefix that matches what they typed.
        let spec = created("payments-api\n\n\n\n\n", &surroundings());
        assert_eq!(spec.name.as_deref(), Some("payments-api"));
        assert_eq!(spec.prefix, "PA");
    }

    #[test]
    fn each_answer_overrides_its_own_default() {
        let spec = created("Widgets\nWID\ny\nn\ny\n", &surroundings());
        assert_eq!(spec.name.as_deref(), Some("Widgets"));
        assert_eq!(spec.prefix, "WID");
        assert_eq!(spec.attach, Attach::Cwd);
        assert!(spec.no_agents_md, "`n` at the AGENTS.md question");
    }

    #[test]
    fn a_lowercase_prefix_is_canonicalized_rather_than_re_asked() {
        let spec = created("\nwid\n\n\n\n", &surroundings());
        assert_eq!(spec.prefix, "WID");
    }

    #[test]
    fn declining_the_checkout_produces_the_no_attach_shape() {
        let spec = created("\n\nn\n\n", &surroundings());
        assert_eq!(spec.attach, Attach::Nothing);
        assert!(
            spec.no_agents_md,
            "there is no repository to generate a file in"
        );
    }

    #[test]
    fn the_agents_md_question_is_skipped_when_the_file_is_already_there() {
        let mut surroundings = surroundings();
        surroundings.agents_md_exists = true;
        let (outcome, prompts) = run("\n\n\n\n", &surroundings);
        assert!(matches!(outcome, Ok(Answered::Create(_))), "{outcome:?}");
        assert!(
            !prompts.contains("Generate AGENTS.md"),
            "an unanswerable question must not be asked:\n{prompts}"
        );
    }

    /// The origin is reported, not asked about — see the module header. What
    /// matters is that the user is told what is about to be claimed.
    #[test]
    fn the_summary_names_the_origin_that_is_about_to_be_claimed() {
        let mut surroundings = surroundings();
        surroundings.origin = Some("git@github.com:acme/widgets.git".to_string());
        let (_, prompts) = run("\n\n\n\n\n", &surroundings);
        assert!(
            prompts.contains("git@github.com:acme/widgets.git"),
            "{prompts}"
        );
        assert!(
            !prompts.contains("Register origin"),
            "there is no --no-origin switch, so this must not be a question:\n{prompts}"
        );
    }

    #[test]
    fn the_summary_precedes_the_last_question_rather_than_following_it() {
        let (_, prompts) = run("\n\n\n\n\n", &surroundings());
        let summary = prompts.find("prefix     MCA").expect("a summary");
        let confirm = prompts.find("Create this project?").expect("the question");
        assert!(summary < confirm, "{prompts}");
    }

    #[test]
    fn no_at_the_last_question_cancels_and_says_so() {
        let (outcome, prompts) = run("\n\n\n\nn\n", &surroundings());
        assert_eq!(outcome.unwrap(), Answered::Cancelled);
        assert!(
            prompts.contains("cancelled; nothing was created"),
            "{prompts}"
        );
    }

    #[test]
    fn an_invalid_prefix_is_re_asked_exactly_three_times_then_abandoned() {
        let (outcome, prompts) = run(
            "\nhello world\nhello world\nhello world\nhello world\n",
            &surroundings(),
        );
        let error = outcome.expect_err("four unusable answers must abandon the run");
        assert!(matches!(error, AppError::Validation(_)));
        assert_eq!(error.exit_code(), 2);
        assert!(error.to_string().contains("Nothing was created"), "{error}");
        assert_eq!(
            prompts.matches("Story-id prefix").count(),
            MAX_RETRIES + 1,
            "one ask and {MAX_RETRIES} re-asks:\n{prompts}"
        );
    }

    #[test]
    fn an_invalid_prefix_followed_by_a_good_one_succeeds() {
        let spec = created("\nhello world\nWID\n\n\n\n", &surroundings());
        assert_eq!(spec.prefix, "WID");
    }

    #[test]
    fn an_unrecognized_yes_no_answer_is_re_asked_rather_than_guessed() {
        let (outcome, prompts) = run("\n\nmaybe\ny\n\n\n", &surroundings());
        assert!(matches!(outcome, Ok(Answered::Create(_))), "{outcome:?}");
        assert!(prompts.contains("Please answer y or n."), "{prompts}");
    }

    #[test]
    fn a_yes_no_question_answered_wrongly_four_times_is_abandoned() {
        let (outcome, _) = run("\n\nmaybe\nmaybe\nmaybe\nmaybe\n", &surroundings());
        let error = outcome.expect_err("four unusable answers must abandon the run");
        assert!(error.to_string().contains("Nothing was created"), "{error}");
    }

    /// Category 1: the stream ending at *each* of the questions, not just the
    /// first. A cancellation is exit 0 wherever it happens.
    #[test]
    fn end_of_input_at_any_question_is_a_cancellation() {
        for answers in ["", "\n", "\n\n", "\n\n\n", "\n\n\n\n"] {
            let (outcome, prompts) = run(answers, &surroundings());
            assert_eq!(
                outcome.unwrap(),
                Answered::Cancelled,
                "EOF after {:?} answers must cancel",
                answers.len()
            );
            assert!(
                prompts.contains("cancelled; nothing was created"),
                "{prompts}"
            );
        }
    }

    /// Category 8: text that is not ASCII survives the round trip intact, and
    /// is never silently transliterated into the prefix.
    #[test]
    fn a_name_may_carry_emoji_rtl_text_and_multi_byte_graphemes() {
        let spec = created("🚀 مشروع café\nOK\n\n\n\n", &surroundings());
        assert_eq!(spec.name.as_deref(), Some("🚀 مشروع café"));
        assert_eq!(spec.prefix, "OK");
    }

    /// …and a name whose derivation yields nothing usable still offers a valid
    /// default rather than an unusable one.
    #[test]
    fn a_name_with_no_usable_initials_still_offers_a_valid_default_prefix() {
        let spec = created("日本語\n\n\n\n\n", &surroundings());
        assert_eq!(spec.name.as_deref(), Some("日本語"));
        assert_eq!(spec.prefix, "SH");
    }

    #[test]
    fn a_directory_with_no_basename_still_produces_a_name() {
        let surroundings = Surroundings {
            dir: PathBuf::from("/"),
            origin: None,
            agents_md_exists: false,
        };
        let spec = created("\n\n\n\n\n", &surroundings);
        assert_eq!(spec.name.as_deref(), Some("/"));
        assert_eq!(spec.prefix, "SH");
    }

    /// Whitespace is trimmed, so a name that is only whitespace is empty and
    /// takes the default rather than creating a project called `"   "`.
    #[test]
    fn a_whitespace_only_answer_is_the_same_as_an_empty_one() {
        let spec = created("   \n   \n\n\n\n", &surroundings());
        assert_eq!(spec.name.as_deref(), Some("my-cool-app"));
        assert_eq!(spec.prefix, "MCA");
    }

    /// Every prompt goes to the writer, and none of it to the value. A caller
    /// redirecting stdout must still see the conversation.
    #[test]
    fn every_prompt_is_written_to_the_stream_the_caller_supplied() {
        let (_, prompts) = run("\n\n\n\n\n", &surroundings());
        for needle in [
            "Project name",
            "Story-id prefix",
            "Attach this checkout",
            "Generate AGENTS.md?",
            "Create this project?",
        ] {
            assert!(prompts.contains(needle), "missing {needle}:\n{prompts}");
        }
    }
}
