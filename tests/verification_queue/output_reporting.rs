//! SH-713: publish actual file observations through the store and owned slot.

use super::*;
use serde_json::{Value, json};
use std::fs::{self, File, FileTimes};
use std::io::Write;
use std::os::unix::fs::MetadataExt;

const START: &str = "2026-01-01T00:00:00Z";
const RECENT: &str = "2026-01-01T00:09:59Z";
const NOW: &str = "2026-01-01T00:10:00Z";
const LATER: &str = "2026-01-01T00:14:00Z";

struct OutputFixture {
    fixture: ServiceFixture,
    candidate: VerificationCandidate,
    activity: VerificationActivity,
    guard: Option<VerificationGuard>,
    journal: PathBuf,
    log: PathBuf,
}

impl OutputFixture {
    fn new() -> Self {
        let fixture = ServiceFixture::new();
        fixture.link_origin("https://github.com/acme/widgets");
        submitted(&fixture, "foreign gate output", Priority::High, PR_ONE);
        let candidate = VerificationQueue::new(fixture.store())
            .next()
            .unwrap()
            .unwrap();
        let activity = VerificationActivity::new();
        let guard = Some(activity.acquire(&candidate, START.into()));
        let journal = journal_path(fixture.env(), &candidate);
        fs::create_dir_all(journal.parent().unwrap()).unwrap();
        let log = journal.with_extension("raw.log");
        fs::write(&log, "ordinary output without newline").unwrap();
        set_modified(&log, RECENT);
        let this = Self {
            fixture,
            candidate,
            activity,
            guard,
            journal,
            log,
        };
        this.write_journal(this.records());
        this
    }

    fn records(&self) -> Vec<Value> {
        let held = self.activity.active_for(self.fixture.project()).unwrap();
        let metadata = fs::metadata(&self.log).unwrap();
        vec![
            json!({"kind":"run", "generation":held.generation.unwrap().get(), "attempt_id":held.attempt_id, "at":START}),
            json!({"kind":"output", "attempt_id":held.attempt_id, "path":self.log, "dev":metadata.dev(), "ino":metadata.ino(), "at":START}),
            json!({"kind":"item", "path":"release gate", "status":"running", "at":START}),
        ]
    }

    fn write_journal(&self, rows: Vec<Value>) {
        let text = rows
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        fs::write(&self.journal, text).unwrap();
        set_modified(&self.journal, START);
    }

    fn publish(&self, now: &str) -> String {
        publish_once(
            self.fixture.store(),
            self.fixture.env(),
            now,
            &self.activity,
        )
        .unwrap();
        last_comment(&self.fixture, &self.candidate.story_id)
    }

    fn current_step(&self) -> Option<String> {
        let rows = status_snapshot(
            std::slice::from_ref(&self.candidate),
            self.activity.active_for(self.fixture.project()).as_ref(),
            self.fixture.env(),
            NOW,
        );
        match &rows[0].2 {
            VerificationStatus::Running {
                current_step,
                tests,
                ..
            } => {
                assert_eq!(*tests, None, "raw output must never invent test counts");
                current_step.as_ref().map(|step| step.label.clone())
            }
            status => panic!("the owned attempt must remain running: {status:?}"),
        }
    }
}

fn set_modified(path: &Path, at: &str) {
    let time = chrono::DateTime::parse_from_rfc3339(at).unwrap();
    File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_times(FileTimes::new().set_modified(time.into()))
        .unwrap();
}

#[test]
fn completed_capture_does_not_report_silence_during_landing() {
    let fixture = OutputFixture::new();
    fixture.publish(NOW);
    let mut records = fixture.records();
    records[2]["status"] = "passed".into();
    records.push(json!({"kind":"item", "path":"land pull request", "status":"running", "at":NOW}));
    fixture.write_journal(records);
    let text = fixture.publish(LATER);
    assert!(text.contains("running"), "{text}");
    assert!(!text.contains("No stdout/stderr output observed"), "{text}");
    assert!(!text.contains("Output observation unavailable"), "{text}");
}

fn assert_output_available(body: &str) {
    assert!(!body.contains("Output observation unavailable"), "{body}");
    assert!(!body.contains("No stdout/stderr output observed"), "{body}");
    assert!(!body.contains("NO GATE OUTPUT"), "{body}");
}

#[test]
fn recent_raw_output_does_not_hide_old_structured_progress() {
    let run = OutputFixture::new();
    let body = run.publish(NOW);
    assert_output_available(&body);
    assert!(body.contains("No structured progress for 10m 0s"), "{body}");
    assert!(
        body.contains("release gate — running; detailed counts unavailable"),
        "{body}"
    );
    assert_eq!(run.current_step().as_deref(), Some("release gate"));
}

#[test]
fn raw_growth_resets_only_raw_silence_and_touches_do_not_reset_it() {
    let run = OutputFixture::new();
    set_modified(&run.log, START);
    let initial = run.publish(NOW);
    assert!(
        initial.contains("No stdout/stderr output observed for 10m 0s"),
        "{initial}"
    );
    let journal_before = fs::read(&run.journal).unwrap();
    File::options()
        .append(true)
        .open(&run.log)
        .unwrap()
        .write_all(b"; stderr fragment")
        .unwrap();
    assert_output_available(&run.publish(LATER));
    assert_eq!(fs::read(&run.journal).unwrap(), journal_before);
    set_modified(&run.log, "2026-01-01T00:18:00Z");
    let quiet = run.publish("2026-01-01T00:18:00Z");
    assert!(
        quiet.contains("No stdout/stderr output observed for 4m 0s"),
        "{quiet}"
    );
    assert!(
        quiet.contains("No structured progress for 18m 0s"),
        "{quiet}"
    );
    assert_eq!(run.current_step().as_deref(), Some("release gate"));
}

#[test]
fn unavailable_files_never_become_silence_or_recover_by_rebasing() {
    for damage in ["missing", "truncated", "replaced"] {
        let run = OutputFixture::new();
        assert_output_available(&run.publish(NOW));
        match damage {
            "missing" => fs::remove_file(&run.log).unwrap(),
            "truncated" => fs::write(&run.log, "").unwrap(),
            _ => {
                fs::rename(&run.log, run.log.with_extension("previous")).unwrap();
                fs::write(&run.log, "replacement output").unwrap();
            }
        }
        let body = run.publish(LATER);
        assert!(
            body.contains("Output observation unavailable"),
            "{damage}: {body}"
        );
        assert!(
            !body.contains("No stdout/stderr output observed"),
            "{damage}: {body}"
        );
        fs::write(
            &run.log,
            "apparently healthy output that cannot repair lost evidence",
        )
        .unwrap();
        set_modified(&run.log, LATER);
        let body = run.publish("2026-01-01T00:18:00Z");
        assert!(
            body.contains("Output observation unavailable"),
            "{damage}: {body}"
        );
    }
}

#[test]
fn same_generation_retry_rejects_old_uuid_and_accepts_its_new_reference() {
    let mut run = OutputFixture::new();
    assert_output_available(&run.publish(NOW));
    let previous = run.activity.active_for(run.fixture.project()).unwrap();
    drop(run.guard.take());
    run.guard = Some(run.activity.acquire(&run.candidate, START.into()));
    let current = run.activity.active_for(run.fixture.project()).unwrap();
    assert_eq!(previous.generation, current.generation);
    assert_ne!(previous.attempt_id, current.attempt_id);
    let rejected = run.publish(LATER);
    assert!(
        rejected.contains("Output observation unavailable"),
        "{rejected}"
    );
    assert!(
        !rejected.contains("No stdout/stderr output observed"),
        "{rejected}"
    );
    assert_eq!(run.current_step(), None);
    set_modified(&run.log, "2026-01-01T00:18:00Z");
    run.write_journal(run.records());
    assert_output_available(&run.publish("2026-01-01T00:18:00Z"));
    assert_eq!(run.current_step().as_deref(), Some("release gate"));
}

#[test]
fn legacy_and_foreign_identity_cannot_authenticate_recent_output() {
    for mismatch in ["legacy", "generation", "run-uuid", "output-uuid"] {
        let run = OutputFixture::new();
        let mut rows = run.records();
        match mismatch {
            "legacy" => {
                rows[0].as_object_mut().unwrap().remove("attempt_id");
            }
            "generation" => {
                rows[0]["generation"] = json!(run.candidate.verifying_generation.unwrap().get() + 1)
            }
            "run-uuid" => rows[0]["attempt_id"] = json!(uuid::Uuid::new_v4().to_string()),
            _ => rows[1]["attempt_id"] = json!(uuid::Uuid::new_v4().to_string()),
        }
        run.write_journal(rows);
        let body = run.publish(NOW);
        assert!(
            body.contains("Output observation unavailable"),
            "{mismatch}: {body}"
        );
        assert!(
            !body.contains("No stdout/stderr output observed"),
            "{mismatch}: {body}"
        );
        if matches!(mismatch, "generation" | "run-uuid") {
            assert_eq!(run.current_step(), None, "{mismatch}");
        }
    }
}
