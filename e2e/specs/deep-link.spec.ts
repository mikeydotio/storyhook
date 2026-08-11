import { test, expect } from "@playwright/test";
import { projectSlug, requiredEnv, seedToken } from "./support";

/**
 * Exercises SH-197's `?project=<slug>&story=<id>` deep link against a real
 * daemon and the two seeded projects `project-selector.spec.ts` also uses:
 *
 *   - "Alpha Project" (prefix AA) — has a checkout, two open stories,
 *     including `DASHBOARD_ALPHA_STORY_ID` ("Wire up the auth flow")
 *   - "Gamma Archive" (prefix GA) — `--no-attach`: no checkout, so it's
 *     reachable read-only, exactly the case `resolveDeepLinkProject()`
 *     (`src/web_dashboard.html`) must gate on `canOpen`, not `available`
 *
 * `projectSlug()` (`./support.ts`) asks the daemon for each project's slug
 * rather than assuming the shape `story project new` derives from a name —
 * that derivation is not this suite's to depend on.
 */

const ALPHA_STORY_ID = requiredEnv("DASHBOARD_ALPHA_STORY_ID");

test.beforeEach(async ({ page }) => {
  await seedToken(page);
});

test("?project= lands directly on that project's board, bypassing Home", async ({
  page,
  request,
}) => {
  const alpha = await projectSlug(request, "Alpha Project");
  await page.goto(`/?project=${alpha}`);

  await expect(page.locator("#board-view")).toBeVisible();
  await expect(page.locator("#home-view")).toBeHidden();
  await expect(page.locator("#projsel-btn")).toContainText("AA · Alpha Project");
});

test("?project=&story= also opens that story's drawer", async ({
  page,
  request,
}) => {
  const alpha = await projectSlug(request, "Alpha Project");
  await page.goto(`/?project=${alpha}&story=${ALPHA_STORY_ID}`);

  await expect(page.locator("#board-view")).toBeVisible();
  await expect(page.locator("#drawer")).toHaveClass(/open/);
  await expect(page.locator("#drawer-id")).toHaveText(ALPHA_STORY_ID);
});

test("an unknown ?project= lands on Home with an error toast", async ({
  page,
}) => {
  await page.goto("/?project=no-such-project-xyz");

  await expect(page.locator("#home-view")).toBeVisible();
  const toast = page.locator("#toast-stack .toast.error");
  await expect(toast).toBeVisible();
  await expect(toast).toContainText("no-such-project-xyz");
});

test("a project with no checkout is still reachable by deep link (canOpen, not available)", async ({
  page,
  request,
}) => {
  const gamma = await projectSlug(request, "Gamma Archive");
  await page.goto(`/?project=${gamma}`);

  await expect(page.locator("#board-view")).toBeVisible();
  await expect(page.locator("#projsel-btn")).toContainText("GA · Gamma Archive");
});

test("?story= naming a story absent from the project errors and is stripped from the address bar", async ({
  page,
  request,
}) => {
  const alpha = await projectSlug(request, "Alpha Project");
  await page.goto(`/?project=${alpha}&story=AA-9999`);

  await expect(page.locator("#board-view")).toBeVisible();
  const toast = page.locator("#toast-stack .toast.error");
  await expect(toast).toBeVisible();
  await expect(toast).toContainText("AA-9999");
  await expect(page.locator("#drawer")).not.toHaveClass(/open/);

  // `syncUrl()` runs synchronously off the same fetch callback that shows
  // the toast, so by the time the toast above is visible the address bar
  // has already settled -- no retry needed.
  const url = new URL(page.url());
  expect(url.searchParams.get("project")).toBe(alpha);
  expect(url.searchParams.get("story")).toBeNull();
});

test("selecting a project and a story updates the address bar; closing keeps the project", async ({
  page,
  request,
}) => {
  const alpha = await projectSlug(request, "Alpha Project");
  await page.goto("/");
  expect(new URL(page.url()).searchParams.get("project")).toBeNull();

  await page.locator(".repo-card-name", { hasText: "Alpha Project" }).click();
  await expect(page.locator("#board-view")).toBeVisible();
  expect(new URL(page.url()).searchParams.get("project")).toBe(alpha);
  expect(new URL(page.url()).searchParams.get("story")).toBeNull();

  await page
    .locator(".card-title", { hasText: "Wire up the auth flow" })
    .click();
  await expect(page.locator("#drawer")).toHaveClass(/open/);
  expect(new URL(page.url()).searchParams.get("story")).toBe(ALPHA_STORY_ID);

  await page.locator("#drawer-close").click();
  await expect(page.locator("#drawer")).not.toHaveClass(/open/);
  const closedUrl = new URL(page.url());
  expect(closedUrl.searchParams.get("project")).toBe(alpha);
  expect(closedUrl.searchParams.get("story")).toBeNull();
});

test("unrelated query parameters survive selecting a project", async ({
  page,
  request,
}) => {
  const alpha = await projectSlug(request, "Alpha Project");
  await page.goto("/?sseStaleAfterMs=400");

  await page.locator(".repo-card-name", { hasText: "Alpha Project" }).click();
  await expect(page.locator("#board-view")).toBeVisible();

  const url = new URL(page.url());
  expect(url.searchParams.get("sseStaleAfterMs")).toBe("400");
  expect(url.searchParams.get("project")).toBe(alpha);
});
