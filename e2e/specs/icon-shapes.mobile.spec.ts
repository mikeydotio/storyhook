import { test, expect } from "./support";
import {
  createStory,
  deleteStory,
  openProject,
  seedToken,
  THEMES,
} from "./support";
import type { Locator } from "@playwright/test";

/**
 * SH-620's coarse-pointer coverage. Board, table, and mobile-list action
 * buttons all open the same story-actions menu, so they deliberately share
 * the tools emoji and keep their story-specific aria-labels.
 */

test.beforeEach(async ({ page }) => {
  await seedToken(page);
  await page.goto("/");
});

async function expectActionsEmoji(locator: Locator): Promise<void> {
  await expect(locator).toBeVisible();
  await expect(locator).toHaveAttribute("data-emoji", "actions");
  await expect(locator).toHaveAttribute("aria-hidden", "true");
  await expect(locator).toHaveText("🛠️");
  const box = await locator.boundingBox();
  expect(box).not.toBeNull();
  expect(box!.width).toBeGreaterThan(0);
  expect(box!.height).toBeGreaterThan(0);
}

test("every mobile story-actions path uses the tools emoji and keeps its name", async ({
  page,
}) => {
  const title = "SH-620 emoji actions fixture";
  await openProject(page, "Alpha Project");
  await createStory(page, title);

  const card = page.locator(".card", { hasText: title });
  await expectActionsEmoji(card.locator(".card-actions-btn .emoji-icon"));
  await expect(card.locator(".card-actions-btn")).toHaveAttribute(
    "aria-label",
    `Actions for ${await card.getAttribute("data-id")}`,
  );

  await page.locator('#view-toggle button[data-view="list"]').click();
  const mobileItem = page.locator("#mobile-list-body > li", { hasText: title });
  await expectActionsEmoji(
    mobileItem.locator(".mobile-story-actions .emoji-icon"),
  );
  await expect(mobileItem.locator(".mobile-story-actions")).toHaveAttribute(
    "aria-label",
    `Actions for ${await mobileItem.getAttribute("data-id")}`,
  );

  await page.locator('#view-toggle button[data-view="board"]').click();
  await deleteStory(page, title);
});

test("the story-actions emoji stays present in every theme", async ({ page }) => {
  const title = "SH-620 emoji actions themes";
  await openProject(page, "Alpha Project");
  await createStory(page, title);

  const emoji = page
    .locator(".card", { hasText: title })
    .locator(".card-actions-btn .emoji-icon");
  for (const theme of THEMES) {
    await theme.apply(page);
    await expectActionsEmoji(emoji);
  }

  await deleteStory(page, title);
});
