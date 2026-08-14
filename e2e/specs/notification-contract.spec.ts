import { test, expect } from "@playwright/test";
import type { Page } from "@playwright/test";
import { cleanUpCreatedStories, deleteStory, openProject, seedToken } from "./support";

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
 */

cleanUpCreatedStories("Alpha Project");

/** How long a success notice is held before its fade starts, plus the fade
 * itself — `TOAST_LIFETIME_MS` + `TOAST_FADE_MS` in `web_dashboard.html`,
 * restated here rather than imported because the dashboard is a single HTML
 * file with no module boundary a spec can reach into. */
const SUCCESS_VISIBLE_MS = 3000;
const FADE_MS = 1000;

/** Generous enough that a loaded machine's timer jitter cannot fail a test
 * whose subject is "this eventually goes away", while still well under the
 * durability probe below — the two must not overlap or neither assertion
 * means anything. */
const GONE_TIMEOUT = SUCCESS_VISIBLE_MS + FADE_MS + 4000;

/** How long a durable notice must survive to prove it has no timer at all.
 * Comfortably past both the old 4.5s/9s lifetimes this work deleted and the
 * new 3s+1s one, so a regression to ANY of the three fails here. */
const DURABILITY_PROBE_MS = 5500;

test.beforeEach(async ({ page }) => {
  await seedToken(page);
  await page.goto("/");
});

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
 * dispatch straight from the drawer footer and are still in it. */
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

test("a successful attended dispatch is a short toast that fades on its own", async ({
  page,
}) => {
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

  await expect(page.locator("#toast-stack .toast")).toHaveCount(0, {
    timeout: GONE_TIMEOUT,
  });

  await cleanUp(page, title);
});

test("a successful --auto dispatch also fades, and names itself autonomous", async ({
  page,
}) => {
  const title = "SH-304 — auto success fades";
  const id = await openFreshStory(page, title);
  await stubDispatch(page, id, true, "ok");

  await page.locator("#dispatch-auto-btn").click();

  // SH-232 sent this to a durable row because "nobody is necessarily
  // watching". SH-304's council reversed that for SUCCESS specifically: the
  // story moving on the board and the tmux window both corroborate it, so
  // the notice is a courtesy and may clear itself.
  const toast = page.locator("#toast-stack .toast.success");
  await expect(toast).toBeVisible();
  await expect(toast).toHaveText(`${id} dispatched (auto)`);
  await expect(page.locator("#dispatch-history .dispatch-history-row")).toHaveCount(0);

  await expect(page.locator("#toast-stack .toast")).toHaveCount(0, {
    timeout: GONE_TIMEOUT,
  });

  await cleanUp(page, title);
});

test("a refused attended dispatch is durable, keeps its diagnosis, and dismisses only by click", async ({
  page,
}) => {
  const title = "SH-304 — attended refusal is durable";
  const id = await openFreshStory(page, title);
  await stubDispatch(page, id, false, "refused");

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

  // The whole point: no timer at all. This outlives the deleted 4.5s and 9s
  // lifetimes and the new 3s+1s one alike.
  await page.waitForTimeout(DURABILITY_PROBE_MS);
  await expect(toast).toBeVisible();

  await toast.locator(".toast-dismiss").click();
  await expect(page.locator("#toast-stack .toast")).toHaveCount(0);

  await cleanUp(page, title);
});

test("a failed attended dispatch is durable too, and reads differently from a refusal", async ({
  page,
}) => {
  const title = "SH-304 — attended failure is durable";
  const id = await openFreshStory(page, title);
  await stubDispatch(page, id, false, "failed");

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

  await page.waitForTimeout(DURABILITY_PROBE_MS);
  await expect(toast).toBeVisible();

  // Dismissed before cleanup, and not only for tidiness: the toast stack is
  // top-right at `z-index: 60`, which is over the drawer's own header, so a
  // durable notice left standing intercepts the click on `#drawer-close`.
  await toast.locator(".toast-dismiss").click();
  await cleanUp(page, title);
});

test("a refused --auto dispatch stays a durable history row (SH-232's surviving half)", async ({
  page,
}) => {
  const title = "SH-304 — auto refusal is durable";
  const id = await openFreshStory(page, title);
  await stubDispatch(page, id, true, "refused");

  await page.locator("#dispatch-auto-btn").click();

  // The case SH-227's incident review was actually about: an unattended run
  // whose only report is this row. Geography is unchanged for failures --
  // an auto failure still lands bottom-right, where SH-232 put it.
  const row = page.locator("#dispatch-history .dispatch-history-row.error");
  await expect(row).toBeVisible();
  await expect(row).toContainText(`${id} refused (auto)`);
  await expect(row.locator(".notice-detail")).toContainText("already in-progress");
  await expect(page.locator("#toast-stack .toast")).toHaveCount(0);

  await page.waitForTimeout(DURABILITY_PROBE_MS);
  await expect(row).toBeVisible();

  await row.locator(".dispatch-history-dismiss").click();
  await expect(page.locator("#dispatch-history .dispatch-history-row")).toHaveCount(0);

  await cleanUp(page, title);
});

test("hovering a fading notice holds its clock, and leaving resumes it (SC 2.2.1)", async ({
  page,
}) => {
  const title = "SH-304 — hover pauses the clock";
  const id = await openFreshStory(page, title);
  await stubDispatch(page, id, false, "ok");

  await page.locator("#dispatch-btn").click();
  const toast = page.locator("#toast-stack .toast.success");
  await expect(toast).toBeVisible();

  // WCAG 2.2 SC 2.2.1 (Timing Adjustable) is satisfied by letting the user
  // pause the limit; a pointer resting on the notice IS that request. Held
  // well past the full 3s+1s, the notice must still be there.
  await toast.hover();
  await page.waitForTimeout(SUCCESS_VISIBLE_MS + FADE_MS + 1500);
  await expect(toast).toBeVisible();

  // And the pause is a pause, not a cancellation: moving away resumes it.
  await page.mouse.move(0, 0);
  await expect(page.locator("#toast-stack .toast")).toHaveCount(0, {
    timeout: GONE_TIMEOUT,
  });

  await cleanUp(page, title);
});

test("a fading notice still clears under prefers-reduced-motion, without animating", async ({
  page,
}) => {
  await page.emulateMedia({ reducedMotion: "reduce" });

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

test("a backgrounded tab does not burn a notice's clock down unseen", async ({
  page,
  context,
}) => {
  const title = "SH-304 — hidden tab holds the clock";
  const id = await openFreshStory(page, title);
  await stubDispatch(page, id, false, "ok");

  await page.locator("#dispatch-btn").click();
  const toast = page.locator("#toast-stack .toast.success");
  await expect(toast).toBeVisible();

  // Backgrounding the tab for real does NOT express this, and the test says
  // so rather than leaving the next reader to rediscover it: headless
  // Chromium keeps every page's `visibilityState` "visible" whatever has
  // focus, so `bringToFront` on a sibling page changes nothing observable.
  // This assertion is the evidence for that claim -- if a future Playwright
  // or browser starts reporting it honestly, this line fails and whoever
  // sees it can delete the override below in favour of the real thing.
  const other = await context.newPage();
  await other.goto("about:blank");
  await other.bringToFront();
  expect(
    await page.evaluate(() => document.hidden),
    "headless Chromium used to report a backgrounded page as visible; if this now fails, drive this test with a real background tab instead of the override below",
  ).toBe(false);
  await other.close();
  await page.bringToFront();

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
  await page.waitForTimeout(SUCCESS_VISIBLE_MS + FADE_MS + 1500);
  await expect(toast).toBeVisible();

  // Coming back resumes it rather than restarting it -- and the notice does
  // eventually leave, which is what makes this a pause and not a leak.
  await page.evaluate(() => {
    Object.defineProperty(document, "hidden", { configurable: true, get: () => false });
    document.dispatchEvent(new Event("visibilitychange"));
  });
  await expect(page.locator("#toast-stack .toast")).toHaveCount(0, {
    timeout: GONE_TIMEOUT,
  });

  await cleanUp(page, title);
});
