import { test, expect } from "@playwright/test";
import { openFilters, openProject, seedToken } from "./support";

/**
 * Exercises SH-155: the filter bar and sort column carry over across a
 * project switch, within one tab's lifetime, instead of resetting the way
 * `selectRepo()` used to unconditionally reset them.
 *
 * Fixtures, from `scripts/run-e2e.sh`:
 *
 *   - "Alpha Project" (prefix AA) — two stories: "Wire up the auth flow"
 *     and "Fix the flaky upload test". Also carries an extra `review`
 *     state, added to Alpha's vocabulary only and deliberately absent from
 *     Beta and Gamma — see the council's finding (verdict on SH-155)
 *     that a per-repo-vocabulary filter value must be pruned against the
 *     *next* project's own vocabulary, not just carried over raw: without
 *     that, a `states` filter set to `review` would silently hide every
 *     story in whatever project it lands in next, since none of them are
 *     ever in a state that doesn't exist for them.
 *   - "Beta Project" (prefix BB) — one story: "Draft the release notes".
 *
 * `text`/`priorities`/`showClosed`/`sort` are project-agnostic and carry
 * over unvalidated; `assignees`/`types`/`states` are per-repo vocabulary
 * and get pruned once the next project's own vocabulary is known.
 */

test.beforeEach(async ({ page }) => {
  await seedToken(page);
  await page.goto("/");
});

function switchToProject(page: import("@playwright/test").Page, name: string) {
  return page
    .locator("#projsel-btn")
    .click()
    .then(() =>
      page.locator("#projsel-menu .projsel-item", { hasText: name }).click(),
    );
}

test("a text search carries over across a project switch", async ({
  page,
}) => {
  await openProject(page, "Alpha Project");
  await page.locator("#search-input").fill("flow");
  await expect(
    page.locator(".card-title", { hasText: "Wire up the auth flow" }),
  ).toBeVisible();
  await expect(
    page.locator(".card-title", { hasText: "Fix the flaky upload test" }),
  ).not.toBeVisible();

  await switchToProject(page, "Beta Project");

  // The search text is a project-agnostic preference, so it survives —
  // and, still applied, filters out Beta's one story ("Draft the release
  // notes" doesn't match "flow"), proving it's not just cosmetically
  // retained in the input but actually still filtering.
  await expect(page.locator("#search-input")).toHaveValue("flow");
  await expect(page.locator("#filter-count")).toHaveText("0 / 1");
  await expect(page.locator("#empty-msg")).toBeVisible();
});

test("Clear filters wipes the carried-over search for the next switch too", async ({
  page,
}) => {
  await openProject(page, "Alpha Project");
  await page.locator("#search-input").fill("flow");

  await switchToProject(page, "Beta Project");
  await expect(page.locator("#search-input")).toHaveValue("flow");

  await page.locator("#filter-clear").click();
  await expect(page.locator("#search-input")).toHaveValue("");

  await switchToProject(page, "Alpha Project");

  // If Clear Filters only reset the live `state.filter` and not the
  // persisted copy, "flow" would reappear here.
  await expect(page.locator("#search-input")).toHaveValue("");
  await expect(
    page.locator(".card-title", { hasText: "Fix the flaky upload test" }),
  ).toBeVisible();
});

test("a state filter absent from the next project is pruned, with a toast, not silently hiding everything", async ({
  page,
}) => {
  await openProject(page, "Alpha Project");
  // SH-235: the State dropdown lives in the filter panel, which now
  // defaults collapsed.
  await openFilters(page);

  await page.locator("#fdd-states .fdd-btn").click();
  await page
    .locator("#fdd-states .fdd-option", { hasText: "review" })
    .locator("input[type=checkbox]")
    .check();
  // Nothing in Alpha is in `review`, so filtering by it empties the board
  // — the starting point that proves the filter is genuinely active
  // before the switch, not merely cosmetically checked.
  await expect(page.locator("#filter-count")).toHaveText("0 / 2");

  await switchToProject(page, "Beta Project");

  await expect(page.locator("#toast-stack")).toContainText(
    "Some filters didn't carry over to this project",
  );
  // Beta has no `review` state at all — if the carried-over value weren't
  // pruned, `state.filter.states` would still hold it, and Beta's one
  // story (which can never be in a state it doesn't have) would stay
  // hidden forever behind a filter with no visible checkbox to explain it.
  await expect(
    page.locator(".card-title", { hasText: "Draft the release notes" }),
  ).toBeVisible();
  await expect(page.locator("#filter-count")).toHaveText("1 / 1");
  // The dropdown itself reflects the prune too: no lingering "(1)" badge.
  await expect(page.locator("#fdd-states .fdd-btn")).toHaveText("State▾");
});

test("sort order carries over across a project switch", async ({ page }) => {
  await openProject(page, "Alpha Project");
  await page.locator('#view-toggle button[data-view="list"]').click();
  await page.locator('thead th[data-col="priority"]').click();
  await expect(page.locator("#sort-priority")).toHaveText("▲");

  await switchToProject(page, "Beta Project");

  await expect(page.locator("#sort-priority")).toHaveText("▲");
});

test("filters survive a page reload on the same project", async ({
  page,
}) => {
  await openProject(page, "Alpha Project");
  await page.locator("#search-input").fill("flow");

  await page.reload();

  // `bootstrap()` re-enters `selectRepo()` for the last-viewed project on
  // every reload — the exact path the council's decision (sessionStorage,
  // not a bare in-memory variable) was chosen to survive.
  await expect(page.locator("#board-view")).toBeVisible();
  await expect(page.locator("#search-input")).toHaveValue("flow");
  await expect(
    page.locator(".card-title", { hasText: "Wire up the auth flow" }),
  ).toBeVisible();
  await expect(
    page.locator(".card-title", { hasText: "Fix the flaky upload test" }),
  ).not.toBeVisible();
});
