import { test, expect } from "./support";
import type { Page, Locator } from "@playwright/test";
import { cleanUpCreatedStories, deleteStory, openProject, seedToken } from "./support";

/**
 * Exercises a real structural gap in `captureDrawerFocus`/`restoreDrawerFocus`
 * (`src/web_dashboard.html`): the snapshot they hand off across a
 * `renderDrawer()` rebuild is keyed only on `data-field`, never on which
 * story's body it was taken from. If `document.activeElement` is ever still
 * inside `#drawer-body` at the moment `renderDrawer()` runs for a
 * *different* story, that story's field value gets written into the new
 * story's matching field and focused.
 *
 * SH-283 filed this as reachable through a real WebKit `storyRef` click,
 * reasoning that WebKit doesn't move focus to a clicked `<button>`. That
 * premise does not survive contact with either engine: in both real WebKit
 * and Chromium, `mousedown`'s default action blurs whatever was previously
 * focused *before* the click event (and this app's `onClick` handlers) ever
 * run -- confirmed by direct event-order instrumentation against Playwright's
 * WebKit build, and independently corroborated by Safari's own documented
 * behavior (clicking a button doesn't focus it, but *does* remove focus from
 * whatever else was focused). No click- or keyboard-driven path in this
 * dashboard was found that leaves a field's focus surviving into a
 * different story's render (see SH-283's own comment for the full record,
 * including the other paths ruled out: the story context menu, roving-
 * tabindex Enter/Space, Escape-then-click).
 *
 * The guard this spec exercises is therefore shipped as defensive
 * hardening of a structurally unsound key, not as a fix for a reachable
 * data leak -- and this spec constructs its precondition rather than
 * observing it organically: `event.preventDefault()` on a capturing
 * `mousedown` listener is the standard, spec-legitimate way to suppress a
 * click's native focus-shift (the same trick UI libraries use, e.g.
 * `onMouseDown={e => e.preventDefault()}`), and is what a genuine
 * "this click doesn't blur the field" engine behavior would look like from
 * the DOM's perspective, if one existed. It proves the guard code is
 * correct without claiming any user can trigger it today.
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
 * (SH-218's second, full-detail render) before returning -- every
 * assertion below needs the settled render, not the first synchronous one
 * built from cached summary data. Assumes no drawer is already open: the
 * backdrop of an already-open drawer intercepts pointer events aimed at a
 * board card behind it.
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
 * Opens `card`'s drawer just long enough to read its assigned id off
 * `#drawer-id`, then closes it -- the id is not otherwise predictable
 * client-side (assigned by the daemon), and every test below needs a real
 * one to relate to or type before its actual drawer stays open.
 */
async function readStoryId(page: Page, card: Locator): Promise<string> {
  const id = await openStory(page, card);
  await closeDrawerAndWait(page);
  return id;
}

/** Closes the detail panel before the next test action. */
async function closeDrawerAndWait(page: Page): Promise<void> {
  await page.locator("#drawer-close").click();
  await expect(page.locator("#drawer")).not.toHaveClass(/open/);
}

/**
 * Relates the currently-open drawer's story to `targetId` through the real
 * add-relation form, and waits for the resulting `.rel-row` -- the
 * `storyRef` this file's specs click to trigger the story switch.
 */
async function addRelation(page: Page, targetId: string): Promise<void> {
  await page.locator('input[data-field="relationship-id"]').fill(targetId);
  await page.locator(".inline-add button.btn", { hasText: "Add" }).click();
  await expect(page.locator(".rel-row", { hasText: targetId })).toBeVisible();
}

/**
 * Clicks `locator` while suppressing the native focus-shift a real click
 * would otherwise cause -- see this file's header comment for why. Installs
 * a capturing `mousedown` listener that `preventDefault()`s once the event
 * target is inside `.rel-id`, then performs a real click; `click()` still
 * fires normally (mousedown's default action controls focus, not the click
 * event itself), so the app's own `onClick` handler runs exactly as it
 * would for a genuine user gesture.
 */
async function clickWithoutBlurring(page: Page, locator: Locator): Promise<void> {
  await page.evaluate(() => {
    document.addEventListener(
      "mousedown",
      (e) => {
        const target = e.target as HTMLElement;
        if (target.closest && target.closest(".rel-id")) e.preventDefault();
      },
      true,
    );
  });
  await locator.click();
}

/** Every PATCH `/story/<id>` this test's route interception has observed. */
function recordPatches(page: Page): Array<{ id: string; body: unknown }> {
  const patches: Array<{ id: string; body: unknown }> = [];
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

test("a focus-preserving storyRef click does not carry a description edit into the story it opens", async ({
  page,
}) => {
  const titleA = "SH-283 description identity — source A";
  const titleB = "SH-283 description identity — target B";
  const descA = "A's original description";
  const descB = "B's original description";
  const editedText = "A's edited but uncommitted description";

  const cardA = await createStory(page, titleA, descA);
  const cardB = await createStory(page, titleB, descB);
  const idB = await readStoryId(page, cardB);
  await openStory(page, cardA);
  await addRelation(page, idB);

  const patches = recordPatches(page);

  await page.locator(".description-view").click();
  await page.locator(".description-field").fill(editedText);

  await clickWithoutBlurring(page, page.locator(".rel-row .rel-id", { hasText: idB }));

  await expect(page.locator("#drawer-id")).toHaveText(idB);
  await expect(page.locator(".description-section")).not.toHaveClass(/editing/);
  await expect(page.locator(".description-field")).toBeHidden();
  await expect(page.locator(".description-view")).toContainText(descB);
  await expect(page.locator(".description-view")).not.toContainText(editedText);
  await expect(page.locator(".description-field")).toHaveValue(descB);

  // Whatever now has focus, blur it the way a real next action would, and
  // confirm the leak never reaches the server either.
  await page.locator(".drawer-title").click();
  await page.waitForTimeout(300);
  expect(
    patches.filter((p) => p.id === idB && (p.body as { description?: string })?.description === editedText),
  ).toEqual([]);

  await closeDrawerAndWait(page);
  await deleteStory(page, titleA);
  await deleteStory(page, titleB);
});

test("a focus-preserving storyRef click does not carry a title edit into the story it opens", async ({ page }) => {
  const titleA = "SH-283 title identity — source A";
  const titleB = "SH-283 title identity — target B";
  const editedTitle = "A's edited but uncommitted title";

  const cardA = await createStory(page, titleA, "");
  const cardB = await createStory(page, titleB, "");
  const idB = await readStoryId(page, cardB);
  await openStory(page, cardA);
  await addRelation(page, idB);

  const patches = recordPatches(page);

  await page.locator(".drawer-title").fill(editedTitle);

  await clickWithoutBlurring(page, page.locator(".rel-row .rel-id", { hasText: idB }));

  await expect(page.locator("#drawer-id")).toHaveText(idB);
  await expect(page.locator(".drawer-title")).toHaveValue(titleB);
  await expect(page.locator(".drawer-title")).not.toHaveValue(editedTitle);

  await page.locator(".description-view").click();
  await page.waitForTimeout(300);
  expect(
    patches.filter((p) => p.id === idB && (p.body as { title?: string })?.title === editedTitle),
  ).toEqual([]);

  await closeDrawerAndWait(page);
  await deleteStory(page, titleA);
  await deleteStory(page, titleB);
});

test("a focus-preserving storyRef click does not carry an unsubmitted relation ID into the story it opens", async ({
  page,
}) => {
  const titleA = "SH-283 relation-id identity — source A";
  const titleB = "SH-283 relation-id identity — target B";
  const titleX = "SH-283 relation-id identity — typed but unsubmitted X";

  const cardA = await createStory(page, titleA, "");
  const cardB = await createStory(page, titleB, "");
  const cardX = await createStory(page, titleX, "");
  const idB = await readStoryId(page, cardB);
  const idX = await readStoryId(page, cardX);
  await openStory(page, cardA);
  await addRelation(page, idB);

  // Typed into the relationship-id input, updating both the DOM and the
  // pendingRelation scratch -- but never submitted with Add.
  await page.locator('input[data-field="relationship-id"]').fill(idX);

  await clickWithoutBlurring(page, page.locator(".rel-row .rel-id", { hasText: idB }));

  await expect(page.locator("#drawer-id")).toHaveText(idB);
  // openDrawer() always clears pendingRelation before rendering B, so a
  // correctly-gated snapshot leaves B's own (freshly built, unrelated)
  // relationship-id input exactly as empty as that scratch is.
  await expect(page.locator('input[data-field="relationship-id"]')).toHaveValue("");

  await closeDrawerAndWait(page);
  await deleteStory(page, titleA);
  await deleteStory(page, titleB);
  await deleteStory(page, titleX);
});
