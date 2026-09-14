import { test, expect, cleanUpCreatedStories, openProject, seedToken } from "./support";

cleanUpCreatedStories("Alpha Project");

test("touch opens primary and nested menus by tap and applies priority", async ({ page }) => {
  await seedToken(page);
  await page.goto("/");
  await openProject(page, "Alpha Project");
  await page.locator("#new-story-btn").tap();
  await page.locator("#create-title").fill("SH-715 touch fixture");
  await page.locator("#create-submit").tap();
  await expect(page.locator("#create-modal")).not.toHaveClass(/open/);
  const card = page.locator(".card", { hasText: "SH-715 touch fixture" });
  await expect(card).toBeVisible();
  await expect(page.locator(".ctxmenu")).toHaveCount(0);
  await card.locator(".card-actions-btn").tap();
  const menu = page.getByRole("menu", { name: "Story actions", exact: true });
  await expect(menu).toBeVisible();
  await expect(page.locator(".ctxmenu-sub")).toHaveCount(0);
  await menu.getByRole("menuitem", { name: "Set Priority", exact: true }).tap();
  await expect(page.getByRole("menu", { name: "Set priority", exact: true })).toBeVisible();
  await page.getByRole("menuitemradio", { name: "critical" }).tap();
  await expect(page.locator(".ctxmenu")).toHaveCount(0);
  await card.locator(".card-actions-btn").tap();
  await menu.getByRole("menuitem", { name: "Set Priority", exact: true }).tap();
  await expect(page.getByRole("menuitemradio", { name: "critical" })).toHaveAttribute("aria-checked", "true");
});
