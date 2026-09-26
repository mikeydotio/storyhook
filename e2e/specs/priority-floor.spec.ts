import { test, expect } from "./support";
import {
  cleanUpCreatedStories,
  openProject,
  resolvedTokenColor,
  seedToken,
} from "./support";

/**
 * SH-788's browser half: a story that blocks more urgent open work sorts at
 * that work's level -- its blocker floor, which the server derives
 * (`domain::BlockerFloors`) and ships as `blocker_floor` -- and its card's
 * accent stripe interleaves its own colour with the floor's. Everything
 * here runs through the real API and the drawer's own controls; nothing is
 * injected, because the claim under test is that the server's derivation
 * reaches the board, not that the board draws a field it is handed.
 *
 * The stories are this spec's own. `cleanUpCreatedStories` removes them
 * whether the test passes or not, so Alpha Project's two-story fixture
 * shape survives for the specs that assert on it.
 */

cleanUpCreatedStories("Alpha Project");

test.beforeEach(async ({ page }) => {
  await seedToken(page);
  await page.goto("/");
  await openProject(page, "Alpha Project");
});

type Page = import("@playwright/test").Page;

async function createStory(page: Page, title: string, priority: string) {
  await page.locator("#new-story-btn").click();
  await expect(page.locator("#create-modal")).toHaveClass(/open/);
  await page.locator("#create-title").fill(title);
  await page.locator("#create-priority").selectOption(priority);
  await page.locator("#create-submit").click();
  await expect(page.locator("#create-modal")).not.toHaveClass(/open/);
  const card = page.locator('.column[data-state="todo"] .card', { hasText: title });
  await expect(card).toBeVisible();
  return card;
}

/** The todo column's card titles, top to bottom. */
async function todoOrder(page: Page): Promise<string[]> {
  return page.locator('.column[data-state="todo"] .card .card-title').allTextContents();
}

async function closeDrawer(page: Page) {
  await page.locator("#drawer-close").click();
  await expect(page.locator("#drawer")).not.toHaveClass(/open/);
}

test("a low story that blocks a critical one sorts, stripes and reads at critical only while it blocks", async ({
  page,
}) => {
  const blocker = "SH-788 floor — low blocker";
  const dependent = "SH-788 floor — critical dependent";
  const unrelated = "SH-788 floor — unrelated high";
  const blockerCard = await createStory(page, blocker, "low");
  const dependentCard = await createStory(page, dependent, "critical");
  await createStory(page, unrelated, "high");
  const dependentId = (await dependentCard.getAttribute("data-id"))!;
  const low = await resolvedTokenColor(page, "--p-low");
  const critical = await resolvedTokenColor(page, "--p-critical");

  // Before any edge: an ordinary low card, below the high one.
  await expect(blockerCard).not.toHaveClass(/priority-floor/);
  let order = await todoOrder(page);
  expect(order.indexOf(unrelated)).toBeLessThan(order.indexOf(blocker));

  // The blocker's own drawer records the edge, the way a user would.
  await blockerCard.click();
  await expect(page.locator("#drawer")).toHaveClass(/open/);
  const relationships = page
    .locator("#drawer-body > div")
    .filter({ has: page.locator(".section-toggle", { hasText: "Relationships" }) });
  await relationships.locator('input[data-field="relationship-id"]').fill(dependentId);
  await relationships.locator(".inline-add select").selectOption("blocks");
  await relationships.locator("button.btn", { hasText: "Add" }).click();
  await expect(page.locator(".rel-row", { hasText: dependentId })).toBeVisible();
  // The drawer edits the stored level and states the derived one beside it.
  await expect(page.locator("#drawer-priority-floor")).toHaveText(
    "Sorts as critical while it blocks more urgent work",
  );
  await closeDrawer(page);

  // The card: the class, both colours in the stripe, and the words.
  await expect(blockerCard).toHaveClass(/priority-floor/);
  await expect
    .poll(() => blockerCard.evaluate((n) => getComputedStyle(n).backgroundImage))
    .toContain(critical);
  expect(await blockerCard.evaluate((n) => getComputedStyle(n).backgroundImage)).toContain(low);
  await expect(blockerCard).toHaveAttribute(
    "aria-label",
    /priority low, sorted as critical while it blocks more urgent work/,
  );
  // The default Priority ↓ sort places it at critical: above the high card.
  order = await todoOrder(page);
  expect(order.indexOf(blocker)).toBeLessThan(order.indexOf(unrelated));

  // The List prints the CLI's parenthetical and splits the dot.
  await page.locator('#view-toggle button[data-view="list"]').click();
  await expect(page.locator("#list-view")).toBeVisible();
  const row = (title: string) => page.locator("tr[data-id]", { hasText: title });
  await expect(row(blocker)).toContainText("low (critical)");
  await expect(row(blocker).locator(".dot.priority-floor")).toHaveCount(1);
  await expect(row(unrelated)).not.toContainText("(");
  await page.locator('#view-toggle button[data-view="board"]').click();

  // Removing the edge ends the blockage, and the floor with it.
  await blockerCard.click();
  await expect(page.locator("#drawer")).toHaveClass(/open/);
  await relationships.locator(".rel-remove").click();
  await expect(page.locator(".rel-row", { hasText: dependentId })).toHaveCount(0);
  await expect(page.locator("#drawer-priority-floor")).toHaveCount(0);
  await closeDrawer(page);

  await expect(blockerCard).not.toHaveClass(/priority-floor/);
  await expect
    .poll(() => blockerCard.evaluate((n) => getComputedStyle(n).borderLeftColor))
    .toBe(low);
  order = await todoOrder(page);
  expect(order.indexOf(unrelated)).toBeLessThan(order.indexOf(blocker));
});
