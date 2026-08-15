import { test, expect } from "@playwright/test";
import type { Page } from "@playwright/test";
import {
  cleanUpCreatedStories,
  deleteStory,
  onAFrozenClock,
  openProject,
  seedToken,
} from "./support";

/**
 * The notification contract SH-304's council settled
 * (`.council/sh-304-dashboard-notification-contract/DECISION.md`).
 *
 * **Routing is by OUTCOME, not by the `--auto` flag** — which is the rule
 * SH-232's own doc comments got wrong, and the reason those comments were
 * rewritten alongside this file rather than left citing the old rule:
 *
 *   - a SUCCESS (attended or auto) is a short, client-composed toast that
 *     fades on its own, because it is corroborated elsewhere on the board
 *     (the card moves) and by the tmux window that now exists;
 *   - a REFUSED/FAILED outcome (attended or auto) is DURABLE, dismissed only
 *     by the user, because once its notification clears there is no trace of
 *     it anywhere in the UI. The daemon's persisted `DispatchRecord` is not
 *     that trace: no route exposes it, this page never reads it, and it
 *     evicts after 30 minutes or 32 records (`RETAIN_FOR`/`RETAIN_FINISHED`,
 *     `src/api/dispatch.rs`).
 *
 * Every dispatch here is stubbed with `page.route`, the same way
 * `story-context-menu-dispatch.spec.ts` does it: the outcomes this file needs
 * (`refused`, `failed`, and both under `--auto`) are the ones a real
 * `story.sh` run cannot be asked for on demand, and stubbing means no story is
 * ever really claimed out from under `dispatch.spec.ts`, which owns the real
 * end-to-end path against Alpha's own fixtures.
 *
 * Fixtures, from `scripts/run-e2e.sh`: "Alpha Project" (a checkout, so the
 * drawer's dispatch buttons render at all).
 *
 * ## This file drives the clock; it does not outwait it (SH-318)
 *
 * All but two of these tests fake the page's timers and advance them by hand.
 * **Two deliberately do not, and must not be changed to** — see the canary note
 * below, which is the load-bearing half of this arrangement.
 *
 * The two tests that hold a notice's clock open used to spend ~9.5 seconds each
 * of real waiting inside a 15-second budget: 5.5s proving the clock was held,
 * then ~4s proving it resumed. Measured at 12.6s and 12.3s on a machine at load
 * average 8, against siblings at 6.1-8.0s. They had 2.4 seconds of margin, on a
 * machine whose documented normal state is three or four concurrent worktree
 * sessions, so contention did not make them fragile — it only ever consumed the
 * last 2.4s of a margin that was never there.
 *
 * A wider budget was the wrong answer twice over. `page.waitForTimeout` on a
 * loaded machine is not the duration it names, so the elastic part of these
 * tests *is* the waiting; and the file's own `GONE_TIMEOUT` was already dead in
 * exactly those two tests (2.5s setup + 5.5s hold + its 8000ms ceiling = 16.0s
 * against 15.0s), so the test timeout always fired first and a genuine "the
 * clock never resumed" regression reported `Test timeout of 15000ms exceeded` —
 * byte-identical to a loaded machine. One root cause surfacing twice: once as a
 * timeout, once as a failure that could not name itself.
 *
 * So the durations under test are unchanged at 3000/1000 and the *waiting* is
 * gone. `onAFrozenClock` pauses the page's clock, and `page.clock.runFor()`
 * advances it. Inside a frozen window the arithmetic is not merely robust to
 * machine load, it is independent of it — fake time moves only when a line here
 * says so — which is why the assertions below can pin a departure to the
 * millisecond where an 8000ms ceiling once accepted anything under eight
 * seconds. `support.ts`'s `latch()` gives the same reasoning for the network
 * side; this is its timer-side equivalent.
 *
 * ## The two canaries stay on the real clock
 *
 * Named below rather than cited by line number, which is what they were until
 * SH-322 appended a section and moved both.
 *
 * `page.clock.install()` fakes `Date`, `setTimeout`, `setInterval`,
 * `requestAnimationFrame`, `requestIdleCallback` and `performance` all at once.
 * A file that faked all of them everywhere could go green against a simulation
 * of itself, so two tests keep real timers. **A canary earns its place by
 * covering a distinct escape route, not by being another instance of a covered
 * one** — which is why there are two and not one or three.
 *
 *   - **"a successful attended dispatch ... fades on its own"** is the
 *     instrument canary: it proves a toast leaves a real browser on a real
 *     timer, through a real animation and real SSE/poll interleaving. If the
 *     apparatus below is measuring itself, this is what says so.
 *   - **"a fading notice still clears under prefers-reduced-motion"** covers a
 *     second, narrower route. `.toast.leaving`'s animation lives *inside*
 *     `@media (prefers-reduced-motion: no-preference)` (`web_dashboard.html`),
 *     so under reduced motion there is no animation and no `animationend` to
 *     fire. Were `depart()` ever refactored to gate removal on that event
 *     instead of its bare `setTimeout` — a plausible edit, since this codebase
 *     already pairs `animationend` with a `setTimeout` safety net elsewhere —
 *     the instrument canary would still pass (its animation runs) and a
 *     *clocked* version of this test would pass vacuously (`runFor` fires the
 *     JS timer regardless). Only this test, on a real clock, would catch
 *     notices stranded permanently for the readers who asked for less motion.
 *
 * The auto-success test is NOT a canary: it differs from the instrument canary
 * only in which button was clicked and the headline text, so it witnesses an
 * already-covered route. It runs clocked, as an ordinary member.
 *
 * Full reasoning, both spikes and the rejected alternatives:
 * `.council/sh-318-notification-clock-e2e-timing/DECISION.md`.
 *
 * ## The turn-off (SH-322)
 *
 * The last six tests are a section of their own, with their own header above
 * them: they pin `storyhook.keepNotices`, the preference that removes a
 * self-clearing notice's time limit outright.
 */

cleanUpCreatedStories("Alpha Project");

/** How long a success notice is held before its fade starts, plus the fade
 * itself — `TOAST_LIFETIME_MS` + `TOAST_FADE_MS` in `web_dashboard.html`,
 * restated here rather than imported because the dashboard is a single HTML
 * file with no module boundary a spec can reach into. */
const SUCCESS_VISIBLE_MS = 3000;
const FADE_MS = 1000;

/** Generous enough that a loaded machine's timer jitter cannot fail a test
 * whose subject is "this eventually goes away".
 *
 * Used only by the two real-clock canaries now, and reachable in both: each
 * begins its wait ~2.7s in, leaving 12.3s of the budget for an 8s ceiling. It
 * was NOT reachable in the two tests this story fixed, which is the defect
 * described in this file's header — a clocked test needs no ceiling at all,
 * because `runFor` returns with the DOM already settled. */
const GONE_TIMEOUT = SUCCESS_VISIBLE_MS + FADE_MS + 4000;

/** How far a frozen clock is advanced to prove a notice has no timer at all.
 *
 * Thirty seconds, where the wall-clock version of this probe could only afford
 * 5.5. It costs nothing now, and it is comfortably past every lifetime this
 * dashboard has ever had — the deleted 4.5s and 9s ones and the current 3s+1s
 * alike — so a regression to any of them fails here. */
const NO_TIMER_HORIZON_MS = 30_000;

test.beforeEach(async ({ page }) => {
  await seedToken(page);
});

/** Opens the dashboard with the page's timers faked but still running.
 *
 * `install()` must precede navigation, and it does not freeze anything on its
 * own — timers run normally, which is what lets the whole of `openFreshStory`,
 * the SSE bootstrap and the board's own re-renders proceed as usual. Freezing
 * is `onAFrozenClock`'s job, and happens later, around the behaviour under
 * test. */
async function openClocked(page: Page): Promise<void> {
  await page.clock.install();
  await page.goto("/");
}

/** Opens the dashboard with nothing faked. The two canaries, and only those —
 * see this file's header before moving a third test onto this. */
async function openOnRealTime(page: Page): Promise<void> {
  await page.goto("/");
}

/** A stubbed dispatch envelope in a terminal state (`DispatchEnvelope`,
 * `src/api/dispatch.rs`). The client always follows its POST with a GET poll
 * whatever the POST reported, so answering both with a finished record
 * short-circuits the real 5s poll interval. `reason` rides only on a
 * non-`ok` record, matching `classify()`: a plain `fail()` refusal carries
 * none, and that absence is meaningful rather than a gap to paper over. */
function stubbedDispatch(
  storyId: string,
  auto: boolean,
  state: "ok" | "refused" | "failed",
) {
  const record: Record<string, unknown> = {
    handle: "stub-handle",
    project: "alpha",
    story: storyId,
    auto,
    state,
    started_at: "2026-01-01T00:00:00Z",
    finished_at: "2026-01-01T00:00:01Z",
  };
  if (state === "failed") {
    record.error =
      "the dispatch script exited without printing a result — check `story doctor`";
  } else {
    record.payload = {
      display:
        state === "ok"
          ? "[story] AA-1 (a title) → opened tmux window `AA-1` on a worktree based on `origin/main`, launched `claude`, submitted the prompt, and claimed it (now `in-progress`)."
          : "[story] refused: that story is already in-progress — claimed by another session",
    };
  }
  if (state === "refused") record.reason = "claim-conflict";
  return JSON.stringify({ result: "ok", dispatch: record });
}

async function stubDispatch(
  page: Page,
  storyId: string,
  auto: boolean,
  state: "ok" | "refused" | "failed",
): Promise<void> {
  await page.route("**/dispatch**", async (route) => {
    await route.fulfill({
      status: route.request().method() === "POST" ? 202 : 200,
      contentType: "application/json",
      body: stubbedDispatch(storyId, auto, state),
    });
  });
}

/** Closes the drawer this file's tests leave open, then deletes the story.
 *
 * `deleteStory` starts by clicking the card, which the open drawer's own
 * backdrop intercepts -- so the close is not tidiness, it is what makes the
 * cleanup reach the card at all. Every other spec that calls `deleteStory`
 * has already dismissed whatever it opened by this point; these tests
 * dispatch straight from the drawer footer and are still in it.
 *
 * Always called with the clock running: it drives real requests and real
 * re-renders, and `onAFrozenClock` resumes in a `finally` precisely so this
 * still works after a failed assertion. */
async function cleanUp(page: Page, title: string): Promise<void> {
  await page.locator("#drawer-close").click();
  await expect(page.locator("#drawer")).not.toHaveClass(/open/);
  await deleteStory(page, title);
}

/** Creates a story, opens its drawer, and returns its id — every test here
 * needs a story of its own, since a dispatch (even a stubbed one) is keyed
 * per-story in `state.dispatches` and two tests sharing one would collide. */
async function openFreshStory(page: Page, title: string): Promise<string> {
  await openProject(page, "Alpha Project");
  await page.locator("#new-story-btn").click();
  await expect(page.locator("#create-modal")).toHaveClass(/open/);
  await page.locator("#create-title").fill(title);
  await page.locator("#create-submit").click();
  await expect(page.locator("#create-modal")).not.toHaveClass(/open/);

  const card = page.locator('.column[data-state="todo"] .card', { hasText: title });
  await expect(card).toBeVisible();
  const id = await card.getAttribute("data-id");
  await card.click();
  await expect(page.locator("#drawer")).toHaveClass(/open/);
  return id!;
}

/**
 * Advances a frozen clock through the whole remaining life of a self-clearing
 * notice, asserting at each boundary, and leaves the stack empty.
 *
 * `owed` is what the notice still has to run — the full lifetime for one that
 * has never been paused, less whatever was burned before a pause for one that
 * has. Splitting the advance in three is what pins the two production
 * durations *separately*: a change to either the hold or the fade fails here,
 * where a single "is it gone yet" ceiling would absorb both.
 */
async function runOutTheClock(page: Page, owed: number): Promise<void> {
  const toast = page.locator("#toast-stack .toast");

  // One millisecond short of departure: nothing has begun to leave.
  await page.clock.runFor(owed - 1);
  await expect(toast).toHaveCount(1);
  await expect(toast).not.toHaveClass(/leaving/);

  // The hold elapses and the fade begins -- the node is still in the DOM.
  await page.clock.runFor(1);
  await expect(toast).toHaveClass(/leaving/);

  // One millisecond short of the fade's end, and then past it.
  await page.clock.runFor(FADE_MS - 1);
  await expect(toast).toHaveCount(1);
  await page.clock.runFor(1);
  await expect(toast).toHaveCount(0);
}

test("a successful attended dispatch is a short toast that fades on its own", async ({
  page,
}) => {
  // CANARY -- real timers on purpose. See this file's header before changing.
  await openOnRealTime(page);

  const title = "SH-304 — attended success fades";
  const id = await openFreshStory(page, title);
  await stubDispatch(page, id, false, "ok");

  await page.locator("#dispatch-btn").click();

  const toast = page.locator("#toast-stack .toast.success");
  await expect(toast).toBeVisible();
  // The headline is composed here from typed fields, never relayed from
  // `story.sh`'s `display` — which is authored for its Claude skill consumer
  // and is the ~90-word paragraph SH-304 was filed about.
  await expect(toast).toHaveText(`${id} dispatched`);
  await expect(toast).not.toContainText("worktree");
  // No durable row for a success, whatever else is true.
  await expect(page.locator("#dispatch-history .dispatch-history-row")).toHaveCount(0);

  // Real elapsed seconds, in a real browser, on the real `setTimeout`. This is
  // the assertion the whole clocked apparatus below is checked against.
  await expect(page.locator("#toast-stack .toast")).toHaveCount(0, {
    timeout: GONE_TIMEOUT,
  });

  await cleanUp(page, title);
});

test("a successful --auto dispatch also fades, and names itself autonomous", async ({
  page,
}) => {
  await openClocked(page);

  const title = "SH-304 — auto success fades";
  const id = await openFreshStory(page, title);
  await stubDispatch(page, id, true, "ok");

  await onAFrozenClock(page, async () => {
    await page.locator("#dispatch-auto-btn").click();

    // SH-232 sent this to a durable row because "nobody is necessarily
    // watching". SH-304's council reversed that for SUCCESS specifically: the
    // story moving on the board and the tmux window both corroborate it, so
    // the notice is a courtesy and may clear itself.
    const toast = page.locator("#toast-stack .toast.success");
    await expect(toast).toBeVisible();
    await expect(toast).toHaveText(`${id} dispatched (auto)`);
    await expect(page.locator("#dispatch-history .dispatch-history-row")).toHaveCount(0);

    await runOutTheClock(page, SUCCESS_VISIBLE_MS);
  });

  await cleanUp(page, title);
});

test("a refused attended dispatch is durable, keeps its diagnosis, and dismisses only by click", async ({
  page,
}) => {
  await openClocked(page);

  const title = "SH-304 — attended refusal is durable";
  const id = await openFreshStory(page, title);
  await stubDispatch(page, id, false, "refused");

  await onAFrozenClock(page, async () => {
    await page.locator("#dispatch-btn").click();

    const toast = page.locator("#toast-stack .toast.error");
    await expect(toast).toBeVisible();
    await expect(toast).toContainText(`${id} refused`);
    // SH-196's diagnosis-and-remedy text is not dropped, just demoted beneath
    // the headline -- and it stays in the DOM (never a `title` tooltip), so an
    // `aria-live` announcement and a screen reader can both still reach it.
    await expect(toast.locator(".notice-detail")).toContainText("already in-progress");
    // The typed reason (SH-232's taxonomy) keeps its own line, on both
    // surfaces, rather than being concatenated into the prose it qualifies.
    await expect(toast.locator(".notice-reason")).toHaveText("claim-conflict");

    // The whole point: no timer at all. Thirty seconds of it, which outlives
    // the deleted 4.5s and 9s lifetimes and the current 3s+1s alike.
    await page.clock.runFor(NO_TIMER_HORIZON_MS);
    await expect(toast).toBeVisible();

    await toast.locator(".toast-dismiss").click();
    await expect(page.locator("#toast-stack .toast")).toHaveCount(0);
  });

  await cleanUp(page, title);
});

test("a failed attended dispatch is durable too, and reads differently from a refusal", async ({
  page,
}) => {
  await openClocked(page);

  const title = "SH-304 — attended failure is durable";
  const id = await openFreshStory(page, title);
  await stubDispatch(page, id, false, "failed");

  await onAFrozenClock(page, async () => {
    await page.locator("#dispatch-btn").click();

    const toast = page.locator("#toast-stack .toast.error");
    await expect(toast).toBeVisible();
    // `failed` (the script itself never produced a result) and `refused` (a
    // well-formed business refusal) are different words, not one red box --
    // SH-196's distinction, preserved in the composed headline.
    await expect(toast).toContainText(`${id} failed`);
    await expect(toast).not.toContainText("refused");
    await expect(toast.locator(".notice-detail")).toContainText("without printing a result");
    // A `failed` record carries no typed reason -- `classify()` reads one from
    // `story.sh`'s own result, and a script that never printed one had none to
    // give. A real, meaningful absence, not a gap to paper over.
    await expect(toast.locator(".notice-reason")).toHaveCount(0);

    await page.clock.runFor(NO_TIMER_HORIZON_MS);
    await expect(toast).toBeVisible();

    // Dismissed before cleanup, and not only for tidiness: the toast stack is
    // top-right at `z-index: 60`, which is over the drawer's own header, so a
    // durable notice left standing intercepts the click on `#drawer-close`.
    await toast.locator(".toast-dismiss").click();
  });

  await cleanUp(page, title);
});

test("a refused --auto dispatch stays a durable history row (SH-232's surviving half)", async ({
  page,
}) => {
  await openClocked(page);

  const title = "SH-304 — auto refusal is durable";
  const id = await openFreshStory(page, title);
  await stubDispatch(page, id, true, "refused");

  await onAFrozenClock(page, async () => {
    await page.locator("#dispatch-auto-btn").click();

    // The case SH-227's incident review was actually about: an unattended run
    // whose only report is this row. Geography is unchanged for failures --
    // an auto failure still lands bottom-right, where SH-232 put it.
    const row = page.locator("#dispatch-history .dispatch-history-row.error");
    await expect(row).toBeVisible();
    await expect(row).toContainText(`${id} refused (auto)`);
    await expect(row.locator(".notice-detail")).toContainText("already in-progress");
    await expect(page.locator("#toast-stack .toast")).toHaveCount(0);

    await page.clock.runFor(NO_TIMER_HORIZON_MS);
    await expect(row).toBeVisible();

    await row.locator(".dispatch-history-dismiss").click();
    await expect(page.locator("#dispatch-history .dispatch-history-row")).toHaveCount(0);
  });

  await cleanUp(page, title);
});

test("hovering a fading notice holds its clock, and leaving resumes it (SC 2.2.1)", async ({
  page,
}) => {
  await openClocked(page);

  const title = "SH-304 — hover pauses the clock";
  const id = await openFreshStory(page, title);
  await stubDispatch(page, id, false, "ok");

  await onAFrozenClock(page, async () => {
    await page.locator("#dispatch-btn").click();
    const toast = page.locator("#toast-stack .toast.success");
    await expect(toast).toBeVisible();

    // WCAG 2.2 SC 2.2.1 (Timing Adjustable) is satisfied by letting the user
    // pause the limit; a pointer resting on the notice IS that request. Held
    // for thirty seconds — ten times the lifetime — the notice must still be
    // there. The wall-clock version of this could only afford 5.5s.
    await toast.hover();
    await page.clock.runFor(NO_TIMER_HORIZON_MS);
    await expect(toast).toBeVisible();

    // And the pause is a pause, not a cancellation: moving away resumes it.
    // The full lifetime is still owed, because no fake time elapsed between
    // the notice appearing and the hover — which is a property of the frozen
    // clock, and is what lets the assertion be exact rather than a ceiling.
    await page.mouse.move(0, 0);
    await runOutTheClock(page, SUCCESS_VISIBLE_MS);
  });

  await cleanUp(page, title);
});

test("pausing preserves what is left of the clock rather than restarting it", async ({
  page,
}) => {
  await openClocked(page);

  // The load-bearing half of the SC 2.2.1 claim, and the half nothing tested
  // until SH-318. `scheduleAutoDismiss`'s own doc comment promises that
  // "pausing preserves what is left rather than restarting it, so a reader who
  // hovers twice does not get a fresh three seconds each time" — but every
  // other test here hovers immediately, when preserving and restarting are
  // indistinguishable because nothing has been spent yet. Spend some first and
  // the two come apart: a `resume()` that reset `remaining` to the full
  // lifetime would owe 4000ms from the mouseleave below, and would still be on
  // screen at the last assertion.
  //
  // This is the one assertion in the file that a real clock cannot make
  // honestly — the discrimination is a 1ms window on a 2000ms difference, and
  // only fake time is that exact.
  const title = "SH-304 — a second hover grants no fresh lifetime";
  const id = await openFreshStory(page, title);
  await stubDispatch(page, id, false, "ok");

  const BURNED_MS = 1000;

  await onAFrozenClock(page, async () => {
    await page.locator("#dispatch-btn").click();
    const toast = page.locator("#toast-stack .toast.success");
    await expect(toast).toBeVisible();

    // Spend a third of the lifetime before pausing.
    await page.clock.runFor(BURNED_MS);
    await expect(toast).toBeVisible();

    await toast.hover();
    await page.clock.runFor(NO_TIMER_HORIZON_MS);
    await expect(toast).toBeVisible();

    // Only the unspent remainder is owed.
    await page.mouse.move(0, 0);
    await runOutTheClock(page, SUCCESS_VISIBLE_MS - BURNED_MS);
  });

  await cleanUp(page, title);
});

test("a fading notice still clears under prefers-reduced-motion, without animating", async ({
  page,
}) => {
  // CANARY -- real timers on purpose, and this one is not merely a second
  // opinion. Under reduced motion `.toast.leaving` has no animation at all, so
  // a removal ever gated on `animationend` would strand notices permanently
  // here and nowhere else. See this file's header.
  await page.emulateMedia({ reducedMotion: "reduce" });
  await openOnRealTime(page);

  const title = "SH-304 — reduced motion still dismisses";
  const id = await openFreshStory(page, title);
  await stubDispatch(page, id, false, "ok");

  await page.locator("#dispatch-btn").click();
  const toast = page.locator("#toast-stack .toast.success");
  await expect(toast).toBeVisible();

  // The guard drops the ANIMATION, never the dismissal -- a user who asked
  // for less motion did not ask for notices that pile up forever. `.card`
  // has drawn this distinction since SH-203 ("information stays under
  // reduced motion, decoration drops"); the toast rules simply sat outside
  // the media query until this story moved them inside it.
  const animation = await toast.evaluate(
    (node) => getComputedStyle(node).animationName,
  );
  expect(animation).toBe("none");

  await expect(page.locator("#toast-stack .toast")).toHaveCount(0, {
    timeout: GONE_TIMEOUT,
  });

  await cleanUp(page, title);
});

// ============================================================
// SC 2.2.1, THE TURN-OFF (SH-322)
// ============================================================
//
// The six tests below pin a conformance route this dashboard did not have.
// WCAG 2.2 SC 2.2.1 (Timing Adjustable) offers exactly three mechanisms for a
// time limit — **Turn off, Adjust, Extend** — plus three exceptions, none of
// which fits a 3s toast. This takes the first: `storyhook.keepNotices`, a
// persisted preference on the Settings screen that removes the clock entirely
// rather than lengthening it, set before any notice is encountered.
//
// It defaults OFF, and that is the criterion's own shape rather than a
// compromise: what has to exist in advance is the mechanism, not the choice.
// Defaulting it on would make every one of ~26 `toast()` call sites permanent
// and gut SH-304's corroboration premise, which is settled and not this
// preference's to reopen.
//
// Reasoning, the alternatives rejected 3-0, and the five follow-up defects the
// review turned up:
// `.council/sh-322-self-clearing-notice-keyboard-pause/DECISION.md`.

/** The most Tab presses {@link tabTo} will spend before it gives up.
 *
 * A termination guard, deliberately not a budget. The property under test is
 * that the control can be *arrived at* from the keyboard at all — not that it
 * can be arrived at in some particular number of presses, which is a fact about
 * the Settings screen's control order that this story does not own and that
 * would go red the day another control lands above it. Generous enough to
 * survive that, small enough to fail in a second rather than hang. */
const TAB_BOUND = 30;

/** Tabs forward from wherever focus is until `selector` holds it.
 *
 * Deliberately does NOT use `locator.press()`, `locator.check()` or
 * `locator.focus()` on the target: all three put focus on the element
 * implicitly, so what they prove is "this control can be operated once you are
 * on it". That was true of the dead `focusin` branch this story deletes, which
 * is why proving it is not enough here — it is the same class of evidence that
 * would have declared the rejected `tabindex="0"` repair working. Arriving is
 * the assertion. */
async function tabTo(page: Page, selector: string): Promise<void> {
  const target = page.locator(selector);
  await expect(target).toBeVisible();
  for (let press = 0; press < TAB_BOUND; press++) {
    await page.keyboard.press("Tab");
    if (await target.evaluate((node) => node === document.activeElement)) return;
  }
  throw new Error(
    `${selector} was not reachable by Tab within ${TAB_BOUND} presses. ` +
      "Reachability is the property under test: a control that exists but " +
      "cannot be arrived at from the keyboard does not satisfy SC 2.2.1's " +
      "turn-off clause, however well it works once focused.",
  );
}

/** Turns the notice clock off through the real Settings control, by pointer,
 * and returns to Home.
 *
 * A pointer is fine *here*: that the same control is reachable by keyboard is
 * pinned once, on its own, by the first test below. Repeating that traversal in
 * every test that merely needs the preference set would pin nothing further
 * while coupling four more tests to the Settings screen's control order.
 *
 * What every one of them does need is that the preference is set **through the
 * page** rather than seeded into `localStorage` — "available before the limit is
 * encountered" is half the conformance claim, and a seeded storage key would
 * satisfy the other half while proving no mechanism exists at all. */
async function keepNotices(page: Page): Promise<void> {
  await page.locator("#settings-btn").click();
  await expect(page.locator("#settings-view")).toBeVisible();
  await page.locator("#toggle-keep-notices").click();
  await expect(page.locator("#toggle-keep-notices")).toBeChecked();
  await page.locator("#home-btn").click();
}

test("the notice clock can be turned off from the keyboard alone, before any notice", async ({
  page,
}) => {
  await openClocked(page);

  // A fixed origin, so what the traversal below measures is a property of the
  // Settings screen rather than of wherever the previous assertion happened to
  // leave focus. The origin is a topbar button present on every screen; the
  // control under test is what has to be found from there.
  await page.locator("#settings-btn").focus();
  await page.keyboard.press("Enter");
  await expect(page.locator("#settings-view")).toBeVisible();

  await tabTo(page, "#toggle-keep-notices");
  await expect(page.locator("#toggle-keep-notices")).not.toBeChecked();
  await page.keyboard.press(" ");
  await expect(page.locator("#toggle-keep-notices")).toBeChecked();

  // The accessible name is part of the mechanism, not decoration: a checkbox
  // announced as unlabelled is not one a user can knowingly turn a time limit
  // off with.
  await expect(
    page.getByRole("checkbox", { name: "Keep notices until I dismiss them" }),
  ).toBeChecked();
});

test("with the limit turned off, a success notice has no timer at all", async ({
  page,
}) => {
  await openClocked(page);
  await keepNotices(page);

  const title = "SH-322 — the limit turned off";
  const id = await openFreshStory(page, title);
  await stubDispatch(page, id, false, "ok");

  await onAFrozenClock(page, async () => {
    await page.locator("#dispatch-btn").click();
    const toast = page.locator("#toast-stack .toast.success");
    await expect(toast).toBeVisible();
    await expect(toast).toContainText(`${id} dispatched`);

    await page.clock.runFor(NO_TIMER_HORIZON_MS);
    await expect(toast).toBeVisible();
    // Not merely "still there": it never even began to leave. That is what
    // separates "no clock at all" from "a clock that something paused", and
    // unlike a visibility assertion it survives a change to `TOAST_FADE_MS`.
    await expect(toast).not.toHaveClass(/leaving/);

    // SH-304's shape is preserved rather than widened. A kept success is still
    // its headline plus the one control needed to close it -- that council's
    // rule governs a notice that clears *itself*, and this one no longer does.
    await expect(toast.locator(".toast-dismiss")).toHaveCount(1);
    await expect(toast.locator(".notice-detail")).toHaveCount(0);
    await expect(toast.locator(".notice-reason")).toHaveCount(0);

    await toast.locator(".toast-dismiss").click();
    await expect(page.locator("#toast-stack .toast")).toHaveCount(0);
  });

  await cleanUp(page, title);
});

test("a kept notice is dismissed from the keyboard, which closes the loop without a pointer", async ({
  page,
}) => {
  await openClocked(page);
  await keepNotices(page);

  const title = "SH-322 — kept notice dismissed by keyboard";
  const id = await openFreshStory(page, title);
  await stubDispatch(page, id, false, "ok");

  await onAFrozenClock(page, async () => {
    await page.locator("#dispatch-btn").click();
    const toast = page.locator("#toast-stack .toast.success");
    await expect(toast).toBeVisible();

    // The half a turn-off would be hollow without: having stopped the clock,
    // the user must be able to clear the notice again without reaching for a
    // mouse. Arrived at by Tab and activated by Enter, both for real.
    await tabTo(page, "#toast-stack .toast-dismiss");
    await page.keyboard.press("Enter");
    await expect(page.locator("#toast-stack .toast")).toHaveCount(0);
  });

  await cleanUp(page, title);
});

test("the choice survives a reload, which is what 'before encountering it' means", async ({
  page,
}) => {
  await openClocked(page);
  await keepNotices(page);

  await page.reload();
  await page.locator("#settings-btn").click();
  await expect(page.locator("#toggle-keep-notices")).toBeChecked();
  // Pinned by name: the storage key is the part a refactor can rename in
  // silence, and a user's answer to "stop taking my notices away" surviving
  // exactly one page load would be a session toggle wearing a conformance
  // claim.
  expect(
    await page.evaluate(() => localStorage.getItem("storyhook.keepNotices")),
  ).toBe("true");
  await page.locator("#home-btn").click();

  // And it still governs a notice afterwards, not merely the checkbox.
  const title = "SH-322 — the choice outlives the visit";
  const id = await openFreshStory(page, title);
  await stubDispatch(page, id, false, "ok");

  await onAFrozenClock(page, async () => {
    await page.locator("#dispatch-btn").click();
    const toast = page.locator("#toast-stack .toast.success");
    await expect(toast).toBeVisible();
    await page.clock.runFor(NO_TIMER_HORIZON_MS);
    await expect(toast).toBeVisible();
    await expect(toast).not.toHaveClass(/leaving/);
    await toast.locator(".toast-dismiss").click();
  });

  await cleanUp(page, title);
});

test("the preference is off until the user turns it on", async ({ page }) => {
  await openClocked(page);

  // The unanimous amendment to the council's own winning proposal, pinned as a
  // property rather than left to the five tests above to imply: the mechanism
  // must EXIST in advance, not be pre-engaged. Defaulting it on would make
  // every notice permanent for everyone.
  expect(
    await page.evaluate(() => localStorage.getItem("storyhook.keepNotices")),
  ).toBeNull();

  const title = "SH-322 — untouched, the clock still runs";
  const id = await openFreshStory(page, title);
  await stubDispatch(page, id, false, "ok");

  await onAFrozenClock(page, async () => {
    await page.locator("#dispatch-btn").click();
    await expect(page.locator("#toast-stack .toast.success")).toBeVisible();
    await runOutTheClock(page, SUCCESS_VISIBLE_MS);
  });

  await cleanUp(page, title);
});

test("the preference does not change a durable notice, which never had a clock", async ({
  page,
}) => {
  await openClocked(page);
  await keepNotices(page);

  const title = "SH-322 — a refusal is unaffected";
  const id = await openFreshStory(page, title);
  await stubDispatch(page, id, false, "refused");

  await onAFrozenClock(page, async () => {
    await page.locator("#dispatch-btn").click();
    const toast = page.locator("#toast-stack .toast.error");
    await expect(toast).toBeVisible();
    // An error was already durable and already carried its diagnosis; the
    // preference must be inert over it rather than quietly re-deriving it.
    await expect(toast).toContainText(`${id} refused`);
    await expect(toast.locator(".notice-detail")).toContainText("already in-progress");
    await expect(toast.locator(".notice-reason")).toHaveText("claim-conflict");
    await expect(toast.locator(".toast-dismiss")).toHaveCount(1);

    await page.clock.runFor(NO_TIMER_HORIZON_MS);
    await expect(toast).toBeVisible();
    await toast.locator(".toast-dismiss").click();
    await expect(page.locator("#toast-stack .toast")).toHaveCount(0);
  });

  await cleanUp(page, title);
});

test("a backgrounded tab does not burn a notice's clock down unseen", async ({
  page,
  context,
}) => {
  await openClocked(page);

  const title = "SH-304 — hidden tab holds the clock";
  const id = await openFreshStory(page, title);
  await stubDispatch(page, id, false, "ok");

  // Backgrounding the tab for real does NOT express this, and the test says
  // so rather than leaving the next reader to rediscover it: headless
  // Chromium keeps every page's `visibilityState` "visible" whatever has
  // focus, so `bringToFront` on a sibling page changes nothing observable.
  // This assertion is the evidence for that claim -- if a future Playwright
  // or browser starts reporting it honestly, this line fails and whoever
  // sees it can delete the override below in favour of the real thing.
  //
  // Taken before the clock is frozen: a second page is a second real browser
  // context to open and close, and it has nothing to do with the timer under
  // test.
  const other = await context.newPage();
  await other.goto("about:blank");
  await other.bringToFront();
  expect(
    await page.evaluate(() => document.hidden),
    "headless Chromium used to report a backgrounded page as visible; if this now fails, drive this test with a real background tab instead of the override below",
  ).toBe(false);
  await other.close();
  await page.bringToFront();

  await onAFrozenClock(page, async () => {
    await page.locator("#dispatch-btn").click();
    const toast = page.locator("#toast-stack .toast.success");
    await expect(toast).toBeVisible();

    // So the browser's own signal is simulated, exactly as `emulateMedia`
    // simulates the reduced-motion one above. What is NOT simulated is the
    // code under test: `scheduleAutoDismiss`'s real `visibilitychange`
    // handler runs, reads `document.hidden`, and pauses its real clock.
    await page.evaluate(() => {
      Object.defineProperty(document, "hidden", { configurable: true, get: () => true });
      document.dispatchEvent(new Event("visibilitychange"));
    });

    // The perverse case a bare `setTimeout` gets wrong: the notice exists
    // because the user may not be looking, and a hidden tab is the one state
    // where "3 seconds of being visible" definitively did not happen.
    await page.clock.runFor(NO_TIMER_HORIZON_MS);
    await expect(toast).toBeVisible();

    // Coming back resumes it rather than restarting it -- and the notice does
    // eventually leave, which is what makes this a pause and not a leak.
    await page.evaluate(() => {
      Object.defineProperty(document, "hidden", { configurable: true, get: () => false });
      document.dispatchEvent(new Event("visibilitychange"));
    });
    await runOutTheClock(page, SUCCESS_VISIBLE_MS);
  });

  await cleanUp(page, title);
});
