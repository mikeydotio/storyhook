import { test, expect } from "./support";
import {
  cleanUpCreatedStories,
  deleteStory,
  openProject,
  seedToken,
} from "./support";

/**
 * Exercises SH-44: the "+ New" create-story modal preselects the project's
 * own first-configured OPEN state and first-configured type, sourced from
 * `meta.defaults` (`src/api/rest.rs::meta_json`) rather than a hardcoded
 * `"todo"`/`"story"` guess. Also exercises SH-127: creating a story no
 * longer flashes a success toast -- the new card's own "entering" animation
 * is the confirmation (council verdict on SH-127).
 *
 * Fixtures, from `scripts/run-e2e.sh`:
 *
 *   - "Alpha Project" (prefix AA) — the stock state catalog (todo,
 *     in-progress, blocked, done) plus an appended `review` OPEN state, and
 *     the stock, untouched type catalog (normal, epic, bug, chore). Neither
 *     append changes what sorts *first* in configured order, so Alpha's
 *     defaults stay `todo`/`normal` — the same values a fresh project would
 *     have. This spec's job is to prove the modal reads them from the
 *     catalog rather than assuming they'd always be those values regardless.
 */

cleanUpCreatedStories("Alpha Project");

test.beforeEach(async ({ page }) => {
  await seedToken(page);
  await page.goto("/");
  await openProject(page, "Alpha Project");
});

test("the create-story modal preselects required state, type, and priority defaults", async ({
  page,
}) => {
  await page.locator("#new-story-btn").click();
  await expect(page.locator("#create-modal")).toHaveClass(/open/);

  await expect(page.locator("#create-state")).toHaveValue("todo");
  await expect(page.locator("#create-type")).toHaveValue("normal");
  await expect(page.locator("#create-priority")).toHaveValue("low");
});

test("creating a story shows no success toast (SH-127) -- the card's own entrance animation is the confirmation", async ({
  page,
}) => {
  const title = "A story that should not flash a toast";

  await page.locator("#new-story-btn").click();
  await expect(page.locator("#create-modal")).toHaveClass(/open/);
  await page.locator("#create-title").fill(title);
  await page.locator("#create-submit").click();

  await expect(page.locator("#create-modal")).not.toHaveClass(/open/);

  const card = page.locator(".column[data-state=\"todo\"] .card", {
    hasText: title,
  });
  await expect(card).toBeVisible();
  await expect(page.locator("#toast-stack .toast.success")).toHaveCount(0);

  // Cleanup, same pattern as the sibling test below.
  await deleteStory(page, title);
});

test("submitting without touching metadata creates a story with the preselected defaults", async ({
  page,
}) => {
  const title = "A story left at its metadata defaults";

  await page.locator("#new-story-btn").click();
  await expect(page.locator("#create-modal")).toHaveClass(/open/);
  await page.locator("#create-title").fill(title);
  await page.locator("#create-submit").click();

  await expect(page.locator("#create-modal")).not.toHaveClass(/open/);

  const card = page.locator('.column[data-state="todo"] .card', {
    hasText: title,
  });
  await expect(card).toBeVisible();
  await expect(card.locator(".type-badge")).toHaveAttribute(
    "aria-label",
    "Type: normal",
  );

  // Alpha Project's story count is asserted by other specs (e.g.
  // filter-persistence.spec.ts's "0 / 2"), all sharing this one seeded
  // daemon for the whole e2e run — so this test cleans up the story it just
  // created rather than leaving it behind for the next spec to trip over.
  await deleteStory(page, title);
});
