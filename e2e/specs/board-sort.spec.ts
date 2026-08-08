import { test, expect } from "@playwright/test";
import { seedToken } from "./support";

/**
 * Exercises SH-128: two glyph-based buttons in the filter bar ("Priority",
 * "Order") pick how cards within a board column are ordered.
 *
 * Scope decided by council vote (`.council/sh128-board-sort-options/`,
 * unanimous 3-0): "story order" mirrors `domain::story_number`/`ready_order`
 * (src/domain.rs) — a numeric-aware comparison of the id's trailing digits,
 * not `created_at` and not raw string comparison — and the two buttons live
 * once in the shared `.filter-bar` (board-wide state, same precedent as
 * "Hide empty columns"), not repeated per column, despite the story's own
 * text literally saying "in the status column header" (a documented
 * deviation, recorded in DECISION.md). Default: priority descending (most
 * urgent first), with story order ascending as the secondary/tiebreak sort
 * when priority is selected — both spec'd directly by the story.
 *
 * This spec creates and deletes its own stories rather than touching the
 * "Alpha Project" fixture, whose exact two-story shape other specs
 * (filter-persistence.spec.ts, column-visibility.spec.ts) assert on
 * byte-for-byte per run-e2e.sh's own comment.
 */

test.beforeEach(async ({ page }) => {
  await seedToken(page);
  await page.goto("/");
  await page.locator(".repo-card-name", { hasText: "Alpha Project" }).click();
  await expect(page.locator("#board-view")).toBeVisible();
});

async function createStory(
  page: import("@playwright/test").Page,
  title: string,
  priority: string,
) {
  await page.locator("#new-story-btn").click();
  await expect(page.locator("#create-modal")).toHaveClass(/open/);
  await page.locator("#create-title").fill(title);
  await page.locator("#create-priority").selectOption(priority);
  await page.locator("#create-submit").click();
  await expect(page.locator("#create-modal")).not.toHaveClass(/open/);
  await expect(
    page.locator('.column[data-state="todo"] .card', { hasText: title }),
  ).toBeVisible();
}

async function deleteStory(
  page: import("@playwright/test").Page,
  title: string,
) {
  const card = page.locator('.column[data-state="todo"] .card', {
    hasText: title,
  });
  await card.click();
  await expect(page.locator("#drawer")).toHaveClass(/open/);
  await page.locator("#drawer-footer button", { hasText: "Delete" }).click();
  await page
    .locator('input[placeholder="Reason for deletion (required)"]')
    .fill("e2e cleanup");
  await page
    .locator("#drawer-footer button", { hasText: "Confirm delete" })
    .click();
  await expect(card).not.toBeVisible();
}

test("default board sort is priority descending, most urgent first", async ({
  page,
}) => {
  const low = "SH-128 sort test — low priority card";
  const critical = "SH-128 sort test — critical priority card";
  const high = "SH-128 sort test — high priority card";

  // Deliberately created out of priority order, so a passing assertion
  // proves the sort actually reordered them rather than happening to match
  // insertion order.
  await createStory(page, low, "low");
  await createStory(page, critical, "critical");
  await createStory(page, high, "high");

  await expect(page.locator("#boardsort-priority")).toHaveClass(/active/);
  await expect(page.locator("#boardsort-arrow-priority")).toHaveText("▼");

  const titles = page.locator(
    '.column[data-state="todo"] .card .card-title',
  );
  const ourTitles = (await titles.allTextContents()).filter((t) =>
    t.startsWith("SH-128 sort test"),
  );
  expect(ourTitles).toEqual([critical, high, low]);

  await deleteStory(page, low);
  await deleteStory(page, critical);
  await deleteStory(page, high);
});

test("within a tied priority, story order ascending breaks the tie", async ({
  page,
}) => {
  const first = "SH-128 sort test — tie A (created first)";
  const second = "SH-128 sort test — tie B (created second)";

  // Both `medium`, so a passing assertion proves the tiebreak is story
  // order (creation sequence), not insertion order in the DOM or a stable
  // sort artifact of some other field.
  await createStory(page, first, "medium");
  await createStory(page, second, "medium");

  const titles = page.locator(
    '.column[data-state="todo"] .card .card-title',
  );
  const ourTitles = (await titles.allTextContents()).filter((t) =>
    t.startsWith("SH-128 sort test"),
  );
  expect(ourTitles).toEqual([first, second]);

  await deleteStory(page, first);
  await deleteStory(page, second);
});

test("the Order button switches to story-order sort, and a second click flips its direction", async ({
  page,
}) => {
  const a = "SH-128 sort test — order toggle A";
  const b = "SH-128 sort test — order toggle B";

  await createStory(page, a, "none");
  await createStory(page, b, "none");

  await page.locator("#boardsort-order").click();
  await expect(page.locator("#boardsort-order")).toHaveClass(/active/);
  await expect(page.locator("#boardsort-priority")).not.toHaveClass(/active/);
  await expect(page.locator("#boardsort-arrow-order")).toHaveText("▲");

  let titles = page.locator('.column[data-state="todo"] .card .card-title');
  let ourTitles = (await titles.allTextContents()).filter((t) =>
    t.startsWith("SH-128 sort test"),
  );
  expect(ourTitles).toEqual([a, b]);

  await page.locator("#boardsort-order").click();
  await expect(page.locator("#boardsort-arrow-order")).toHaveText("▼");

  titles = page.locator('.column[data-state="todo"] .card .card-title');
  ourTitles = (await titles.allTextContents()).filter((t) =>
    t.startsWith("SH-128 sort test"),
  );
  expect(ourTitles).toEqual([b, a]);

  await deleteStory(page, a);
  await deleteStory(page, b);
});

test("the selected board sort persists across a reload on the same project", async ({
  page,
}) => {
  await page.locator("#boardsort-order").click();
  await expect(page.locator("#boardsort-order")).toHaveClass(/active/);

  await page.reload();

  await expect(page.locator("#board-view")).toBeVisible();
  await expect(page.locator("#boardsort-order")).toHaveClass(/active/);
  await expect(page.locator("#boardsort-arrow-order")).toHaveText("▲");
});
