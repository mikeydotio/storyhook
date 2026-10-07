//! Board pages stay bounded and disposable even when story histories grow.
use serde_json::{Value, json};
use std::sync::Arc;
use storyhook::api::{http::TrustedHosts, rest::route};
use storyhook::cli::parse_invocation;
use storyhook::daemon::http1::Method;
use storyhook::env::Environment;
use storyhook::invoke::{dispatch, dispatch_unscoped};
use storyhook::service::Ctx;
use storyhook::store::{ReadOps, SqliteStore, Store};
use storyhook_test_support::{TestEnv, project_id_at, scratch_dir};

struct Fixture {
    _env: TestEnv,
    store: Arc<SqliteStore>,
    environment: Environment,
    dir: tempfile::TempDir,
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
            parse_invocation(&[
                "project".into(),
                "new".into(),
                "--prefix".into(),
                "SH".into(),
                "--no-agents-md".into(),
            ])
            .unwrap(),
        )
        .unwrap();
        let project = project_id_at(&store, dir.path()).unwrap();
        let repo = store
            .read(|tx| Ok(tx.project(project)?.unwrap().slug))
            .unwrap();
        Self {
            _env: env,
            store,
            environment,
            dir,
            repo,
        }
    }
    fn command(&self, args: &[&str]) {
        let project = project_id_at(&self.store, self.dir.path()).unwrap();
        let ctx = Ctx::new(
            &*self.store,
            project,
            self.dir.path(),
            self.environment.clone(),
        )
        .no_hooks(true);
        dispatch(
            &ctx,
            parse_invocation(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>()).unwrap(),
        )
        .unwrap();
    }
    fn get(&self, tail: &str) -> (u16, Value, usize) {
        let reply = route(
            &*self.store,
            &self.environment,
            &Method::Get,
            &format!("/api/repos/{}/{tail}", self.repo),
            &[],
            "",
            &TrustedHosts::default(),
        )
        .reply;
        (
            reply.status,
            serde_json::from_slice(reply.body()).unwrap(),
            reply.body().len(),
        )
    }
    fn page(&self, options: Value) -> (u16, Value, usize) {
        // Encode all bytes so quotes, ampersands and non-ASCII exercise the
        // same query decoding boundary as encodeURIComponent in the browser.
        let encoded: String = options
            .to_string()
            .bytes()
            .map(|b| format!("%{b:02X}"))
            .collect();
        self.get(&format!("board?options={encoded}"))
    }
}

#[test]
fn board_pages_strip_large_details_but_detail_and_legacy_data_remain_complete() {
    let f = Fixture::new();
    let description = "description-private-marker ".repeat(10_000);
    let comment = "history-private-marker ".repeat(10_000);
    f.command(&["new", "Summary card", "--description", &description]);
    f.command(&["comment", "SH-1", &comment]);
    let (status, page, bytes) = f.page(json!({"limit":50}));
    assert_eq!(status, 200);
    assert!(bytes < 32 * 1024, "bounded card bytes: {bytes}");
    let card = &page["stories"][0];
    assert_eq!(card["is_summary"], true);
    for field in [
        "description",
        "comments",
        "attachments",
        "referenced_by_commits",
    ] {
        assert!(
            card["story"].get(field).is_none(),
            "unexpected detail field {field}"
        );
    }
    assert!(!page.to_string().contains("private-marker"));
    let (_, detail, _) = f.get("story/SH-1");
    assert!(detail.to_string().contains("history-private-marker"));
    let (_, legacy, _) = f.get("data");
    assert!(legacy.to_string().contains("description-private-marker"));
    let project = project_id_at(&f.store, f.dir.path()).unwrap();
    f.store
        .read(|tx| {
            let rows = tx.board_stories(project)?;
            assert!(rows[0].snapshot.comments.is_empty());
            assert!(rows[0].snapshot.description.is_none());
            assert!(rows[0].description.is_none());
            Ok(())
        })
        .unwrap();
}

#[test]
fn board_pages_filter_hidden_columns_and_empty_facets_before_serialization() {
    let f = Fixture::new();
    f.command(&["new", "First & café"]);
    f.command(&["new", "Second"]);
    f.command(&["move", "SH-2", "in-progress"]);
    let (_, page, _) = f.page(json!({"hidden_columns":["in-progress"],"limit":50}));
    assert_eq!(page["stories"].as_array().unwrap().len(), 1);
    assert_eq!(page["counts"]["columns"]["in-progress"], 1);
    assert!(!page["stories"].to_string().contains("Second"));
    for facet in ["states", "types", "priorities"] {
        let mut options = json!({"limit":50});
        options[facet] = json!([]);
        let (_, empty, _) = f.page(options);
        assert_eq!(empty["counts"]["total"], 0);
        assert_eq!(empty["counts"]["all"], 2);
        assert!(empty["stories"].as_array().unwrap().is_empty(), "{facet}");
    }
    let (_, searched, _) = f.page(json!({"text":"& café","sort":"title","dir":1}));
    assert_eq!(searched["stories"][0]["story"]["id"], "SH-1");
    let (_, meta, _) = f.get("board?limit=0");
    assert!(meta["stories"].as_array().unwrap().is_empty());
    assert_eq!(meta["counts"]["total"], 2);
}

#[test]
fn board_pages_bound_card_bytes_and_pin_cursor_to_revision_and_filters() {
    let f = Fixture::new();
    for i in 0..55 {
        f.command(&["new", &format!("Card {i:03}")]);
    }
    let options = json!({"sort":"created","dir":1,"limit":50});
    let (status, first, bytes) = f.page(options.clone());
    assert_eq!(status, 200);
    assert_eq!(first["stories"].as_array().unwrap().len(), 50);
    assert!(bytes < 1024 * 1024);
    let mut next_options = options.clone();
    next_options["cursor"] = first["page"]["next_cursor"].clone();
    let (status, second, _) = f.page(next_options.clone());
    assert_eq!(status, 200);
    assert_eq!(second["stories"].as_array().unwrap().len(), 5);
    assert_eq!(second["stories"][0]["story"]["id"], "SH-51");
    assert!(second["page"]["next_cursor"].is_null());
    let mut changed = next_options.clone();
    changed["text"] = json!("other");
    assert_eq!(f.page(changed).0, 409);
    f.command(&["comment", "SH-1", "same-second revision advancement"]);
    assert_eq!(f.page(next_options).0, 409);
    assert_eq!(f.page(json!({"limit":51})).0, 422);
    assert_eq!(f.page(json!({"sort":"injected sort"})).0, 422);
    assert_eq!(f.page(json!({"cursor":"broken"})).0, 409);
}

#[test]
fn board_pages_catalog_drafts_are_lightweight_and_on_demand() {
    let f = Fixture::new();
    f.command(&[
        "new",
        "Draft",
        "--draft",
        "--description",
        "draft-private-detail",
    ]);
    let (_, page, _) = f.page(json!({"drafts":true}));
    assert_eq!(page["stories"].as_array().unwrap().len(), 1);
    assert!(!page.to_string().contains("draft-private-detail"));
    let reply = route(
        &*f.store,
        &f.environment,
        &Method::Get,
        "/api/repos?board=1",
        &[],
        "",
        &TrustedHosts::default(),
    )
    .reply;
    let catalog: Value = serde_json::from_slice(reply.body()).unwrap();
    assert!(!catalog.to_string().contains("draft-private-detail"));
    assert_eq!(catalog[0]["draft_count"], 1);
}

#[test]
fn board_pages_keep_sort_direction_and_cursor_stable_across_clock_ticks() {
    let mut f = Fixture::new();
    f.command(&["new", "Low", "--priority", "low"]);
    f.command(&["new", "Critical", "--priority", "critical"]);
    f.command(&["new", "Medium", "--priority", "medium"]);
    let (_, first, _) = f.page(json!({"column":"todo","sort":"priority","dir":-1,"limit":1}));
    assert_eq!(first["stories"][0]["story"]["id"], "SH-2");
    let cursor = first["page"]["next_cursor"].clone();
    f.environment = f
        .environment
        .clone()
        .clock(storyhook::service::Clock::Fixed(
            "2030-01-01T00:00:00Z".into(),
        ));
    let (status, second, _) =
        f.page(json!({"column":"todo","sort":"priority","dir":-1,"limit":1,"cursor":cursor}));
    assert_eq!(status, 200);
    assert_eq!(second["stories"][0]["story"]["id"], "SH-3");
    let (_, low_first, _) = f.page(json!({"column":"todo","sort":"priority","dir":1}));
    assert_eq!(low_first["stories"][0]["story"]["id"], "SH-1");
    let (_, list_first, _) = f.page(json!({"sort":"priority","dir":1}));
    assert_eq!(list_first["stories"][0]["story"]["id"], "SH-2");
    let (_, titled, _) = f.page(json!({"sort":"title","dir":1}));
    assert_eq!(titled["stories"][0]["story"]["title"], "Critical");
}
