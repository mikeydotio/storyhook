import { test, expect } from "./support";
import type { Page, Locator } from "@playwright/test";
import { cleanUpCreatedStories, deleteStory, openProject, seedToken } from "./support";

/**
 * The touch half of SH-283, on a real WebKit engine.
 *
 * `drawer-focus-story-identity.spec.ts` proves the `renderDrawer()` identity
 * guard is correct, but it has to *construct* its precondition: it suppresses
 * a click's native focus-shift with `preventDefault()` on a capturing
 * `mousedown`, because no real gesture on either desktop engine leaves a
 * drawer field focused into a different story's render. This file asks the
 * complementary question with a real, unmodified gesture -- a **tap** -- and
 * asserts what the user actually gets.
 *
 * Why a separate mobile file rather than more cases over there: SH-283's
 * severity rested on a claim about touch specifically ("a tap on a relation
 * row leaves focus in the description textarea, so the captured value rides
 * across to the next story"), and until SH-348 added the `mobile-webkit`
 * project there was no engine in this suite that could answer it. The
 * `.mobile.spec.ts` suffix is the whole wiring: `MOBILE_SPECS` in
 * `e2e/playwright.config.ts` selects this file into `mobile-chromium` (Blink
 * under emulation, the control) and `mobile-webkit` (`devices["iPhone 15"]`,
 * the engine the claim was about), with no config edit and no hand-listing.
 *
 * The measurement that closed the claim, recorded here so the next reader
 * does not re-derive it. Event order for a tap on a `.rel-id` button while
 * the description textarea is focused, instrumented on both mobile engines:
 *
 *     pointerdown → touchstart → touchend → mousedown
 *       → blur → focusout → mouseup → click
 *
 * WebKit's *synthesized* `mousedown` carries the same focus-clearing default
 * action its real one does, so the field is already blurred by the time
 * `click` -- and therefore this app's `onClick`, and therefore `openDrawer`
 * -- runs. Identical on Blink. The pair's only divergence is where focus
 * lands afterwards (`ASIDE`, the drawer, on WebKit; the `BUTTON` on Blink),
 * which is SH-335's documented difference and cannot matter here, because the
 * field the snapshot would have been taken from is blurred either way.
 *
 * So the two things worth pinning are consequences, not the event order --
 * asserting that order would be asserting what the browser does, which the
 * guard makes irrelevant to this app's correctness anyway:
 *
 *   1. B's description is B's own. No leak, and no edit mode primed with A's
 *      text (the worse half of the leak, via SH-217's derived edit mode).
 *   2. **A's edit is saved.** This is the half nothing else covers. The blur
 *      above is what commits it; had it not fired, the identity guard would
 *      have converted SH-283's leak into silent loss of A's edit instead --
 *      the gap SH-283's plan named and then correctly dropped once the
 *      desktop measurement showed the blur always precedes the click.
 *
 * Creates and deletes its own stories rather than touching the "Alpha
 * Project" fixture, whose exact two-story shape other specs
 * (filter-persistence.spec.ts, column-visibility.spec.ts) assert on
 * byte-for-byte per run-e2e.sh's own comment.
 */

cleanUpCreatedStories("Alpha Project");

test.beforeEach(async ({ page }) => {
  await seedToken(page);
  await page.goto("/");
  await openProject(page, "Alpha Project");
});

async function createStory(page: Page, title: string, description: string): Promise<Locator> {
  await page.locator("#new-story-btn").click();
  await expect(page.locator("#create-modal")).toHaveClass(/open/);
  await page.locator("#create-title").fill(title);
  await page.locator("#create-description").fill(description);
  await page.locator("#create-submit").click();
  await expect(page.locator("#create-modal")).not.toHaveClass(/open/);
  const card = page.locator('.column[data-state="todo"] .card', { hasText: title });
  await expect(card).toBeVisible();
  return card;
}

/**
 * Opens `card`'s drawer and waits for the async detail GET to resolve
 * (SH-218's second, full-detail render) before returning -- every assertion
 * below needs the settled render, not the first synchronous one built from
 * cached summary data. Assumes no drawer is already open: an open drawer's
 * backdrop intercepts pointer events aimed at a board card behind it.
 */
async function openStory(page: Page, card: Locator): Promise<string> {
  const detailLoaded = page.waitForResponse(
    (resp) => /\/story\/[^/]+$/.test(new URL(resp.url()).pathname) && resp.request().method() === "GET",
  );
  await card.click();
  await expect(page.locator("#drawer")).toHaveClass(/open/);
  await detailLoaded;
  const id = (await page.locator("#drawer-id").textContent())?.trim();
  if (!id) throw new Error("no drawer id read after opening a story");
  return id;
}

/**
 * Closes the drawer and waits for the backdrop to finish re-hiding itself --
 * `closeDrawer()` only sets its `hidden` attribute 180ms later (a CSS fade),
 * and an un-hidden backdrop still intercepts a tap aimed at a board card
 * behind it even at opacity 0.
 */
async function closeDrawerAndWait(page: Page): Promise<void> {
  await page.locator("#drawer-close").click();
  await expect(page.locator("#drawer")).not.toHaveClass(/open/);
  await expect(page.locator("#drawer-backdrop")).toBeHidden();
}

/** Every PATCH `/story/<id>` this test's route interception has observed. */
function recordPatches(page: Page): Array<{ id: string; body: { description?: string } }> {
  const patches: Array<{ id: string; body: { description?: string } }> = [];
  page.route(/\/story\/[^/]+$/, async (route) => {
    const req = route.request();
    if (req.method() === "PATCH") {
      const match = /\/story\/([^/]+)$/.exec(new URL(req.url()).pathname);
      patches.push({ id: match ? decodeURIComponent(match[1]) : "", body: req.postDataJSON() });
    }
    await route.continue();
  });
  return patches;
}

test("a tap on a storyRef mid-edit saves the edit and opens the other story unpolluted", async ({ page }) => {
  const titleA = "SH-283 touch identity — source A";
  const titleB = "SH-283 touch identity — target B";
  const descA = "A's original description";
  const descB = "B's original description";
  const editedText = "A's edit, committed by the tap's own blur";

  const cardA = await createStory(page, titleA, descA);
  const cardB = await createStory(page, titleB, descB);
  const idB = await openStory(page, cardB);
  await closeDrawerAndWait(page);
  const idA = await openStory(page, cardA);

  // The `storyRef` the tap below lands on, added through the real form.
  await page.locator('input[data-field="relationship-id"]').fill(idB);
  await page.locator(".inline-add button.btn", { hasText: "Add" }).click();
  await expect(page.locator(".rel-row", { hasText: idB })).toBeVisible();

  const patches = recordPatches(page);

  // Mid-edit in A: a tap enters SH-217's edit mode and focuses the textarea.
  await page.locator(".description-view").tap();
  await expect(page.locator(".description-field")).toBeFocused();
  await page.locator(".description-field").fill(editedText);

  // The gesture under test. Real tap, nothing suppressed.
  await page.locator(".rel-row .rel-id", { hasText: idB }).tap();

  // 1 · B is B. Not A's text, and not primed in edit mode with it.
  await expect(page.locator("#drawer-id")).toHaveText(idB);
  await expect(page.locator(".description-section")).not.toHaveClass(/editing/);
  await expect(page.locator(".description-field")).toBeHidden();
  await expect(page.locator(".description-view")).toContainText(descB);
  await expect(page.locator(".description-view")).not.toContainText(editedText);
  await expect(page.locator(".description-field")).toHaveValue(descB);

  // 2 · A's edit was saved, not dropped on the floor. Asserted at the wire
  // first, then read back from the server -- a PATCH that was sent but
  // rejected would satisfy the first check alone.
  await expect
    .poll(() => patches.filter((p) => p.id === idA && p.body.description === editedText).length)
    .toBe(1);
  expect(patches.filter((p) => p.id === idB && p.body.description === editedText)).toEqual([]);

  await closeDrawerAndWait(page);
  await openStory(page, cardA);
  await expect(page.locator(".description-view")).toContainText(editedText);

  await closeDrawerAndWait(page);
  await deleteStory(page, titleA);
  await deleteStory(page, titleB);
});
