import { test, expect } from "@playwright/test";
import {
  cleanUpCreatedStories,
  deleteStory,
  openProject,
  projectSlug,
  requiredEnv,
  seedToken,
  waitUntilStoreClockPasses,
} from "./support";

/**
 * Exercises SH-305: every board column gets its own sort, chosen from a
 * popup menu opened by a `.column-sort-btn` glyph at the column header's
 * top-right corner -- replacing SH-128's single pair of board-wide buttons
 * in the filter panel (`.council/sh128-board-sort-options/` explicitly
 * chose that placement, and this story reverses it on new information: a
 * `todo` column and a `done` column can legitimately want different
 * orders, which SH-128's board-wide state could never express).
 *
 * The option set changed too. SH-128's "story order" (`domain::
 * story_number`, a numeric-aware comparison of the id's trailing digits)
 * is gone as a user-facing choice -- it still runs as the tiebreak inside
 * `columnCardCompare` (see that function's own comment), but the menu no
 * longer offers it directly. In its place: "Added" (`created_at`) and
 * "Modified" (`updated_at`), the two SH-128's own council explicitly
 * rejected as a sort basis (fixture evidence at the time: SH-1 created
 * after SH-2 but numbered lower, so id order and creation order disagree).
 * Priority is unchanged -- descending (most urgent first) is still the
 * default for a column with no sort of its own.
 *
 * This spec creates and deletes its own stories rather than touching the
 * "Alpha Project" fixture, whose exact two-story shape other specs
 * (filter-persistence.spec.ts, column-visibility.spec.ts) assert on
 * byte-for-byte per run-e2e.sh's own comment. One test (the one proving
 * per-column isolation) moves a story into "in-progress" and leaves it
 * there rather than moving it back to "todo" before the spec ends --
 * `deleteStory()` is scoped to the todo column, but `cleanUpCreatedStories`'s
 * own `afterEach` deletes every story missing from the baseline through a
 * direct API call keyed by id, not by column, so it cleans up a story
 * wherever it ended up.
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

/** Moves `title`'s story to `stateSlug` via the drawer's own State select
 * (the same helper pattern `story-context-menu-status.spec.ts` and
 * `story-status-light.spec.ts` each keep a local copy of) -- used here to
 * populate a second column for the per-column isolation test. */
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

/** Changes `title`'s priority via the drawer's own Priority select -- used
 * only to bump the story's `updated_at` for the "Modified" sort test,
 * where the actual value chosen doesn't matter. */
async function touchPriority(
  page: import("@playwright/test").Page,
  title: string,
  priority: string,
) {
  await page.locator(".card", { hasText: title }).click();
  await expect(page.locator("#drawer")).toHaveClass(/open/);
  // Scoped to #drawer-body: an unscoped `.field` also matches the (hidden)
  // create-story modal's own Priority field, which is what SH-305's first
  // draft of this helper hit -- a strict-mode violation, since both are in
  // the DOM regardless of which is visible.
  await page
    .locator("#drawer-body .field", { hasText: "Priority" })
    .locator("select")
    .selectOption(priority);
  await page.locator("#drawer-close").click();
  await expect(page.locator("#drawer")).not.toHaveClass(/open/);
}

function columnSortBtn(page: import("@playwright/test").Page, slug: string) {
  return page.locator(`.column[data-state="${slug}"] .column-sort-btn`);
}

function columnSortMenu(page: import("@playwright/test").Page, slug: string) {
  return page.locator(`.ctxmenu[aria-label="Sort ${slug}"]`);
}

/** Opens `slug`'s sort menu and picks `label` (one of the six
 * `role="menuitemradio"` options), waiting for the menu to close. */
async function selectColumnSort(
  page: import("@playwright/test").Page,
  slug: string,
  label: string,
) {
  await columnSortBtn(page, slug).click();
  const menu = columnSortMenu(page, slug);
  await expect(menu).toBeVisible();
  await menu.locator(".ctxmenu-item", { hasText: label }).click();
  await expect(menu).not.toBeVisible();
}

/** Reads `title`'s story's `updated_at` from the store, through the same
 * `/data` endpoint the board itself renders from -- so an assertion made on
 * it is an assertion about what the store actually holds, not about what the
 * page happens to be showing. Used by the "Modified" test to prove its own
 * precondition (SH-329) rather than assume it. */
async function updatedAtOf(
  page: import("@playwright/test").Page,
  title: string,
): Promise<string> {
  const slug = await projectSlug(page.request, "Alpha Project");
  const resp = await page.request.get(
    `/api/repos/${encodeURIComponent(slug)}/data`,
    { headers: { "X-Storyhook-Token": requiredEnv("DASHBOARD_TOKEN") } },
  );
  if (!resp.ok()) {
    throw new Error(
      `updatedAtOf: GET /data answered ${resp.status()}: ${await resp.text()}`,
    );
  }
  const data: { stories?: Array<{ story: { title: string; updated_at: string } }> } =
    await resp.json();
  const match = (data.stories ?? []).find((v) => v.story.title === title);
  if (!match) {
    throw new Error(`updatedAtOf: no story titled "${title}" in GET /data`);
  }
  return match.story.updated_at;
}

/** This spec's own cards' titles within `slug`'s column, in DOM order --
 * filtered to this file's own prefix rather than asserted unfiltered,
 * since "todo" always carries the Alpha Project fixture's own two stories
 * alongside whatever a test creates. */
async function ourColumnTitles(
  page: import("@playwright/test").Page,
  slug: string,
) {
  const titles = page.locator(`.column[data-state="${slug}"] .card .card-title`);
  return (await titles.allTextContents()).filter((t) =>
    t.startsWith("SH-305 sort test"),
  );
}

test("a column's default sort is priority descending, most urgent first", async ({
  page,
}) => {
  const low = "SH-305 sort test — default, low priority card";
  const critical = "SH-305 sort test — default, critical priority card";
  const high = "SH-305 sort test — default, high priority card";

  // Deliberately created out of priority order, so a passing assertion
  // proves the sort actually reordered them rather than happening to match
  // insertion order.
  await createStory(page, low, "low");
  await createStory(page, critical, "critical");
  await createStory(page, high, "high");

  await expect(columnSortBtn(page, "todo")).toHaveAttribute(
    "title",
    "Sort: Priority ↓",
  );
  expect(await ourColumnTitles(page, "todo")).toEqual([critical, high, low]);

  await deleteStory(page, low);
  await deleteStory(page, critical);
  await deleteStory(page, high);
});

test("within a tied priority, story number ascending breaks the tie", async ({
  page,
}) => {
  const first = "SH-305 sort test — tie A (created first)";
  const second = "SH-305 sort test — tie B (created second)";

  // Both `medium`, so a passing assertion proves the tiebreak is story
  // number (creation sequence, mirroring `domain::story_number`), not
  // insertion order in the DOM or a stable-sort artifact of some other
  // field. Story order is no longer a menu option of its own (SH-305
  // dropped it), but it still runs as the tiebreak here.
  await createStory(page, first, "medium");
  await createStory(page, second, "medium");

  expect(await ourColumnTitles(page, "todo")).toEqual([first, second]);

  await deleteStory(page, first);
  await deleteStory(page, second);
});

test("the sort glyph opens a menu of exactly six options, with the active one checked", async ({
  page,
}) => {
  await columnSortBtn(page, "todo").click();
  const menu = columnSortMenu(page, "todo");
  await expect(menu).toBeVisible();

  const items = menu.locator('[role="menuitemradio"]');
  await expect(items).toHaveCount(6);
  for (const label of [
    "Added ↑",
    "Added ↓",
    "Modified ↑",
    "Modified ↓",
    "Priority ↑",
    "Priority ↓",
  ]) {
    await expect(menu.locator(".ctxmenu-item", { hasText: label })).toBeVisible();
  }

  // Priority descending is the default for a column with no sort of its
  // own -- it, and only it, should read as checked.
  await expect(
    menu.locator(".ctxmenu-item", { hasText: "Priority ↓" }),
  ).toHaveAttribute("aria-checked", "true");
  await expect(
    menu.locator(".ctxmenu-item", { hasText: "Priority ↑" }),
  ).toHaveAttribute("aria-checked", "false");
});

test('choosing "Added" reorders a column by creation time, not priority', async ({
  page,
}) => {
  const low = "SH-305 sort test — added, low priority, created first";
  const critical = "SH-305 sort test — added, critical priority, created second";

  // Created in an order that DISAGREES with priority order, so the default
  // render shows [critical, low] -- a passing assertion after switching to
  // "Added ↑" proves the reorder actually happened rather than the two
  // orders coincidentally matching.
  await createStory(page, low, "low");
  await createStory(page, critical, "critical");
  expect(await ourColumnTitles(page, "todo")).toEqual([critical, low]);

  await selectColumnSort(page, "todo", "Added ↑");
  await expect(columnSortBtn(page, "todo")).toHaveAttribute(
    "title",
    "Sort: Added ↑",
  );
  expect(await ourColumnTitles(page, "todo")).toEqual([low, critical]);

  await deleteStory(page, low);
  await deleteStory(page, critical);
});

test('choosing "Modified" reorders a column by last-touched time, not creation time', async ({
  page,
}) => {
  const untouched = "SH-305 sort test — modified, created first, never touched";
  const touched = "SH-305 sort test — modified, created second, touched after";

  await createStory(page, untouched, "medium");
  await createStory(page, touched, "medium");
  // The store stamps at one-second precision, and all three of this test's
  // writes fit comfortably inside one second on a warm machine -- which
  // makes "touched after" unrepresentable and hands the sort a tie it
  // breaks on story number, i.e. creation order (SH-329). Waiting the
  // second out is what makes the bump below a bump at all.
  await waitUntilStoreClockPasses(await updatedAtOf(page, untouched));
  // Bumps `touched`'s updated_at past both stories' created_at -- by
  // creation order alone the pair would read [untouched, touched]; a
  // passing assertion for "Modified ↓" (most recent first) below proves
  // the sort is reading `updated_at`, not `created_at`.
  await touchPriority(page, touched, "high");

  // Asserted from the store's own answer, before the board is asked
  // anything: if this ever stops holding -- a coarser clock, a priority
  // change that no longer counts as a modification -- the failure names
  // the two timestamps and the reason, instead of surfacing as an
  // unexplained ordering mismatch below.
  const untouchedAt = await updatedAtOf(page, untouched);
  const touchedAt = await updatedAtOf(page, touched);
  expect(
    touchedAt > untouchedAt,
    `the touched story's updated_at (${touchedAt}) is not later than the ` +
      `untouched story's (${untouchedAt}), so "Modified" cannot tell them ` +
      "apart and the assertion below would be testing nothing",
  ).toBe(true);

  await selectColumnSort(page, "todo", "Modified ↓");
  await expect(columnSortBtn(page, "todo")).toHaveAttribute(
    "title",
    "Sort: Modified ↓",
  );
  expect(await ourColumnTitles(page, "todo")).toEqual([touched, untouched]);

  await deleteStory(page, untouched);
  await deleteStory(page, touched);
});

test("changing one column's sort leaves every other column's sort and order untouched", async ({
  page,
}) => {
  const inTodo = "SH-305 sort test — isolation, stays in todo";
  const critical = "SH-305 sort test — isolation, moved, critical priority";
  const medium = "SH-305 sort test — isolation, moved, medium priority";

  await createStory(page, inTodo, "medium");
  await createStory(page, medium, "medium");
  await createStory(page, critical, "critical");
  await moveToState(page, medium, "in-progress");
  await moveToState(page, critical, "in-progress");

  // in-progress is still on its own default (priority descending) -- SH-128
  // built a global sort with no per-column identity; this is the assertion
  // that design could not pass.
  expect(await ourColumnTitles(page, "in-progress")).toEqual([critical, medium]);
  await expect(columnSortBtn(page, "in-progress")).toHaveAttribute(
    "title",
    "Sort: Priority ↓",
  );

  await selectColumnSort(page, "todo", "Added ↑");

  await expect(columnSortBtn(page, "todo")).toHaveAttribute(
    "title",
    "Sort: Added ↑",
  );
  await expect(columnSortBtn(page, "in-progress")).toHaveAttribute(
    "title",
    "Sort: Priority ↓",
  );
  expect(await ourColumnTitles(page, "in-progress")).toEqual([critical, medium]);

  await deleteStory(page, inTodo);
  // `medium` and `critical` are left in "in-progress" -- deleteStory() is
  // scoped to the todo column, but cleanUpCreatedStories's afterEach deletes
  // any story missing from the baseline by id, regardless of which column
  // it's in, so no explicit move-back is needed.
});

test("outside click and Escape both dismiss the sort menu", async ({
  page,
}) => {
  const menu = columnSortMenu(page, "todo");

  await columnSortBtn(page, "todo").click();
  await expect(menu).toBeVisible();
  await page.mouse.click(10, 10);
  await expect(menu).toHaveCount(0);

  await columnSortBtn(page, "todo").click();
  await expect(menu).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(menu).toHaveCount(0);
  // Escape restores focus to the button that opened the menu, mirroring
  // the story context menu's own Escape handling (story-context-menu.spec.ts).
  await expect(columnSortBtn(page, "todo")).toBeFocused();
});

test("a column's chosen sort persists across a reload on the same project", async ({
  page,
}) => {
  await selectColumnSort(page, "todo", "Added ↑");
  await expect(columnSortBtn(page, "todo")).toHaveAttribute(
    "title",
    "Sort: Added ↑",
  );

  await page.reload();

  await expect(page.locator("#board-view")).toBeVisible();
  await expect(columnSortBtn(page, "todo")).toHaveAttribute(
    "title",
    "Sort: Added ↑",
  );
});
