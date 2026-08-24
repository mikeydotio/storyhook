import { test, expect } from "./support";
import { createStory, deleteStory, openProject, seedToken } from "./support";

/**
 * SH-444's mobile half. `.card-actions-btn` (board) and `.row-actions-btn`
 * (list) used to render the same `⋯` (U+22EF MIDLINE HORIZONTAL ELLIPSIS)
 * character as `icon-shapes.spec.ts`'s desktop controls; converted to the
 * same `<svg class="icon">` shape for the same reason. Both are `display:
 * none` outside `pointer: coarse` (SH-235), which is why they belong here
 * rather than in the desktop spec -- this file only runs under
 * `mobile-chromium`/`mobile-webkit`, where that media query matches on
 * either engine (SH-348).
 */

test.beforeEach(async ({ page }) => {
  await seedToken(page);
  await page.goto("/");
});

test("the card and list-row actions buttons render an svg icon, not a character", async ({
  page,
}) => {
  const title = "SH-444 icon-shapes mobile fixture";
  await openProject(page, "Alpha Project");
  await createStory(page, title);

  const card = page.locator(".card", { hasText: title });
  const cardIcon = card.locator(".card-actions-btn svg.icon");
  await expect(cardIcon).toBeVisible();
  const cardBox = await cardIcon.boundingBox();
  expect(cardBox).not.toBeNull();
  expect(cardBox!.width).toBeGreaterThan(0);
  expect(cardBox!.height).toBeGreaterThan(0);

  await page.locator('#view-toggle button[data-view="list"]').click();
  const row = page.locator("tr[data-id]", { hasText: title });
  const rowIcon = row.locator(".row-actions-btn svg.icon");
  await expect(rowIcon).toBeVisible();
  const rowBox = await rowIcon.boundingBox();
  expect(rowBox).not.toBeNull();
  expect(rowBox!.width).toBeGreaterThan(0);
  expect(rowBox!.height).toBeGreaterThan(0);

  // Both buttons keep the accessible name they had before -- the icon swap
  // touched only what paints inside, not the label a screen reader announces.
  await expect(card.locator(".card-actions-btn")).toHaveAttribute(
    "aria-label",
    `Actions for ${await card.getAttribute("data-id")}`,
  );
  await expect(row.locator(".row-actions-btn")).toHaveAttribute(
    "aria-label",
    `Actions for ${await row.getAttribute("data-id")}`,
  );

  await page.locator('#view-toggle button[data-view="board"]').click();
  await expect(page.locator("#board-view")).toBeVisible();
  await deleteStory(page, title);
});
