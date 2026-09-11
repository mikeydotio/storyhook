use std::num::NonZeroUsize;
use ste_lint::{Diagnostic, Format, Options, Rule, Severity, lint};

fn check(text: &str) -> Vec<Diagnostic> {
    lint(
        text,
        Options {
            format: Format::Markdown,
            sentence_limit: NonZeroUsize::new(20).unwrap(),
        },
    )
}

fn errors(text: &str) -> Vec<Diagnostic> {
    check(text)
        .into_iter()
        .filter(|d| d.severity == Severity::Error)
        .collect()
}

#[test]
fn sentence_limit_is_exact_and_caller_selected() {
    for count in [0, 1, 19, 20, 21, 200] {
        let text = format!("{}.", vec!["word"; count].join(" "));
        assert_eq!(errors(&text).len(), usize::from(count > 20), "{count}");
        assert!(
            lint(
                &text,
                Options {
                    format: Format::Plain,
                    sentence_limit: NonZeroUsize::new(200).unwrap()
                }
            )
            .is_empty()
        );
    }
}

#[test]
fn formatting_and_soft_breaks_cannot_reset_a_sentence() {
    for separator in [" ", "\n", "  \n"] {
        let text = format!(
            "{}{}{}",
            ["word"; 10].join(" "),
            separator,
            ["**word**"; 11].join(" ")
        );
        assert_eq!(errors(&text)[0].rule, Rule::SentenceLength);
    }
    assert_eq!(errors("Do**n't** use it.")[0].rule, Rule::Contraction);
    assert_eq!(errors("Don&#39;t use it.")[0].rule, Rule::Contraction);
}

#[test]
fn sentences_and_list_items_have_independent_limits() {
    let sentence = vec!["word"; 20].join(" ");
    for text in [
        format!("{sentence}. {sentence}!"),
        format!("- {sentence}\n- {sentence}"),
        format!("{sentence}\n\n{sentence}"),
    ] {
        assert!(errors(&text).is_empty(), "{text}");
    }
}

#[test]
fn contractions_include_curly_quotes_but_not_possessives() {
    for text in [
        "Don't stop.",
        "It isn’t ready.",
        "We're ready.",
        "It’s ready.",
        "He's ready.",
        "She’s ready.",
        "I'll start.",
        "They've stopped.",
    ] {
        assert_eq!(errors(text)[0].rule, Rule::Contraction, "{text}");
    }
    assert!(errors("The user's file and James’ file are ready.").is_empty());
}

#[test]
fn vocabulary_checks_whole_words_and_inflections() {
    for text in [
        "Utilize it.",
        "The tool utilizes it.",
        "We utilized it.",
        "Commence work.",
        "Work commences now.",
        "We commenced work.",
    ] {
        assert_eq!(errors(text)[0].rule, Rule::Vocabulary, "{text}");
    }
    assert!(errors("The commencement date and utilization rate").is_empty());
}

#[test]
fn literal_evidence_is_preserved_but_link_labels_are_checked() {
    for text in [
        "Use `utilize()` now.",
        "```text\nDon't utilize this.\n```",
        "    Don't utilize this.\n",
        "> Don't utilize this.\n",
        "See https://example.test/utilize and src/utilize.rs.",
        "Use [the tool](https://example.test/utilize).",
    ] {
        assert!(errors(text).is_empty(), "{text}");
    }
    assert_eq!(
        errors("[Utilize it](https://example.test).")[0].rule,
        Rule::Vocabulary
    );
    assert_eq!(
        errors("`long technical expression` utilize it")[0].rule,
        Rule::Vocabulary
    );
}

#[test]
fn unmatched_code_markers_do_not_hide_prose() {
    assert_eq!(errors("`Don't stop")[0].rule, Rule::Contraction);
}

#[test]
fn diagnostics_point_into_original_unicode_source() {
    let text = "# Résumé\n\nÉlan: don't stop.";
    let d = &errors(text)[0];
    assert_eq!(&text[d.span.clone()], "don't");
    assert_eq!((d.line, d.column), (3, 7));
    assert!(!d.help.is_empty());
    assert_eq!(
        serde_json::from_str::<Diagnostic>(&serde_json::to_string(d).unwrap()).unwrap(),
        *d
    );
}

#[test]
fn passive_voice_is_advisory_and_does_not_block_a_write() {
    assert!(errors("The file was removed.").is_empty());
    assert!(
        check("The file was removed.")
            .iter()
            .any(|d| d.rule == Rule::PossiblePassive && d.severity == Severity::Advice)
    );
}

#[test]
fn titles_empty_descriptions_and_plain_text_are_supported() {
    for text in [
        "",
        " \n\t",
        "Fix file parsing",
        "Use v2.4.2 with io.mikey.storyhook",
        "1.25 seconds",
        "The e.g. abbreviation",
    ] {
        assert!(errors(text).is_empty(), "{text}");
    }
    let found = lint(
        "`Don't stop`",
        Options {
            format: Format::Plain,
            sentence_limit: NonZeroUsize::new(20).unwrap(),
        },
    );
    assert!(found.iter().any(|d| d.rule == Rule::Contraction));
}

#[test]
fn an_autolink_does_not_hide_the_text_after_it() {
    assert_eq!(
        errors("See <https://example.test>. Don't stop.")[0].rule,
        Rule::Contraction
    );
    assert_eq!(
        errors("See <user@example.test>. Utilize it.")[0].rule,
        Rule::Vocabulary
    );
}

#[test]
fn tables_and_nested_evidence_keep_their_boundaries() {
    assert_eq!(
        errors("| Task |\n| --- |\n| Utilize it. |\n")[0].rule,
        Rule::Vocabulary
    );
    assert_eq!(
        errors("> Evidence\n> > Don't stop.\n>\n> ```\n> utilize\n> ```\n\nCommence work.")[0].rule,
        Rule::Vocabulary
    );
}

#[test]
fn inline_code_counts_as_one_technical_unit() {
    let prefix = vec!["word"; 19].join(" ");
    assert!(errors(&format!("{prefix} `several technical words`.")).is_empty());
    assert_eq!(
        errors(&format!("{prefix} `several technical words` more."))[0].rule,
        Rule::SentenceLength
    );
}
