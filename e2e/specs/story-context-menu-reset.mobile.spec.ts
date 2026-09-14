import { test, expect, cleanUpCreatedStories, openProject, seedToken } from "./support";

cleanUpCreatedStories("Alpha Project");

test("the touch actions menu opens Reset and Cancel preserves the story", async ({ page }) => {
  await seedToken(page);
  await page.goto("/");
  await openProject(page, "Alpha Project");
  await page.locator("#new-story-btn").click();
  await page.locator("#create-title").fill("SH-717 mobile reset");
  await page.locator("#create-submit").click();
  const card = page.locator(".card", { hasText: "SH-717 mobile reset" });
  await expect(card).toBeVisible();
  const id = (await card.getAttribute("data-id"))!;
  await card.getByRole("button", { name: `Actions for ${id}`, exact: true }).tap();
  await page.getByRole("menuitem", { name: "Reset…", exact: true }).tap();
  await expect(page.locator("#reset-modal")).toHaveClass(/open/);
  await page.locator("#reset-modal-cancel").tap();
  await expect(page.locator("#reset-modal")).not.toHaveClass(/open/);
  await expect(card).toBeVisible();
});
