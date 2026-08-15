//! A fence around the browser suite's shared fixtures (SH-245).
//!
//! `scripts/run-e2e.sh` seeds four projects and every spec shares them.
//! "Alpha Project" in particular is seeded with exactly two stories, a shape
//! `filter-persistence.spec.ts` and `column-visibility.spec.ts` assert on by
//! count — so a story some other spec created and did not remove makes those
//! two fail, in a file the actual defect never touched.
//!
//! Every spec deletes what it creates as the last statement of the test body,
//! which is exactly the statement a failing test never reaches. SH-245 is the
//! bill: one genuinely red spec was reported as three, the extra two naming
//! behaviour that was never involved. `cleanUpCreatedStories()`
//! (`e2e/specs/support.ts`) closes it with an `afterEach`, which runs whether
//! the test passed or failed.
//!
//! This test is the part that keeps closing it. A spec added later that
//! creates a story and forgets the registration fails here — in the Rust
//! suite, deterministically, on any machine — rather than intermittently, in
//! someone else's spec, on a loaded one.

use std::path::Path;

/// Every tracked browser spec, paired with its contents.
fn all_specs(root: &Path) -> Vec<(String, String)> {
    let listed = std::process::Command::new("git")
        .current_dir(root)
        .args(["ls-files", "-z", "--", "e2e/specs/*.spec.ts"])
        .output()
        .expect("listing this repository's tracked browser specs");
    assert!(
        listed.status.success(),
        "`git ls-files` failed, so this scan proved nothing: {}",
        String::from_utf8_lossy(&listed.stderr)
    );

    listed
        .stdout
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
        .map(|path| {
            let relative = std::str::from_utf8(path).expect("a UTF-8 path").to_string();
            let text = std::fs::read_to_string(root.join(&relative))
                .unwrap_or_else(|e| panic!("reading {relative}: {e}"));
            (relative, text)
        })
        .collect()
}

/// Whether a spec creates stories: every creation path in the suite goes
/// through the New Story modal's submit button, and the one spec that also
/// wants the create response's own body (`reopen-soft-deleted-confirm`)
/// still clicks it. A future spec that POSTs the story collection directly
/// is caught by the second arm rather than slipping past.
fn creates_stories(text: &str) -> bool {
    text.contains("#create-submit") || (text.contains(".post(") && text.contains("/story"))
}

/// The browser specs that create stories, paired with their contents.
fn story_creating_specs(root: &Path) -> Vec<(String, String)> {
    let specs: Vec<(String, String)> = all_specs(root)
        .into_iter()
        .filter(|(_, text)| creates_stories(text))
        .collect();

    // A scan that matches nothing passes every assertion built on top of it,
    // which would make a broken pattern indistinguishable from a clean tree.
    assert!(
        specs.len() >= 10,
        "expected the browser suite to have at least ten story-creating specs, \
         found {}: {:?}. Either the suite shrank dramatically or `creates_stories` \
         no longer recognises how a spec creates one.",
        specs.len(),
        specs.iter().map(|(p, _)| p).collect::<Vec<_>>()
    );
    specs
}

/// Every browser spec that creates a story registers the `afterEach` that
/// sweeps it, so a failing test cannot strand one in a fixture the rest of
/// the suite reads.
#[test]
fn every_spec_that_creates_a_story_registers_the_cleanup() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));

    let unswept: Vec<String> = story_creating_specs(root)
        .iter()
        .filter(|(_, text)| !text.contains("cleanUpCreatedStories("))
        .map(|(relative, _)| relative.clone())
        .collect();

    assert!(
        unswept.is_empty(),
        "{unswept:?} create stories without registering cleanUpCreatedStories(). \
         A test that fails before its own delete strands the story it created in \
         a fixture project other specs count the cards of, so one red spec \
         becomes several and the extra ones name behaviour that was never \
         involved (SH-245). Add `cleanUpCreatedStories(\"<project name>\");` at \
         the top of the file."
    );
}

// ---------------------------------------------------------------------------
// SH-222: a board on screen is not a board with data
// ---------------------------------------------------------------------------

/// The one file allowed to click a project's Home card, because it is where
/// the wait that has to follow the click lives.
const PROJECT_OPENER: &str = "e2e/specs/support.ts";

/// Whether `text` clicks a project's card on the Home screen itself, rather
/// than calling `openProject()`.
///
/// Whitespace is stripped first: the click is often a chained
/// `.locator(…)\n.click()` across two lines, and the shape is otherwise the
/// same everywhere. A `.repo-card-name` locator that is *asserted* on rather
/// than clicked — `expect(page.locator(".repo-card-name", …)).toBeVisible()`,
/// which several specs use to prove Home rendered at all — is deliberately
/// not a match: it navigates nowhere, so there is nothing to wait for.
fn clicks_a_project_card(text: &str) -> bool {
    let dense: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    dense
        .match_indices(r#"locator(".repo-card-name""#)
        .any(|(at, _)| {
            let rest = &dense[at..];
            match rest.find(')') {
                Some(close) => rest[close + 1..].starts_with(".click("),
                None => false,
            }
        })
}

#[test]
fn clicks_a_project_card_reads_the_shapes_the_suite_actually_writes() {
    assert!(clicks_a_project_card(
        r#"await page.locator(".repo-card-name", { hasText: "Alpha Project" }).click();"#
    ));
    // Chained across lines, as `project-selector.spec.ts` used to write it.
    assert!(clicks_a_project_card(
        "await page\n  .locator(\".repo-card-name\", { hasText: \"Gamma Archive\" })\n  .click();"
    ));
    assert!(!clicks_a_project_card(
        r#"await expect(page.locator(".repo-card-name", { hasText: "Alpha Project" })).toBeVisible();"#
    ));
    assert!(!clicks_a_project_card(
        r#"await openProject(page, "Alpha Project");"#
    ));
}

/// Every browser spec reaches a project's board through `openProject()`,
/// which waits for that project's data — never by clicking the card itself.
///
/// `selectRepo()` sets `state.data = null`, renders the board screen and
/// *then* fetches, so `#board-view` is visible for as long as the fetch takes
/// with no stories, no metadata and no per-project vocabulary behind it. A
/// spec that treated the click plus a `toBeVisible()` as arrival was racing
/// that fetch, and losing it is not a slow assertion that retries: the create
/// modal and the filter-bar dropdowns are built **once**, synchronously, from
/// whatever `meta()` holds at the moment they open. Opened in that window
/// they hold placeholders and never repopulate, so `selectOption()` spends
/// the entire 15s test timeout reporting "did not find some options" — which
/// is what SH-223 recorded against `board-sort.spec.ts` twice and
/// `create-story-defaults.spec.ts` once, each time on a machine busy with
/// something else.
///
/// Scoped to the card-click path on purpose, and that is the whole scope: the
/// other two ways into a board are `?project=` deep links and the header
/// selector's menu, and no spec follows either with an action on a
/// `meta()`-derived control — they assert, and assertions retry. A spec that
/// starts to would need the same wait, and this fence would not catch it.
/// `board-readiness.spec.ts` documents the window from the deep-link side for
/// exactly that reason.
///
/// SH-265 closed the create-modal half of this at the source: `#new-story-btn`
/// is now disabled for the whole window, so no click — real or test-driven —
/// reaches `openCreateModal()` before data arrives. The filter-bar dropdowns
/// are still built from `meta()` the same way and are not gated by any
/// button, so this fence and `openProject()`'s wait still matter for them.
#[test]
fn every_spec_opens_a_board_through_the_helper_that_waits_for_its_data() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let opener = std::fs::read_to_string(root.join(PROJECT_OPENER))
        .unwrap_or_else(|e| panic!("reading {PROJECT_OPENER}: {e}"));
    assert!(
        clicks_a_project_card(&opener),
        "{PROJECT_OPENER} no longer clicks a project card, so this scan's one \
         exclusion excludes nothing and the pattern below may match nothing for \
         that reason rather than because the tree is clean"
    );

    let specs = all_specs(root);
    assert!(
        specs.len() >= 20,
        "expected the browser suite to have at least twenty specs, found {}: \
         either it shrank dramatically or the `git ls-files` pathspec no longer \
         matches it",
        specs.len()
    );

    let direct: Vec<String> = specs
        .into_iter()
        .filter(|(_, text)| clicks_a_project_card(text))
        .map(|(relative, _)| relative)
        .collect();

    assert!(
        direct.is_empty(),
        "{direct:?} click a project's Home card directly. The board becomes \
         visible before its data arrives, so the two lines that look like \
         arrival — the click and `expect(#board-view).toBeVisible()` — are the \
         start of a race, not the end of one (SH-222). Call \
         `openProject(page, \"<project name>\")` from ./support instead, which \
         clicks and then waits for the data."
    );
}

// ---------------------------------------------------------------------------
// SH-318: a spec that sleeps for seconds is racing the machine
// ---------------------------------------------------------------------------

/// The one file allowed to pause the page's clock, because it is where the
/// `finally` that resumes it lives.
const CLOCK_KEEPER: &str = "e2e/specs/support.ts";

/// The longest a spec may sleep on the wall clock without having declared a
/// budget for it.
///
/// Not a round number chosen for looks. Below it are the suite's four
/// legitimate sleeps, every one of which waits out a real sub-second interval
/// the page genuinely has — the 150ms fetch debounce, a rAF deferral — and
/// which no clock can shorten because the thing being waited for is not a
/// timer this test owns. Above it were the five in
/// `notification-contract.spec.ts`, which waited out the dashboard's *own*
/// timers, could always have driven them instead, and cost two tests all but
/// 2.4 seconds of a 15-second budget (SH-318).
const SLEEP_BUDGET_MS: i64 = 1000;

/// Evaluates the argument of a `page.waitForTimeout(...)` call in
/// milliseconds, resolving `const` bindings declared anywhere in the same
/// file.
///
/// Deliberately a small evaluator rather than a regex over literals: the
/// suite's real sleeps are written as `SETTLE_MS` and as
/// `staleAfterMs + watchdogIntervalMs * 2 + 300`, and a scan that could not
/// read those would have to either ignore them (and fence nothing) or reject
/// them (and fence the wrong thing). It handles integer literals with `_`
/// separators, identifiers bound by `const NAME = …`, `+` and `*`. Anything
/// else returns `None`, and the caller fails loudly rather than passing —
/// a scan that cannot read an expression has not cleared it.
fn eval_ms(expr: &str, text: &str, depth: usize) -> Option<i64> {
    if depth > 8 {
        return None;
    }
    let expr = expr.trim();
    if expr.is_empty() {
        return None;
    }

    // Lowest precedence first, so `a + b * 2` splits at the `+`.
    if let Some(at) = expr.rfind('+') {
        let left = eval_ms(&expr[..at], text, depth + 1)?;
        let right = eval_ms(&expr[at + 1..], text, depth + 1)?;
        return Some(left + right);
    }
    if let Some(at) = expr.rfind('*') {
        let left = eval_ms(&expr[..at], text, depth + 1)?;
        let right = eval_ms(&expr[at + 1..], text, depth + 1)?;
        return Some(left * right);
    }

    let bare = expr.replace('_', "");
    if let Ok(literal) = bare.parse::<i64>() {
        return Some(literal);
    }

    // An identifier: find its `const` binding in this file.
    if !expr
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
    {
        return None;
    }
    let needle = format!("const {expr} = ");
    let at = text.find(&needle)?;
    let rest = &text[at + needle.len()..];
    let end = rest.find(';')?;
    eval_ms(&rest[..end], text, depth + 1)
}

/// Every `page.waitForTimeout(...)` argument in `text`, as written.
fn sleeps(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    for (at, _) in text.match_indices("waitForTimeout(") {
        let rest = &text[at + "waitForTimeout(".len()..];
        let Some(close) = rest.find(')') else {
            continue;
        };
        found.push(rest[..close].to_string());
    }
    found
}

#[test]
fn eval_ms_reads_the_shapes_the_suite_actually_writes() {
    let text = "const RAF_DEFERRAL_MS = 400;\nconst SETTLE_MS = RAF_DEFERRAL_MS + 200;\n\
                const staleAfterMs = 400;\nconst watchdogIntervalMs = 100;\n";

    assert_eq!(eval_ms("250", text, 0), Some(250));
    assert_eq!(eval_ms("5_000", text, 0), Some(5000));
    // `overlay-reopen-race.spec.ts`, through two levels of binding.
    assert_eq!(eval_ms("SETTLE_MS", text, 0), Some(600));
    // `sse-watchdog.spec.ts`, with precedence that matters: 400 + 200 + 300.
    assert_eq!(
        eval_ms("staleAfterMs + watchdogIntervalMs * 2 + 300", text, 0),
        Some(900)
    );
    // Unreadable rather than silently cleared.
    assert_eq!(eval_ms("someFunction()", text, 0), None);
    assert_eq!(eval_ms("NOT_DECLARED_ANYWHERE", text, 0), None);

    assert_eq!(
        sleeps("await page.waitForTimeout(250);\nawait page.waitForTimeout(SETTLE_MS);"),
        vec!["250".to_string(), "SETTLE_MS".to_string()]
    );
}

/// No spec sleeps for a second or more unless it has declared a budget that
/// covers the sleep.
///
/// The cheap fix for a timing-sensitive browser test is to wait longer, and it
/// is almost always the wrong one. A spec that sleeps is racing the machine:
/// win the race narrowly and it goes red for a reason unrelated to the
/// behaviour under test; win it widely and the window closed before the test
/// acted, so it passes having exercised nothing (`latch()`'s own comment in
/// `support.ts` records both). SH-318 is the bill for the first: two tests
/// spent ~9.5s each outwaiting the dashboard's own toast timers inside a 15s
/// budget, leaving 2.4s of margin on a machine whose normal state is three or
/// four concurrent worktree sessions — and, because the assertion's own ceiling
/// was then unreachable, a genuine regression in them reported
/// `Test timeout of 15000ms exceeded`, indistinguishable from load.
///
/// The remedy those two took, and the one this fence points at, is to stop
/// waiting: `page.clock.install()` plus `onAFrozenClock()` and
/// `page.clock.runFor()` drive the page's timers instead of outrunning them, so
/// the arithmetic stops depending on machine load at all.
///
/// `test.setTimeout` is the escape hatch, and it is deliberately not a hard
/// refusal: `dispatch.spec.ts` waits on a real subprocess, which no clock can
/// hurry, and it already declares a budget derived from its own constants. What
/// the fence removes is the *silent* multi-second sleep — an unbudgeted one
/// becomes a written number with a justification beside it.
#[test]
fn no_spec_sleeps_for_seconds_without_declaring_a_budget_for_it() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let specs = all_specs(root);

    let mut sleeping = 0usize;
    let mut offenders: Vec<String> = Vec::new();

    for (relative, text) in &specs {
        let declares_budget = text.contains("test.setTimeout(");
        for expr in sleeps(text) {
            sleeping += 1;
            let Some(ms) = eval_ms(&expr, text, 0) else {
                panic!(
                    "{relative}: this scan cannot evaluate `waitForTimeout({expr})`, so it \
                     has not cleared it. Either write the wait in terms this scan reads \
                     (an integer literal, or a `const` binding of `+`/`*` over such \
                     literals), or teach `eval_ms` the new shape — but do not leave it \
                     unreadable, because an unreadable wait is exactly the one worth \
                     reading."
                );
            };
            if ms >= SLEEP_BUDGET_MS && !declares_budget {
                offenders.push(format!("{relative}: waitForTimeout({expr}) = {ms}ms"));
            }
        }
    }

    // A scan that matches nothing passes whatever is built on top of it.
    assert!(
        sleeping >= 4,
        "expected the browser suite to still contain at least four \
         `waitForTimeout` calls, found {sleeping}: either they were all removed \
         or `sleeps()` no longer recognises the shape, in which case this fence \
         is clearing a tree it never read"
    );

    assert!(
        offenders.is_empty(),
        "{offenders:?} sleep for {SLEEP_BUDGET_MS}ms or more without declaring a \
         budget. A spec that outwaits the dashboard's own timers is racing the \
         machine, and loses that race on a loaded one (SH-318). Drive the timers \
         instead — `page.clock.install()` before the navigation, then \
         `onAFrozenClock(page, …)` and `page.clock.runFor(…)` from ./support — \
         which removes the wall-clock wait rather than widening the margin \
         against it. If the wait is on something no clock can hurry (a real \
         subprocess, as in dispatch.spec.ts), declare `test.setTimeout(…)` \
         derived from the constants that justify it."
    );
}

/// Only `support.ts` pauses the page's clock.
///
/// A paused clock stops every timer on the page, not merely the one under
/// test, so the teardown that follows — closing a drawer, re-rendering a board,
/// running a delete through the API — needs it running again. A `pauseAt` in a
/// test body has no guaranteed partner: an assertion that fails between the
/// pause and the resume leaves the clock stopped for the rest of the test, and
/// one honest red becomes a hang plus a story stranded in a project the rest of
/// the suite counts the cards of (SH-245, and the reason
/// `cleanUpCreatedStories` exists).
///
/// `onAFrozenClock(page, body)` resumes in a `finally`, so the pairing holds
/// however `body` ends. Keeping `pauseAt` to that one helper makes "someone
/// paused without resuming" a red test here rather than a matter of discipline.
#[test]
fn only_the_shared_helper_pauses_the_pages_clock() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let keeper = std::fs::read_to_string(root.join(CLOCK_KEEPER))
        .unwrap_or_else(|e| panic!("reading {CLOCK_KEEPER}: {e}"));
    assert!(
        keeper.contains("clock.pauseAt(") && keeper.contains("finally"),
        "{CLOCK_KEEPER} no longer pauses the clock inside a `finally`, so this \
         scan's one exclusion excludes nothing and the pattern below may match \
         nothing for that reason rather than because the tree is clean"
    );

    let direct: Vec<String> = all_specs(root)
        .into_iter()
        .filter(|(_, text)| text.contains("clock.pauseAt("))
        .map(|(relative, _)| relative)
        .collect();

    assert!(
        direct.is_empty(),
        "{direct:?} pause the page's clock directly. A pause with no guaranteed \
         resume stops the timers the teardown needs, so a single failed \
         assertion becomes a hung test and a stranded fixture story on top of \
         the real failure. Use `onAFrozenClock(page, async () => {{ … }})` from \
         ./support, which resumes in a `finally`."
    );
}
