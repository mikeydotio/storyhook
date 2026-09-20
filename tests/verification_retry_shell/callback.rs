//! Transport the private callback to the registry owned by the in-process verifier.
use std::io::{Read, Write};
use std::os::unix::{
    fs::PermissionsExt,
    net::{UnixListener, UnixStream},
};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::thread::{self, JoinHandle};
use std::time::Duration;
use storyhook::daemon::verification::VerificationActivity;
use storyhook::service::Ctx;
use storyhook::store::SqliteStore;
use storyhook_test_support::ServiceFixture;

pub(super) struct Callback {
    pub(super) executable: PathBuf,
    socket: PathBuf,
    stop: Arc<AtomicBool>,
    calls: Arc<AtomicUsize>,
    thread: Option<JoinHandle<()>>,
}

impl Callback {
    pub(super) fn start(
        root: &Path,
        fixture: &ServiceFixture,
        activity: VerificationActivity,
    ) -> Self {
        let socket = root.join("admission.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        let executable = root.join("story-callback");
        let real = storyhook_test_support::story_binary();
        std::fs::write(
            &executable,
            format!(
                r#"#!/usr/bin/env python3
import json, os, socket, sys
args = sys.argv[1:]
if len(args) < 4 or args[2:4] != ['verifier', 'repair-admit']:
    os.execv({real}, [{real}] + args)
with socket.socket(socket.AF_UNIX) as client:
    client.settimeout(20)
    client.connect({socket})
    client.sendall(json.dumps(args).encode())
    client.shutdown(socket.SHUT_WR)
    with client.makefile() as stream:
        reply = json.load(stream)
print(reply['text'], end='')
raise SystemExit(0 if reply['ok'] else 1)
"#,
                real = serde_json::to_string(real.to_str().unwrap()).unwrap(),
                socket = serde_json::to_string(socket.to_str().unwrap()).unwrap()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let store_path = fixture.store().path().to_path_buf();
        let project = fixture.project();
        let env = fixture.env().clone();
        let cwd = root.join("checkout");
        let stop = Arc::new(AtomicBool::new(false));
        let calls = Arc::new(AtomicUsize::new(0));
        let (stopped, observed) = (stop.clone(), calls.clone());
        let thread = thread::spawn(move || {
            let store = SqliteStore::open(&store_path).unwrap();
            while !stopped.load(Ordering::Acquire) {
                let mut stream = match listener.accept() {
                    Ok((stream, _)) => stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(error) => panic!("callback accept: {error}"),
                };
                if stopped.load(Ordering::Acquire) {
                    break;
                }
                stream
                    .set_read_timeout(Some(Duration::from_secs(20)))
                    .unwrap();
                let mut request = String::new();
                stream.read_to_string(&mut request).unwrap();
                let args: Vec<String> = serde_json::from_str(&request).unwrap();
                assert_eq!(
                    &args[..4],
                    ["--project", "fixture", "verifier", "repair-admit"]
                );
                assert_eq!(args.last().unwrap(), "--json");
                let invocation =
                    storyhook::cli::parse_invocation(&args[2..args.len() - 1]).unwrap();
                let ctx = Ctx::new(&store, project, cwd.clone(), env.clone())
                    .with_verification_activity(Some(&activity));
                let answer = match storyhook::invoke::dispatch(&ctx, invocation) {
                    Ok(response) => {
                        serde_json::json!({"ok":true, "text":storyhook::output::render_response(&response, true, false)})
                    }
                    Err(error) => serde_json::json!({"ok":false, "text":error.to_string()}),
                };
                observed.fetch_add(1, Ordering::Release);
                stream.write_all(answer.to_string().as_bytes()).unwrap();
            }
        });
        Self {
            executable,
            socket,
            stop,
            calls,
            thread: Some(thread),
        }
    }

    pub(super) fn calls(&self) -> usize {
        self.calls.load(Ordering::Acquire)
    }
}

impl Drop for Callback {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = UnixStream::connect(&self.socket);
        if let Some(thread) = self.thread.take() {
            thread.join().expect("callback transport thread");
        }
    }
}
