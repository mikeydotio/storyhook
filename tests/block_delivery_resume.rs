//! SH-772 decision D5: a Resume reaches the story's live session whether or
//! not the block's interrupt was acknowledged.
//!
//! A Resume carries its own authority: it is still Pending under the workspace
//! lock, and every door that registers a new session revokes pending rows first.
//! The episode's interrupt only decides HOW the session is named: a Delivered
//! interrupt pins its exact acknowledged session (`--expected-target`); anything
//! else asks the helper for the story's current registered session
//! (`--registered-session`), which never adopts and never types into a composer
//! that is not idle. MT-32 lost its resume because its interrupt was Unreached.

use std::path::{Path, PathBuf};
use storyhook::daemon::block_delivery::process_one;
use storyhook::service::block_delivery::UNBLOCK_PROMPT;
use storyhook::service::{NewStoryInput, StoryService};
use storyhook::store::{
    BlockAction, BlockDelivery, DeliveryStatus, ReadOps, Store, StoryNo, WriteOps,
};
use storyhook_test_support::ServiceFixture;

/// A delivery helper that records each call's arguments and answers from files
/// the test writes: `interrupt-reply` for `--interrupt`, `resume-reply` otherwise.
struct Helper {
    script: PathBuf,
    dir: PathBuf,
}

impl Helper {
    fn new(f: &ServiceFixture) -> Self {
        let dir = f.cwd().join("helper");
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("notify.sh");
        std::fs::write(
            &script,
            format!(
                r#"dir='{dir}'
n=$(( $(cat "$dir/count" 2>/dev/null || echo 0) + 1 ))
printf '%s' "$n" > "$dir/count"
for arg in "$@"; do printf '%s\n' "$arg"; done > "$dir/call-$n"
if [ "$5" = --interrupt ]; then cat "$dir/interrupt-reply"; else cat "$dir/resume-reply"; fi
"#,
                dir = dir.display()
            ),
        )
        .unwrap();
        Self { script, dir }
    }

    fn interrupt_replies(&self, reply: &str) {
        std::fs::write(self.dir.join("interrupt-reply"), reply).unwrap();
    }

    fn resume_replies(&self, reply: &str) {
        std::fs::write(self.dir.join("resume-reply"), reply).unwrap();
    }

    /// The arguments after `--project <slug> notify <id>` of call `n` (1-based).
    fn call(&self, n: usize) -> Vec<String> {
        std::fs::read_to_string(self.dir.join(format!("call-{n}")))
            .unwrap_or_else(|e| panic!("helper call {n} was never made: {e}"))
            .lines()
            .skip(4)
            .map(str::to_owned)
            .collect()
    }

    fn calls(&self) -> usize {
        std::fs::read_to_string(self.dir.join("count"))
            .map(|n| n.parse().unwrap())
            .unwrap_or(0)
    }

    fn deliver(&self, f: &ServiceFixture) -> bool {
        process_one(f.store(), f.env(), Some(&self.script)).unwrap()
    }
}

fn working_story(f: &ServiceFixture) -> String {
    let ctx = f.ctx();
    let svc = StoryService::new(&ctx);
    let id = svc
        .create(&NewStoryInput {
            title: "Worked".into(),
            ..Default::default()
        })
        .unwrap()
        .id;
    svc.set_state(&id, "in-progress", None, None, None).unwrap();
    id
}

fn with_checkout() -> ServiceFixture {
    let f = ServiceFixture::new();
    let output = storyhook::env::git_env::command(f.cwd())
        .args(["init", "-b", "main"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    f.store()
        .write(|tx| tx.set_checkout_path(f.project(), Some(f.cwd())))
        .unwrap();
    f
}

fn rows(f: &ServiceFixture) -> Vec<BlockDelivery> {
    f.store()
        .read(|tx| tx.block_deliveries(f.project()))
        .unwrap()
}

fn block(f: &ServiceFixture, id: &str) {
    StoryService::new(&f.ctx())
        .set_awaiting(id, "a hold")
        .unwrap();
}

fn unblock(f: &ServiceFixture, id: &str) {
    StoryService::new(&f.ctx()).clear_awaiting(id).unwrap();
}

const DELIVERED: &str = r#"{"ok":true,"target":"live-session","display":"prompt delivered"}"#;

fn registered_argv() -> Vec<String> {
    vec![UNBLOCK_PROMPT.to_owned(), "--registered-session".to_owned()]
}

#[test]
fn a_resume_after_an_unacknowledged_interrupt_reaches_the_registered_session() {
    for (interrupt, outcome) in [
        // MT-32: the bind step's ps probe timed out.
        (
            r#"{"ok":false,"reason":"pane-query-failed","display":"ps timed out"}"#,
            DeliveryStatus::Unreached,
        ),
        (
            r#"{"ok":false,"reason":"pane-unavailable","display":"no window yet"}"#,
            DeliveryStatus::Unreached,
        ),
        // The Escape may already have stopped the turn; it must still resume.
        (
            r#"{"ok":false,"reason":"interruption-failed","display":"cleanup unconfirmed"}"#,
            DeliveryStatus::Uncertain,
        ),
        ("not json", DeliveryStatus::Uncertain),
    ] {
        let f = with_checkout();
        let helper = Helper::new(&f);
        helper.interrupt_replies(interrupt);
        helper.resume_replies(DELIVERED);
        let id = working_story(&f);
        block(&f, &id);
        assert!(helper.deliver(&f));
        assert_eq!(rows(&f)[0].status, outcome, "{interrupt}");

        unblock(&f, &id);
        assert!(helper.deliver(&f));

        assert_eq!(helper.call(2), registered_argv(), "{interrupt}");
        let resume = &rows(&f)[1];
        assert_eq!(resume.action, BlockAction::Resume);
        assert_eq!(resume.status, DeliveryStatus::Delivered, "{interrupt}");
        assert_eq!(resume.target.as_deref(), Some("live-session"));
        assert!(!helper.deliver(&f), "nothing is replayed");
    }
}

#[test]
fn a_superseded_interrupt_does_not_strand_the_resume() {
    let f = with_checkout();
    let helper = Helper::new(&f);
    helper.resume_replies(DELIVERED);
    let id = working_story(&f);
    block(&f, &id);
    // The interrupt never started: a replacement door revoked it while pending.
    let story = StoryNo::parse_id("SH", &id).unwrap();
    f.store()
        .write(|tx| {
            let mut interrupt = tx.block_deliveries(f.project())?.remove(0);
            interrupt.status = DeliveryStatus::Superseded;
            interrupt.detail = "session replacement reserved".into();
            assert_eq!(interrupt.story, story);
            tx.update_block_delivery(&interrupt, DeliveryStatus::Pending)
        })
        .unwrap();
    unblock(&f, &id);

    assert!(helper.deliver(&f));

    assert_eq!(helper.calls(), 1, "the superseded interrupt was never sent");
    assert_eq!(helper.call(1), registered_argv());
    assert_eq!(rows(&f)[1].status, DeliveryStatus::Delivered);
}

/// The target comes from the Resume's own episode. An earlier episode's
/// acknowledged session names a lifetime that may be long gone.
#[test]
fn an_earlier_episodes_acknowledged_session_never_binds_a_later_resume() {
    let f = with_checkout();
    let helper = Helper::new(&f);
    helper.interrupt_replies(r#"{"ok":true,"target":"first-session","display":"interrupted"}"#);
    helper.resume_replies(DELIVERED);
    let id = working_story(&f);
    block(&f, &id);
    assert!(helper.deliver(&f));
    unblock(&f, &id);
    assert!(helper.deliver(&f));
    assert_eq!(
        helper.call(2),
        [UNBLOCK_PROMPT, "--expected-target", "first-session"]
    );

    // A second episode with no interrupt row at all (a read-model heal can
    // produce one): the Resume is the story's only effect since the first.
    let story = StoryNo::parse_id("SH", &id).unwrap();
    f.store()
        .write(|tx| tx.enqueue_block_delivery(f.project(), story, BlockAction::Resume))
        .unwrap();
    assert!(helper.deliver(&f));

    assert_eq!(helper.call(3), registered_argv());
    assert_eq!(rows(&f)[2].status, DeliveryStatus::Delivered);
}

#[test]
fn an_acknowledged_interrupt_still_pins_its_exact_session() {
    let f = with_checkout();
    let helper = Helper::new(&f);
    helper.interrupt_replies(r#"{"ok":true,"target":"blocked-session","display":"interrupted"}"#);
    helper.resume_replies(
        r#"{"ok":false,"reason":"target-changed","display":"the interrupted session was replaced; no prompt sent"}"#,
    );
    let id = working_story(&f);
    block(&f, &id);
    assert!(helper.deliver(&f));
    unblock(&f, &id);
    assert!(helper.deliver(&f));

    assert_eq!(
        helper.call(2),
        [UNBLOCK_PROMPT, "--expected-target", "blocked-session"]
    );
    assert_eq!(
        rows(&f)[1].status,
        DeliveryStatus::Unreached,
        "a replaced session is refused, never re-bound (SH-718)"
    );
}

#[test]
fn a_registered_acknowledgement_without_a_bound_session_is_uncertain() {
    let f = with_checkout();
    let helper = Helper::new(&f);
    helper.interrupt_replies(r#"{"ok":false,"reason":"pane-unavailable","display":"none"}"#);
    helper.resume_replies(r#"{"ok":true,"display":"delivered somewhere"}"#);
    let id = working_story(&f);
    block(&f, &id);
    assert!(helper.deliver(&f));
    unblock(&f, &id);
    assert!(helper.deliver(&f));

    let resume = &rows(&f)[1];
    assert_eq!(resume.status, DeliveryStatus::Uncertain, "{resume:?}");
    assert_eq!(resume.target, None);
}

/// A refused Resume is loud: its outcome comment says what the operator can do.
#[test]
fn a_resume_nobody_received_names_the_remedy() {
    for reply in [
        r#"{"ok":false,"reason":"composer-busy","display":"the composer is not idle; nothing typed"}"#,
        r#"{"ok":false,"reason":"pane-unavailable","display":"no window"}"#,
    ] {
        let f = with_checkout();
        let helper = Helper::new(&f);
        helper.interrupt_replies(r#"{"ok":false,"reason":"pane-unavailable","display":"none"}"#);
        helper.resume_replies(reply);
        let id = working_story(&f);
        block(&f, &id);
        assert!(helper.deliver(&f));
        unblock(&f, &id);
        assert!(helper.deliver(&f));

        let resume = &rows(&f)[1];
        assert_eq!(resume.status, DeliveryStatus::Unreached, "{reply}");
        assert!(
            resume
                .detail
                .contains("tell the agent the block was lifted"),
            "{resume:?}"
        );
        let comments = StoryService::new(&f.ctx())
            .clear_awaiting(&id)
            .unwrap()
            .comments;
        assert!(
            comments.iter().any(|c| c.text.contains("resume unreached")
                && c.text.contains("tell the agent the block was lifted")),
            "{reply}"
        );
    }
}

#[test]
fn a_resume_is_not_attempted_without_a_checkout() {
    let f = ServiceFixture::new();
    f.store()
        .write(|tx| tx.set_checkout_path(f.project(), None))
        .unwrap();
    let id = working_story(&f);
    block(&f, &id);
    unblock(&f, &id);
    // One pass: the unblock already retired the never-started interrupt.
    assert!(process_one(f.store(), f.env(), Some(Path::new("/must-not-run"))).unwrap());
    assert!(!process_one(f.store(), f.env(), Some(Path::new("/must-not-run"))).unwrap());
    let resume = &rows(&f)[1];
    assert_eq!(resume.action, BlockAction::Resume);
    assert_eq!(resume.status, DeliveryStatus::Unreached, "{resume:?}");
    assert!(resume.detail.contains("no linked checkout"), "{resume:?}");
}
