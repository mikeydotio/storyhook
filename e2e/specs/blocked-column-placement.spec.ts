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
 * Exercises SH-407's third ask, narrowed by SH-487: a story shows in the
 * Blocked column only when clearing its blocks needs a PERSON, not merely
 * when it has an unmet dependency. An `awaiting` reason or an open
 * `obviated-by` edge always needs a person; a plain `blocked-by` edge onto
 * an ORDINARY open story does not -- that is the natural procession of the
 * backlog, and SH-487 was filed because this project's own live backlog had
 * 16 cards sitting in Blocked for exactly that reason, none of which needed
 * a person. Server-side mechanism: `domain::needs_intervention`, reached by
 * `compute_display_state` (generalized from `compute_epic_display_state`,
 * SH-165) through the same `display_state || state` idiom
 * `story-status-light.spec.ts`/`list-state-pill.spec.ts` already exercise
 * for the epic case -- this file is that idiom's SH-407/SH-487 case.
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

test("a story blocked by an ordinary open story stays in Todo with its badge intact; only a blocker that itself needs a person moves it to the Blocked column", async ({
  page,
  request,
}) => {
  const blockerTitle = "SH-487 blocked column — the blocker";
  const workerTitle = "SH-487 blocked column — the blocked story";
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

  // SH-487: the blocker is ordinary open work -- it will clear itself as
  // the backlog is worked, so the worker stays in Todo. `blocked_ids`
  // stays broad regardless (only board PLACEMENT narrows), so the card
  // still carries its badge naming the real dependency.
  const workerInTodo = page.locator('.column[data-state="todo"] .card', {
    hasText: workerTitle,
  });
  await expect(workerInTodo).toBeVisible();
  await expect(workerInTodo.locator(".flag-blocked .rel-id")).toHaveText(
    blockerId,
  );
  await expect(
    page.locator('.column[data-state="blocked"] .card', { hasText: workerTitle }),
  ).toHaveCount(0);

  // The state filter (`filteredStories`) reads `display_state || st.state`
  // (SH-407) -- filtering by "blocked" must NOT find a card that is not
  // shown there.
  await openFilters(page);
  await page.locator("#fdd-states .fdd-btn").click();
  await page
    .locator("#fdd-states .fdd-option", { hasText: "blocked" })
    .locator("input[type=checkbox]")
    .check();
  await expect(
    page.locator(".card", { hasText: workerTitle }),
  ).toHaveCount(0);
  // Checking a checkbox doesn't close the dropdown panel -- no second
  // `.fdd-btn` click needed before reaching the checkbox again to uncheck.
  await page
    .locator("#fdd-states .fdd-option", { hasText: "blocked" })
    .locator("input[type=checkbox]")
    .uncheck();

  // Now the blocker itself needs a person -- both it and the worker
  // display-promote, the worker transitively through the unchanged edge.
  const slug = await projectSlug(request, "Alpha Project");
  const block = await request.post(
    `/api/repos/${slug}/story/${blockerId}/block`,
    {
      headers: {
        "X-Storyhook": "1",
        "X-Storyhook-Token": DASHBOARD_TOKEN,
        "Content-Type": "application/json",
      },
      data: { reason: "waiting on a vendor API key" },
    },
  );
  expect(block.ok()).toBe(true);

  await expect(
    page.locator('.column[data-state="blocked"] .card', { hasText: workerTitle }),
  ).toBeVisible({ timeout: 8000 });
  await expect(
    page.locator('.column[data-state="blocked"] .card', { hasText: blockerTitle }),
  ).toBeVisible();

  await page.locator("#fdd-states .fdd-btn").click();
  await page
    .locator("#fdd-states .fdd-option", { hasText: "blocked" })
    .locator("input[type=checkbox]")
    .check();
  await expect(page.locator(".card", { hasText: workerTitle })).toBeVisible();
  await page
    .locator("#fdd-states .fdd-option", { hasText: "blocked" })
    .locator("input[type=checkbox]")
    .uncheck();

  // Clearing the blocker's own reason clears the whole chain in one write,
  // with no edge ever removed.
  const clear = await request.post(
    `/api/repos/${slug}/story/${blockerId}/unblock`,
    {
      headers: {
        "X-Storyhook": "1",
        "X-Storyhook-Token": DASHBOARD_TOKEN,
        "Content-Type": "application/json",
      },
      data: {},
    },
  );
  expect(clear.ok()).toBe(true);

  await expect(
    page.locator('.column[data-state="todo"] .card', { hasText: workerTitle }),
  ).toBeVisible({ timeout: 8000 });
  await expect(
    page.locator('.column[data-state="todo"] .card', { hasText: blockerTitle }),
  ).toBeVisible();

  await deleteStory(page, workerTitle);
  await deleteStory(page, blockerTitle);
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
  // (`StoryAction::Block`, `src/api/rest.rs`), to isolate this promotion
  // arm on its own subject from the transitive blocked-by case above.
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
