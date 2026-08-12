import { test, expect } from "@playwright/test";
import {
  cleanUpCreatedStories,
  deleteStory,
  holdDetailFetch,
  latch,
  seedToken,
} from "./support";

/**
 * Exercises SH-218: openDrawer() renders once synchronously from cached
 * summary data, then fires an async `GET .../story/<id>` for full detail;
 * when that resolves, renderDrawer() clears and rebuilds the whole drawer
 * body from scratch. Any uncontrolled field the user is mid-edit in when
 * that second render lands used to be silently wiped -- a click on Block
 * right after would hit a fresh, empty input and no-op.
 *
 * These specs hold the detail GET in flight with `page.route()` and release
 * it themselves, rather than relying on real network timing (sub-100ms
 * locally, per the story's own comment -- unreliable to hit without help).
 * They used to hold it for a fixed 500ms instead, which is deterministic
 * only for as long as the test out-races the timer: lose that race and
 * either the assertion goes red for a reason unrelated to SH-218, or the
 * window has closed before the test types and the spec passes having
 * exercised nothing (SH-245).
 *
 * This spec creates and deletes its own stories rather than touching the
 * "Alpha Project" fixture, whose exact two-story shape other specs
 * (filter-persistence.spec.ts, column-visibility.spec.ts) assert on
 * byte-for-byte per run-e2e.sh's own comment.
 */

cleanUpCreatedStories("Alpha Project");

test.beforeEach(async ({ page }) => {
  await seedToken(page);
  await page.goto("/");
  await page.locator(".repo-card-name", { hasText: "Alpha Project" }).click();
  await expect(page.locator("#board-view")).toBeVisible();
});

async function createStory(
  page: import("@playwright/test").Page,
  title: string,
) {
  await page.locator("#new-story-btn").click();
  await expect(page.locator("#create-modal")).toHaveClass(/open/);
  await page.locator("#create-title").fill(title);
  await page.locator("#create-submit").click();
  await expect(page.locator("#create-modal")).not.toHaveClass(/open/);
  await expect(
    page.locator('.column[data-state="todo"] .card', { hasText: title }),
  ).toBeVisible();
}

test("typing a block reason during the detail fetch survives the re-render and still blocks", async ({
  page,
}) => {
  const title = "SH-218 drawer race — block reason";
  await createStory(page, title);
  const releaseDetail = await holdDetailFetch(page);

  const detailLoaded = page.waitForResponse(
    (resp) =>
      /\/story\/[^/]+$/.test(new URL(resp.url()).pathname) &&
      resp.request().method() === "GET",
  );
  await page
    .locator('.column[data-state="todo"] .card', { hasText: title })
    .click();
  await expect(page.locator("#drawer")).toHaveClass(/open/);

  const reasonInput = page.locator('input[placeholder="Reason for blocking…"]');
  await reasonInput.fill("typed before the detail fetch resolved");

  // The detail fetch resolves here, mid-edit -- this is the race, and the
  // edit is already in the field before the release that starts it.
  releaseDetail();
  await detailLoaded;
  await expect(reasonInput).toHaveValue("typed before the detail fetch resolved");

  await page.locator("#drawer-body button", { hasText: "Block" }).click();
  await expect(page.locator(".banner-blocked")).toContainText(
    "typed before the detail fetch resolved",
  );

  await page.locator("#drawer-close").click();
  await expect(page.locator("#drawer")).not.toHaveClass(/open/);
  await deleteStory(page, title);
});

test("editing the title during the detail fetch survives the re-render without a duplicate save", async ({
  page,
}) => {
  const title = "SH-218 drawer race — title";
  const newTitle = "SH-218 drawer race — title (edited)";
  await createStory(page, title);

  // One handler covers both concerns for this test: hold the detail GET
  // (to open the race window) and record every title PATCH (to prove the
  // re-render's forced blur never smuggles out an early or duplicate save).
  // Its own route rather than holdDetailFetch()'s, since only the GET half
  // is shared.
  const patches: string[] = [];
  const detail = latch();
  await page.route(/\/story\/[^/]+$/, async (route) => {
    const req = route.request();
    if (req.method() === "GET") {
      await detail.held;
    } else if (req.method() === "PATCH") {
      const body = req.postDataJSON();
      if (body && typeof body.title === "string") patches.push(body.title);
    }
    await route.continue();
  });

  const detailLoaded = page.waitForResponse(
    (resp) =>
      /\/story\/[^/]+$/.test(new URL(resp.url()).pathname) &&
      resp.request().method() === "GET",
  );
  await page
    .locator('.column[data-state="todo"] .card', { hasText: title })
    .click();
  await expect(page.locator("#drawer")).toHaveClass(/open/);

  const titleInput = page.locator(".drawer-title");
  await titleInput.fill(newTitle);

  // The detail fetch resolves here, mid-edit -- browsers fire a real `blur`
  // on the old input as the re-render detaches it. Without SH-218's guard
  // that blur would autosave the title early (and, on real per-keystroke
  // typing rather than fill()'s atomic write, could autosave a partial
  // value); the assertions below catch either a duplicate PATCH or a lost
  // edit.
  detail.release();
  await detailLoaded;
  await expect(titleInput).toHaveValue(newTitle);

  await page.locator(".description-field").click();
  await expect.poll(() => patches.length).toBeGreaterThan(0);
  expect(patches).toEqual([newTitle]);

  await expect(
    page.locator('.column[data-state="todo"] .card', { hasText: newTitle }),
  ).toBeVisible();

  await page.locator("#drawer-close").click();
  await expect(page.locator("#drawer")).not.toHaveClass(/open/);
  await deleteStory(page, newTitle);
});
