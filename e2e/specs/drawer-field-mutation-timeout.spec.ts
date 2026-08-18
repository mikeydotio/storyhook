import { test, expect } from "./support";
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
  // deliberately delayed `route.fulfill()` below lands. Measured, not
  // hypothesized (SH-347's `interception-contract.spec.ts` probes A and C,
  // and this exact test run deterministically against a temporarily-lifted
  // quarantine): WebKit does not enforce `XMLHttpRequest.timeout` on a
  // request Playwright's route interception layer is holding -- the client
  // silently receives the late reply as an ordinary 200 instead of ever
  // seeing `ontimeout`. Reproduces every time, at idle, not load-sensitive --
  // a genuine engine/driver contract gap, not a flake. Chromium's `ontimeout`
  // fires correctly and stays fully load-bearing.
  test.skip(
    browserName === "webkit",
    "WebKit never fires XMLHttpRequest.ontimeout on a request held by Playwright route interception -- measured, deterministic (SH-347)",
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
    // Unconditional (SH-347): a route.fulfill() delivered after the client
    // has abandoned the request can throw -- `finally` is what stops that
    // from stranding the closing `await routeDone` below.
    try {
      await route.fulfill({ response });
    } finally {
      markRouteDone();
    }
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

/**
 * SH-367: the same lie, at the sites SH-310's fix never reached.
 *
 * `runFieldMutation` was routed through `describeMutationFailure` above, but
 * that function has only ever had four call sites. `toastError` has
 * seventeen, and at least eight of them guard a POST that mutates the store
 * -- `/reopen`, `/labels` add and remove, `/relate`, `/unrelate`,
 * `/comment`, `/archive`. Every one of those renders `err.message`
 * verbatim, so `api()`'s `{status: 0, message: "request timed out"}` is
 * reported as a definite failure by exactly the surfaces `runFieldMutation`
 * does not own.
 *
 * That is CLAUDE.md's SH-312 rule violated, not merely unimplemented: it
 * binds "any client this daemon has, present or future," and its recorded
 * consequence was a duplicate story filed 24 seconds later because the user
 * believed the message and retried. `runFieldMutation`'s own doc comment
 * says the fix arrived for "every caller of this function," which reads as
 * complete and is false one function over.
 *
 * Driven through the drawer's comment box because it is the cheapest
 * unambiguous `toastError` mutation site: one textarea, one button, no
 * second story to relate to and no label vocabulary to arrange.
 */
test("a slow but successful comment is reported honestly, not as a failure", async ({
  page,
  browserName,
}) => {
  // Same WebKit limitation as the test above, same story owns it, same
  // measurement: WebKit never fires `xhr.ontimeout` on a request Playwright's
  // route interception layer is holding (SH-347's `interception-contract.spec.ts`
  // probes A and C) -- deterministic, not load-sensitive.
  test.skip(
    browserName === "webkit",
    "WebKit never fires XMLHttpRequest.ontimeout on a request held by Playwright route interception -- measured, deterministic (SH-347)",
  );
  const shrunkTimeoutMs = 300;

  let markRouteDone: () => void;
  const routeDone = new Promise<void>((resolve) => {
    markRouteDone = resolve;
  });

  await page.route(/\/story\/[^/]+\/comment$/, async (route) => {
    if (route.request().method() !== "POST") {
      await route.continue();
      return;
    }
    // Real request, real daemon, real commit -- the comment DOES land. The
    // client just never hears so, which is the entire point: the outcome is
    // unknown to the client and known-good to the store.
    const response = await route.fetch();
    await new Promise((resolve) => setTimeout(resolve, shrunkTimeoutMs + 500));
    // Unconditional (SH-347): a route.fulfill() delivered after the client
    // has abandoned the request can throw -- `finally` is what stops that
    // from stranding the closing `await routeDone` below.
    try {
      await route.fulfill({ response });
    } finally {
      markRouteDone();
    }
  });

  await page.goto(`/?mutationTimeoutMs=${shrunkTimeoutMs}`);
  await openProject(page, "Alpha Project");

  // A seeded story, not a created one. This pin is about what the comment
  // form's `.catch` SAYS, so the story only has to exist -- and creating one
  // here opened the new story's drawer, whose backdrop then intercepted the
  // click meant to open it. Commenting leaves the fixture's own stories
  // untouched apart from one comment, and each engine runs against its own
  // seed (SH-335), so there is nothing to clean up and nothing to collide.
  const card = page.locator('.column[data-state="todo"] .card').first();
  await expect(card).toBeVisible();
  await card.click();
  await expect(page.locator("#drawer")).toHaveClass(/open/);

  await page.locator('textarea[placeholder="Add a comment…"]').fill("SH-367 probe");
  await page.locator("#drawer-body .comment-add button").click();

  const toast = page.locator("#toast-stack .toast.error");
  // The defect: this read "request timed out" before this story -- a definite
  // claim of failure that the committed comment disproves.
  await expect(toast).toContainText("may or may not have gone through");
  await expect(toast).not.toContainText("request timed out");

  // And the sentence names its subject. Without this the honest message is
  // still unfinished: a reader mid-burst cannot tell WHICH write is in doubt.
  const subject = toast.locator(".notice-detail");
  await expect(subject).toContainText("comment");
  // Composed, not relayed (SH-304's rule): the reader gets `<id> · comment`,
  // never the raw request line.
  await expect(subject).not.toContainText("/api/");
  await expect(subject).not.toContainText("POST");

  await routeDone;
});
