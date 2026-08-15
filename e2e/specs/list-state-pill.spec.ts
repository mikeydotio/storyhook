import { test, expect } from "@playwright/test";
import {
  cleanUpCreatedStories,
  deleteStory,
  openProject,
  resolvedTokenColor,
  seedToken,
} from "./support";

/**
 * Exercises SH-277: the list view's `.state-pill` reads
 * `display_state || state`, the same expression every other renderer in
 * the file already uses (the board's column placement, the drag-drop
 * no-op guard, and SH-203's own status light) -- and only that layer,
 * `renderBoard`'s comment on why, applies here too: the list pill must
 * never disagree with the column the same story's board card sits in --
 * and is coloured by the same semantic `stateColor()` the board's column
 * dots and the status light already use, rather than staying plain text.
 *
 * `tests/web_test.rs` pins the source text and the CSS rules exist; this
 * is the layer that proves a browser actually renders the promoted word
 * for an epic `compute_epic_display_state` (SH-165) has promoted, and
 * actually resolves a semantic colour rather than merely referencing one.
 *
 * This spec creates and deletes its own stories rather than touching the
 * "Alpha Project" fixture, whose exact two-story shape other specs
 * (filter-persistence.spec.ts, column-visibility.spec.ts) assert on
 * byte-for-byte per run-e2e.sh's own comment.
 */

cleanUpCreatedStories("Alpha Project");

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

/** Moves `title`'s story to `stateSlug` via the drawer's own State select
 * -- same shape as story-status-light.spec.ts's own helper. */
async function moveToState(
  page: import("@playwright/test").Page,
  title: string,
  stateSlug: string,
) {
  await page.locator(".card", { hasText: title }).click();
  await expect(page.locator("#drawer")).toHaveClass(/open/);
  await page.locator("#drawer-body select").first().selectOption(stateSlug);
  await expect(
    page.locator('.column[data-state="' + stateSlug + '"] .card', {
      hasText: title,
    }),
  ).toBeVisible();
  await page.locator("#drawer-close").click();
  await expect(page.locator("#drawer")).not.toHaveClass(/open/);
}

/** Adds a `parent-of` relation from whichever story's drawer is currently
 * open to `otherId` -- same shape as story-status-light.spec.ts's own
 * `addRelation`, narrowed to the one relation this spec needs. */
async function addChild(
  page: import("@playwright/test").Page,
  childId: string,
) {
  await page
    .locator('input[placeholder="Story ID (e.g. SH-2)"]')
    .fill(childId);
  await page
    .locator("#drawer-body .inline-add select")
    .selectOption("parent-of");
  await page
    .locator("#drawer-body .inline-add button", { hasText: "Add" })
    .click();
  await expect(page.locator(".rel-row", { hasText: childId })).toBeVisible();
}

/** Switches to the list view and returns the `<tr>` for `id`. */
async function listRow(page: import("@playwright/test").Page, id: string) {
  await page.locator('#view-toggle button[data-view="list"]').click();
  await expect(page.locator("#list-view")).toBeVisible();
  return page.locator('tr[data-id="' + id + '"]');
}

test("an epic's list pill shows the state its card actually sits in, not its own literal recorded state", async ({
  page,
}) => {
  const epicTitle = "SH-277 list pill -- epic";
  const childTitle = "SH-277 list pill -- child";
  const epicCard = await createStory(page, epicTitle);
  const childCard = await createStory(page, childTitle);
  const epicId = (await epicCard.getAttribute("data-id"))!;
  const childId = (await childCard.getAttribute("data-id"))!;

  await epicCard.click();
  await expect(page.locator("#drawer")).toHaveClass(/open/);
  await addChild(page, childId);
  await page.locator("#drawer-close").click();
  await expect(page.locator("#drawer")).not.toHaveClass(/open/);

  // Not yet promoted -- the epic's own literal state is still `todo`, and
  // its list pill/dot must read exactly like any other unpromoted todo
  // story: stateColor()'s quiet default, no title, no disagreement to name.
  const epicPillTodo = (await listRow(page, epicId)).locator(".state-pill");
  await expect(epicPillTodo).toHaveText("todo");
  await expect(epicPillTodo).not.toHaveAttribute("title", /.+/);
  await expect(epicPillTodo.locator(".dot")).toHaveCSS(
    "background-color",
    await resolvedTokenColor(page, "--fg-faint"),
  );
  const todoTint = await epicPillTodo.evaluate(
    (el) => getComputedStyle(el).backgroundColor,
  );

  // Promotes the epic's display_state to in-progress (compute_epic_display_state,
  // SH-165) without touching its own literal `state`, which stays `todo`.
  await page.locator('#view-toggle button[data-view="board"]').click();
  await moveToState(page, childTitle, "in-progress");
  await expect(
    page.locator('.column[data-state="in-progress"] .card', {
      hasText: epicTitle,
    }),
  ).toBeVisible();

  const row = await listRow(page, epicId);
  const pill = row.locator(".state-pill");
  await expect(pill).toHaveText("in-progress");
  await expect(pill).toHaveAttribute("title", /recorded state is todo/);
  // `in-progress` carries stateColor()'s "active" anchor -> --accent,
  // proving the semantic mapping (not the positional palette) drives the
  // promoted pill exactly as it drives storyLight() and the column dot.
  await expect(pill.locator(".dot")).toHaveCSS(
    "background-color",
    await resolvedTokenColor(page, "--accent"),
  );
  // The pill's own tint changes with the state it's colouring, proving
  // buildStatePill() actually feeds stateColor() into --state-color rather
  // than leaving today's fixed --bg-sunken in place -- without hardcoding
  // the color-mix() percentage into this assertion.
  const promotedTint = await pill.evaluate(
    (el) => getComputedStyle(el).backgroundColor,
  );
  expect(promotedTint).not.toBe(todoTint);

  // A story with no display_state override shows and sorts by its own
  // literal state, unaffected by the epic's promotion.
  const childRow = await listRow(page, childId);
  const childPill = childRow.locator(".state-pill");
  await expect(childPill).toHaveText("in-progress");
  await expect(childPill).not.toHaveAttribute("title", /recorded state/);
  await expect(childPill.locator(".dot")).toHaveCSS(
    "background-color",
    await resolvedTokenColor(page, "--accent"),
  );

  // Moving the child back to todo un-promotes the epic (no active child
  // left), which returns its own card -- and so deleteStory's own
  // `.column[data-state="todo"]` locator -- to the epic's literal state.
  // `moveToState`, like `deleteStory`, drives the board's own `.card`, so
  // switch back to it first.
  await page.locator('#view-toggle button[data-view="board"]').click();
  await moveToState(page, childTitle, "todo");
  await deleteStory(page, childTitle);
  await deleteStory(page, epicTitle);
});
