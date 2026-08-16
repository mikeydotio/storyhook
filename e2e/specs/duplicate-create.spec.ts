import { test, expect } from "@playwright/test";
import { cleanUpCreatedStories, openProject, seedToken } from "./support";

/**
 * SH-312: two identical stories were filed from the dashboard 24 seconds
 * apart (SH-310/SH-311). Root cause, verified by store forensics and three
 * independent code explorations: `submitCreate()` had no in-flight guard
 * (`#create-submit` was never disabled), and `api()`'s flat 10s mutation
 * timeout was shorter than the daemon's own documented worst case for a
 * write -- so a slow-but-successful create was reported to the user as
 * `"request timed out"`, a lie the user believed and acted on by clicking
 * Create again.
 *
 * This file has two specs, matching the two distinct failure modes the
 * council-approved fix (verdict on SH-312; the reasoning is written up in
 * `docs/rca/duplicate-story-from-the-dashboard.md`) closes:
 *
 * 1. The gesture path -- two submissions while the first is genuinely still
 *    in flight -- is now fully prevented by the in-flight guard (F1). This
 *    is the actual SH-310/SH-311 mechanism and the one this fix makes
 *    unrepresentable at the UI layer.
 * 2. The message path -- a real timeout is no longer reported as a definite
 *    failure (F3), and the deadline itself is raised to the server's own
 *    ceiling rather than a hand-copied guess (F7, pinned separately by
 *    `tests/dashboard_mutation_deadline.rs`).
 *
 * What this file deliberately does NOT assert: that clicking Create again
 * *after* a genuine, reported timeout is prevented from filing a second
 * story. The council considered and rejected server-side idempotency for
 * that residual case (Position A, unanimous) -- it remains representable by
 * design, recoverable with one `story delete`, and is out of scope for this
 * fix. `tests/web_test.rs::web_create_story_twice_with_identical_bodies_files_two_stories`
 * pins that as current, intentional behavior at the HTTP layer.
 */

cleanUpCreatedStories("Alpha Project");

test.beforeEach(async ({ page }) => {
  await seedToken(page);
});

test("a second click while the first create is still in flight files only one story", async ({
  page,
}) => {
  const title = `SH-312 in-flight guard ${Date.now()}`;
  let requestsSeen = 0;
  let releaseHold: (() => void) | null = null;
  const held = new Promise<void>((resolve) => {
    releaseHold = resolve;
  });

  // Holds the real POST open indefinitely -- `route.fetch()` sends the
  // actual request to the real daemon so the story genuinely gets created,
  // and `route.fulfill()` is delayed until the test releases it, the same
  // technique `holdDetailFetch` in support.ts uses for a GET. This models
  // the SH-310/SH-311 window directly: the daemon is still working when the
  // user's second gesture arrives.
  await page.route(/\/api\/repos\/[^/]+\/story$/, async (route) => {
    if (route.request().method() !== "POST") {
      await route.continue();
      return;
    }
    requestsSeen++;
    const response = await route.fetch();
    await held;
    await route.fulfill({ response });
  });

  await page.goto("/");
  await openProject(page, "Alpha Project");

  await page.locator("#new-story-btn").click();
  await expect(page.locator("#create-modal")).toHaveClass(/open/);
  await page.locator("#create-title").fill(title);

  const submit = page.locator("#create-submit");
  await submit.click();
  // The in-flight guard (SH-312) disables the button for the duration of
  // the outstanding request -- without it, this assertion is the first
  // thing that fails, before the second click even needs to run.
  await expect(submit).toBeDisabled();

  // The second gesture: a further click, and Enter in the title field --
  // both entry points `submitCreate()` is reachable from. Neither should
  // produce a second request while the guard holds.
  await submit.click({ force: true });
  await page.locator("#create-title").press("Enter");

  releaseHold!();
  await expect(page.locator("#create-modal")).not.toHaveClass(/open/);
  await expect(
    page.locator('.column[data-state="todo"] .card', { hasText: title }),
  ).toBeVisible();

  expect(requestsSeen).toBe(1);
  await expect(
    page.locator(".card", { hasText: title }),
  ).toHaveCount(1);
});

test("a slow but successful create is reported honestly, not as a failure", async ({
  page,
  browserName,
}) => {
  // The client's own shrunk XHR timeout is expected to fire before the
  // deliberately delayed `route.fulfill()` below lands -- reliable under
  // Chromium, not under WebKit here, likely a Playwright/WebKit driver
  // interaction rather than an `api()` bug (plain, engine-agnostic XHR
  // code). Not yet root caused to the byte -- SH-347 owns that, alongside
  // the same shape in `drawer-field-mutation-timeout.spec.ts` and
  // `board-readiness.spec.ts`.
  test.skip(
    browserName === "webkit",
    "WebKit doesn't reliably surface a delayed route's timeout to the page's XHR (SH-347)",
  );
  const title = `SH-312 honest timeout ${Date.now()}`;
  const shrunkTimeoutMs = 300;

  // Signals once the intercepted route has actually called `fulfill()` --
  // awaited at the end of this test so Playwright cannot tear the page down
  // (between this test and the next) while that call is still pending. Without
  // it, `route.fulfill()` intermittently throws "Fetch response has been
  // disposed": every assertion below can resolve well before this delayed
  // reply lands (fetchData()'s own GET already sees the commit), and a test
  // that returns then races its own still-sleeping route handler.
  let markRouteDone: () => void;
  const routeDone = new Promise<void>((resolve) => {
    markRouteDone = resolve;
  });

  await page.route(/\/api\/repos\/[^/]+\/story$/, async (route) => {
    if (route.request().method() !== "POST") {
      await route.continue();
      return;
    }
    // Real request, real daemon, real commit -- delayed comfortably past
    // the page's own (shrunk, via the query override) mutation deadline so
    // `ontimeout` fires on the client before this reply is delivered.
    const response = await route.fetch();
    await new Promise((resolve) => setTimeout(resolve, shrunkTimeoutMs + 500));
    await route.fulfill({ response });
    markRouteDone();
  });

  await page.goto(`/?mutationTimeoutMs=${shrunkTimeoutMs}`);
  await openProject(page, "Alpha Project");

  await page.locator("#new-story-btn").click();
  await expect(page.locator("#create-modal")).toHaveClass(/open/);
  await page.locator("#create-title").fill(title);
  await page.locator("#create-submit").click();

  // The old defect: this text used to read "request timed out" -- a
  // definite claim of failure the write had already disproven.
  await expect(page.locator("#create-error")).toContainText(
    "may or may not have gone through",
  );
  await expect(page.locator("#create-error")).not.toContainText(
    "request timed out",
  );

  // F3's refetch means the board tells the truth even though the modal,
  // showing an ambiguous outcome, stays open with the user's title intact.
  await expect(
    page.locator('.column[data-state="todo"] .card', { hasText: title }),
  ).toBeVisible();
  await expect(page.locator("#create-modal")).toHaveClass(/open/);

  await routeDone;
});
