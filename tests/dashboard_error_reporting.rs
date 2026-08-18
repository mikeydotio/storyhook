//! Pins every path that renders a failed request's message to the user
//! through one honest describer (SH-367).
//!
//! ## The defect this fences
//!
//! `api()` rejects with `{status, message, body}`, and `status === 0` is its
//! signal that **no reply ever arrived** — `onerror` (network failure) or
//! `ontimeout` (the client gave up waiting). Neither means the daemon did not
//! act: a mutation's deadline is set to the server's own documented worst case
//! (`MUTATION_TIMEOUT_MS`, derived from `HOOK_TIMEOUT_CEILING_SECS`), so a
//! timeout can fire while the write is still committing or has already
//! committed.
//!
//! Rendering `err.message` verbatim therefore reports "request timed out" — a
//! definite claim of failure — for an outcome the client provably cannot
//! determine. CLAUDE.md carries SH-312's verdict as a **standing rule** binding
//! "any client this daemon has, present or future": an ambiguous outcome is
//! reported as ambiguous, never as failure, *because the reader acts on the
//! lie*. Its recorded consequence was a duplicate story filed 24 seconds later.
//!
//! SH-310 fixed this for `runFieldMutation` and SH-312 for the create modal.
//! Neither reached the rest, and the extent was larger than SH-367 was filed
//! believing — the same shape as SH-357, where "only execution settled it":
//!
//! * `toastError` — **seventeen** call sites, at least eight guarding a POST
//!   that mutates the store (`/reopen`, `/labels` add and remove, `/relate`,
//!   `/unrelate`, `/comment`, `/archive`).
//! * three **inline modal** sites the story never named, found by sweeping for
//!   siblings before closing: create project, delete project, archive column.
//!
//! ## The predicate, and why it is this one
//!
//! After the fix a correct site names **no message field at all** — it calls
//! `toastError(err)` or `describeMutationFailure(err)`, both of which end in
//! the one pure describer. So the rule is:
//!
//! > No line may render `err.message` into a notice or into an element's text.
//!
//! Matching the **render**, not the `.catch`, is deliberate. A predicate keyed
//! to `.catch(` cannot see a handler that stores the error and renders it three
//! lines later, and cannot tell a mutating call from a read without resolving
//! the `api()` argument — a static impossibility this repository has already
//! been burned by reasoning about (SH-357's shape scan found nine candidates
//! where the real count was twenty-five). Asking "does this line put a raw
//! message on screen" is decidable by reading the line.
//!
//! Why a derived scan rather than trusting the fix: the honest repair makes
//! `toastError` itself ambiguity-aware, after which all seventeen of its sites
//! are correct *by construction* and there is nothing per-site left to count.
//! The residual risk is not an unconverted site — it is a **new** site that
//! hand-rolls `toast(..., err.message, ...)` and bypasses the helper. That is
//! precisely what this file refuses, and it is the class SH-136, SH-198,
//! SH-260 and SH-364 have each cost this project once already.
//!
//! ## The allowlist
//!
//! Matched **semantically**, by what else the line says, so ordinary edits do
//! not break this test and a reformatted line needs no re-blessing. Each entry
//! states the property that makes it honest.

use std::path::PathBuf;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn dashboard() -> String {
    std::fs::read_to_string(repo_root().join("src/web_dashboard.html"))
        .expect("src/web_dashboard.html is readable")
}

/// Does this line put a raw error message in front of a user?
///
/// Two shapes, which are the only two this page has: a notice (`toast(...)`)
/// and an element's own text (`.textContent = ...`).
fn renders_a_raw_message(line: &str) -> bool {
    let names_a_message = line.contains("err.message") || line.contains("error.message");
    if !names_a_message {
        return false;
    }
    let into_a_notice = line.contains("toast(");
    let into_an_element = line.contains(".textContent");
    into_a_notice || into_an_element
}

/// The idioms that may name a message and still be honest.
///
/// Semantic, not positional: each is recognised by a property of the line
/// itself, so this list does not drift as the file is edited around it.
fn is_allowlisted(line: &str) -> Option<&'static str> {
    // The clipboard's own rejection is a `DOMException` from
    // `navigator.clipboard`, never an `api()` rejection — there is no request,
    // no daemon, and therefore no ambiguity about whether anything "went
    // through". The copy either happened in this tab or it did not.
    if line.contains("Couldn't copy") {
        return Some("a clipboard DOMException, not an api() rejection — no request, no ambiguity");
    }
    None
}

/// A line of code rather than prose. The doc comments in this file's subject
/// discuss `err.message` at length, and a scan that read them would fail on
/// its own documentation.
fn is_code(line: &str) -> bool {
    let trimmed = line.trim_start();
    !(trimmed.starts_with('*')
        || trimmed.starts_with("/*")
        || trimmed.starts_with("//")
        || trimmed.starts_with("<!--"))
}

#[test]
fn no_surface_renders_a_raw_failure_message() {
    let source = dashboard();
    let mut offenders: Vec<String> = Vec::new();

    for (index, line) in source.lines().enumerate() {
        if !is_code(line) || !renders_a_raw_message(line) {
            continue;
        }
        if is_allowlisted(line).is_some() {
            continue;
        }
        offenders.push(format!(
            "  src/web_dashboard.html:{}\n    {}",
            index + 1,
            line.trim()
        ));
    }

    assert!(
        offenders.is_empty(),
        "these lines render a failed request's message verbatim, so `api()`'s \
         `status: 0` — which means NO REPLY EVER ARRIVED — reaches the user as a \
         definite failure (SH-312's standing rule, SH-367):\n\n{}\n\n\
         Route the failure through `toastError(err)` for a notice, or \
         `describeMutationFailure(err)` for an inline modal error. Both end in \
         `describeFailure`, which reports an unprovable outcome as unprovable. \
         If a site genuinely cannot be ambiguous — it never called `api()` — add \
         it to `is_allowlisted` with the property that makes it honest.",
        offenders.join("\n\n")
    );
}

/// The describer has to actually branch on the ambiguous case, or every caller
/// above is honest only by spelling.
#[test]
fn the_one_describer_distinguishes_no_reply_from_a_refusal() {
    let source = dashboard();

    let describer = source
        .split_once("function describeFailure(")
        .expect(
            "`describeFailure` is the single place a failure becomes prose. If it was renamed, \
             rename it here too — this test is what stops the branch being deleted.",
        )
        .1;
    let body: String = describer.chars().take(700).collect();

    assert!(
        body.contains("status === 0"),
        "`describeFailure` must branch on `err.status === 0` — `api()`'s signal that no reply \
         ever arrived. Without it every caller reports a client-side timeout as a definite \
         server refusal, which is the whole defect (SH-312, SH-367)."
    );
    assert!(
        body.contains("may or may not"),
        "`describeFailure`'s ambiguous branch must say the outcome is unknown. The CLI's own \
         door says `may or may not have run` (src/invoke.rs); this is the same daemon answering \
         the same ambiguity to a second client, and it may not answer it differently."
    );
}

/// A positive control: the scan must be capable of failing.
///
/// A predicate that silently stopped matching would report a clean tree
/// forever, which is the vacuous pass this repository fences everywhere else
/// (SH-364's positive control, SH-306's "a gate that did not refuse is not
/// evidence that a gate ran").
#[test]
fn the_scan_can_still_see_an_offending_line() {
    // Assembled at run time so the literal never sits in this file as
    // something the scan over its own tree could trip on.
    let notice = format!(
        "      {}(\"Couldn't reach it — \" + err.{}, \"error\");",
        "toast", "message"
    );
    let element = format!(
        "      $(\"x-error\").{} = err.{};",
        "textContent", "message"
    );

    assert!(
        renders_a_raw_message(&notice),
        "the notice shape must still match"
    );
    assert!(
        renders_a_raw_message(&element),
        "the element shape must still match"
    );
    assert!(
        is_allowlisted(&notice).is_none(),
        "a fabricated offender must not be swallowed by the allowlist"
    );

    // And the inverse: a converted site names no message at all.
    let fixed = "        .catch(toastError);";
    assert!(
        !renders_a_raw_message(fixed),
        "a site routed through the describer must read as clean"
    );
}
