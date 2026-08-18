import { test, expect } from "./support";
import type { Locator, Page } from "@playwright/test";
import { cleanUpCreatedStories, deleteStory, openProject, seedToken } from "./support";

/**
 * Exercises SH-199: every drawer section's add control now leads its list
 * (Comments, Relationships, Labels) instead of trailing it, so the control a
 * reader acts through sits right under the section heading rather than at
 * the bottom of however long the list has grown -- for Comments specifically,
 * that used to mean scrolling past the entire discussion just to add to it.
 *
 * This spec creates and deletes its own stories rather than touching the
 * "Alpha Project" fixture, whose exact two-story shape other specs
 * (filter-persistence.spec.ts, column-visibility.spec.ts) assert on
 * byte-for-byte per run-e2e.sh's own comment.
 */

cleanUpCreatedStories("Alpha Project");

test.beforeEach(async ({ page }) => {
  await seedToken(page);
  await page.goto("/");
  await openProject(page, "Alpha Project");
});

async function createStory(page: Page, title: string): Promise<void> {
  await page.locator("#new-story-btn").click();
  await expect(page.locator("#create-modal")).toHaveClass(/open/);
  await page.locator("#create-title").fill(title);
  await page.locator("#create-submit").click();
  await expect(page.locator("#create-modal")).not.toHaveClass(/open/);
  await expect(
    page.locator('.column[data-state="todo"] .card', { hasText: title }),
  ).toBeVisible();
}

async function openStory(page: Page, title: string): Promise<void> {
  await page
    .locator('.column[data-state="todo"] .card', { hasText: title })
    .click();
  await expect(page.locator("#drawer")).toHaveClass(/open/);
}

async function closeDrawer(page: Page): Promise<void> {
  await page.locator("#drawer-close").click();
  await expect(page.locator("#drawer")).not.toHaveClass(/open/);
}

/**
 * Opens `title`'s drawer just long enough to read its assigned id off
 * `#drawer-id`, then closes it -- the id is not otherwise predictable
 * client-side (assigned by the daemon), and the relationships test needs a
 * real one to type into the "Story ID" field.
 */
async function readStoryId(page: Page, title: string): Promise<string> {
  await openStory(page, title);
  const id = (await page.locator("#drawer-id").textContent())?.trim();
  await closeDrawer(page);
  if (!id) throw new Error(`no drawer id read for "${title}"`);
  return id;
}

/**
 * The direct child of `#drawer-body` that wraps the named section --
 * identified by its own heading rather than by position, since the drawer's
 * section order is exactly what this spec must not assume. Comments and
 * Relationships are collapsible (`.section-toggle`); Labels is not
 * (`.section-label`, a static heading).
 */
function sectionWrap(page: Page, headingSelector: string, name: string): Locator {
  return page
    .locator("#drawer-body > div")
    .filter({ has: page.locator(headingSelector, { hasText: name }) });
}

/**
 * True if `beforeSel` appears in the DOM before `afterSel`, scoped to
 * `scope`. The classes this spec checks (`.inline-add`, `.rel-list`,
 * `.label-chips`, `.comment-add`, `.comments`) are shared by more than one
 * drawer section, so an unscoped `document.querySelector` would silently
 * resolve to the wrong section's element rather than fail.
 */
async function precedes(scope: Locator, beforeSel: string, afterSel: string): Promise<boolean> {
  return scope.evaluate(
    (root, [before, after]) => {
      const b = root.querySelector(before);
      const a = root.querySelector(after);
      if (!b || !a) throw new Error(`missing "${before}" or "${after}" in scope`);
      return !!(b.compareDocumentPosition(a) & Node.DOCUMENT_POSITION_FOLLOWING);
    },
    [beforeSel, afterSel],
  );
}

test("the comment box leads the comments list, newest comment closest to it", async ({
  page,
}) => {
  const title = "SH-199 drawer order — comments";
  await createStory(page, title);
  await openStory(page, title);

  const scope = sectionWrap(page, ".section-toggle", "Comments");
  const textarea = scope.locator('textarea[data-field="comment"]');
  const submit = scope.locator("button.btn", { hasText: "Comment" });

  await textarea.fill("first comment");
  await submit.click();
  await expect(scope.locator(".comment")).toHaveCount(1);

  await textarea.fill("second comment");
  await submit.click();
  await expect(scope.locator(".comment")).toHaveCount(2);

  expect(await precedes(scope, ".comment-add", ".comments")).toBe(true);
  await expect(
    scope.locator(".comment").first().locator(".comment-text"),
  ).toHaveText("second comment");

  await closeDrawer(page);
  await deleteStory(page, title);
});

test("the relate box leads the relationships list", async ({ page }) => {
  const targetTitle = "SH-199 drawer order — relate target";
  const title = "SH-199 drawer order — relationships";
  await createStory(page, targetTitle);
  const targetId = await readStoryId(page, targetTitle);
  await createStory(page, title);
  await openStory(page, title);

  const scope = sectionWrap(page, ".section-toggle", "Relationships");
  await scope.locator('input[data-field="relationship-id"]').fill(targetId);
  await scope.locator("button.btn", { hasText: "Add" }).click();
  await expect(scope.locator(".rel-row")).toHaveCount(1);

  expect(await precedes(scope, ".inline-add", ".rel-list")).toBe(true);

  await closeDrawer(page);
  await deleteStory(page, title);
  await deleteStory(page, targetTitle);
});

test("the add-label box leads the label chips", async ({ page }) => {
  const title = "SH-199 drawer order — labels";
  await createStory(page, title);
  await openStory(page, title);

  const scope = sectionWrap(page, ".section-label", "Labels");
  const input = scope.locator('input[data-field="label-add"]');
  await input.fill("e2e-order-check");
  await input.press("Enter");
  await expect(scope.locator(".label-chip")).toHaveCount(1);

  expect(await precedes(scope, ".inline-add", ".label-chips")).toBe(true);

  await closeDrawer(page);
  await deleteStory(page, title);
});
