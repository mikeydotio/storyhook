import { test, expect } from "@playwright/test";
import { seedToken } from "./support";

/**
 * Exercises the dashboard's header project selector against a real daemon
 * seeded by `scripts/run-e2e.sh` with three projects:
 *
 *   - "Alpha Project" (prefix AA) — has a checkout, two open stories
 *   - "Beta Project" (prefix BB) — has a checkout, one open story
 *   - "Gamma Archive" (prefix GA) — created with `--no-attach`: no checkout
 *     on this machine, so the dashboard serves it read-only
 *
 * Names, prefixes and story counts are fixed by the seed script; a spec that
 * changes what it expects here must change the seed to match, and vice
 * versa — there is one source of truth for "what the harness seeded", split
 * only because bash creates it and TypeScript reads it.
 *
 * The control under test is a `role="menu"` popover behind a
 * `#projsel-btn` button — SH-42's replacement for the native
 * `<select id="repo-select">` this file drove before. It is hand-built to
 * the WAI-ARIA menu-button pattern, so this file is the only thing that
 * proves that pattern actually works: there is no other keyboard or
 * screen-reader test anywhere in the repo.
 */

test.beforeEach(async ({ page }) => {
  await seedToken(page);
  await page.goto("/");
});

function openMenu(page: import("@playwright/test").Page) {
  return page.locator("#projsel-btn").click();
}

test("first load lands on Home with a card per seeded project", async ({
  page,
}) => {
  await expect(page.locator("#home-view")).toBeVisible();
  await expect(
    page.locator(".repo-card-name", { hasText: "Alpha Project" }),
  ).toBeVisible();
  await expect(
    page.locator(".repo-card-name", { hasText: "Beta Project" }),
  ).toBeVisible();
  await expect(
    page.locator(".repo-card-name", { hasText: "Gamma Archive" }),
  ).toBeVisible();
  // No project is open on Home, so the selector must not claim one.
  await expect(page.locator("#projsel-btn")).toContainText("All projects");
});

test("clicking a project's card opens its board and updates the selector label", async ({
  page,
}) => {
  await page.locator(".repo-card-name", { hasText: "Alpha Project" }).click();
  await expect(page.locator("#board-view")).toBeVisible();
  await expect(
    page.locator(".card-title", { hasText: "Wire up the auth flow" }),
  ).toBeVisible();
  await expect(page.locator("#projsel-btn")).toContainText("AA · Alpha Project");
});

test("choosing another project from the selector switches the board", async ({
  page,
}) => {
  await page.locator(".repo-card-name", { hasText: "Alpha Project" }).click();
  await expect(
    page.locator(".card-title", { hasText: "Wire up the auth flow" }),
  ).toBeVisible();

  await openMenu(page);
  await page
    .locator("#projsel-menu .projsel-item", { hasText: "Beta Project" })
    .click();

  await expect(page.locator("#projsel-btn")).toContainText("BB · Beta Project");
  await expect(
    page.locator(".card-title", { hasText: "Draft the release notes" }),
  ).toBeVisible();
  await expect(
    page.locator(".card-title", { hasText: "Wire up the auth flow" }),
  ).not.toBeVisible();
});

test("the menu lists every project and marks the open one as checked", async ({
  page,
}) => {
  await page.locator(".repo-card-name", { hasText: "Alpha Project" }).click();
  await openMenu(page);

  const menu = page.locator("#projsel-menu");
  const alpha = menu.locator(".projsel-item", { hasText: "Alpha Project" });
  const beta = menu.locator(".projsel-item", { hasText: "Beta Project" });
  await expect(alpha).toHaveAttribute("aria-checked", "true");
  await expect(beta).toHaveAttribute("aria-checked", "false");
  await expect(menu.locator(".projsel-item", { hasText: "Gamma Archive" })).toBeVisible();
});

test("a project with no checkout on this machine is reachable from its home card", async ({
  page,
}) => {
  await page
    .locator(".repo-card-name", { hasText: "Gamma Archive" })
    .click();
  await expect(page.locator("#board-view")).toBeVisible();
  await expect(page.locator("#projsel-btn")).toContainText("GA · Gamma Archive");
});

test("the selector is fully operable by keyboard, with no mouse involved", async ({
  page,
}) => {
  await page.locator(".repo-card-name", { hasText: "Alpha Project" }).click();

  await page.locator("#projsel-btn").focus();
  await page.keyboard.press("Enter");
  await expect(page.locator("#projsel-menu")).toBeVisible();
  // Opening focuses the current project (Alpha, first in the seeded list),
  // so one ArrowDown reaches Beta.
  await page.keyboard.press("ArrowDown");
  await page.keyboard.press("Enter");

  await expect(page.locator("#projsel-btn")).toContainText("BB · Beta Project");
  await expect(
    page.locator(".card-title", { hasText: "Draft the release notes" }),
  ).toBeVisible();
});

test("Escape closes the menu and returns focus to the button", async ({
  page,
}) => {
  await page.locator(".repo-card-name", { hasText: "Alpha Project" }).click();
  await openMenu(page);
  await expect(page.locator("#projsel-menu")).toBeVisible();

  await page.keyboard.press("Escape");

  await expect(page.locator("#projsel-menu")).toBeHidden();
  await expect(page.locator("#projsel-btn")).toBeFocused();
  await expect(page.locator("#projsel-btn")).toHaveAttribute("aria-expanded", "false");
});

// --- SH-171 regression guards ---------------------------------------------
//
// `canOpen()` (r.available || r.read_only) is what decides whether a home
// card or a menu item can be opened. SH-171 was `renderRepoSelect()` and
// `bootstrap()` disagreeing with it by checking bare `r.available`, which is
// false for a read-only project (no checkout on this machine) even though
// such a project is fully openable — just not editable. These specs drove
// the native `<select>`'s disabled-option state at the time SH-171 was
// fixed; SH-42 replaced that control, so they now drive its popover
// equivalent instead — the underlying claim they guard is unchanged.

test("a project with no checkout is not disabled in the selector menu", async ({
  page,
}) => {
  await page.locator(".repo-card-name", { hasText: "Alpha Project" }).click();
  await openMenu(page);

  const item = page.locator("#projsel-menu .projsel-item", {
    hasText: "Gamma Archive",
  });
  await expect(item).not.toHaveAttribute("aria-disabled", "true");

  await item.click();
  await expect(page.locator("#board-view")).toBeVisible();
  await expect(page.locator("#projsel-btn")).toContainText("GA · Gamma Archive");
});

test("reloading while a project with no checkout is open keeps it open", async ({
  page,
}) => {
  await page
    .locator(".repo-card-name", { hasText: "Gamma Archive" })
    .click();
  await expect(page.locator("#board-view")).toBeVisible();

  await page.reload();

  await expect(page.locator("#board-view")).toBeVisible();
  await expect(page.locator("#home-view")).toBeHidden();
  await expect(page.locator("#projsel-btn")).toContainText("GA · Gamma Archive");
});
