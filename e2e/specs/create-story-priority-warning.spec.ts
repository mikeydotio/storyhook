import { test, expect } from "@playwright/test";
import {
  cleanUpCreatedStories,
  deleteStory,
  openProject,
  seedToken,
} from "./support";

/**
 * SH-358: the create modal's `POST /api/repos/{id}/story` reply carries the
 * same `warnings` field `story new` has warned in since SH-354/SH-359 — the
 * server side of this closed with no dashboard-specific code at all, since
 * the modal builds an ordinary `Invocation::New` (`src/api/rest.rs::
 * route_create_story`). What did not exist before this story is a browser
 * surface for it: `api()` already resolved the parsed envelope, and nothing
 * read `warnings`. `web_test.rs`'s
 * `web_create_story_with_no_priority_carries_the_unassessed_warning` proves
 * the server half; this is the one leg that can prove a human actually sees
 * it (`tests/web_test.rs` serves the HTML and greps its source, it cannot
 * click a button).
 */

cleanUpCreatedStories("Alpha Project");

test.beforeEach(async ({ page }) => {
  await seedToken(page);
  await page.goto("/");
  await openProject(page, "Alpha Project");
});

test("creating a story with no priority shows a warning toast naming the remedy", async ({
  page,
}) => {
  const title = "Unassessed via the dashboard";

  await page.locator("#new-story-btn").click();
  await expect(page.locator("#create-modal")).toHaveClass(/open/);
  await page.locator("#create-title").fill(title);
  // Priority left at its "Default priority" placeholder -- the omission
  // under test.
  await page.locator("#create-submit").click();

  await expect(page.locator("#create-modal")).not.toHaveClass(/open/);

  const toast = page.locator("#toast-stack .toast.warn");
  await expect(toast).toBeVisible();
  await expect(toast).toContainText("priority not set");
  await expect(toast).toContainText("story help priority-rubric");

  // A warning toast is durable (`noticeIsDurable`, SH-358) -- the same
  // reasoning "error" already has: this names an action the reader must
  // take, and a clock on it is a notice designed to be missed. Confirmed
  // here rather than assumed, since a self-clearing one would make this
  // whole test a race against `scheduleAutoDismiss`.
  const dismiss = toast.locator(".toast-dismiss");
  await expect(dismiss).toBeVisible();

  await deleteStory(page, title);
});

test("creating a story with a stated priority shows no warning toast", async ({
  page,
}) => {
  const title = "Assessed via the dashboard";

  await page.locator("#new-story-btn").click();
  await expect(page.locator("#create-modal")).toHaveClass(/open/);
  await page.locator("#create-title").fill(title);
  await page.locator("#create-priority").selectOption("high");
  await page.locator("#create-submit").click();

  await expect(page.locator("#create-modal")).not.toHaveClass(/open/);

  const card = page.locator('.column[data-state="todo"] .card', {
    hasText: title,
  });
  await expect(card).toBeVisible();
  await expect(page.locator("#toast-stack .toast.warn")).toHaveCount(0);

  await deleteStory(page, title);
});
