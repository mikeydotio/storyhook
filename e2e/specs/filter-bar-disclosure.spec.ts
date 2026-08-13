import { test, expect } from "@playwright/test";
import { openFilters, openProject, seedToken } from "./support";

/**
 * SH-235: the filter bar's dropdowns, checkboxes and sort buttons collapse
 * behind a "Filters" disclosure, at every viewport size -- not gated by
 * `max-width` or `pointer: coarse`, since a narrow desktop window has the
 * exact same wrapping problem a phone does (measured: 145px tall at a
 * 390px width, on top of the topbar's own 108px). This file runs under the
 * default desktop `chromium` project for that reason; the coarse-pointer
 * variant of "does everything still fit" belongs to
 * `responsive.mobile.spec.ts` instead.
 *
 * `#filter-count` and `#filter-clear` stay in the always-visible
 * `.filter-summary` row -- a reader should never have to open the panel to
 * see whether a filter is active or to clear one.
 *
 * Fixtures, from `scripts/run-e2e.sh`: "Alpha Project" (prefix AA) has two
 * stories, both in `todo` -- "Wire up the auth flow" and "Fix the flaky
 * upload test" -- the same fixture `filter-persistence.spec.ts` and
 * `column-visibility.spec.ts` assert on byte-for-byte, so this file reads
 * it but creates nothing of its own.
 */

test.beforeEach(async ({ page }) => {
  await seedToken(page);
  await page.goto("/");
  await openProject(page, "Alpha Project");
});

test("the panel defaults collapsed, with the toggle's ARIA and chevron matching", async ({
  page,
}) => {
  await expect(page.locator("#filter-panel")).toBeHidden();
  await expect(page.locator("#filter-toggle-btn")).toHaveAttribute(
    "aria-expanded",
    "false",
  );
  await expect(page.locator("#filter-toggle-chevron")).toHaveText("▸");
  // Always visible regardless -- the point of the redesign.
  await expect(page.locator("#filter-count")).toBeVisible();
  await expect(page.locator("#filter-clear")).toBeVisible();
});

test("clicking the toggle opens the panel and its dropdowns become usable", async ({
  page,
}) => {
  await page.locator("#filter-toggle-btn").click();

  await expect(page.locator("#filter-panel")).toBeVisible();
  await expect(page.locator("#filter-toggle-btn")).toHaveAttribute(
    "aria-expanded",
    "true",
  );
  await expect(page.locator("#filter-toggle-chevron")).toHaveText("▾");

  await page.locator("#fdd-priorities .fdd-btn").click();
  await expect(page.locator("#fdd-priorities .fdd-panel")).toBeVisible();

  await page.locator("#filter-toggle-btn").click();
  await expect(page.locator("#filter-panel")).toBeHidden();
  await expect(page.locator("#filter-toggle-btn")).toHaveAttribute(
    "aria-expanded",
    "false",
  );
});

test("Clear filters works without opening the panel first", async ({
  page,
}) => {
  await page.locator("#search-input").fill("flow");
  await expect(page.locator("#filter-count")).toHaveText("1 / 2");
  await expect(page.locator("#filter-panel")).toBeHidden();

  await page.locator("#filter-clear").click();

  await expect(page.locator("#search-input")).toHaveValue("");
  await expect(page.locator("#filter-count")).toHaveText("2 / 2");
});

test("the toggle reads as active while a filter narrows the board, even collapsed", async ({
  page,
}) => {
  await expect(page.locator("#filter-toggle-btn")).not.toHaveClass(/active/);

  await page.locator("#search-input").fill("flow");
  await expect(page.locator("#filter-count")).toHaveText("1 / 2");
  await expect(page.locator("#filter-panel")).toBeHidden();
  await expect(page.locator("#filter-toggle-btn")).toHaveClass(/active/);

  await page.locator("#search-input").fill("");
  await expect(page.locator("#filter-count")).toHaveText("2 / 2");
  await expect(page.locator("#filter-toggle-btn")).not.toHaveClass(/active/);
});

test("the panel's open state is a durable preference: it survives a reload and a project switch", async ({
  page,
}) => {
  await openFilters(page);

  await page.reload();
  await expect(page.locator("#board-view")).toBeVisible();
  await expect(page.locator("#filter-panel")).toBeVisible();
  await expect(page.locator("#filter-toggle-btn")).toHaveAttribute(
    "aria-expanded",
    "true",
  );

  await page.locator("#projsel-btn").click();
  await page
    .locator("#projsel-menu .projsel-item", { hasText: "Beta Project" })
    .click();
  await expect(page.locator("#filter-panel")).toBeVisible();
});

/**
 * Regression test for the `closeAllPopovers` scoping fix this story also
 * made: that function resets every `[aria-expanded="true"]` element on any
 * outside click (it's what dismisses the priority/assignee/etc. dropdowns
 * and the project selector), and used to do so unconditionally. Without
 * excluding `.filter-toggle-btn`, opening the panel and then clicking a
 * board card (which doesn't itself call renderView/syncFilterToggle) would
 * silently flip the toggle's aria-expanded back to "false" while the panel
 * stayed visibly open -- a real mismatch for assistive tech, not merely a
 * cosmetic one.
 */
test("opening a card's drawer does not desync the filter toggle's aria-expanded from the still-open panel", async ({
  page,
}) => {
  await openFilters(page);

  await page
    .locator(".card-title", { hasText: "Wire up the auth flow" })
    .click();
  await expect(page.locator("#drawer")).toHaveClass(/open/);

  await expect(page.locator("#filter-panel")).toBeVisible();
  await expect(page.locator("#filter-toggle-btn")).toHaveAttribute(
    "aria-expanded",
    "true",
  );
});
