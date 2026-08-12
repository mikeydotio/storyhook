import { test, expect } from "@playwright/test";
import { cleanUpCreatedStories, seedToken } from "./support";

/**
 * Pins SH-245's containment rule: a spec that never reaches its own cleanup
 * must not leave a story behind in a fixture project the rest of the suite
 * reads.
 *
 * Most specs create their stories in "Alpha Project", whose exact two-story
 * shape `filter-persistence.spec.ts` and `column-visibility.spec.ts` assert
 * on by count. Every one of them deletes what it created as the *last*
 * statement of the test body, so any failure above that line strands a
 * story — and the next spec to count Alpha's cards fails too, naming
 * something that was never involved. That is how SH-245's one genuine
 * failure was reported as three.
 *
 * The first test below is the stranding, deliberately: it creates a story
 * and returns without deleting it, exactly as a test that failed mid-body
 * would. The second proves the `afterEach` swept it anyway.
 */

cleanUpCreatedStories("Alpha Project");

const STRAY = "SH-245 stray — never cleaned up by its own test";

test.beforeEach(async ({ page }) => {
  await seedToken(page);
  await page.goto("/");
  await page.locator(".repo-card-name", { hasText: "Alpha Project" }).click();
  await expect(page.locator("#board-view")).toBeVisible();
});

test("a test can end without deleting the story it created", async ({
  page,
}) => {
  await page.locator("#new-story-btn").click();
  await expect(page.locator("#create-modal")).toHaveClass(/open/);
  await page.locator("#create-title").fill(STRAY);
  await page.locator("#create-submit").click();
  await expect(page.locator("#create-modal")).not.toHaveClass(/open/);
  await expect(
    page.locator('.column[data-state="todo"] .card', { hasText: STRAY }),
  ).toBeVisible();

  // No cleanup here on purpose — this test *is* the stranding.
});

test("the stray is gone by the next test, without that test asking", async ({
  page,
}) => {
  await expect(
    page.locator(".card", { hasText: STRAY }),
  ).toHaveCount(0);
});
