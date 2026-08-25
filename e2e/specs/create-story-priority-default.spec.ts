import {
  cleanUpCreatedStories,
  deleteStory,
  expect,
  openProject,
  seedToken,
  test,
} from "./support";

/** Browser coverage for SH-449's required priority vocabulary and default. */

cleanUpCreatedStories("Alpha Project");

test.beforeEach(async ({ page }) => {
  await seedToken(page);
  await page.goto("/");
  await openProject(page, "Alpha Project");
});

test("the create form offers four priorities and preselects low", async ({
  page,
}) => {
  await page.locator("#new-story-btn").click();
  await expect(page.locator("#create-modal")).toHaveClass(/open/);

  await expect(page.locator("#create-priority")).toHaveValue("low");
  await expect(page.locator("#create-priority option")).toHaveText([
    "critical",
    "high",
    "medium",
    "low",
  ]);
});

test("submitting the preselected low priority creates without a warning", async ({
  page,
}) => {
  const title = "Low by default via the dashboard";

  await page.locator("#new-story-btn").click();
  await expect(page.locator("#create-modal")).toHaveClass(/open/);
  await page.locator("#create-title").fill(title);
  await page.locator("#create-submit").click();

  await expect(page.locator("#create-modal")).not.toHaveClass(/open/);
  await expect(
    page.locator('.column[data-state="todo"] .card', { hasText: title }),
  ).toBeVisible();
  await expect(page.locator("#toast-stack .toast.warn")).toHaveCount(0);

  await deleteStory(page, title);
});
