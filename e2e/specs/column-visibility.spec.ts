import { test, expect } from "@playwright/test";
import { openProject, seedToken } from "./support";

/**
 * Exercises SH-162: a "Columns" filter-bar dropdown picks which board
 * columns are shown, and a "Hide empty columns" toggle collapses any column
 * with zero currently-visible cards.
 *
 * Fixtures, from `scripts/run-e2e.sh`:
 *
 *   - "Alpha Project" (prefix AA) — every project's four default states
 *     (todo, in-progress, blocked, done) plus an Alpha-only `review` state.
 *     Two stories, both created fresh and so both in `todo`: "Wire up the
 *     auth flow" and "Fix the flaky upload test". `in-progress`, `blocked`,
 *     `done` and `review` all start empty — Alpha's board always has four
 *     empty columns alongside `todo`'s two cards, with no extra setup
 *     needed for "Hide empty columns" to have something to do.
 *   - "Beta Project" (prefix BB) — the four default states only, no
 *     `review`. One story: "Draft the release notes".
 *
 * `hiddenColumns` is session-scoped and per-repo-vocabulary, pruned the
 * same way `filter.states` is (SH-155); `hideEmptyColumns` carries no
 * vocabulary and is a durable, cross-project preference like
 * `showArchived`. See `state.hiddenColumns`/`state.hideEmptyColumns`'s own
 * comments in `web_dashboard.html`.
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

test("hiding a column via the Columns dropdown removes its cards without changing the filter count", async ({
  page,
}) => {
  await openProject(page, "Alpha Project");
  await expect(page.locator("#filter-count")).toHaveText("2 / 2");

  await page.locator("#fdd-columns .fdd-btn").click();
  await page
    .locator("#fdd-columns .fdd-option", { hasText: "todo" })
    .locator("input[type=checkbox]")
    .uncheck();

  await expect(page.locator('.column[data-state="todo"]')).toHaveCount(0);
  await expect(
    page.locator(".card-title", { hasText: "Wire up the auth flow" }),
  ).not.toBeVisible();
  // The column is hidden, not filtered — the cards behind it still count.
  await expect(page.locator("#filter-count")).toHaveText("2 / 2");
  await expect(page.locator("#fdd-columns .fdd-btn")).toHaveText("Columns (1)▾");

  await page
    .locator("#fdd-columns .fdd-option", { hasText: "todo" })
    .locator("input[type=checkbox]")
    .check();

  await expect(page.locator('.column[data-state="todo"]')).toHaveCount(1);
  await expect(
    page.locator(".card-title", { hasText: "Wire up the auth flow" }),
  ).toBeVisible();
  await expect(page.locator("#fdd-columns .fdd-btn")).toHaveText("Columns▾");
});

test("Hide empty columns collapses columns with no visible cards", async ({
  page,
}) => {
  await openProject(page, "Alpha Project");

  await expect(page.locator('.column[data-state="in-progress"]')).toHaveCount(1);
  await page.locator("#toggle-hide-empty-columns").check();

  await expect(page.locator('.column[data-state="todo"]')).toHaveCount(1);
  await expect(page.locator('.column[data-state="in-progress"]')).toHaveCount(0);
  await expect(page.locator('.column[data-state="blocked"]')).toHaveCount(0);
  await expect(page.locator('.column[data-state="done"]')).toHaveCount(0);
  await expect(page.locator('.column[data-state="review"]')).toHaveCount(0);

  // A search that also empties `todo` collapses it live, on the next
  // render — this isn't a one-time snapshot taken when the toggle flipped.
  await page.locator("#search-input").fill("nonexistent story title");
  await expect(page.locator('.column[data-state="todo"]')).toHaveCount(0);
  await page.locator("#search-input").fill("");

  await page.locator("#toggle-hide-empty-columns").uncheck();
  await expect(page.locator('.column[data-state="in-progress"]')).toHaveCount(1);
});

test("a hidden column persists across a reload on the same project", async ({
  page,
}) => {
  await openProject(page, "Alpha Project");
  await page.locator("#fdd-columns .fdd-btn").click();
  await page
    .locator("#fdd-columns .fdd-option", { hasText: "todo" })
    .locator("input[type=checkbox]")
    .uncheck();
  await expect(page.locator('.column[data-state="todo"]')).toHaveCount(0);

  await page.reload();

  await expect(page.locator("#board-view")).toBeVisible();
  await expect(page.locator('.column[data-state="todo"]')).toHaveCount(0);
  await expect(page.locator("#fdd-columns .fdd-btn")).toHaveText("Columns (1)▾");
});

test("a hidden column absent from the next project is pruned, with a toast", async ({
  page,
}) => {
  await openProject(page, "Alpha Project");
  await page.locator("#fdd-columns .fdd-btn").click();
  await page
    .locator("#fdd-columns .fdd-option", { hasText: "review" })
    .locator("input[type=checkbox]")
    .uncheck();
  await expect(page.locator("#fdd-columns .fdd-btn")).toHaveText("Columns (1)▾");

  await switchToProject(page, "Beta Project");

  await expect(page.locator("#toast-stack")).toContainText(
    "Some filters didn't carry over to this project",
  );
  // Beta has no `review` state at all, so a carried-over hidden slug that
  // never gets pruned would just be harmless dead weight here — but the
  // badge count proving it's actually gone is what shows the prune ran
  // rather than merely being masked by Beta's own vocabulary.
  await expect(page.locator("#fdd-columns .fdd-btn")).toHaveText("Columns▾");
});

test("Hide empty columns is a durable preference that carries across a project switch", async ({
  page,
}) => {
  await openProject(page, "Alpha Project");
  await page.locator("#toggle-hide-empty-columns").check();

  await switchToProject(page, "Beta Project");

  await expect(page.locator("#toggle-hide-empty-columns")).toBeChecked();
  // Beta's one story is in `todo`; its other three default states are
  // empty and should already be collapsed on arrival, not just after a
  // manual re-toggle.
  await expect(page.locator('.column[data-state="in-progress"]')).toHaveCount(0);
});
