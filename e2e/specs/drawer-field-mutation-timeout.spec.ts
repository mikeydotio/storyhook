import { test, expect } from "@playwright/test";
import {
  cleanUpCreatedStories,
  deleteStory,
  openProject,
  resolvedTokenColor,
  seedToken,
} from "./support";

/**
 * SH-310: `runFieldMutation()` -- the applier behind every drawer field edit
 * (State, Priority, Assignee, Type) and, since this story, the context
 * menu's Set Priority -- used to end in a bare `.catch(toastError)`. That
 * cannot tell a definite daemon refusal apart from `api()`'s `status: 0`
 * ("no reply ever arrived, for reasons the client cannot diagnose" -- see
 * that function's own comment), so a client-side mutation timeout was
 * reported as the flat, sometimes-false "request timed out". This is the
 * identical defect SH-312 fixed for the create modal specifically
 * (`describeMutationFailure`; `duplicate-create.spec.ts`'s second test is
 * this spec's direct sibling) -- left unfixed for every other mutating
 * control in the drawer until now.
 *
 * Drives the fix through the drawer's own Priority select: it was the one
 * mutating priority control in the app before this story, and it is
 * `runFieldMutation`'s most direct caller -- so this spec is independent of
 * whether the context menu's Set Priority exists yet.
 *
 * The drawer itself does not repaint from this failure path (`fetchData()`'s
 * reply only calls `renderAll()`, which re-renders the board/list --
 * `renderDrawer()` runs only from `handleMutationSuccess`, for its own
 * story -- see that function's own comment), so the proof that the write
 * actually landed is read off the board card's colour, the same way
 * `duplicate-create.spec.ts` reads it off the board rather than the modal
 * that reported the ambiguous outcome.
 */

cleanUpCreatedStories("Alpha Project");

test.beforeEach(async ({ page }) => {
  await seedToken(page);
});

async function createStory(
  page: import("@playwright/test").Page,
  title: string,
) {
  await page.locator("#new-story-btn").click();
  await expect(page.locator("#create-modal")).toHaveClass(/open/);
  await page.locator("#create-title").fill(title);
  await page.locator("#create-submit").click();
  await expect(page.locator("#create-modal")).not.toHaveClass(/open/);
  const card = page.locator('.column[data-state="todo"] .card', {
    hasText: title,
  });
  await expect(card).toBeVisible();
  return card;
}

test("a slow but successful drawer field edit is reported honestly, not as a failure", async ({
  page,
  browserName,
}) => {
  // The client's own shrunk XHR timeout is expected to fire before the
  // deliberately delayed `route.fulfill()` below lands -- reliable under
  // Chromium, not under WebKit here, likely a Playwright/WebKit driver
  // interaction rather than a `runFieldMutation`/`api()` bug (both are
  // plain, engine-agnostic XHR code). Not yet root caused to the byte --
  // SH-347 owns that, alongside the same shape in `duplicate-create.spec.ts`
  // and `board-readiness.spec.ts`.
  test.skip(
    browserName === "webkit",
    "WebKit doesn't reliably surface a delayed route's timeout to the page's XHR (SH-347)",
  );
  const title = `SH-310 honest field-edit timeout ${Date.now()}`;
  const shrunkTimeoutMs = 300;

  // Signals once the intercepted route has actually called `fulfill()` --
  // awaited at the end of this test so Playwright cannot tear the page down
  // while that call is still pending (`duplicate-create.spec.ts`'s own
  // identical comment explains the "Fetch response has been disposed"
  // failure this closes).
  let markRouteDone: () => void;
  const routeDone = new Promise<void>((resolve) => {
    markRouteDone = resolve;
  });

  await page.route(/\/story\/[^/]+\/priority$/, async (route) => {
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

  const card = await createStory(page, title);
  await card.click();
  await expect(page.locator("#drawer")).toHaveClass(/open/);

  // Scoped to #drawer-body: an unscoped `.field` also matches the (hidden)
  // create-story modal's own Priority field (board-sort.spec.ts's own
  // comment records the strict-mode violation that caused).
  const prioritySelect = page
    .locator("#drawer-body .field", { hasText: "Priority" })
    .locator("select");
  await prioritySelect.selectOption("critical");

  // The old defect: this used to read "request timed out" -- a definite
  // claim of failure the write had already disproven.
  const toast = page.locator("#toast-stack .toast.error");
  await expect(toast).toContainText("may or may not have gone through");
  await expect(toast).not.toContainText("request timed out");

  await page.locator("#drawer-close").click();
  await expect(page.locator("#drawer")).not.toHaveClass(/open/);

  // The honest message's own refetch (`describeMutationFailure` calls
  // `fetchData()`) is what puts the truth on screen: the card's accent
  // stripe already reflects the committed write, with no further action.
  await expect
    .poll(() => card.evaluate((n) => getComputedStyle(n).borderLeftColor))
    .toBe(await resolvedTokenColor(page, "--p-critical"));

  await deleteStory(page, title);
  await routeDone;
});
