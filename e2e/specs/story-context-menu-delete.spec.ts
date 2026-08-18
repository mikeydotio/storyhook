import { test, expect } from "./support";
import { cleanUpCreatedStories, openProject, seedToken } from "./support";

/**
 * Exercises SH-197's context menu Delete item: it reaches the exact same
 * `#delete-modal` (`src/web_dashboard.html`'s `openDeleteModal`, from the
 * shared-delete-modal commit) the drawer footer's own Delete button opens
 * -- one confirmation surface for the one destructive action. The modal
 * itself (empty-reason validation, Cancel, backdrop, Escape, a real
 * deletion) is already thoroughly covered by delete-story.spec.ts against
 * the drawer-footer entry point; this file's own job is narrower: proving
 * the menu's Delete item reaches it correctly, with no drawer open first.
 *
 * This spec creates and deletes its own story rather than touching the
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

async function createStory(page: import("@playwright/test").Page, title: string) {
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

test("Delete from the menu opens the shared modal with no drawer open, and completing it removes the card", async ({
  page,
}) => {
  const title = "SH-197 context menu — delete";
  const card = await createStory(page, title);

  await card.click({ button: "right" });
  const menu = page.locator(".ctxmenu");
  await expect(menu).toBeVisible();

  const deleteItem = menu.locator(".ctxmenu-item", { hasText: "Delete" });
  await expect(deleteItem).toBeVisible();
  await expect(deleteItem).toHaveClass(/danger/);
  await deleteItem.click();

  // Straight to the modal -- no drawer was ever opened for this story.
  await expect(page.locator(".ctxmenu")).toHaveCount(0);
  await expect(page.locator("#delete-modal")).toHaveClass(/open/);
  await expect(page.locator("#drawer")).not.toHaveClass(/open/);
  await expect(page.locator("#delete-modal-summary")).toContainText(title);

  await page.locator("#delete-reason").fill("e2e: context menu delete");
  await page.locator("#delete-modal-submit").click();

  await expect(page.locator("#toast-stack .toast.success")).toContainText(
    "deleted",
  );
  await expect(page.locator("#delete-modal")).not.toHaveClass(/open/);
  await expect(card).not.toBeVisible();
});
