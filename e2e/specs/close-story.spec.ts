import { test, expect } from "./support";
import {
  cleanUpCreatedStories,
  createStory,
  deleteStory,
  openFilters,
  openProject,
  seedToken,
} from "./support";

/**
 * SH-507 separates deliberate closure from permanent deletion in both story
 * action surfaces. A Close keeps the story and writes its required reason as a
 * comment; Delete keeps SH-498's typed-id gate and offers Close as the safe
 * alternative only while that alternative is actually available.
 */

cleanUpCreatedStories("Alpha Project");

test.beforeEach(async ({ page }) => {
  await seedToken(page);
  await page.goto("/");
  await openProject(page, "Alpha Project");
});

function todoCard(page: import("@playwright/test").Page, title: string) {
  return page.locator('.column[data-state="todo"] .card', { hasText: title });
}

async function showClosed(page: import("@playwright/test").Page) {
  await openFilters(page);
  const toggle = page.locator("#toggle-closed");
  if (!(await toggle.isChecked())) await toggle.check();
  await expect(toggle).toBeChecked();
}

test("drawer Close requires a reason, records it as a comment, and leaves the closed story open", async ({
  page,
}) => {
  const title = `SH-507 drawer close ${Date.now()}`;
  const id = await createStory(page, title);
  const card = todoCard(page, title);
  await card.click();
  await expect(page.locator("#drawer")).toHaveClass(/open/);

  const footer = page.locator("#drawer-footer");
  await expect(footer.getByRole("button", { name: "Close", exact: true })).toBeVisible();
  await expect(footer.getByRole("button", { name: "Delete", exact: true })).toBeVisible();
  await footer.getByRole("button", { name: "Close", exact: true }).click();

  await expect(page.locator("#close-modal")).toHaveClass(/open/);
  await expect(page.locator("#close-reason")).toBeFocused();
  await page.locator("#close-modal-submit").click();
  await expect(page.locator("#close-modal-error")).toContainText("A closing reason is required.");
  await expect(card).toBeVisible();

  const reason = "Superseded by the smaller implementation";
  const request = page.waitForRequest((candidate) =>
    candidate.method() === "POST" &&
    candidate.url().endsWith(`/story/${id}/move`) &&
    candidate.postDataJSON()?.state === "closed",
  );
  await page.locator("#close-reason").fill(`  ${reason}  `);
  await page.locator("#close-modal-submit").click();
  expect((await request).postDataJSON()).toEqual({ state: "closed", comment: reason });

  await expect(page.locator("#close-modal")).not.toHaveClass(/open/);
  await expect(page.locator("#drawer")).toHaveClass(/open/);
  await expect(page.locator("#drawer-body")).toContainText(reason);
  await expect(footer.getByRole("button", { name: "Close", exact: true })).toHaveCount(0);
  await expect(footer.getByRole("button", { name: "Delete", exact: true })).toBeVisible();

  // A story that is already CLOSED cannot be closed again, so its permanent
  // delete confirmation must not offer a dead-end "Close instead" action.
  await footer.getByRole("button", { name: "Delete", exact: true }).click();
  await expect(page.locator("#delete-modal-summary")).toContainText(id);
  await expect(page.locator("#delete-modal-alternative")).toBeHidden();
  await page.locator("#delete-modal-cancel").click();
});

test("Cancel, backdrop, and Escape dismiss Close without changing the story", async ({ page }) => {
  const title = `SH-507 cancel close ${Date.now()}`;
  await createStory(page, title);
  const card = todoCard(page, title);
  await card.click();
  const close = page.locator("#drawer-footer").getByRole("button", { name: "Close", exact: true });

  await close.click();
  await page.locator("#close-modal-cancel").click();
  await expect(page.locator("#close-modal")).not.toHaveClass(/open/);
  await expect(page.locator("#drawer")).toHaveClass(/open/);
  await expect(card).toBeVisible();

  await close.click();
  await page.locator("#close-modal-backdrop").click({ position: { x: 5, y: 5 } });
  await expect(page.locator("#close-modal")).not.toHaveClass(/open/);
  await expect(page.locator("#drawer")).toHaveClass(/open/);
  await expect(card).toBeVisible();

  await close.click();
  await page.keyboard.press("Escape");
  await expect(page.locator("#close-modal")).not.toHaveClass(/open/);
  await expect(page.locator("#drawer")).toHaveClass(/open/);
  await expect(card).toBeVisible();

  await page.locator("#drawer-close").click();
  await deleteStory(page, title);
});

test("context-menu Close uses the shared modal and retains the story in closed", async ({ page }) => {
  const title = `SH-507 menu close ${Date.now()}`;
  await createStory(page, title);
  const card = todoCard(page, title);

  await card.click({ button: "right" });
  const menu = page.locator('.ctxmenu[aria-label="Story actions"]');
  await menu.getByRole("menuitem", { name: "Close", exact: true }).click();
  await expect(page.locator("#drawer")).not.toHaveClass(/open/);
  await expect(page.locator("#close-modal")).toHaveClass(/open/);

  await page.locator("#close-reason").fill("The experiment is complete");
  await page.locator("#close-modal-submit").click();
  await expect(page.locator("#toast-stack .toast.success")).toContainText("closed");

  await showClosed(page);
  await expect(page.locator('.column[data-state="closed"] .card', { hasText: title })).toBeVisible();
});

test("Delete offers Close instead and never asks for a deletion reason", async ({ page }) => {
  const title = `SH-507 delete alternative ${Date.now()}`;
  const id = await createStory(page, title);
  await todoCard(page, title).click();
  await page.locator("#drawer-footer").getByRole("button", { name: "Delete", exact: true }).click();

  await expect(page.locator("#delete-modal")).toHaveClass(/open/);
  await expect(page.locator("#delete-modal-cancel")).toBeFocused();
  await expect(page.locator("#delete-modal-summary")).toContainText(id);
  await expect(page.locator("#delete-reason")).toHaveCount(0);
  await expect(page.locator("#delete-modal-alternative")).toBeVisible();
  await page.locator("#delete-modal-close-instead").click();

  await expect(page.locator("#delete-modal")).not.toHaveClass(/open/);
  await expect(page.locator("#close-modal")).toHaveClass(/open/);
  await expect(page.locator("#close-modal-summary")).toContainText(title);
  await page.locator("#close-reason").fill("Worth keeping as historical context");
  await page.locator("#close-modal-submit").click();
  await expect(page.locator("#drawer")).toHaveClass(/open/);
  await expect(page.locator("#drawer-body")).toContainText("Worth keeping as historical context");
});
