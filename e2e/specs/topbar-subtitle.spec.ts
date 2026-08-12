import { test, expect } from "@playwright/test";
import { seedToken } from "./support";

/**
 * The topbar's `#subtitle`, per screen (SH-234).
 *
 * A board used to carry "N stories · N ready · N blocked" beside the
 * wordmark. It was retired: the board itself already shows every one of
 * those numbers, in the form that can be acted on — the columns are the
 * story count, the blocked badge is the blocked count — so the label
 * restated them in a place nothing could be done with them, and did it in
 * a corner of the topbar the eye goes to for the project name.
 *
 * Home's own subtitle is kept and asserted here too, as the control: it is
 * what makes this a scoped removal rather than the element being deleted,
 * and it fails if the repo-screen branch is removed by deleting the
 * subtitle outright.
 */

test.beforeEach(async ({ page }) => {
  await seedToken(page);
  await page.goto("/");
});

test("a board carries no story-count summary in the topbar", async ({
  page,
}) => {
  await page.locator(".repo-card-name", { hasText: "Alpha Project" }).click();
  await expect(page.locator("#board-view")).toBeVisible();
  // The cards are on screen, so the data the label was rendered from has
  // arrived — this is the moment the counts would have appeared, not a
  // window before them.
  await expect(
    page.locator(".card-title", { hasText: "Wire up the auth flow" }),
  ).toBeVisible();

  await expect(page.locator("#subtitle")).toBeHidden();
  await expect(page.locator(".topbar")).not.toContainText("ready");
  await expect(page.locator(".topbar")).not.toContainText("blocked");
});

test("Home still names how many projects there are", async ({ page }) => {
  await expect(page.locator("#home-view")).toBeVisible();
  await expect(page.locator("#subtitle")).toBeVisible();
  await expect(page.locator("#subtitle")).toHaveText(/^\d+ projects?$/);
});

test("returning to Home from a board restores its subtitle", async ({
  page,
}) => {
  await page.locator(".repo-card-name", { hasText: "Alpha Project" }).click();
  await expect(page.locator("#subtitle")).toBeHidden();

  await page.locator("#home-btn").click();

  await expect(page.locator("#home-view")).toBeVisible();
  await expect(page.locator("#subtitle")).toBeVisible();
  await expect(page.locator("#subtitle")).toHaveText(/^\d+ projects?$/);
});
