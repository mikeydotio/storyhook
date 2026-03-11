use serde::Serialize;

use crate::domain::StorySnapshot;
use crate::error::AppError;

#[derive(Clone, Debug, Serialize)]
pub struct StoryView {
    pub story: StorySnapshot,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub flagged_reasons: Vec<String>,
}

#[derive(Clone, Debug)]
pub enum Response {
    Message(String),
    Story(Box<StoryView>),
    Stories(Vec<StoryView>),
    Issues(Vec<String>),
}

#[derive(Serialize)]
struct JsonEnvelope<'a> {
    result: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    story: Option<&'a StoryView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stories: Option<&'a [StoryView]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    issues: Option<&'a [String]>,
    #[serde(default, skip_serializing_if = "<[_]>::is_empty")]
    warnings: &'a [String],
    #[serde(default, skip_serializing_if = "<[_]>::is_empty")]
    flagged_reasons: &'a [String],
}

pub fn render_response(response: &Response, json: bool, quiet: bool) -> String {
    if quiet {
        return String::new();
    }

    if json {
        return render_json(response);
    }

    render_human(response)
}

pub fn render_error(error: &AppError, json: bool) -> String {
    if json {
        return format!(
            "{}\n",
            serde_json::json!({
                "result": "error",
                "error": error.to_string(),
                "exit_code": error.exit_code(),
            })
        );
    }

    format!("error: {error}\n")
}

fn render_json(response: &Response) -> String {
    let rendered = match response {
        Response::Message(message) => serde_json::to_string_pretty(&JsonEnvelope {
            result: "ok",
            message: Some(message),
            story: None,
            stories: None,
            issues: None,
            warnings: &[],
            flagged_reasons: &[],
        }),
        Response::Story(view) => serde_json::to_string_pretty(&JsonEnvelope {
            result: "ok",
            message: None,
            story: Some(view.as_ref()),
            stories: None,
            issues: None,
            warnings: &view.warnings,
            flagged_reasons: &view.flagged_reasons,
        }),
        Response::Stories(stories) => serde_json::to_string_pretty(&JsonEnvelope {
            result: "ok",
            message: None,
            story: None,
            stories: Some(stories),
            issues: None,
            warnings: &[],
            flagged_reasons: &[],
        }),
        Response::Issues(issues) => serde_json::to_string_pretty(&JsonEnvelope {
            result: "ok",
            message: None,
            story: None,
            stories: None,
            issues: Some(issues),
            warnings: &[],
            flagged_reasons: &[],
        }),
    }
    .expect("response should serialize");

    format!("{rendered}\n")
}

fn render_human(response: &Response) -> String {
    match response {
        Response::Message(message) => format!("{message}\n"),
        Response::Story(view) => render_story(view),
        Response::Stories(stories) => {
            if stories.is_empty() {
                return "no stories found\n".to_string();
            }

            let mut body = String::new();
            for story in stories {
                let flagged = if story.flagged_reasons.is_empty() {
                    ""
                } else {
                    " [flagged]"
                };
                body.push_str(&format!(
                    "{} [{}] {}{}\n",
                    story.story.id, story.story.state, story.story.title, flagged
                ));
            }
            body
        }
        Response::Issues(issues) => {
            if issues.is_empty() {
                return "no integrity issues found\n".to_string();
            }
            let mut body = String::new();
            for issue in issues {
                body.push_str(issue);
                body.push('\n');
            }
            body
        }
    }
}

fn render_story(view: &StoryView) -> String {
    let story = &view.story;
    let assignee = story.assignee.as_deref().unwrap_or("-");
    let mut body = String::new();
    body.push_str(&format!("{} {}\n", story.id, story.title));
    body.push_str(&format!(
        "state: {} ({})\n",
        story.state,
        story.superstate.as_str()
    ));
    body.push_str(&format!("assignee: {assignee}\n"));

    if let Some(closed_at) = &story.closed_at {
        body.push_str(&format!("closed_at: {closed_at}\n"));
    }

    if view.flagged_reasons.is_empty() {
        body.push_str("flagged: no\n");
    } else {
        body.push_str("flagged: yes\n");
        for reason in &view.flagged_reasons {
            body.push_str(&format!("flagged_reason: {reason}\n"));
        }
    }

    if !story.relationships.is_empty() {
        body.push_str("relationships:\n");
        for relation in &story.relationships {
            body.push_str(&format!("- {} {}\n", relation.relation, relation.other_id));
        }
    }

    if !story.comments.is_empty() {
        body.push_str("comments:\n");
        for comment in &story.comments {
            body.push_str(&format!("- {} {}\n", comment.at, comment.text));
        }
    }

    body
}
