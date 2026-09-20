//! The CLI may give up after the daemon committed a continuation request.

use std::io::Write;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use storyhook_test_support::{ChildGuard, STORY_COMMAND_DEADLINE, TestEnv, scratch_dir, slug_at};

/// Three seconds exceeds the unchanged two-second CLI deadline after admission.
const POST_COMMIT_REPLY_DELAY: Duration = Duration::from_secs(3);
/// Fifteen seconds allows loaded test runners to observe each fixture milestone
/// while still detecting a stuck capture, daemon admission, or Stop hook.
const FIXTURE_MILESTONE_CEILING: Duration = Duration::from_secs(15);
/// A 200 ms margin around the two-second CLI deadline rejects immediate errors
/// without treating small timer and process-scheduling variation as a defect.
const STOP_FEEDBACK_FLOOR: Duration = Duration::from_millis(1800);

#[test]
fn committed_request_with_lost_cli_reply_enters_exact_status_review() {
    let env = TestEnv::isolated();
    let dir = scratch_dir();
    env.story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    let created = env
        .story(dir.path())
        .args(["new", "Lost continuation reply", "--json"])
        .output()
        .unwrap();
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );
    let id = serde_json::from_slice::<Value>(&created.stdout).unwrap()["story"]["story"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    env.story(dir.path())
        .args(["move", &id, "in-progress"])
        .assert()
        .success();
    let slug = slug_at(&env, dir.path());
    env.stop_daemon();

    // The provider endpoint is a fixture. Admission, persistence, supervision,
    // CLI deadline, and Stop adapter remain the production implementations.
    let plugin = dir.path().join("fixture-plugin");
    std::fs::create_dir_all(plugin.join("bin")).unwrap();
    std::fs::create_dir_all(plugin.join("lib")).unwrap();
    let dispatch = plugin.join("bin/story.sh");
    std::fs::write(&dispatch, "#!/bin/sh\nDISPATCH_PROTOCOL=5\n").unwrap();
    let runtime = plugin.join("lib/continuation_runtime.py");
    let capture_marker = dir.path().join("capture.observed");
    let runtime_source = format!(
        r#"import json,sys
v=json.load(sys.stdin)
if sys.argv[1]=='capture':
    o=v['origin']; sid=v['handoff']['story_id']
    c={{'lease':{{'version':1,'project_slug':{slug:?},'story_id':sid,'repository_path':'/tmp/repo','worktree_path':'/tmp/repo/lane','branch':'work','tmux':{{'socket_path':'/tmp/tmux'}}}},'head':'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa','fingerprint':'dirty-1','provider':v['provider'],'session_id':o['session_id'],'turn_id':o['turn_id'],'message_id':'message-1','mode':o['collaboration_mode'],'socket':'/tmp/tmux','pane':'%1','pid':123,'started':'start','autonomy':True}}
    with open({capture_marker:?},'w') as marker: marker.write('captured')
    print(json.dumps({{'ok':True,'capture':c}}))
elif sys.argv[1]=='observe':
    print(json.dumps({{'ok':True,'phase':'idle','capture':v['capture']}}))
else:
    print(json.dumps({{'ok':False,'detail':'unexpected provider effect'}}))
"#,
        capture_marker = capture_marker.to_string_lossy()
    );
    std::fs::write(&runtime, runtime_source).unwrap();

    let handoff = json!({"type":"storyhook.session-handoff","version":1,
        "story_id":id,"kind":"context","evidence":{
            "context":"Keep the approved work and queued corrections.",
            "outstanding_work":"Finish the assigned regression."}});
    let message = handoff.to_string();
    let transcript = dir.path().join("transcript.jsonl");
    std::fs::write(
        &transcript,
        format!(
            "{}\n",
            json!({
                "type":"assistant","sessionId":"session-1","cwd":dir.path(),
                "isSidechain":false,"uuid":"turn-1","message":{
                    "role":"assistant","content":[{"type":"text","text":message}]}
            })
        ),
    )
    .unwrap();
    let payload = json!({"hook_event_name":"Stop","session_id":"session-1",
        "cwd":dir.path(),"transcript_path":transcript,"stop_hook_active":false,
        "permission_mode":"plan","last_assistant_message":message});
    let hook = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("plugins/story/hooks/session_handoff.py");
    let mut command = Command::new("python3");
    env.apply(&mut command);
    let started = Instant::now();
    command
        .arg(hook)
        .current_dir(dir.path())
        .env("STORYHOOK_DISPATCH_SCRIPT", &dispatch)
        .env(
            "STORYHOOK_TEST_CONTINUATION_REPLY_DELAY_MS",
            POST_COMMIT_REPLY_DELAY.as_millis().to_string(),
        )
        .env("STORYHOOK_AUTO", &id)
        .env("TMUX", "/tmp/tmux,1,0")
        .env("TMUX_PANE", "%1")
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .stdin(Stdio::piped());
    let mut child = ChildGuard::spawn_with_output(&mut command).unwrap();
    let mut hook_stdin = child.take_stdin().unwrap();
    hook_stdin
        .write_all(payload.to_string().as_bytes())
        .unwrap();
    drop(hook_stdin);
    while !capture_marker.exists() {
        assert!(
            started.elapsed() < FIXTURE_MILESTONE_CEILING,
            "capture did not run"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let capture_elapsed = started.elapsed();
    let (admission_elapsed, status): (Duration, Value) = loop {
        let response = env
            .story(dir.path())
            .args(["continuation", "status", &id, "--json"])
            .output()
            .unwrap();
        assert!(
            response.status.success(),
            "{}",
            String::from_utf8_lossy(&response.stderr)
        );
        let status: Value = serde_json::from_slice(&response.stdout).unwrap();
        if status["requests"]
            .as_array()
            .is_some_and(|requests| !requests.is_empty())
        {
            break (started.elapsed(), status);
        }
        assert!(
            started.elapsed() < FIXTURE_MILESTONE_CEILING,
            "request not admitted"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    let output = child.wait_with_output_within(STORY_COMMAND_DEADLINE, || {
        "Stop hook did not return after the two-second client deadline".into()
    });
    let hook_elapsed = started.elapsed();
    assert!(
        capture_elapsed < admission_elapsed,
        "capture must precede admission"
    );
    assert!(
        admission_elapsed < hook_elapsed,
        "admission must precede lost reply"
    );
    assert!(hook_elapsed >= STOP_FEEDBACK_FLOOR, "{hook_elapsed:?}");
    assert!(hook_elapsed < FIXTURE_MILESTONE_CEILING, "{hook_elapsed:?}");
    eprintln!(
        "fixture capture {capture_elapsed:?}; admission {admission_elapsed:?}; Stop reply {hook_elapsed:?}; injected post-commit delay {POST_COMMIT_REPLY_DELAY:?}"
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let feedback: Value = serde_json::from_slice(&output.stdout).unwrap();
    let reason = feedback["reason"].as_str().unwrap();
    assert_eq!(feedback["decision"], "block");
    assert!(reason.contains("status inspection only"), "{reason}");
    assert!(reason.contains("exited 12"), "{reason}");
    assert!(reason.contains("stdout:"), "{reason}");
    assert!(reason.contains(&format!("story continuation status {id} --json")));

    let requests = status["requests"].as_array().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0]["status"], "awaiting-ack");
    assert_eq!(requests[0]["phase"], "native-continuation");
    let request_id = requests[0]["id"].as_str().unwrap();

    let mut duplicate = env.raw_story(dir.path());
    duplicate
        .args(["continuation", "request", &id, "--stdin", "--json"])
        .stdin(Stdio::piped());
    let mut duplicate = ChildGuard::spawn_with_output(&mut duplicate).unwrap();
    let mut duplicate_stdin = duplicate.take_stdin().unwrap();
    duplicate_stdin
        .write_all(
            json!({
                "handoff":handoff,"provider":"claude","origin":{
                    "hook_event_name":"Stop","session_id":"session-1","cwd":dir.path(),
                    "transcript_path":transcript,"stop_hook_active":false,
                    "permission_mode":"plan","last_assistant_message":message,
                    "turn_id":"turn-1","collaboration_mode":"plan",
                    "tmux":"/tmp/tmux,1,0","tmux_pane":"%1","autonomy":"auto"}
            })
            .to_string()
            .as_bytes(),
        )
        .unwrap();
    drop(duplicate_stdin);
    let duplicate = duplicate.wait_with_output_within(STORY_COMMAND_DEADLINE, || {
        "duplicate continuation request did not return".into()
    });
    assert!(
        duplicate.status.success(),
        "{}",
        String::from_utf8_lossy(&duplicate.stderr)
    );
    let duplicate: Value = serde_json::from_slice(&duplicate.stdout).unwrap();
    assert_eq!(duplicate["continuation"]["id"], request_id);
    assert_eq!(duplicate["native_feedback"], false);
    env.stop_daemon();
}
