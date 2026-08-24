import { test, expect } from "./support";
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
 * popup menu opened by a `.column-sort-btn` icon at the column header's
 * top-right corner -- replacing SH-128's single pair of board-wide buttons
 * in the filter panel (SH-128's council explicitly
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

/** Reads `title`'s story's `closed_at` from the store, the same way
 * `updatedAtOf` above reads `updated_at` -- used by the "Completed" tests
 * (SH-407) to prove their own precondition rather than assume it. */
async function closedAtOf(
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
      `closedAtOf: GET /data answered ${resp.status()}: ${await resp.text()}`,
    );
  }
  const data: {
    stories?: Array<{ story: { title: string; closed_at?: string | null } }>;
  } = await resp.json();
  const match = (data.stories ?? []).find((v) => v.story.title === title);
  if (!match || !match.story.closed_at) {
    throw new Error(`closedAtOf: no closed_at for a story titled "${title}" in GET /data`);
  }
  return match.story.closed_at;
}

/** Adds a `parent-of` relation from whichever story's drawer is currently
 * open to `childId` -- same shape as `list-state-pill.spec.ts`'s own
 * `addChild`. Used by the "Next" test (SH-407) to make one card a real
 * epic, since only `has_children` is excluded from the ready queue. */
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

test("an OPEN column's sort glyph opens a menu of exactly eight options, with the active one checked", async ({
  page,
}) => {
  await columnSortBtn(page, "todo").click();
  const menu = columnSortMenu(page, "todo");
  await expect(menu).toBeVisible();

  const items = menu.locator('[role="menuitemradio"]');
  // SH-407 added "Next ↑/↓" to the six SH-305 already offered -- and offers
  // it only on an OPEN column, since a CLOSED column's contents will never
  // be handed out by `story next` again.
  await expect(items).toHaveCount(8);
  for (const label of [
    "Added ↑",
    "Added ↓",
    "Modified ↑",
    "Modified ↓",
    "Priority ↑",
    "Priority ↓",
    "Next ↑",
    "Next ↓",
  ]) {
    await expect(menu.locator(".ctxmenu-item", { hasText: label })).toBeVisible();
  }
  await expect(
    menu.locator(".ctxmenu-item", { hasText: "Completed" }),
  ).toHaveCount(0);

  // Priority descending is the default for an OPEN column with no sort of
  // its own -- it, and only it, should read as checked.
  await expect(
    menu.locator(".ctxmenu-item", { hasText: "Priority ↓" }),
  ).toHaveAttribute("aria-checked", "true");
  await expect(
    menu.locator(".ctxmenu-item", { hasText: "Priority ↑" }),
  ).toHaveAttribute("aria-checked", "false");
});

test("a CLOSED column's sort glyph offers \"Completed\" instead of \"Next\", defaulting to most-recently-finished-first", async ({
  page,
}) => {
  await columnSortBtn(page, "done").click();
  const menu = columnSortMenu(page, "done");
  await expect(menu).toBeVisible();

  const items = menu.locator('[role="menuitemradio"]');
  await expect(items).toHaveCount(8);
  for (const label of [
    "Added ↑",
    "Added ↓",
    "Modified ↑",
    "Modified ↓",
    "Priority ↑",
    "Priority ↓",
    "Completed ↑",
    "Completed ↓",
  ]) {
    await expect(menu.locator(".ctxmenu-item", { hasText: label })).toBeVisible();
  }
  await expect(menu.locator(".ctxmenu-item", { hasText: "Next" })).toHaveCount(0);

  // Completion order, most recent first, is the default for a CLOSED
  // column with no sort of its own -- priority reads as near-arbitrary for
  // work that is already finished.
  await expect(
    menu.locator(".ctxmenu-item", { hasText: "Completed ↓" }),
  ).toHaveAttribute("aria-checked", "true");
  await expect(columnSortBtn(page, "done")).toHaveAttribute(
    "title",
    "Sort: Completed ↓",
  );
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
  // SH-336 gave "Modified" an exact same-second tiebreak (`head_global_seq`,
  // the write's position in the store's change feed), which resolved SH-329's
  // original flake -- but it did *not* make this wait removable, and for a
  // sharper reason than the flake: the tiebreak fires off `sort.key ===
  // "modified"`, not off which timestamp field actually tied. If this test's
  // three writes landed in one second, `created_at` would tie too, and a
  // mutant that read `created_at` instead of `updated_at` for "Modified"
  // would still pass -- the tiebreak alone would produce the right answer for
  // the wrong reason. The wait keeps `updated_at` and `created_at` distinct,
  // which is the only condition under which the assertion below can tell the
  // two fields apart. `board-sort-tiebreak.spec.ts` covers the tiebreak
  // itself, deliberately forcing the tie this wait exists to avoid here.
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

test('choosing "Next" ranks a column by the order story next would hand it out, sorting a parent last regardless of its own priority', async ({
  page,
}) => {
  const epic = "SH-305 sort test — next, epic (excluded, has a child)";
  const child = "SH-305 sort test — next, epic's own child";
  const leaf = "SH-305 sort test — next, plain leaf, same priority as the epic";

  // Both critical, and the epic created first (lower story number) -- under
  // "Priority ↓" this pair would read [epic, leaf] (story-number tiebreak).
  // A passing assertion below, after switching to "Next", proves the epic
  // was excluded from the queue entirely rather than merely reordered.
  await createStory(page, epic, "critical");
  await createStory(page, leaf, "critical");
  await createStory(page, child, "low");

  const epicCard = page.locator(".card", { hasText: epic });
  const childId = (await page
    .locator(".card", { hasText: child })
    .getAttribute("data-id"))!;

  await epicCard.click();
  await expect(page.locator("#drawer")).toHaveClass(/open/);
  await addChild(page, childId);
  await page.locator("#drawer-close").click();
  await expect(page.locator("#drawer")).not.toHaveClass(/open/);

  // `next_ids` is a project-wide aggregate the server recomputes on its own
  // `/data` fetch (same category as `ready_ids`/`blocked_ids`, per
  // `blockedFlag()`'s own comment on that distinction) -- unlike a
  // mutation's own response, which patches only the one story it touched.
  // A reload forces a fresh `/data` fetch, so the epic's new `parent-of`
  // relation is guaranteed to have reached the aggregate before the
  // assertion below reads it, rather than racing a live-update push.
  await page.reload();
  await expect(page.locator("#board-view")).toBeVisible();
  await expect(page.locator(".card", { hasText: leaf })).toBeVisible();

  await selectColumnSort(page, "todo", "Next ↓");
  await expect(columnSortBtn(page, "todo")).toHaveAttribute(
    "title",
    "Sort: Next ↓",
  );

  const titles = await ourColumnTitles(page, "todo");
  expect(
    titles.indexOf(epic),
    "the epic (has_children) is never offered by story next, so it must sort last regardless of its own critical priority",
  ).toBe(titles.length - 1);
  expect(titles.indexOf(leaf)).toBeLessThan(titles.indexOf(epic));
  expect(titles.indexOf(child)).toBeLessThan(titles.indexOf(epic));

  // Direction-independence: the epic stays last under "Next ↑" too --
  // an unranked card has no position on this axis to reverse.
  await selectColumnSort(page, "todo", "Next ↑");
  const titlesAsc = await ourColumnTitles(page, "todo");
  expect(titlesAsc.indexOf(epic)).toBe(titlesAsc.length - 1);

  await deleteStory(page, epic);
  await deleteStory(page, leaf);
  await deleteStory(page, child);
});

test('the Done column defaults to completion order, most recently finished first, and "Completed ↑" reverses it', async ({
  page,
}) => {
  const first = "SH-305 sort test — completed, finished first";
  const second = "SH-305 sort test — completed, finished second";

  await createStory(page, first, "medium");
  await createStory(page, second, "medium");
  await moveToState(page, first, "done");
  // Same SH-336 reasoning `board-sort.spec.ts`'s own "Modified" test
  // documents above: the store's clock is one-second resolution, so this
  // wait is what makes the two closed_at values provably distinct rather
  // than coincidentally so.
  await waitUntilStoreClockPasses(await closedAtOf(page, first));
  await moveToState(page, second, "done");

  const firstAt = await closedAtOf(page, first);
  const secondAt = await closedAtOf(page, second);
  expect(
    secondAt > firstAt,
    `the second story's closed_at (${secondAt}) is not later than the ` +
      `first's (${firstAt}), so "Completed" cannot tell them apart and ` +
      "the assertion below would be testing nothing",
  ).toBe(true);

  // No selectColumnSort() call -- this is the CLOSED column's own default.
  await expect(columnSortBtn(page, "done")).toHaveAttribute(
    "title",
    "Sort: Completed ↓",
  );
  await expect
    .poll(() => ourColumnTitles(page, "done"), {
      message: "the Done column settles into its default completion order",
    })
    .toEqual([second, first]);

  await selectColumnSort(page, "done", "Completed ↑");
  // The menu closes as soon as the preference is accepted, while the board's
  // live-data render may still be replacing the just-sorted cards. Assert the
  // eventual order instead of sampling that hand-off at one instant.
  await expect
    .poll(() => ourColumnTitles(page, "done"), {
      message: "the Done column settles into ascending completion order",
    })
    .toEqual([first, second]);

  // `deleteStory()` is scoped to the todo column (see this file's own
  // header comment on the per-column isolation test) -- both stories are
  // left in "done" for `cleanUpCreatedStories`'s afterEach to reopen and
  // delete by id, the same way that test leaves its moved stories behind.
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
