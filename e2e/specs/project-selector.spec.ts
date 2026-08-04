import { test, expect } from "@playwright/test";

/**
 * Exercises the dashboard's header project control against a real daemon
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
 * This file currently drives the native `<select id="repo-select">` — the
 * control SH-42 replaces. It pins today's behavior so the commits that
 * follow (the read-only-reachability fix, then the header popover itself)
 * have a working harness to go red against before they go green. Both of
 * those commits update the interaction parts of this file to match; the
 * assertions about *what* the dashboard should do do not change, only *how*
 * the spec drives the control.
 */

test.beforeEach(async ({ page }) => {
  await page.goto("/");
});

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
});

test("clicking a project's card opens its board", async ({ page }) => {
  await page.locator(".repo-card-name", { hasText: "Alpha Project" }).click();
  await expect(page.locator("#board-view")).toBeVisible();
  await expect(
    page.locator(".card-title", { hasText: "Wire up the auth flow" }),
  ).toBeVisible();
});

test("the header selector switches between projects", async ({ page }) => {
  await page.locator(".repo-card-name", { hasText: "Alpha Project" }).click();
  await expect(
    page.locator(".card-title", { hasText: "Wire up the auth flow" }),
  ).toBeVisible();

  await page.locator("#repo-select").selectOption({ label: "Beta Project" });

  await expect(
    page.locator(".card-title", { hasText: "Draft the release notes" }),
  ).toBeVisible();
  await expect(
    page.locator(".card-title", { hasText: "Wire up the auth flow" }),
  ).not.toBeVisible();
});

test("a project with no checkout on this machine is reachable from its home card", async ({
  page,
}) => {
  await page
    .locator(".repo-card-name", { hasText: "Gamma Archive" })
    .click();
  await expect(page.locator("#board-view")).toBeVisible();
});
