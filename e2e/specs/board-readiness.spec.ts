import { test, expect } from "@playwright/test";
import { openProject, projectSlug, seedToken } from "./support";

/**
 * Pins SH-222's readiness rule: reaching a board means its data has arrived,
 * not that the board is on screen.
 *
 * Both tests run with `/data` deliberately slowed. That delay is the only
 * thing a busy machine was ever contributing: `selectRepo()` renders the
 * board screen from `state.data = null` and fetches afterwards, so the
 * window between "board visible" and "board has data" exists on every run —
 * it is simply too narrow to lose on an idle machine, and wide enough to
 * lose after a fifteen-minute gate. Reproducing it with a route delay rather
 * than with load is what makes it a test instead of an anecdote.
 */

const DATA_DELAY_MS = 2000;

/** Holds every project-data read for `DATA_DELAY_MS`. Registered before the
 * navigation that triggers one, since the very first `/data` is the one
 * under test. */
async function slowData(page: import("@playwright/test").Page): Promise<void> {
  await page.route(/\/data(\?|$)/, async (route) => {
    await new Promise((resolve) => setTimeout(resolve, DATA_DELAY_MS));
    await route.continue();
  });
}

test("the board is visible, and empty, before its data arrives", async ({
  page,
}) => {
  await seedToken(page);
  await slowData(page);
  // Deep-linked rather than clicked, so this test can observe the window
  // without going through `openProject()` -- the helper whose whole job is
  // to close it.
  const slug = await projectSlug(page.request, "Alpha Project");
  await page.goto(`/?project=${encodeURIComponent(slug)}`);

  await expect(page.locator("#board-view")).toBeVisible();
  // What "visible but not loaded" actually looks like: no cards, and a
  // filter count that renderView() leaves empty because `state.data` is
  // still null. A spec that treated the assertion above as readiness would
  // be acting on this.
  await expect(page.locator(".card")).toHaveCount(0);
  await expect(page.locator("#filter-count")).toHaveText("");

  // And it does resolve on its own -- the window is a window, not a
  // breakage.
  await expect(page.locator("#filter-count")).not.toHaveText("", {
    timeout: DATA_DELAY_MS * 3,
  });
});

test("openProject waits for the data, so the create modal is fully built", async ({
  page,
}) => {
  await seedToken(page);
  await slowData(page);
  await page.goto("/");

  const started = Date.now();
  await openProject(page, "Alpha Project");
  const waited = Date.now() - started;

  // A lower bound on waiting, which is the safe direction to assert in: load
  // can only make this number larger (SH-238's flake was an upper-bound
  // margin, and this is deliberately not one). Below it, `openProject` would
  // have returned on the board screen alone, which is the defect.
  expect(waited).toBeGreaterThan(DATA_DELAY_MS * 0.75);

  // The consequence, which is the reason any of this matters. The create
  // modal is built once, synchronously, from `meta()` at the moment it
  // opens: opened in the window above it holds nothing but its placeholder
  // options and never repopulates, so this line spins out the full test
  // timeout with "did not find some options" rather than failing fast.
  await page.locator("#new-story-btn").click();
  await expect(page.locator("#create-modal")).toHaveClass(/open/);
  await page.locator("#create-priority").selectOption("critical");
  await expect(page.locator("#create-priority")).toHaveValue("critical");

  // Nothing is created: Escape does not dismiss this modal (draft-stories
  // .spec.ts pins that), so the draft is discarded explicitly.
  await page.locator("#create-discard").click();
  await expect(page.locator("#create-modal")).not.toHaveClass(/open/);
});
