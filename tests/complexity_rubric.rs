//! Complexity choices must lead agents to one canonical rubric.
use storyhook::domain::Complexity;
use storyhook::help_topics::{compact_reference, get_help_topic, list_topics};

#[test]
fn each_level_and_its_assessment_semantics_are_discoverable() {
    let text = get_help_topic("complexity-rubric").unwrap();
    for level in Complexity::ALL {
        assert!(text.contains(level.as_str()));
    }
    for required in [
        "unassessed",
        "priority",
        "highest applicable",
        "dispatch-policy",
        "Sol/Astra",
        "Opus/Fable",
    ] {
        assert!(text.contains(required), "missing {required}");
    }
    assert!(compact_reference().contains("story help complexity-rubric"));
}

#[test]
fn every_help_topic_with_a_complexity_flag_points_at_the_rubric() {
    for name in list_topics() {
        let text = get_help_topic(name).unwrap();
        if text.contains("--complexity") {
            assert!(
                text.contains("story help complexity-rubric"),
                "{name} has no criteria link"
            );
        }
    }
}
