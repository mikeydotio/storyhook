//! Every shipped agent startup surface must lead to the same review procedure.

use std::path::Path;
use storyhook::help_topics::get_help_topic;
use storyhook::service::{SessionService, templates};
use storyhook_test_support::ServiceFixture;

const POINTER: &str = "story help obviation-review";

#[test]
fn the_review_procedure_preserves_human_authority_and_names_real_commands() {
    let topic = get_help_topic("obviation-review").expect("the review topic must ship");
    for needle in [
        POINTER,
        "load-context --story",
        "obviated-by",
        "blocked",
        "--if-state",
        "--reason",
        "human",
        "unrelate",
        "unblock",
        "story close",
        "evidence",
        "in-progress",
        "verifying",
        "done",
    ] {
        assert!(topic.contains(needle), "missing {needle} from {topic}");
    }
}

#[test]
fn every_scaffold_and_skill_carries_the_shared_review_pointer() {
    for (name, text) in [
        ("AGENTS", templates::agents_md("ACME", "done")),
        ("CLAUDE", templates::claude_md()),
        ("Cursor", templates::cursor_rules()),
    ] {
        assert!(text.contains(POINTER), "{name} has no obviation review");
    }
    for file in [
        "AGENTS.md",
        "plugins/story/skills/story/SKILL.md",
        "plugins/story/skills/story-context/SKILL.md",
    ] {
        let text =
            std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(file)).unwrap();
        assert!(text.contains(POINTER), "{file} has no review pointer");
    }
}

#[test]
fn session_start_keeps_the_review_pointer_even_when_project_context_is_large() {
    let f = ServiceFixture::new();
    let id = storyhook::service::StoryService::new(&f.ctx())
        .create(&storyhook::service::NewStoryInput {
            title: "a".repeat(4000),
            ..Default::default()
        })
        .unwrap()
        .id;
    let text = SessionService::new(&f.ctx()).context().unwrap();
    assert!(
        text.contains(POINTER),
        "{id}: session guidance omitted review"
    );
}
