import { test, expect } from "./support";
import {
  cleanUpCreatedStories,
  openFilters,
  openProject,
  projectSlug,
  requiredEnv,
  seedToken,
} from "./support";

/**
 * Exercises SH-407's third ask: a story that is itself blocked -- an open
 * `blocked-by` edge, or an `awaiting` reason -- shows in the Blocked column,
 * not wherever its literal `state` happens to be. Server-side mechanism:
 * `domain::compute_display_state` (generalized from `compute_epic_display_state`,
 * SH-165), reached by the board through the same `display_state || state`
 * idiom `story-status-light.spec.ts`/`list-state-pill.spec.ts` already
 * exercise for the epic case -- this file is that idiom's SH-407 case.
 *
 * This spec creates and deletes its own stories rather than touching the
 * "Alpha Project" fixture, whose exact two-story shape other specs
 * (filter-persistence.spec.ts, column-visibility.spec.ts) assert on
 * byte-for-byte per run-e2e.sh's own comment.
 */

cleanUpCreatedStories("Alpha Project");

const DASHBOARD_TOKEN = requiredEnv("DASHBOARD_TOKEN");

test.beforeEach(async ({ page }) => {
  await seedToken(page);
  await page.goto("/");
  await openProject(page, "Alpha Project");
});

async function createStory(
  page: import("@playwright/test").Page,
  title: string,
) {
  await page.locator("#new-story-btn").click();
  await expect(page.locator("#create-modal")).toHaveClass(/open/);
  await page.locator("#create-title").fill(title);
  await page.locator("#create-submit").click();
  await expect(page.locator("#create-modal")).not.toHaveClass(/open/);
  const card = page.locator('.column[data-state="todo"] .card', {
    hasText: title,
  });
  await expect(card).toBeVisible();
  return card;
}

/** Deletes `title`'s "todo"-column story through the drawer -- the shape
 * every local `deleteStory` in this suite shares (support.ts's own shared
 * helper is scoped the same way). */
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
  await expect(page.locator("#delete-modal")).toHaveClass(/open/);
  await page.locator("#delete-confirmation").fill((await card.getAttribute("data-id"))!);
  await page.locator("#delete-modal-submit").click();
  await expect(card).not.toBeVisible();
}

test("a story blocked by an open story shows in the Blocked column, is found by the blocked state filter, and returns to todo once the blocker clears", async ({
  page,
  request,
}) => {
  const blockerTitle = "SH-407 blocked column — the blocker";
  const workerTitle = "SH-407 blocked column — the blocked story";
  const blockerCard = await createStory(page, blockerTitle);
  const blockerId = (await blockerCard.getAttribute("data-id"))!;
  await createStory(page, workerTitle);

  // blocked-by, from the worker's own drawer -- same shape
  // card-blockers.spec.ts uses to set up its own dwell fixture.
  await page.locator(".card", { hasText: workerTitle }).click();
  await expect(page.locator("#drawer")).toHaveClass(/open/);
  await page.locator('input[placeholder="Story ID (e.g. SH-2)"]').fill(blockerId);
  await page.locator("#drawer-body .inline-add select").selectOption("blocked-by");
  await page
    .locator("#drawer-body .inline-add button", { hasText: "Add" })
    .click();
  await expect(page.locator(".rel-row", { hasText: blockerId })).toBeVisible();
  await page.locator("#drawer-close").click();
  await expect(page.locator("#drawer")).not.toHaveClass(/open/);

  const workerCard = page.locator(".card", { hasText: workerTitle });
  await expect(
    page.locator('.column[data-state="blocked"] .card', { hasText: workerTitle }),
  ).toBeVisible();
  await expect(
    page.locator('.column[data-state="todo"] .card', { hasText: workerTitle }),
  ).toHaveCount(0);

  // The state filter (`filteredStories`) reads `display_state || st.state`
  // too (SH-407) -- filtering by "blocked" must find a card the promotion
  // just relocated there, even though its own recorded state is still
  // "todo".
  await openFilters(page);
  await page.locator("#fdd-states .fdd-btn").click();
  await page
    .locator("#fdd-states .fdd-option", { hasText: "blocked" })
    .locator("input[type=checkbox]")
    .check();
  await expect(workerCard).toBeVisible();
  // Checking a checkbox doesn't close the dropdown panel -- no second
  // `.fdd-btn` click needed before reaching the checkbox again to uncheck.
  await page
    .locator("#fdd-states .fdd-option", { hasText: "blocked" })
    .locator("input[type=checkbox]")
    .uncheck();

  // Closes the blocker from outside this tab, forcing a genuine SSE-pushed
  // re-render -- moveStory()'s own request shape, same as
  // card-blockers.spec.ts.
  const slug = await projectSlug(request, "Alpha Project");
  const resp = await request.post(
    `/api/repos/${slug}/story/${blockerId}/move`,
    {
      headers: {
        "X-Storyhook": "1",
        "X-Storyhook-Token": DASHBOARD_TOKEN,
        "Content-Type": "application/json",
      },
      data: { state: "done" },
    },
  );
  expect(resp.ok()).toBe(true);

  // The worker's last blocker is gone, so is_ready is true again and the
  // promotion lifts -- the card returns to its own literal "todo" column.
  await expect(
    page.locator('.column[data-state="todo"] .card', { hasText: workerTitle }),
  ).toBeVisible({ timeout: 8000 });
  await expect(
    page.locator('.column[data-state="blocked"] .card', { hasText: workerTitle }),
  ).toHaveCount(0);

  await deleteStory(page, workerTitle);
  // The blocker is left CLOSED -- cleanUpCreatedStories's afterEach reopens
  // then deletes it, per card-blockers.spec.ts's own deleteStory comment.
});

test("an awaiting reason alone, with no blocked-by edge, also promotes a story to the Blocked column", async ({
  page,
  request,
}) => {
  const title = "SH-407 blocked column — awaiting only, no blocked-by edge";
  const card = await createStory(page, title);
  const id = (await card.getAttribute("data-id"))!;

  // The dashboard has no UI control that sets `awaiting` without also
  // moving the literal state to "blocked" (the drop-into-Blocked-column
  // reason prompt does both at once, per SH-205) -- reached directly, the
  // same way `story block <id> <reason>` reaches it from the CLI
  // (`StoryAction::Block`, `src/api/rest.rs`), to isolate this specific
  // promotion arm from the blocked-by one the test above already covers.
  const slug = await projectSlug(request, "Alpha Project");
  const resp = await request.post(`/api/repos/${slug}/story/${id}/block`, {
    headers: {
      "X-Storyhook": "1",
      "X-Storyhook-Token": DASHBOARD_TOKEN,
      "Content-Type": "application/json",
    },
    data: { reason: "waiting on a vendor API key" },
  });
  expect(resp.ok()).toBe(true);

  await expect(
    page.locator('.column[data-state="blocked"] .card', { hasText: title }),
  ).toBeVisible({ timeout: 8000 });
  await expect(
    page.locator('.column[data-state="todo"] .card', { hasText: title }),
  ).toHaveCount(0);

  const clear = await request.post(`/api/repos/${slug}/story/${id}/unblock`, {
    headers: {
      "X-Storyhook": "1",
      "X-Storyhook-Token": DASHBOARD_TOKEN,
      "Content-Type": "application/json",
    },
    data: {},
  });
  expect(clear.ok()).toBe(true);

  await expect(
    page.locator('.column[data-state="todo"] .card', { hasText: title }),
  ).toBeVisible({ timeout: 8000 });

  await deleteStory(page, title);
});
