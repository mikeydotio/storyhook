import { test, expect } from "./support";
import {
  cleanUpCreatedStories,
  deleteStory,
  openProject,
  seedToken,
} from "./support";

/**
 * Exercises SH-197's shared delete-confirmation modal, which replaced the
 * drawer footer's own inline confirmation form. Both the drawer footer's
 * Delete button and the context menu's Delete item (added by a later
 * commit) open the same `#delete-modal` -- this file only drives the
 * drawer-footer entry point, which is enough to prove the modal itself
 * behaves correctly; the context menu's own spec proves it reaches the
 * same function.
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

async function createStory(page: import("@playwright/test").Page, title: string) {
  await page.locator("#new-story-btn").click();
  await expect(page.locator("#create-modal")).toHaveClass(/open/);
  await page.locator("#create-title").fill(title);
  await page.locator("#create-submit").click();
  await expect(page.locator("#create-modal")).not.toHaveClass(/open/);
  await expect(
    page.locator('.column[data-state="todo"] .card', { hasText: title }),
  ).toBeVisible();
}

test("Delete opens the modal over the still-open drawer", async ({ page }) => {
  const title = "SH-197 delete modal — opens over the drawer";
  await createStory(page, title);

  await page
    .locator('.column[data-state="todo"] .card', { hasText: title })
    .click();
  const id = await page.locator("#drawer-body").getAttribute("data-story-id");
  await expect(page.locator("#drawer")).toHaveClass(/open/);

  await page.locator("#drawer-footer button", { hasText: "Delete" }).click();
  await expect(page.locator("#delete-modal")).toHaveClass(/open/);
  // The drawer is still there, underneath -- Delete opens a modal on top of
  // it now, not a form that replaces the footer in place.
  await expect(page.locator("#drawer")).toHaveClass(/open/);
  await expect(page.locator("#delete-modal-summary")).toContainText(title);

  // Cleanup completes the ALREADY-OPEN modal directly rather than calling
  // the shared deleteStory() helper, which assumes a clean starting state
  // (no drawer, no modal) and would otherwise try to click a card that its
  // own still-open backdrop is covering.
  await page.locator("#delete-confirmation").fill(id!);
  await page.locator("#delete-modal-submit").click();
  await expect(page.locator("#delete-modal")).not.toHaveClass(/open/);
});

test("submitting with an empty confirmation shows an in-modal error and deletes nothing", async ({
  page,
}) => {
  const title = "SH-197 delete modal — empty confirmation";
  await createStory(page, title);
  const card = page.locator('.column[data-state="todo"] .card', {
    hasText: title,
  });

  await card.click();
  const id = (await card.getAttribute("data-id"))!;
  await expect(page.locator("#drawer")).toHaveClass(/open/);
  await page.locator("#drawer-footer button", { hasText: "Delete" }).click();
  await expect(page.locator("#delete-modal")).toHaveClass(/open/);

  await page.locator("#delete-modal-submit").click();
  await expect(page.locator("#delete-modal-error")).toContainText(`Type ${id} exactly`);
  // Still open, still there -- an empty submission is refused, not a no-op
  // that silently closes.
  await expect(page.locator("#delete-modal")).toHaveClass(/open/);
  await expect(card).toBeVisible();

  // Cleanup completes the ALREADY-OPEN modal directly -- see the identical
  // comment on the previous test for why deleteStory() can't be used here.
  await page.locator("#delete-confirmation").fill(id);
  await page.locator("#delete-modal-submit").click();
  await expect(page.locator("#delete-modal")).not.toHaveClass(/open/);
});

async function openDeleteModal(
  page: import("@playwright/test").Page,
  card: import("@playwright/test").Locator,
) {
  await card.click();
  const id = (await card.getAttribute("data-id"))!;
  await expect(page.locator("#drawer")).toHaveClass(/open/);
  await page.locator("#drawer-footer button", { hasText: "Delete" }).click();
  await expect(page.locator("#delete-modal")).toHaveClass(/open/);
}

test("Cancel dismisses the modal without deleting; the drawer stays open", async ({
  page,
}) => {
  const title = "SH-197 delete modal — Cancel";
  await createStory(page, title);
  const card = page.locator('.column[data-state="todo"] .card', {
    hasText: title,
  });

  await openDeleteModal(page, card);
  await page.locator("#delete-modal-cancel").click();
  await expect(page.locator("#delete-modal")).not.toHaveClass(/open/);
  await expect(page.locator("#drawer")).toHaveClass(/open/);
  await expect(card).toBeVisible();

  // SH-554: detail is a peer, so the board card remains reachable while the
  // panel is open. Let deleteStory() exercise that real route directly.
  await deleteStory(page, title);
});

test("clicking the backdrop dismisses the modal without deleting; the drawer stays open", async ({
  page,
}) => {
  const title = "SH-197 delete modal — backdrop click";
  await createStory(page, title);
  const card = page.locator('.column[data-state="todo"] .card', {
    hasText: title,
  });

  await openDeleteModal(page, card);
  await page
    .locator("#delete-modal-backdrop")
    .click({ position: { x: 5, y: 5 } });
  await expect(page.locator("#delete-modal")).not.toHaveClass(/open/);
  await expect(page.locator("#drawer")).toHaveClass(/open/);
  await expect(card).toBeVisible();

  // SH-554: detail is a peer, so the board card remains reachable while the
  // panel is open. Let deleteStory() exercise that real route directly.
  await deleteStory(page, title);
});

test("Escape dismisses the modal (and, like every other dismissible surface, the drawer behind it) without deleting", async ({
  page,
}) => {
  const title = "SH-197 delete modal — Escape";
  await createStory(page, title);
  const card = page.locator('.column[data-state="todo"] .card', {
    hasText: title,
  });

  await openDeleteModal(page, card);
  // The shared Escape handler (bindShell) closes every dismissible surface
  // at once, not just the topmost -- a pre-existing behavior this modal
  // inherits rather than introduces (archive-modal has the same shape).
  // Not this story's fix to make; asserted here so a future change to that
  // handler's scope has to touch this test deliberately.
  await page.keyboard.press("Escape");
  await expect(page.locator("#delete-modal")).not.toHaveClass(/open/);
  await expect(page.locator("#drawer")).not.toHaveClass(/open/);
  await expect(card).toBeVisible();

  await deleteStory(page, title);
});

test("a real deletion toasts, closes the drawer, and removes the card", async ({
  page,
}) => {
  const title = "SH-197 delete modal — real deletion";
  await createStory(page, title);
  const card = page.locator('.column[data-state="todo"] .card', {
    hasText: title,
  });
  const id = (await card.getAttribute("data-id"))!;

  await card.click();
  await expect(page.locator("#drawer")).toHaveClass(/open/);
  await page.locator("#drawer-footer button", { hasText: "Delete" }).click();
  await page.locator("#delete-confirmation").fill(id);
  await page.locator("#delete-modal-submit").click();

  await expect(page.locator("#toast-stack .toast.success")).toContainText(
    "deleted",
  );
  await expect(page.locator("#delete-modal")).not.toHaveClass(/open/);
  await expect(page.locator("#drawer")).not.toHaveClass(/open/);
  await expect(card).not.toBeVisible();
});
