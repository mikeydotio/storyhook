//! SH-388: attachment reads through the production router and HTTP admission gate.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::Instant;

use storyhook::api::http::{CSP, REFERRER_POLICY, TrustedHosts};
use storyhook::api::rest::route;
use storyhook::api::tokens::{TokenRegistry, cookie_name};
use storyhook::cli::parse_invocation;
use storyhook::daemon::http1::{Method, PEER_IO_TIMEOUT};
use storyhook::env::Environment;
use storyhook::invoke::{dispatch, dispatch_unscoped};
use storyhook::service::{AttachmentService, Ctx, MAX_ATTACHMENT_BYTES};
use storyhook::store::{ProjectId, ReadOps, SqliteStore, Store, StoryNo, WriteOps};
use storyhook_test_support::{TestEnv, TestServer, project_id_at, scratch_dir, serve};

const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\x00\xff\xfe";

struct Fixture {
    _env: TestEnv,
    store: Arc<SqliteStore>,
    environment: Environment,
    dir: tempfile::TempDir,
    project: ProjectId,
    repo: String,
}

impl Fixture {
    fn new() -> Self {
        let env = TestEnv::isolated();
        let dir = scratch_dir();
        let store = Arc::new(env.open_store());
        let environment = env.environment();
        dispatch_unscoped(
            &*store,
            &environment,
            dir.path(),
            "2026-01-01T00:00:00Z",
            parse_invocation(
                &["project", "new", "--prefix", "SH", "--no-agents-md"].map(str::to_string),
            )
            .unwrap(),
        )
        .unwrap();
        let project = project_id_at(&store, dir.path()).unwrap();
        let repo = store
            .read(|tx| Ok(tx.project(project)?.unwrap().slug))
            .unwrap();
        let fixture = Self {
            _env: env,
            store,
            environment,
            dir,
            project,
            repo,
        };
        fixture.command(&["new", "An attachment story"]);
        fixture
    }

    fn ctx(&self) -> Ctx<'_, SqliteStore> {
        Ctx::new(
            &*self.store,
            self.project,
            self.dir.path(),
            self.environment.clone(),
        )
        .no_hooks(true)
    }

    fn command(&self, args: &[&str]) {
        dispatch(
            &self.ctx(),
            parse_invocation(
                &args
                    .iter()
                    .map(|arg| (*arg).to_string())
                    .collect::<Vec<_>>(),
            )
            .unwrap(),
        )
        .unwrap();
    }

    fn add(&self, bytes: &[u8]) {
        AttachmentService::new(&self.ctx())
            .add(
                "SH-1",
                bytes,
                "misleading.svg",
                Some("hostile\r\nX-Injected: yes"),
            )
            .unwrap();
    }

    fn path(&self, id: &str) -> String {
        format!("/api/repos/{}/story/SH-1/attachments/{id}", self.repo)
    }

    fn server(&self) -> TestServer {
        serve(Arc::clone(&self.store), &self.environment)
    }
}

struct Response {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Response {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body).unwrap()
    }
}

fn request(server: &TestServer, method: &str, path: &str, headers: &[(&str, &str)]) -> Response {
    let mut socket = TcpStream::connect(("127.0.0.1", server.port())).unwrap();
    socket.set_read_timeout(Some(PEER_IO_TIMEOUT * 2)).unwrap();
    socket.set_write_timeout(Some(PEER_IO_TIMEOUT * 2)).unwrap();
    let mut head = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n",
        server.port()
    );
    for (name, value) in headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("Content-Length: 0\r\nConnection: close\r\n\r\n");
    socket.write_all(head.as_bytes()).unwrap();
    let mut raw = Vec::new();
    socket.read_to_end(&mut raw).unwrap();
    let boundary = raw
        .windows(4)
        .position(|part| part == b"\r\n\r\n")
        .expect("HTTP header terminator");
    let head = std::str::from_utf8(&raw[..boundary]).unwrap();
    let mut lines = head.lines();
    let status = lines
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    let headers = lines
        .map(|line| {
            let (name, value) = line.split_once(':').unwrap();
            (name.to_string(), value.trim().to_string())
        })
        .collect();
    Response {
        status,
        headers,
        body: raw[boundary + 4..].to_vec(),
    }
}

fn get(server: &TestServer, path: &str) -> Response {
    request(server, "GET", path, &[("X-Storyhook-Token", &server.token)])
}

#[test]
fn image_formats_cross_the_wire_without_text_conversion_or_filename_headers() {
    let f = Fixture::new();
    let formats: &[(&[u8], &str)] = &[
        (PNG, "image/png"),
        (b"\xff\xd8\xff\x00\xfe", "image/jpeg"),
        (b"GIF89a\x00\xff", "image/gif"),
        (b"RIFF\x00\x00\x00\x00WEBP\xff", "image/webp"),
    ];
    for (bytes, _) in formats {
        f.add(bytes);
    }
    let server = f.server();
    for (index, (bytes, mime)) in formats.iter().enumerate() {
        let path = f.path(&(index + 1).to_string());
        let response = get(&server, &path);
        assert_eq!(response.status, 200, "{path}");
        assert_eq!(response.body, *bytes);
        assert_eq!(response.header("Content-Type"), Some(*mime));
        assert_eq!(
            response.header("Content-Length"),
            Some(bytes.len().to_string().as_str())
        );
        assert_eq!(response.header("Cache-Control"), Some("no-store"));
        assert_eq!(
            response.header("Cross-Origin-Resource-Policy"),
            Some("same-origin")
        );
        assert_eq!(response.header("X-Content-Type-Options"), Some("nosniff"));
        assert_eq!(response.header("Content-Security-Policy"), Some(CSP));
        assert_eq!(response.header("Referrer-Policy"), Some(REFERRER_POLICY));
        assert_eq!(response.header("X-Frame-Options"), Some("DENY"));
        for absent in [
            "X-Injected",
            "Content-Disposition",
            "Access-Control-Allow-Origin",
            "ETag",
            "Accept-Ranges",
        ] {
            assert_eq!(response.header(absent), None, "{absent}");
        }
        let routed = route(
            &*f.store,
            &f.environment,
            &Method::Get,
            &path,
            &[],
            "",
            &TrustedHosts::default(),
        );
        assert_eq!(routed.reply.status, 200);
        assert!(routed.changed.is_none());
    }
    let full = request(
        &server,
        "GET",
        &f.path("1"),
        &[
            ("X-Storyhook-Token", &server.token),
            ("Range", "bytes=0-1"),
            ("If-None-Match", "*"),
        ],
    );
    assert_eq!(full.status, 200);
    assert_eq!(full.body, PNG);
}

#[test]
fn invalid_ids_and_methods_fail_without_returning_attachment_bytes() {
    let f = Fixture::new();
    f.add(PNG);
    let server = f.server();
    for id in ["0", "-1", "+1", "1.0", "abc", "4294967296", "%31"] {
        assert_eq!(get(&server, &f.path(id)).status, 400, "{id}");
    }
    for path in [
        f.path("2"),
        f.path("1").replace("SH-1", "SH-9"),
        f.path("1")
            .replace(&format!("/{}/", f.repo), "/no-such-project/"),
    ] {
        assert_eq!(get(&server, &path).status, 404, "{path}");
    }
    for method in ["POST", "PATCH", "DELETE", "PUT", "HEAD", "OPTIONS"] {
        let response = request(
            &server,
            method,
            &f.path("1"),
            &[("X-Storyhook-Token", &server.token), ("X-Storyhook", "1")],
        );
        assert_eq!(response.status, 405, "{method}");
        assert_ne!(response.body, PNG);
    }
}

#[test]
fn closed_stories_remain_readable_and_removed_blobs_fail_loudly() {
    let f = Fixture::new();
    f.add(PNG);
    let server = f.server();
    f.command(&["close", "SH-1", "retired"]);
    assert_eq!(get(&server, &f.path("1")).body, PNG);
    f.store
        .write(|tx| tx.delete_attachment_blob(f.project, StoryNo::new(1), 1))
        .unwrap();
    let broken = get(&server, &f.path("1"));
    assert_eq!(broken.status, 500);
    let text = std::str::from_utf8(&broken.body).unwrap();
    assert!(
        text.contains("SH-1") && text.contains("attachment 1") && text.contains("story doctor"),
        "{text}"
    );
}

#[test]
fn removed_attachment_and_another_project_cannot_recover_the_bytes() {
    let f = Fixture::new();
    f.add(PNG);
    let server = f.server();
    // Another project in the SAME store with the same prefix and story number.
    let other = scratch_dir();
    dispatch_unscoped(
        &*f.store,
        &f.environment,
        other.path(),
        "2026-01-01T00:00:00Z",
        parse_invocation(
            &["project", "new", "--prefix", "SH", "--no-agents-md"].map(str::to_string),
        )
        .unwrap(),
    )
    .unwrap();
    let project = project_id_at(&f.store, other.path()).unwrap();
    let ctx = Ctx::new(&*f.store, project, other.path(), f.environment.clone()).no_hooks(true);
    dispatch(
        &ctx,
        parse_invocation(&["new", "Other story"].map(str::to_string)).unwrap(),
    )
    .unwrap();
    let slug = f
        .store
        .read(|tx| Ok(tx.project(project)?.unwrap().slug))
        .unwrap();
    assert_eq!(
        get(
            &server,
            &format!("/api/repos/{slug}/story/SH-1/attachments/1")
        )
        .status,
        404
    );
    AttachmentService::new(&f.ctx()).remove("SH-1", 1).unwrap();
    assert_eq!(get(&server, &f.path("1")).status, 404);
}

#[test]
fn cookie_reads_require_origin_proof_and_urls_never_authenticate() {
    let f = Fixture::new();
    f.add(PNG);
    let server = f.server();
    let minted = request(
        &server,
        "POST",
        "/api/v1/tokens?name=attachment-test",
        &[("X-Storyhook-Token", &server.token)],
    );
    assert_eq!(minted.status, 200);
    let secret = minted.json()["token"].as_str().unwrap().to_string();
    let exchanged = request(
        &server,
        "POST",
        "/token",
        &[("X-Storyhook", "1"), ("X-Storyhook-Token", &secret)],
    );
    assert_eq!(exchanged.status, 204);
    let cookie = exchanged
        .header("Set-Cookie")
        .unwrap()
        .split(';')
        .next()
        .unwrap();
    let referer = format!("http://127.0.0.1:{}/", server.port());
    let allowed = vec![
        vec![("X-Storyhook-Token", secret.as_str())],
        vec![("Cookie", cookie), ("Sec-Fetch-Site", "same-origin")],
        vec![("Cookie", cookie), ("Referer", referer.as_str())],
        vec![("Cookie", cookie), ("X-Storyhook", "1")],
    ];
    for headers in allowed {
        assert_eq!(request(&server, "GET", &f.path("1"), &headers).body, PNG);
    }
    let refused = vec![
        vec![],
        vec![("X-Storyhook-Token", "wrong")],
        vec![("Cookie", cookie)],
        vec![
            ("Cookie", cookie),
            ("Sec-Fetch-Site", "cross-site"),
            ("Referer", referer.as_str()),
        ],
        vec![("Cookie", cookie), ("Sec-Fetch-Site", "same-site")],
        vec![("Cookie", cookie), ("Sec-Fetch-Site", "none")],
        vec![("Cookie", cookie), ("Referer", "http://127.0.0.1:1/")],
        vec![("Cookie", cookie), ("Referer", "http://attacker.example/")],
        vec![
            ("Cookie", "wrong=invalid"),
            ("Sec-Fetch-Site", "same-origin"),
        ],
    ];
    for headers in refused {
        let response = request(&server, "GET", &f.path("1"), &headers);
        assert_eq!(response.status, 401, "{headers:?}");
        assert_ne!(response.body, PNG);
    }
    assert_eq!(
        request(
            &server,
            "GET",
            &format!("{}?token={secret}", f.path("1")),
            &[]
        )
        .status,
        401
    );
    let revoked = request(
        &server,
        "DELETE",
        "/api/v1/tokens/attachment-test",
        &[("X-Storyhook-Token", &server.token)],
    );
    assert_eq!(revoked.status, 200);
    for headers in [
        vec![("Cookie", cookie), ("Sec-Fetch-Site", "same-origin")],
        vec![("X-Storyhook-Token", secret.as_str())],
    ] {
        assert_eq!(request(&server, "GET", &f.path("1"), &headers).status, 401);
    }
}

#[test]
fn an_expired_token_cannot_read_attachment_bytes() {
    let f = Fixture::new();
    f.add(PNG);
    let tokens = TokenRegistry::load(&f.environment);
    let expired = tokens
        .mint(
            "expired".into(),
            chrono::Utc::now(),
            Instant::now(),
            chrono::Duration::seconds(-1),
        )
        .unwrap();
    drop(tokens);
    let server = f.server();
    let cookie = format!("{}={}", cookie_name(&f.environment), expired.secret);
    for headers in [
        vec![
            ("Cookie", cookie.as_str()),
            ("Sec-Fetch-Site", "same-origin"),
        ],
        vec![("X-Storyhook-Token", expired.secret.as_str())],
    ] {
        assert_eq!(request(&server, "GET", &f.path("1"), &headers).status, 401);
    }
}

#[test]
fn story_and_board_metadata_match_the_attachment_service() {
    let f = Fixture::new();
    let server = f.server();
    let detail_path = format!("/api/repos/{}/story/SH-1", f.repo);
    assert!(
        get(&server, &detail_path).json()["story"]["story"]
            .get("attachments")
            .is_none()
    );
    f.add(PNG);
    let expected =
        serde_json::to_value(AttachmentService::new(&f.ctx()).list("SH-1").unwrap()).unwrap();
    let detail = get(&server, &detail_path).json();
    assert_eq!(detail["story"]["story"]["attachments"], expected);
    let board = get(&server, &format!("/api/repos/{}/data", f.repo)).json();
    assert_eq!(board["stories"][0]["story"]["attachments"], expected);
}

#[test]
fn maximum_sized_attachment_is_not_truncated_by_text_request_limits() {
    let f = Fixture::new();
    let mut bytes = vec![0xff; MAX_ATTACHMENT_BYTES];
    bytes[..PNG.len()].copy_from_slice(PNG);
    f.add(&bytes);
    let server = f.server();
    let response = get(&server, &f.path("1"));
    assert_eq!(response.status, 200);
    assert_eq!(
        response.header("Content-Length"),
        Some(bytes.len().to_string().as_str())
    );
    assert_eq!(response.body, bytes);
}
