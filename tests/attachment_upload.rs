//! SH-389: raw browser uploads through the production HTTP worker and store.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::sync::Arc;

use sha2::{Digest, Sha256};
use storyhook::cli::parse_invocation;
use storyhook::daemon::http1::PEER_IO_TIMEOUT;
use storyhook::invoke::{dispatch, dispatch_unscoped};
use storyhook::service::{AttachmentService, Ctx};
use storyhook::store::{ReadOps, SqliteStore, Store};
use storyhook_test_support::{TestEnv, TestServer, project_id_at, scratch_dir, serve};

const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\x00\xff";
const NAME: &str = "X-Storyhook-Attachment-Name";

struct Fixture {
    env: TestEnv,
    dir: tempfile::TempDir,
    store: Arc<SqliteStore>,
    server: TestServer,
    repo: String,
}

impl Fixture {
    fn new() -> Self {
        let env = TestEnv::isolated();
        let dir = scratch_dir();
        let store = Arc::new(env.open_store());
        dispatch_unscoped(
            &*store,
            &env.environment(),
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
        let ctx = Ctx::new(&*store, project, dir.path(), env.environment()).no_hooks(true);
        dispatch(
            &ctx,
            parse_invocation(&["new", "Upload target"].map(str::to_string)).unwrap(),
        )
        .unwrap();
        let server = serve(Arc::clone(&store), &env.environment());
        Self {
            env,
            dir,
            store,
            server,
            repo,
        }
    }

    fn path(&self) -> String {
        format!("/api/repos/{}/story/SH-1/attachments", self.repo)
    }

    fn headers(&self) -> Vec<(&'static str, String)> {
        vec![
            ("Host", format!("127.0.0.1:{}", self.server.port())),
            ("X-Storyhook", "1".into()),
            ("X-Storyhook-Token", self.server.token.clone()),
            ("Content-Type", "application/octet-stream".into()),
        ]
    }

    fn ctx(&self) -> Ctx<'_, SqliteStore> {
        Ctx::new(
            &*self.store,
            project_id_at(&self.store, self.dir.path()).unwrap(),
            self.dir.path(),
            self.env.environment(),
        )
        .no_hooks(true)
    }

    fn assert_stored(&self, id: u32, bytes: &[u8], name: &str) {
        let (metadata, stored) = AttachmentService::new(&self.ctx()).get("SH-1", id).unwrap();
        assert_eq!(stored, bytes);
        assert_eq!(metadata.name, name);
        assert_eq!(metadata.byte_len, bytes.len() as u64);
        assert_eq!(metadata.sha256, format!("{:x}", Sha256::digest(bytes)));
    }
}

struct Answer {
    status: u16,
    body: String,
}

fn connect(port: u16, method: &str, path: &str, headers: &[(&str, String)]) -> TcpStream {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream.set_read_timeout(Some(PEER_IO_TIMEOUT * 2)).unwrap();
    stream.set_write_timeout(Some(PEER_IO_TIMEOUT * 2)).unwrap();
    write!(stream, "{method} {path} HTTP/1.1\r\n").unwrap();
    for (name, value) in headers {
        write!(stream, "{name}: {value}\r\n").unwrap();
    }
    write!(stream, "Connection: close\r\n\r\n").unwrap();
    stream
}

fn answer(stream: TcpStream) -> Answer {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let status = line
        .split_whitespace()
        .nth(1)
        .expect("HTTP status")
        .parse()
        .unwrap();
    let mut length = None;
    loop {
        line.clear();
        assert_ne!(
            reader.read_line(&mut line).unwrap(),
            0,
            "incomplete HTTP head"
        );
        if line == "\r\n" {
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            length = Some(value.trim().parse::<usize>().unwrap());
        }
    }
    let mut bytes = vec![0; length.expect("framed response")];
    reader.read_exact(&mut bytes).unwrap();
    Answer {
        status,
        body: String::from_utf8(bytes).unwrap(),
    }
}

fn request(
    f: &Fixture,
    method: &str,
    path: &str,
    mut headers: Vec<(&str, String)>,
    bytes: &[u8],
) -> Answer {
    headers.push(("Content-Length", bytes.len().to_string()));
    let mut stream = connect(f.server.port(), method, path, &headers);
    stream.write_all(bytes).unwrap();
    answer(stream)
}

#[test]
fn binary_image_larger_than_json_limit_round_trips() {
    let f = Fixture::new();
    let mut bytes = PNG.to_vec();
    bytes.resize(storyhook::api::http::MAX_BODY_BYTES as usize + 1, 0xff);
    let mut headers = f.headers();
    headers.push((NAME, "caf%C3%A9%20%2B%20shot.png".into()));
    let reply = request(&f, "POST", &f.path(), headers, &bytes);
    assert_eq!(reply.status, 201, "{}", reply.body);
    let json: serde_json::Value = serde_json::from_str(&reply.body).unwrap();
    assert_eq!(json["result"], "ok");
    f.assert_stored(1, &bytes, "café + shot.png");
}

#[test]
fn incomplete_upload_never_commits() {
    let f = Fixture::new();
    let mut headers = f.headers();
    headers.push(("Content-Length", (PNG.len() + 10).to_string()));
    let mut stream = connect(f.server.port(), "POST", &f.path(), &headers);
    stream.write_all(PNG).unwrap();
    stream.shutdown(Shutdown::Write).unwrap();
    assert_eq!(answer(stream).status, 400);
    assert!(
        AttachmentService::new(&f.ctx())
            .list("SH-1")
            .unwrap()
            .is_empty()
    );
}

#[test]
fn all_formats_are_sniffed_independently_of_the_declared_type() {
    let f = Fixture::new();
    for (i, (mime, bytes)) in [
        ("image/png", PNG),
        ("image/jpeg", b"\xff\xd8\xff\x00".as_slice()),
        ("image/gif", b"GIF89a\xff".as_slice()),
        ("image/webp", b"RIFF\x00\x00\x00\x00WEBP\xff".as_slice()),
    ]
    .into_iter()
    .enumerate()
    {
        let mut headers = f.headers();
        headers.retain(|(key, _)| *key != "Content-Type");
        headers.push(("Content-Type", format!("{mime}; charset=binary")));
        let reply = request(&f, "POST", &f.path(), headers, bytes);
        assert_eq!(reply.status, 201, "{}", reply.body);
        f.assert_stored(i as u32 + 1, bytes, "attachment");
    }
    let mut headers = f.headers();
    headers.retain(|(key, _)| *key != "Content-Type");
    headers.push(("Content-Type", "IMAGE/JPEG".into()));
    assert_eq!(request(&f, "POST", &f.path(), headers, PNG).status, 201);
    let (metadata, _) = AttachmentService::new(&f.ctx()).get("SH-1", 5).unwrap();
    assert_eq!(metadata.media_type.as_str(), "image/png");
}

#[test]
fn declared_oversize_and_bad_heads_are_refused_without_reading_any_body() {
    let f = Fixture::new();
    let cap = storyhook::service::attachment::MAX_ATTACHMENT_BYTES;
    let mut cases = Vec::new();
    let mut oversized = f.headers();
    oversized.push(("Content-Length", (cap + 1).to_string()));
    cases.push((oversized, 413));
    for (key, value, status) in [
        ("X-Storyhook-Token", None, 401),
        ("X-Storyhook-Token", Some("wrong"), 401),
        ("X-Storyhook", None, 403),
        ("Host", Some("attacker.example"), 403),
        ("Content-Type", None, 415),
        ("Content-Type", Some("application/json"), 415),
        ("Content-Type", Some("multipart/form-data; boundary=x"), 415),
    ] {
        let mut headers = f.headers();
        headers.retain(|(name, _)| *name != key);
        if let Some(value) = value {
            headers.push((key, value.into()));
        }
        headers.push(("Content-Length", cap.to_string()));
        cases.push((headers, status));
    }
    for (headers, status) in cases {
        let stream = connect(f.server.port(), "POST", &f.path(), &headers);
        stream.set_read_timeout(Some(PEER_IO_TIMEOUT / 2)).unwrap();
        assert_eq!(answer(stream).status, status, "{headers:?}");
    }
    assert!(
        AttachmentService::new(&f.ctx())
            .list("SH-1")
            .unwrap()
            .is_empty()
    );
}

#[test]
fn filenames_and_image_refusals_leave_no_attachments() {
    let f = Fixture::new();
    for name in ["", "%", "%0", "%GG", "%ff", "%00", "%0A", "%7F", "%C2%85"] {
        let mut headers = f.headers();
        headers.push((NAME, name.into()));
        let reply = request(&f, "POST", &f.path(), headers, PNG);
        assert_eq!(reply.status, 400, "{name}: {}", reply.body);
    }
    let mut headers = f.headers();
    headers.extend([(NAME, "a.png".into()), (NAME, "b.png".into())]);
    assert_eq!(request(&f, "POST", &f.path(), headers, PNG).status, 400);
    for bytes in [b"".as_slice(), b"<svg></svg>", b"<html></html>", b"\x89PNG"] {
        assert_eq!(
            request(&f, "POST", &f.path(), f.headers(), bytes).status,
            422
        );
    }
    assert!(
        AttachmentService::new(&f.ctx())
            .list("SH-1")
            .unwrap()
            .is_empty()
    );
    let mut headers = f.headers();
    headers.push((NAME, "%2Ftmp%2Fshot+1.png".into()));
    assert_eq!(request(&f, "POST", &f.path(), headers, PNG).status, 201);
    f.assert_stored(1, PNG, "shot+1.png");
}

#[test]
fn exact_limit_and_chunked_uploads_round_trip_but_one_byte_over_does_not() {
    let f = Fixture::new();
    let cap = storyhook::service::attachment::MAX_ATTACHMENT_BYTES;
    let mut bytes = PNG.to_vec();
    bytes.resize(cap, 0xff);
    assert_eq!(
        request(&f, "POST", &f.path(), f.headers(), &bytes).status,
        201
    );
    f.assert_stored(1, &bytes, "attachment");
    for (len, expected) in [(cap, 201), (cap + 1, 413)] {
        bytes.resize(len, 0xff);
        let mut headers = f.headers();
        headers.push(("Transfer-Encoding", "chunked".into()));
        let mut stream = connect(f.server.port(), "POST", &f.path(), &headers);
        write!(stream, "{:x}\r\n", bytes.len()).unwrap();
        stream.write_all(&bytes).unwrap();
        write!(stream, "\r\n0\r\n\r\n").unwrap();
        let reply = answer(stream);
        assert_eq!(reply.status, expected, "{}", reply.body);
    }
    f.assert_stored(2, &bytes[..cap], "attachment");
    assert_eq!(
        AttachmentService::new(&f.ctx()).list("SH-1").unwrap().len(),
        2
    );
}

#[test]
fn malformed_chunked_uploads_and_ordinary_json_limits_are_preserved() {
    let f = Fixture::new();
    for wire in [b"z\r\n".as_slice(), b"a\r\nabc", b"1\r\nxWRONG\r\n"] {
        let mut headers = f.headers();
        headers.push(("Transfer-Encoding", "chunked".into()));
        let mut stream = connect(f.server.port(), "POST", &f.path(), &headers);
        stream.write_all(wire).unwrap();
        stream.shutdown(Shutdown::Write).unwrap();
        assert_eq!(answer(stream).status, 400);
    }
    let path = format!("/api/repos/{}/story/SH-1/comment", f.repo);
    for bytes in [
        vec![b'x'; storyhook::api::http::MAX_BODY_BYTES as usize + 1],
        vec![0xff],
    ] {
        let mut headers = f.headers();
        headers.retain(|(name, _)| *name != "Content-Type");
        headers.push(("Content-Type", "application/json".into()));
        assert_eq!(request(&f, "POST", &path, headers, &bytes).status, 400);
    }
    assert!(
        AttachmentService::new(&f.ctx())
            .list("SH-1")
            .unwrap()
            .is_empty()
    );
}

#[test]
fn route_preserves_project_resolution_story_rules_and_change_signal() {
    use storyhook::api::http::TrustedHosts;
    use storyhook::api::rest::{Changed, RouteRequest, route_with_activity};
    use storyhook::daemon::http1::{Header, Method};
    use storyhook::daemon::verification::VerificationActivity;
    use storyhook::store::{StoryNo, WriteOps};
    let f = Fixture::new();
    let project = project_id_at(&f.store, f.dir.path()).unwrap();
    let headers: Vec<Header> = f
        .headers()
        .iter()
        .map(|(k, v)| Header::from_bytes(k, v).unwrap())
        .collect();
    let route = |method: &Method, path: &str, headers: &[Header], bytes: &[u8]| {
        route_with_activity(
            &*f.store,
            &f.env.environment(),
            &VerificationActivity::new(),
            RouteRequest::binary(method, path, headers, bytes),
            &TrustedHosts::default(),
        )
    };
    let success = route(&Method::Post, &f.path().replace("SH-1", "1"), &headers, PNG);
    assert_eq!(success.reply.status, 201, "{}", success.reply.body());
    assert_eq!(success.changed, Some(Changed::Project(f.repo.clone())));
    let events = f
        .store
        .read(|tx| tx.events_for(project, StoryNo::new(1)))
        .unwrap();
    let last = events.last().unwrap();
    assert_eq!(last.provenance.command.as_deref(), Some("web:attachment"));
    assert_eq!(last.provenance.actor.as_ref().unwrap().as_str(), "web:user");

    for (path, expected) in [
        (f.path().replace(&f.repo, "missing-project"), 404),
        (f.path().replace("SH-1", "SH-999"), 404),
        (f.path().replace("SH-1", "FOREIGN-1"), 422),
        (f.path().replace("attachments", "comment"), 400),
    ] {
        let result = route(&Method::Post, &path, &headers, PNG);
        assert_eq!(
            result.reply.status,
            expected,
            "{path}: {}",
            result.reply.body()
        );
        assert_eq!(result.changed, None);
    }
    for removed in ["X-Storyhook", "Content-Type"] {
        let filtered: Vec<_> = headers
            .iter()
            .filter(|h| !h.field.equiv(removed))
            .cloned()
            .collect();
        let result = route(&Method::Post, &f.path(), &filtered, PNG);
        assert_eq!(
            result.reply.status,
            if removed == "X-Storyhook" { 403 } else { 415 }
        );
        assert_eq!(result.changed, None);
    }
    let invalid = route(&Method::Post, &f.path(), &headers, b"<svg/>");
    assert_eq!(invalid.reply.status, 422);
    assert_eq!(invalid.changed, None);
    dispatch(
        &f.ctx(),
        parse_invocation(&["close", "SH-1", "complete"].map(str::to_string)).unwrap(),
    )
    .unwrap();
    let closed = route(&Method::Post, &f.path(), &headers, PNG);
    assert_eq!(closed.reply.status, 422, "{}", closed.reply.body());
    assert_eq!(closed.changed, None);
    f.store
        .write(|tx| tx.set_checkout_path(project, None))
        .unwrap();
    let pathless = route(&Method::Post, &f.path(), &headers, PNG);
    assert_eq!(pathless.reply.status, 422);
    assert_eq!(pathless.changed, None);
    assert_eq!(
        AttachmentService::new(&f.ctx()).list("SH-1").unwrap().len(),
        1
    );
}

#[test]
fn named_cookie_can_upload_until_its_token_is_revoked() {
    use storyhook::api::tokens::{TOKENS_PATH, cookie_name};
    let f = Fixture::new();
    let minted = request(
        &f,
        "POST",
        &format!("{TOKENS_PATH}?name=uploader"),
        f.headers(),
        b"",
    );
    assert_eq!(minted.status, 200, "{}", minted.body);
    let token: serde_json::Value = serde_json::from_str(&minted.body).unwrap();
    let mut headers = f.headers();
    headers.retain(|(k, _)| *k != "X-Storyhook-Token");
    headers.push((
        "Cookie",
        format!(
            "{}={}",
            cookie_name(&f.env.environment()),
            token["token"].as_str().unwrap()
        ),
    ));
    headers.push(("Sec-Fetch-Site", "same-origin".into()));
    assert_eq!(
        request(&f, "POST", &f.path(), headers.clone(), PNG).status,
        201
    );
    let revoked = request(
        &f,
        "DELETE",
        &format!("{TOKENS_PATH}/uploader"),
        f.headers(),
        b"",
    );
    assert_eq!(revoked.status, 200);
    headers.push(("Content-Length", PNG.len().to_string()));
    let stream = connect(f.server.port(), "POST", &f.path(), &headers);
    stream.set_read_timeout(Some(PEER_IO_TIMEOUT / 2)).unwrap();
    assert_eq!(answer(stream).status, 401);
    assert_eq!(
        AttachmentService::new(&f.ctx()).list("SH-1").unwrap().len(),
        1
    );
}

#[test]
fn stalled_binary_upload_releases_its_connection_at_the_body_deadline() {
    let f = Fixture::new();
    let mut headers = f.headers();
    headers.push(("Content-Length", (PNG.len() + 1).to_string()));
    let mut stream = connect(f.server.port(), "POST", &f.path(), &headers);
    stream.write_all(PNG).unwrap();
    let rejected = answer(stream);
    assert_eq!(rejected.status, 400, "{}", rejected.body);
    assert!(
        rejected.body.contains("failed to read attachment upload"),
        "{}",
        rejected.body
    );
    assert!(
        AttachmentService::new(&f.ctx())
            .list("SH-1")
            .unwrap()
            .is_empty()
    );
    assert_eq!(request(&f, "POST", &f.path(), f.headers(), PNG).status, 201);
}
