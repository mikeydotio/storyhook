//! Per-token project visibility through the dashboard's routed API.

use std::sync::Arc;
use std::time::Instant;

use chrono::Utc;
use storyhook::api::http::TrustedHosts;
use storyhook::api::rest::{Changed, RouteRequest, route_with_activity};
use storyhook::api::tokens::{DEFAULT_TTL, TokenRegistry};
use storyhook::cli::parse_invocation;
use storyhook::daemon::http1::{Header, Method};
use storyhook::daemon::verification::VerificationActivity;
use storyhook::env::Environment;
use storyhook::invoke::dispatch_unscoped;
use storyhook::store::{ReadOps, SqliteStore, Store, WriteOps};
use storyhook_test_support::{TestEnv, scratch_dir};

struct Fixture {
    _env: TestEnv,
    store: Arc<SqliteStore>,
    environment: Environment,
    registry: TokenRegistry,
    slug: String,
    first: String,
    second: String,
}

impl Fixture {
    fn new() -> Self {
        let env = TestEnv::isolated();
        let store = Arc::new(env.open_store());
        let environment = env.environment();
        let dir = scratch_dir();
        dispatch_unscoped(
            &*store,
            &environment,
            dir.path(),
            "2026-01-01T00:00:00Z",
            parse_invocation(&[
                "project".into(),
                "new".into(),
                "--prefix".into(),
                "PV".into(),
                "--no-agents-md".into(),
            ])
            .unwrap(),
        )
        .unwrap();
        let slug = store
            .read(|tx| Ok(tx.projects()?.into_iter().next().unwrap().slug))
            .unwrap();
        let registry = TokenRegistry::load(&environment);
        let first = registry
            .mint("first".into(), Utc::now(), Instant::now(), DEFAULT_TTL)
            .unwrap()
            .secret;
        let second = registry
            .mint("second".into(), Utc::now(), Instant::now(), DEFAULT_TTL)
            .unwrap()
            .secret;
        Self {
            _env: env,
            store,
            environment,
            registry,
            slug,
            first,
            second,
        }
    }

    fn request(
        &self,
        method: Method,
        path: &str,
        body: &str,
        secret: Option<&str>,
        guard: bool,
    ) -> storyhook::api::rest::Routed {
        let mut headers = vec![Header::from_bytes("Host", "127.0.0.1:3456").unwrap()];
        if guard {
            headers.push(Header::from_bytes("X-Storyhook", "1").unwrap());
            headers.push(Header::from_bytes("Content-Type", "application/json").unwrap());
        }
        if let Some(secret) = secret {
            headers.push(Header::from_bytes("X-Storyhook-Token", secret).unwrap());
        }
        self.request_headers(method, path, body, &headers)
    }

    fn request_headers(
        &self,
        method: Method,
        path: &str,
        body: &str,
        headers: &[Header],
    ) -> storyhook::api::rest::Routed {
        route_with_activity(
            &*self.store,
            &self.environment,
            &VerificationActivity::new(),
            RouteRequest::new(&method, path, headers, body).with_token_context(
                &self.registry,
                "storyhook_test",
                "master",
            ),
            &TrustedHosts::default(),
        )
    }

    fn catalog(&self, secret: &str) -> serde_json::Value {
        let reply = self.request(Method::Get, "/api/repos", "", Some(secret), true);
        assert_eq!(reply.reply.status, 200);
        serde_json::from_slice(reply.reply.body()).unwrap()
    }
}

#[test]
fn cookie_credential_can_change_visibility_and_read_its_own_catalog() {
    let fixture = Fixture::new();
    let cookie = format!("storyhook_test={}", fixture.first);
    let headers = [
        Header::from_bytes("Host", "127.0.0.1:3456").unwrap(),
        Header::from_bytes("X-Storyhook", "1").unwrap(),
        Header::from_bytes("Content-Type", "application/json").unwrap(),
        Header::from_bytes("Cookie", cookie).unwrap(),
    ];
    let path = format!("/api/repos/{}/visibility", fixture.slug);
    let changed = fixture.request_headers(Method::Patch, &path, r#"{"visible":false}"#, &headers);
    assert_eq!(changed.reply.status, 200);
    let catalog = fixture.request_headers(Method::Get, "/api/repos", "", &headers);
    assert_eq!(catalog.reply.status, 200);
    let repos: serde_json::Value = serde_json::from_slice(catalog.reply.body()).unwrap();
    assert_eq!(repos[0]["visible"], false);
    assert_eq!(fixture.catalog(&fixture.second)[0]["visible"], true);
}

#[test]
fn visibility_can_be_changed_for_a_project_without_a_checkout() {
    let fixture = Fixture::new();
    fixture
        .store
        .write(|tx| {
            let project = tx.projects()?.into_iter().next().unwrap();
            tx.set_checkout_path(project.id, None)
        })
        .unwrap();

    let path = format!("/api/repos/{}/visibility", fixture.slug);
    let changed = fixture.request(
        Method::Patch,
        &path,
        r#"{"visible":false}"#,
        Some(&fixture.first),
        true,
    );
    assert_eq!(changed.reply.status, 200);
    assert_eq!(fixture.catalog(&fixture.first)[0]["visible"], false);
}

#[test]
fn visibility_is_per_token_and_full_catalog_remains_available() {
    let fixture = Fixture::new();
    let path = format!("/api/repos/{}/visibility", fixture.slug);
    assert_eq!(fixture.catalog(&fixture.first)[0]["visible"], true);

    let changed = fixture.request(
        Method::Patch,
        &path,
        r#"{"visible":false}"#,
        Some(&fixture.first),
        true,
    );
    assert_eq!(changed.reply.status, 200);
    assert_eq!(changed.changed, Some(Changed::Catalog));
    assert_eq!(fixture.catalog(&fixture.first)[0]["visible"], false);
    assert_eq!(fixture.catalog(&fixture.second)[0]["visible"], true);

    let restored = fixture.request(
        Method::Patch,
        &path,
        r#"{"visible":true}"#,
        Some(&fixture.first),
        true,
    );
    assert_eq!(restored.reply.status, 200);
    assert_eq!(fixture.catalog(&fixture.first)[0]["visible"], true);
}

#[test]
fn visibility_patch_rejects_invalid_inputs_and_never_changes_the_catalog() {
    let fixture = Fixture::new();
    let path = format!("/api/repos/{}/visibility", fixture.slug);
    for (target, body, secret, guard, status) in [
        (
            path.as_str(),
            r#"{"visible":false}"#,
            Some(fixture.first.as_str()),
            false,
            403,
        ),
        (
            path.as_str(),
            r#"{"visible":false}"#,
            Some("wrong"),
            true,
            401,
        ),
        (
            path.as_str(),
            r#"{"visible":false}"#,
            Some("master"),
            true,
            403,
        ),
        (path.as_str(), "{}", Some(fixture.first.as_str()), true, 400),
        (
            path.as_str(),
            r#"{"visible":"false"}"#,
            Some(fixture.first.as_str()),
            true,
            400,
        ),
        (
            "/api/repos/unknown/visibility",
            r#"{"visible":false}"#,
            Some(fixture.first.as_str()),
            true,
            404,
        ),
    ] {
        let reply = fixture.request(Method::Patch, target, body, secret, guard);
        assert_eq!(reply.reply.status, status, "{target} {body}");
        assert_eq!(reply.changed, None);
    }
    assert_eq!(fixture.catalog(&fixture.first)[0]["visible"], true);
}
