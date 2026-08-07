//! The `story github-sync` setup wizard — the interactive half of SH-153's D2.
//!
//! # Why this is a pure function over two streams
//!
//! Built to [`questionnaire`](super::questionnaire)'s shape, for the same
//! reason: the daemon never prompts, so this takes [`BufRead`] and [`Write`]
//! rather than reaching for `std::io::stdin`, and everything it needs to know
//! arrives in a [`SetupPlan`] the daemon already computed. Every branch is
//! unit-tested against the real function with `Cursor`s for streams — no
//! mocks, no test-only code path — and the file never names stdin, so it
//! needs no exemption from `tests/invoker_seam.rs`'s allowlist.
//!
//! # It can only produce what the flags can also produce
//!
//! The only outcome is a `(SetupStrategy, SetupMode)` pair — exactly what
//! `--strategy`/`--mode` carry. There is no answer this wizard can reach that
//! a script could not also state directly.
//!
//! # The default is `future-only`, never `import-all`
//!
//! A stray `Enter` at the strategy question must not perform the largest
//! irreversible write this feature has. `import-all` creates a local story for
//! every open issue; `future-only` creates nothing. Every other default in
//! this file may be its cheapest, most reversible option — this one may not be
//! its first-listed option, on purpose.

use std::io::{BufRead, Write};

use crate::cli::{SetupMode, SetupStrategy};
use crate::error::AppError;
use crate::output::{SetupPlan, render_setup_plan};

/// How many times an unusable answer is re-asked before the run is abandoned.
/// Mirrors [`questionnaire::MAX_RETRIES`](super::questionnaire), for the same
/// reason: a terminal supplies a person, but the same code reads a here-doc
/// that repeats one wrong line forever.
const MAX_RETRIES: usize = 3;

/// The outcome of asking.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Answered {
    /// Both questions answered — exactly what `--strategy`/`--mode` carry.
    Setup(SetupStrategy, SetupMode),
    /// The user said no, or the stream ended. **Nothing has been written**,
    /// and this is not an error — a cancellation is a decision.
    Cancelled,
}

/// Asks the two setup questions and returns what to configure.
///
/// Prompts are written to `out` — stderr in production, so that a caller who
/// redirects stdout still sees them — and answers are read from `input`. The
/// plan's own counts are printed first, above either question, because they
/// are what a person needs in order to answer the first one.
///
/// # Errors
///
/// [`AppError::Validation`] when an answer cannot be used after
/// [`MAX_RETRIES`] attempts, or when a stream fails. A stream that simply
/// *ends* is a cancellation rather than an error.
pub fn ask(
    plan: &SetupPlan,
    input: &mut impl BufRead,
    out: &mut impl Write,
) -> Result<Answered, AppError> {
    let mut session = Session { input, out };

    write!(session.out, "{}", render_setup_plan(plan)).map_err(write_failed)?;

    let Some(strategy) = session.strategy()? else {
        return session.cancel();
    };
    let Some(mode) = session.mode()? else {
        return session.cancel();
    };

    session.confirm_summary(strategy, mode)?;
    match session.yes_no("Configure github-sync with these answers?", true)? {
        Some(true) => Ok(Answered::Setup(strategy, mode)),
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

    /// The initial-sync strategy. Default `future-only` — see the module
    /// header for why that is not the first-listed option.
    fn strategy(&mut self) -> Result<Option<SetupStrategy>, AppError> {
        let prompt = "\
Strategy:
  1) import-all     import every open issue as a local story
  2) match-titles   link stories to issues whose titles match exactly
  3) push-only      push local stories to GitHub, import nothing
  4) future-only    sync only changes from now on
Choose [4]: ";
        for _ in 0..=MAX_RETRIES {
            let Some(answer) = self.line(prompt)? else {
                return Ok(None);
            };
            match answer.as_str() {
                "" | "4" | "future-only" => return Ok(Some(SetupStrategy::FutureOnly)),
                "1" | "import-all" => return Ok(Some(SetupStrategy::ImportAll)),
                "2" | "match-titles" => return Ok(Some(SetupStrategy::MatchTitles)),
                "3" | "push-only" => return Ok(Some(SetupStrategy::PushOnly)),
                _ => writeln!(
                    self.out,
                    "Please answer 1, 2, 3 or 4 (or the strategy's name)."
                )
                .map_err(write_failed)?,
            }
        }
        Err(AppError::Validation(format!(
            "giving up after {MAX_RETRIES} more tries; the strategy question needs 1, 2, 3 or \
             4. Nothing was configured."
        )))
    }

    /// The sync mode. Default `manual`, matching `SyncMode`'s own default and
    /// the least surprising answer — unlike the strategy question, every
    /// option here is equally reversible, so there is no irreversible-default
    /// hazard to design around.
    fn mode(&mut self) -> Result<Option<SetupMode>, AppError> {
        let prompt = "\
Mode:
  1) manual   run `story github-sync` explicitly
  2) off      disable sync for this project
Choose [1]: ";
        for _ in 0..=MAX_RETRIES {
            let Some(answer) = self.line(prompt)? else {
                return Ok(None);
            };
            match answer.as_str() {
                "" | "1" | "manual" => return Ok(Some(SetupMode::Manual)),
                "2" | "off" => return Ok(Some(SetupMode::Off)),
                "auto" => writeln!(
                    self.out,
                    "`auto` is not implemented -- storyhook never syncs on its own (SH-68)."
                )
                .map_err(write_failed)?,
                _ => writeln!(self.out, "Please answer 1 or 2 (or the mode's name).")
                    .map_err(write_failed)?,
            }
        }
        Err(AppError::Validation(format!(
            "giving up after {MAX_RETRIES} more tries; the mode question needs 1 or 2. Nothing \
             was configured."
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
            "giving up after {MAX_RETRIES} more tries; `{question}` needs a yes or a no. \
             Nothing was configured."
        )))
    }

    /// What is about to happen, before the last question rather than after
    /// it — the same placement [`questionnaire::ask`](super::questionnaire::ask)
    /// uses for its own summary.
    fn confirm_summary(
        &mut self,
        strategy: SetupStrategy,
        mode: SetupMode,
    ) -> Result<(), AppError> {
        writeln!(self.out).map_err(write_failed)?;
        writeln!(self.out, "  strategy   {}", strategy_word(strategy)).map_err(write_failed)?;
        writeln!(self.out, "  mode       {}", mode_word(mode)).map_err(write_failed)?;
        writeln!(self.out).map_err(write_failed)?;
        Ok(())
    }

    /// The one cancellation message, so every route to it says the same
    /// thing.
    fn cancel(&mut self) -> Result<Answered, AppError> {
        writeln!(self.out, "cancelled; nothing was configured").map_err(write_failed)?;
        Ok(Answered::Cancelled)
    }
}

/// The flag value each strategy is spelled as — the same word `--strategy`
/// takes, so the summary and the escape-hatch command agree.
fn strategy_word(strategy: SetupStrategy) -> &'static str {
    match strategy {
        SetupStrategy::ImportAll => "import-all",
        SetupStrategy::MatchTitles => "match-titles",
        SetupStrategy::PushOnly => "push-only",
        SetupStrategy::FutureOnly => "future-only",
    }
}

/// The flag value each mode is spelled as.
fn mode_word(mode: SetupMode) -> &'static str {
    match mode {
        SetupMode::Manual => "manual",
        SetupMode::Off => "off",
    }
}

fn write_failed(error: std::io::Error) -> AppError {
    AppError::Validation(format!("failed to write a prompt: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn plan() -> SetupPlan {
        SetupPlan {
            owner: "acme".to_string(),
            repo: "widgets".to_string(),
            local_story_count: 12,
            open_issue_count: 30,
            exact_match_count: 4,
        }
    }

    /// Runs the real function over fake streams. Zero mocks: the only thing
    /// substituted is where the bytes come from.
    fn run(answers: &str) -> (Result<Answered, AppError>, String) {
        let mut input = Cursor::new(answers.to_string().into_bytes());
        let mut out: Vec<u8> = Vec::new();
        let outcome = ask(&plan(), &mut input, &mut out);
        (outcome, String::from_utf8(out).expect("prompts are utf-8"))
    }

    fn answered(answers: &str) -> (SetupStrategy, SetupMode) {
        match run(answers).0 {
            Ok(Answered::Setup(strategy, mode)) => (strategy, mode),
            other => panic!("expected an answer, got {other:?}"),
        }
    }

    #[test]
    fn every_default_taken_is_future_only_and_manual() {
        assert_eq!(
            answered("\n\n\n"),
            (SetupStrategy::FutureOnly, SetupMode::Manual)
        );
    }

    /// The load-bearing property: an empty answer at the strategy question —
    /// a stray `Enter` — must never select `import-all`.
    #[test]
    fn a_bare_enter_at_the_strategy_question_never_selects_import_all() {
        let (strategy, _) = answered("\n\n\n");
        assert_ne!(strategy, SetupStrategy::ImportAll);
        assert_eq!(strategy, SetupStrategy::FutureOnly);
    }

    #[test]
    fn every_strategy_number_and_word_is_accepted() {
        for (answer, expected) in [
            ("1", SetupStrategy::ImportAll),
            ("import-all", SetupStrategy::ImportAll),
            ("2", SetupStrategy::MatchTitles),
            ("match-titles", SetupStrategy::MatchTitles),
            ("3", SetupStrategy::PushOnly),
            ("push-only", SetupStrategy::PushOnly),
            ("4", SetupStrategy::FutureOnly),
            ("future-only", SetupStrategy::FutureOnly),
        ] {
            let (strategy, _) = answered(&format!("{answer}\n\n\n"));
            assert_eq!(strategy, expected, "answer {answer:?}");
        }
    }

    #[test]
    fn every_mode_number_and_word_is_accepted() {
        for (answer, expected) in [
            ("1", SetupMode::Manual),
            ("manual", SetupMode::Manual),
            ("2", SetupMode::Off),
            ("off", SetupMode::Off),
        ] {
            let (_, mode) = answered(&format!("\n{answer}\n\n"));
            assert_eq!(mode, expected, "answer {answer:?}");
        }
    }

    /// `auto` at the mode question is refused by name and re-asked, not
    /// silently accepted — nothing implements it (SH-68).
    #[test]
    fn mode_auto_is_refused_and_re_asked() {
        let (outcome, prompts) = run("\nauto\nmanual\n\n");
        assert!(matches!(outcome, Ok(Answered::Setup(_, SetupMode::Manual))));
        assert!(prompts.contains("SH-68"), "{prompts}");
    }

    #[test]
    fn no_at_the_final_question_cancels_and_says_so() {
        let (outcome, prompts) = run("\n\nn\n");
        assert_eq!(outcome.unwrap(), Answered::Cancelled);
        assert!(
            prompts.contains("cancelled; nothing was configured"),
            "{prompts}"
        );
    }

    #[test]
    fn end_of_input_at_either_question_is_a_cancellation() {
        for answers in ["", "\n"] {
            let (outcome, prompts) = run(answers);
            assert_eq!(
                outcome.unwrap(),
                Answered::Cancelled,
                "answers: {answers:?}"
            );
            assert!(prompts.contains("cancelled; nothing was configured"));
        }
    }

    #[test]
    fn an_unrecognized_strategy_answer_is_re_asked_exactly_three_times_then_abandoned() {
        let (outcome, prompts) = run("bogus\nbogus\nbogus\nbogus\n");
        let error = outcome.expect_err("four unusable answers must abandon the run");
        assert!(matches!(error, AppError::Validation(_)));
        assert_eq!(error.exit_code(), 2);
        assert!(
            error.to_string().contains("Nothing was configured"),
            "{error}"
        );
        assert_eq!(
            prompts.matches("Choose [4]:").count(),
            MAX_RETRIES + 1,
            "one ask and {MAX_RETRIES} re-asks:\n{prompts}"
        );
    }

    #[test]
    fn the_counts_are_printed_before_either_question() {
        let (_, prompts) = run("\n\n\n");
        let counts = prompts.find("12 local").expect("the local count");
        let question = prompts.find("Strategy:").expect("the first question");
        assert!(counts < question, "{prompts}");
    }

    #[test]
    fn the_summary_precedes_the_final_question_rather_than_following_it() {
        let (_, prompts) = run("\n\n\n");
        let summary = prompts.find("strategy   future-only").expect("a summary");
        let confirm = prompts.find("Configure github-sync").expect("the question");
        assert!(summary < confirm, "{prompts}");
    }

    #[test]
    fn every_prompt_is_written_to_the_stream_the_caller_supplied() {
        let (_, prompts) = run("\n\n\n");
        for needle in ["Strategy:", "Mode:", "Configure github-sync"] {
            assert!(prompts.contains(needle), "missing {needle}:\n{prompts}");
        }
    }
}
