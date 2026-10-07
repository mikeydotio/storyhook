//! Bounded, disposable board summaries. Full detail remains behind StoryShow.
use super::*;
use crate::output::StoryView;
use serde::Deserialize;
use serde_json::{Value, json};
use std::cmp::Ordering;
use std::hash::{Hash, Hasher};

const MAX_PAGE: usize = 50;
const MAX_CARD_BYTES: usize = 16 * 1024;
const MAX_PAGE_BYTES: usize = 256 * 1024;

#[derive(Debug, Default, Deserialize, serde::Serialize)]
#[serde(default, deny_unknown_fields)]
struct Options {
    column: Option<String>,
    limit: Option<usize>,
    cursor: Option<String>,
    states: Option<Vec<String>>,
    types: Option<Vec<Option<String>>>,
    priorities: Option<Vec<String>>,
    hidden_columns: Vec<String>,
    show_archived: bool,
    text: String,
    sort: Option<String>,
    dir: Option<i8>,
    drafts: bool,
}

fn decode(raw: &str) -> Result<String, AppError> {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let pair = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
                out.push(
                    u8::from_str_radix(pair, 16)
                        .map_err(|_| AppError::Validation("invalid board query escape".into()))?,
                );
                i += 3;
            }
            b'%' => return Err(AppError::Validation("incomplete board query escape".into())),
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8(out).map_err(|_| AppError::Validation("board query must be UTF-8".into()))
}

fn options(path: &str) -> Result<Options, AppError> {
    let query = path.split_once('?').map_or("", |(_, query)| query);
    if query.len() > 16 * 1024 {
        return Err(AppError::Validation("board options exceed 16 KiB".into()));
    }
    let mut result = Options::default();
    for pair in query.split('&').filter(|pair| !pair.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        match key {
            "options" => {
                result = serde_json::from_str(&decode(value)?)
                    .map_err(|e| AppError::Validation(format!("invalid board options: {e}")))?
            }
            "limit" => {
                result.limit = Some(
                    value
                        .parse()
                        .map_err(|_| AppError::Validation("invalid board limit".into()))?,
                )
            }
            _ => return Err(AppError::Validation("unknown board query parameter".into())),
        }
    }
    if result.limit.unwrap_or(MAX_PAGE) > MAX_PAGE {
        return Err(AppError::Validation(
            "board limit must be at most 50".into(),
        ));
    }
    if !matches!(result.dir, None | Some(-1 | 1)) {
        return Err(AppError::Validation(
            "board direction must be -1 or 1".into(),
        ));
    }
    if !matches!(
        result.sort.as_deref(),
        None | Some(
            "next"
                | "order"
                | "priority"
                | "updated"
                | "modified"
                | "created"
                | "added"
                | "completed"
                | "id"
                | "title"
                | "state"
        )
    ) {
        return Err(AppError::Validation("unknown board sort".into()));
    }
    Ok(result)
}

fn bound(value: &mut Value, string_max: usize, array_max: usize) -> bool {
    let mut truncated = false;
    match value {
        Value::String(s) if s.len() > string_max => {
            let mut end = string_max;
            while !s.is_char_boundary(end) {
                end -= 1;
            }
            s.truncate(end);
            truncated = true;
        }
        Value::Array(values) => {
            truncated = values.len() > array_max;
            values.truncate(array_max);
            for value in values {
                truncated |= bound(value, string_max, array_max);
            }
        }
        Value::Object(values) => {
            for value in values.values_mut() {
                truncated |= bound(value, string_max, array_max);
            }
        }
        _ => {}
    }
    truncated
}

pub(super) fn card_json(view: &StoryView) -> Value {
    let mut value = serde_json::to_value(view).unwrap_or(Value::Null);
    let story = value["story"]
        .as_object_mut()
        .expect("story view serializes an object");
    for key in [
        "description",
        "comments",
        "attachments",
        "referenced_by_commits",
    ] {
        story.remove(key);
    }
    let mut truncated = false;
    // Display text may be shortened. Identifiers, labels, state/type slugs
    // and relationship endpoints must never be rewritten by a summary.
    for key in ["title", "awaiting"] {
        if let Some(text) = story.get_mut(key) {
            truncated |= bound(text, 512, usize::MAX);
        }
    }
    for key in ["labels", "relationships"] {
        if let Some(items) = story.get_mut(key).and_then(Value::as_array_mut) {
            if key == "labels" {
                // SH-454: policy labels must survive summary elision, just
                // as the card puts them before its three visible chips.
                items.sort_by_key(|label| !is_policy_label(label));
            }
            truncated |= items.len() > 16;
            items.truncate(16);
            let before = items.len();
            items.retain(|item| item.to_string().len() <= 1024);
            truncated |= items.len() != before;
        }
    }
    let object = value
        .as_object_mut()
        .expect("story view serializes an object");
    for key in [
        "derived_relationships",
        "referenced_by",
        "continuation_alerts",
    ] {
        object.remove(key);
    }
    for key in ["warnings", "flagged_reasons"] {
        if let Some(items) = object.get_mut(key) {
            truncated |= bound(items, 256, 8);
        }
    }
    if let Some(reset) = object.get_mut("reset") {
        // The card needs the identity/active marker; detailed progress and
        // diagnostics belong to the on-demand drawer.
        if let Some(reset) = reset.as_object_mut() {
            reset.retain(|key, _| matches!(key.as_str(), "operation" | "completed"));
        }
    }
    value["continuation_needs_attention"] = json!(!view.continuation_alerts.is_empty());
    value["is_summary"] = json!(true);
    value["summary_truncated"] = json!(truncated);
    finish_card(&mut value);
    value
}

fn is_policy_label(value: &Value) -> bool {
    matches!(value.as_str(), Some("human-only" | "no-auto"))
}

fn finish_card(card: &mut Value) {
    if card.to_string().len() <= MAX_CARD_BYTES {
        return;
    }
    for key in [
        "open_prs",
        "verification",
        "reset",
        "warnings",
        "flagged_reasons",
    ] {
        card.as_object_mut().unwrap().remove(key);
    }
    card["story"]["relationships"] = json!([]);
    let policy_labels: Vec<_> = card["story"]["labels"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|value| is_policy_label(value))
        .cloned()
        .collect();
    card["story"]["labels"] = json!(policy_labels);
    card["summary_truncated"] = json!(true);
}

fn fingerprint(value: &impl Hash) -> String {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    value.hash(&mut h);
    format!("{:016x}", h.finish())
}

fn selected<T: PartialEq>(selection: &Option<Vec<T>>, value: &T) -> bool {
    selection.as_ref().is_none_or(|items| items.contains(value))
}

fn reference_metadata<'a>(
    views: impl Iterator<Item = &'a StoryView>,
    ids: &BTreeSet<String>,
) -> Value {
    let mut refs: serde_json::Map<String, Value> =
        ids.iter().map(|id| (id.clone(), Value::Null)).collect();
    for view in views.filter(|view| ids.contains(&view.story.id)) {
        refs.insert(view.story.id.clone(), json!({"state":view.story.state,"superstate":view.story.superstate,"display_state":view.display_state}));
    }
    Value::Object(refs)
}

pub(super) fn detail_reply<S: Store>(ctx: &Ctx<'_, S>, id: &str) -> Reply {
    let reply = reply_with(ctx, 200, Invocation::Show { id: id.to_string() });
    if reply.status != 200 {
        return reply;
    }
    let result = (|| -> Result<Reply, AppError> {
        let mut response: Value = serde_json::from_slice(reply.body())?;
        let mut ids = BTreeSet::new();
        if let Some(relations) = response["story"]["story"]["relationships"].as_array() {
            for relation in relations.iter().take(256) {
                if let Some(id) = relation["other_id"].as_str() {
                    ids.insert(id.to_string());
                }
            }
            response["refs_truncated"] = json!(relations.len() > 256);
        }
        let refs = ctx.store().read(|tx| {
            Ok(QueryService::new(tx, ctx.project(), &ctx.now())
                .board_data()
                .map(|data| reference_metadata(data.stories.iter(), &ids)))
        })??;
        response["refs"] = refs;
        Ok(json_reply(200, response.to_string()).no_store())
    })();
    result.unwrap_or_else(|e| error_reply(&e))
}

fn verification_position(verifier: &Value, id: &str) -> usize {
    if verifier["active"]["story_id"].as_str() == Some(id) {
        return 0;
    }
    verifier["verifying"]
        .as_array()
        .and_then(|ids| {
            ids.iter()
                .position(|candidate| candidate.as_str() == Some(id))
        })
        .map_or(usize::MAX, |position| position + 1)
}

fn compare_write_order(a: &StoryView, b: &StoryView) -> Ordering {
    match (a.head_global_seq, b.head_global_seq) {
        (Some(a), Some(b)) => a.cmp(&b),
        _ => Ordering::Equal,
    }
}

fn compare_cards(
    a: &StoryView,
    b: &StoryView,
    options: &Options,
    ranks: &BTreeMap<&str, usize>,
    verification_rank: &impl Fn(&str) -> usize,
) -> Ordering {
    let sa = &a.story;
    let sb = &b.story;
    let fallback = crate::domain::story_number(&sa.id)
        .cmp(&crate::domain::story_number(&sb.id))
        .then_with(|| sa.id.cmp(&sb.id));
    let reverse = |order: Ordering| {
        if options.dir.unwrap_or(-1) == -1 {
            order.reverse()
        } else {
            order
        }
    };
    match options.sort.as_deref().unwrap_or("priority") {
        "next" | "order" if options.column.as_deref() == Some("verifying") => {
            let order = verification_rank(&sa.id).cmp(&verification_rank(&sb.id));
            reverse(order.then(fallback))
        }
        "next" | "order" => match (ranks.get(sa.id.as_str()), ranks.get(sb.id.as_str())) {
            (Some(a), Some(b)) => reverse(a.cmp(b)),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            _ => fallback,
        },
        "priority" => {
            let left = a.blocker_floor.as_ref().unwrap_or(&sa.priority);
            let right = b.blocker_floor.as_ref().unwrap_or(&sb.priority);
            let order = left.cmp(right);
            // Board arrows mean urgency; List arrows mean numeric rank.
            let order = if options.column.is_some() {
                order.reverse()
            } else {
                order
            };
            reverse(order).then(fallback)
        }
        "title" => reverse(sa.title.to_lowercase().cmp(&sb.title.to_lowercase())).then(fallback),
        "state" => {
            let left = a.display_state.as_ref().unwrap_or(&sa.state);
            let right = b.display_state.as_ref().unwrap_or(&sb.state);
            reverse(left.cmp(right)).then(fallback)
        }
        "created" | "added" => reverse(sa.created_at.cmp(&sb.created_at).then(fallback)),
        "completed" => match (&sa.closed_at, &sb.closed_at) {
            (Some(left), Some(right)) => {
                let order = left.cmp(right).then(compare_write_order(a, b));
                reverse(order.then(fallback))
            }
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            _ => reverse(compare_write_order(a, b).then(fallback)),
        },
        "updated" | "modified" => {
            let order = sa
                .updated_at
                .cmp(&sb.updated_at)
                .then(compare_write_order(a, b));
            reverse(order.then(fallback))
        }
        "id" if options.column.is_none() => reverse(sa.id.cmp(&sb.id)),
        _ => reverse(fallback),
    }
}

pub(super) fn reply<S: Store>(
    ctx: &Ctx<'_, S>,
    activity: &VerificationActivity,
    path: &str,
) -> Reply {
    let mut opts = match options(path) {
        Ok(opts) => opts,
        Err(e) => return error_reply(&e),
    };
    let cursor = opts.cursor.take();
    let result = activity.read_project(ctx.store(), ctx.project(), |tx, owner, control| {
        Ok((|| -> Result<Reply, AppError> {
            let project = ctx.project();
            let data = QueryService::new(tx, project, &ctx.now()).board_data()?;
            let mut metadata = meta_json(tx, project, &data)?;
            // Filter suggestions are metadata, not a cache of every label ever
            // used. Keep their completeness explicit; server filtering itself
            // still evaluates the complete graph.
            let labels_total = metadata["labels"].as_array().map_or(0, Vec::len);
            if let Some(labels) = metadata["labels"].as_array_mut() { labels.truncate(256); }
            metadata["labels_total"] = json!(labels_total);
            metadata["labels_truncated"] = json!(labels_total > 256);
            for vocabulary in ["states", "types"] {
                if let Some(entries) = metadata[vocabulary].as_array_mut() {
                    for entry in entries {
                        if let Some(description) = entry.get_mut("description") { bound(description, 512, 16); }
                    }
                }
            }
            let (verifier, verification) = crate::daemon::verification::status::snapshot(tx, ctx, owner, control)?;
            let verifier_json = serde_json::to_value(&verifier)?;
            let revision_inputs = (
                tx.max_global_seq(project)?.get(),
                metadata.to_string(),
                tx.automations_enabled(project)?,
                verifier_json["verifying"].to_string(),
                verifier_json["active"]["story_id"].to_string(),
            );
            let revision = fingerprint(&revision_inputs);
            let scope = fingerprint(&serde_json::to_string(&opts)?);
            let offset = match &cursor {
                None => 0,
                Some(cursor) => {
                    let pieces: Vec<_> = cursor.split(':').collect();
                    if pieces.len() != 3 || pieces[0] != revision || pieces[1] != scope {
                        return Ok(json_reply(409, json!({"error":"board page changed; restart pagination", "restart":true}).to_string()).no_store());
                    }
                    pieces[2].parse::<usize>().map_err(|_| AppError::Validation("invalid board cursor".into()))?
                }
            };
            let mut columns: BTreeMap<String, usize> = BTreeMap::new();
            let text = opts.text.to_lowercase();
            let matching = |view: &&StoryView| {
                let s = &view.story;
                let shown = view.display_state.as_ref().unwrap_or(&s.state);
                s.draft == opts.drafts && (opts.show_archived || s.hidden_at.is_none())
                    && selected(&opts.states, shown) && selected(&opts.types, &s.story_type)
                    && selected(&opts.priorities, &s.priority.as_str().to_string())
                    && (text.is_empty() || format!("{} {} {}", s.id, s.title, s.labels.join(" ")).to_lowercase().contains(&text))
            };
            let mut views: Vec<_> = data.stories.iter().filter(matching).collect();
            for view in &views { *columns.entry(view.display_state.as_ref().unwrap_or(&view.story.state).clone()).or_default() += 1; }
            let total = views.len();
            views.retain(|view| {
                let shown = view.display_state.as_ref().unwrap_or(&view.story.state);
                !opts.hidden_columns.contains(shown) && opts.column.as_ref().is_none_or(|column| column == shown)
            });
            let ranks: BTreeMap<_, _> = data.next_ids.iter().enumerate().map(|(i, id)| (id.as_str(), i)).collect();
            let verification_rank = |id: &str| verification_position(&verifier_json, id);
            views.sort_by(|a, b| compare_cards(a, b, &opts, &ranks, &verification_rank));
            let page_total = views.len();
            let mut cards = Vec::new();
            let mut reference_ids = BTreeSet::new();
            let mut bytes = 0;
            let mut ids = BTreeSet::new();
            let prs = tx.open_pr_links(project)?;
            let prefix = tx.project(project)?.ok_or_else(|| AppError::NotFound("project".into()))?;
            for view in views.into_iter().skip(offset).take(opts.limit.unwrap_or(MAX_PAGE)) {
                let mut card = card_json(view);
                card["is_ready"] = json!(data.ready_ids.contains(&view.story.id));
                card["is_blocked"] = json!(data.blocked_ids.contains(&view.story.id));
                card["next_rank"] = json!(ranks.get(view.story.id.as_str()));
                let links: Vec<_> = prs.iter().filter(|(no, _)| no.to_id(&prefix.prefix) == view.story.id).take(16).map(|(_, link)| link).collect();
                card["open_prs"] = serde_json::to_value(links)?;
                if tx.automations_enabled(project)? && let Some((_, _, status)) = verification.iter().find(|(p, id, _)| *p == project && id == &view.story.id) { card["verification"] = serde_json::to_value(status)?; }
                finish_card(&mut card);
                let size = card.to_string().len();
                if size > MAX_CARD_BYTES {
                    return Ok(json_reply(413, json!({"error":"story identifiers exceed the 16 KiB card budget", "story_id":view.story.id}).to_string()).no_store());
                }
                if !cards.is_empty() && bytes + size > MAX_PAGE_BYTES { break; }
                bytes += size;
                if let Some(relations) = card["story"]["relationships"].as_array() {
                    for relation in relations {
                        if let Some(id) = relation["other_id"].as_str() { reference_ids.insert(id.to_string()); }
                    }
                }
                ids.insert(view.story.id.clone());
                cards.push(card);
            }
            let next_offset = offset.saturating_add(cards.len());
            let next_cursor = (next_offset < page_total && !cards.is_empty()).then(|| format!("{revision}:{scope}:{next_offset}"));
            let response = json!({
                "stories":cards,"refs":reference_metadata(data.stories.iter(), &reference_ids),"drafts":[],"summary":data.summary,"meta":metadata,
                "highest_story_number":prefix.next_story_no - 1,
                "ready_ids":data.ready_ids.iter().filter(|id| ids.contains(*id)).collect::<Vec<_>>(),
                "blocked_ids":data.blocked_ids.iter().filter(|id| ids.contains(*id)).collect::<Vec<_>>(),
                "next_ids":data.next_ids.iter().filter(|id| ids.contains(*id)).collect::<Vec<_>>(),
                "automations_enabled":tx.automations_enabled(project)?,"verification_control":{"state":control},
                "verification_incident":verifier_json["incident"],"verifier":verifier_json,
                "counts":{"columns":columns,"drafts":data.stories.iter().filter(|v| v.story.draft).count(),"total":total,"all":data.stories.iter().filter(|v| v.story.draft == opts.drafts).count()},
                "page":{"total":page_total,"next_cursor":next_cursor,"revision":revision,"offset":offset,"limit":opts.limit.unwrap_or(MAX_PAGE)}
            }).to_string();
            // Every successful response is bounded even for adversarial
            // configuration vocabularies, independently of story count.
            if response.len() > 1024 * 1024 {
                return Ok(json_reply(413, json!({"error":"board metadata exceeds the 1 MiB response budget; narrow the project vocabulary"}).to_string()).no_store());
            }
            Ok(json_reply(200, response).no_store())
        })())
    });
    match result {
        Ok(Ok(reply)) => reply,
        Ok(Err(e)) => error_reply(&e),
        Err(e) => error_reply(&e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_summary_preserves_identifiers_and_never_leaks_oversize_diagnostics() {
        let identity = "SH-123";
        let state = "state-with-a-real-identity";
        let value = json!({
            "story": {"id":identity,"title":"é".repeat(100_000),"created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z","state":state,"superstate":"OPEN","closed_at":null,
                "labels":(0..100).map(|i|format!("label-{i}-{}", "x".repeat(2000))).collect::<Vec<_>>(),
                "relationships":(0..100).map(|i|json!({"relation":"blocked-by","other_id":format!("SH-{i}")})).collect::<Vec<_>>(),
                "comments":[],"description":"private full description", "awaiting":"z".repeat(100_000)},
            "warnings":vec!["diagnostic".repeat(100_000); 20],"flagged_reasons":vec!["problem".repeat(100_000);20],
            "continuation_alerts":[],"referenced_by":{},"derived_relationships":[]
        });
        let view: StoryView = serde_json::from_value(value).unwrap();
        let mut card = card_json(&view);
        card["open_prs"] =
            json!([{"url":"https://example.invalid/".to_string()+ &"x".repeat(100_000)}]);
        finish_card(&mut card);
        assert!(card.to_string().len() <= MAX_CARD_BYTES);
        assert_eq!(card["story"]["id"], identity);
        assert_eq!(card["story"]["state"], state);
        assert_eq!(card["summary_truncated"], true);
        assert!(
            card["story"]["title"]
                .as_str()
                .unwrap()
                .is_char_boundary(card["story"]["title"].as_str().unwrap().len())
        );
        assert!(!card.to_string().contains("private full description"));
    }

    fn view(id: &str, sequence: i64) -> StoryView {
        serde_json::from_value(json!({
            "story": {"id":id,"title":id,"created_at":"2026-01-01T00:00:00Z",
                "updated_at":"2026-01-01T00:00:00Z","state":"todo",
                "superstate":"OPEN","closed_at":null},
            "flagged_reasons":[],"head_global_seq":sequence
        }))
        .unwrap()
    }

    fn ordered_ids(
        views: &mut [StoryView],
        options: &Options,
        ranks: &BTreeMap<&str, usize>,
        verifier: &Value,
    ) -> Vec<String> {
        views.sort_by(|a, b| {
            compare_cards(a, b, options, ranks, &|id| {
                verification_position(verifier, id)
            })
        });
        views.iter().map(|view| view.story.id.clone()).collect()
    }

    #[test]
    fn board_summary_timestamp_order_uses_write_sequence_before_story_number() {
        for sort in ["modified", "updated", "completed"] {
            for column in [None, Some("done".to_string())] {
                for dir in [1, -1] {
                    let mut views = vec![view("SH-1", 30), view("SH-3", 10), view("SH-2", 20)];
                    if sort == "completed" {
                        for view in &mut views {
                            view.story.closed_at = Some("2026-01-02T00:00:00Z".into());
                            view.story.superstate = crate::domain::SuperState::Closed;
                            view.story.state = "done".into();
                        }
                    }
                    let options = Options {
                        column: column.clone(),
                        sort: Some(sort.into()),
                        dir: Some(dir),
                        ..Options::default()
                    };
                    let expected = if dir == 1 {
                        vec!["SH-3", "SH-2", "SH-1"]
                    } else {
                        vec!["SH-1", "SH-2", "SH-3"]
                    };
                    assert_eq!(
                        ordered_ids(&mut views, &options, &BTreeMap::new(), &Value::Null),
                        expected,
                        "{sort} {dir}"
                    );
                }
            }
        }
    }

    #[test]
    fn board_summary_next_order_keeps_unranked_cards_last_in_both_directions() {
        let ranks = BTreeMap::from([("SH-3", 0), ("SH-1", 1)]);
        for column in [None, Some("todo".to_string())] {
            for dir in [1, -1] {
                let mut views = vec![view("SH-1", 1), view("SH-2", 2), view("SH-3", 3)];
                let sort = if column.is_some() { "next" } else { "order" };
                let options = Options {
                    column: column.clone(),
                    sort: Some(sort.into()),
                    dir: Some(dir),
                    ..Options::default()
                };
                let expected = if dir == 1 {
                    vec!["SH-3", "SH-1", "SH-2"]
                } else {
                    vec!["SH-1", "SH-3", "SH-2"]
                };
                assert_eq!(
                    ordered_ids(&mut views, &options, &ranks, &Value::Null),
                    expected
                );
            }
        }
    }

    #[test]
    fn board_summary_verifying_order_preserves_owner_queue_and_reversed_numeric_ties() {
        let scenarios = [
            (
                json!({"active":{"story_id":"SH-30"},"verifying":["SH-20","SH-10","SH-30"]}),
                vec!["SH-30", "SH-20", "SH-10", "SH-08", "SH-8"],
            ),
            (
                json!({"active":{"story_id":"SH-30"},"verifying":["SH-20","SH-10"]}),
                vec!["SH-30", "SH-20", "SH-10", "SH-08", "SH-8"],
            ),
            (
                json!({"active":null,"verifying":["SH-20","SH-10"]}),
                vec!["SH-20", "SH-10", "SH-08", "SH-8", "SH-30"],
            ),
            (
                json!({"active":{"story_id":"SH-99"},"verifying":["SH-20","SH-10"]}),
                vec!["SH-20", "SH-10", "SH-08", "SH-8", "SH-30"],
            ),
            (
                json!({"active":{"story_id":""},"verifying":["SH-20","SH-10"]}),
                vec!["SH-20", "SH-10", "SH-08", "SH-8", "SH-30"],
            ),
            (
                json!({"active":{},"verifying":[]}),
                vec!["SH-08", "SH-8", "SH-10", "SH-20", "SH-30"],
            ),
            (
                Value::Null,
                vec!["SH-08", "SH-8", "SH-10", "SH-20", "SH-30"],
            ),
        ];
        for (verifier, expected) in scenarios {
            for dir in [1, -1] {
                let mut views: Vec<_> = ["SH-8", "SH-30", "SH-10", "SH-08", "SH-20"]
                    .into_iter()
                    .map(|id| view(id, 1))
                    .collect();
                let options = Options {
                    column: Some("verifying".into()),
                    sort: Some("next".into()),
                    dir: Some(dir),
                    ..Options::default()
                };
                let mut expected = expected.clone();
                if dir == -1 {
                    expected.reverse();
                }
                assert_eq!(
                    ordered_ids(&mut views, &options, &BTreeMap::new(), &verifier),
                    expected
                );
            }
        }
    }

    #[test]
    fn board_summary_completed_handles_missing_timestamps_without_reversing_absence() {
        for dir in [1, -1] {
            let mut views = vec![view("SH-1", 30), view("SH-2", 20), view("SH-3", 10)];
            views[2].story.closed_at = Some("2026-01-02T00:00:00Z".into());
            let options = Options {
                column: Some("done".into()),
                sort: Some("completed".into()),
                dir: Some(dir),
                ..Options::default()
            };
            let expected = if dir == 1 {
                vec!["SH-3", "SH-2", "SH-1"]
            } else {
                vec!["SH-3", "SH-1", "SH-2"]
            };
            assert_eq!(
                ordered_ids(&mut views, &options, &BTreeMap::new(), &Value::Null),
                expected
            );
            // A missing write ordinal is not smaller than a known one: it
            // restores the same numeric fallback used by the old browser.
            views.iter_mut().for_each(|view| {
                if view.story.id == "SH-2" {
                    view.head_global_seq = None;
                }
            });
            let expected = if dir == 1 {
                vec!["SH-3", "SH-1", "SH-2"]
            } else {
                vec!["SH-3", "SH-2", "SH-1"]
            };
            assert_eq!(
                ordered_ids(&mut views, &options, &BTreeMap::new(), &Value::Null),
                expected
            );
        }
    }

    #[test]
    fn board_summary_list_id_and_displayed_state_sort_by_the_visible_text() {
        for dir in [1, -1] {
            let mut views = vec![view("SH-2", 1), view("SH-10", 2)];
            let options = Options {
                sort: Some("id".into()),
                dir: Some(dir),
                ..Options::default()
            };
            let expected = if dir == 1 {
                vec!["SH-10", "SH-2"]
            } else {
                vec!["SH-2", "SH-10"]
            };
            assert_eq!(
                ordered_ids(&mut views, &options, &BTreeMap::new(), &Value::Null),
                expected
            );
            views.iter_mut().for_each(|view| {
                view.story.state = "todo".into();
                view.display_state = Some(
                    if view.story.id == "SH-2" {
                        "blocked"
                    } else {
                        "in-progress"
                    }
                    .into(),
                );
            });
            let options = Options {
                sort: Some("state".into()),
                dir: Some(dir),
                ..Options::default()
            };
            let expected = if dir == 1 {
                vec!["SH-2", "SH-10"]
            } else {
                vec!["SH-10", "SH-2"]
            };
            assert_eq!(
                ordered_ids(&mut views, &options, &BTreeMap::new(), &Value::Null),
                expected
            );
        }
    }

    #[test]
    fn board_summary_preserves_reserved_policy_labels_through_all_elision() {
        let mut view = view("SH-1", 1);
        view.story.labels = (0..30).map(|index| format!("a-{index:02}")).collect();
        view.story
            .labels
            .extend(["human-only".into(), "no-auto".into()]);
        let mut card = card_json(&view);
        let labels = card["story"]["labels"].as_array().unwrap();
        assert_eq!(labels.len(), 16);
        assert_eq!(labels[0], "human-only");
        assert_eq!(labels[1], "no-auto");
        assert!(
            view.story
                .labels
                .iter()
                .any(|label| label == labels[15].as_str().unwrap())
        );
        assert_eq!(card["summary_truncated"], true);
        card["verification"] = json!({"detail":"oversized diagnostic".repeat(10_000)});
        finish_card(&mut card);
        assert_eq!(card["story"]["labels"], json!(["human-only", "no-auto"]));
        assert!(card.to_string().len() <= MAX_CARD_BYTES);
        assert_eq!(card["story"]["id"], "SH-1");
    }
}
