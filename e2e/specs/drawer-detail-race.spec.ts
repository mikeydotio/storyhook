import { test, expect } from "@playwright/test";
import {
  cleanUpCreatedStories,
  deleteStory,
  holdDetailFetch,
  latch,
  openProject,
  projectSlug,
  requiredEnv,
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
  await openProject(page, "Alpha Project");
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

/** Deletes `title`'s "blocked"-column story through the drawer -- same
 * shape as the imported `deleteStory`, scoped to the Blocked column
 * instead of todo. SH-407: an awaiting reason or an open blocker
 * display-promotes a story out of "todo" and into "blocked", which both
 * of this file's own block/blocked-by tests trigger on their worker
 * story before cleanup. */
async function deleteBlockedStory(
  page: import("@playwright/test").Page,
  title: string,
) {
  const card = page.locator('.column[data-state="blocked"] .card', {
    hasText: title,
  });
  await card.click();
  await expect(page.locator("#drawer")).toHaveClass(/open/);
  await page.locator("#drawer-footer button", { hasText: "Delete" }).click();
  await expect(page.locator("#delete-modal")).toHaveClass(/open/);
  await page.locator("#delete-reason").fill("e2e cleanup");
  await page.locator("#delete-modal-submit").click();
  await expect(card).not.toBeVisible();
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
  // SH-407: the typed block reason display-promoted the card into
  // "blocked" the instant it was recorded, out of "todo".
  await deleteBlockedStory(page, title);
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

  // Blurs the title; the click target is the rendered view, not the raw
  // textarea (SH-217: the description field is display:none outside
  // edit mode, so Playwright's actionability check would time out on
  // `.description-field` here).
  await page.locator(".description-view").click();
  await expect.poll(() => patches.length).toBeGreaterThan(0);
  expect(patches).toEqual([newTitle]);

  await expect(
    page.locator('.column[data-state="todo"] .card', { hasText: newTitle }),
  ).toBeVisible();

  await page.locator("#drawer-close").click();
  await expect(page.locator("#drawer")).not.toHaveClass(/open/);
  await deleteStory(page, newTitle);
});

/**
 * SH-281, and the reason that story spent a day looking like a stale-data
 * bug: the rebuild does not need a *detail* fetch. `fetchData()` calls
 * `renderDrawer()` whenever a board reply reports any change at all while a
 * drawer is open — a story created in another tab will do it — and the
 * rebuild used to reset the add-a-relation `<select>` to its first option.
 * That option is `relates-to`, so a refresh landing between choosing
 * "blocked-by" and pressing Add recorded a relates-to edge instead: the
 * wrong relation, on the user's behalf, with no error anywhere and a
 * Relationships list that reads as if it worked.
 *
 * Downstream this looked like a card that had lost its blockers row, which
 * is what `card-blockers.spec.ts` reported twice inside `make test` and
 * never once in `make e2e` — the difference being how much else was
 * happening to make a board reply land inside that window.
 */
test("choosing a relation kind survives a board refresh that rebuilds the drawer", async ({
  page,
  request,
}) => {
  const blockerTitle = "SH-281 relation kind — the blocker";
  const workerTitle = "SH-281 relation kind — the blocked story";
  await createStory(page, blockerTitle);
  await createStory(page, workerTitle);
  const blockerId = (await page
    .locator('.column[data-state="todo"] .card', { hasText: blockerTitle })
    .getAttribute("data-id"))!;

  await page
    .locator('.column[data-state="todo"] .card', { hasText: workerTitle })
    .click();
  await expect(page.locator("#drawer")).toHaveClass(/open/);

  const kind = page.locator("#drawer-body .inline-add select");
  await kind.selectOption("blocked-by");
  await page
    .locator('input[placeholder="Story ID (e.g. SH-2)"]')
    .fill(blockerId);

  // The rebuild, from outside this tab and outside this drawer: a story
  // created through the API raises SSE `repo-changed`, whose board fetch
  // reports a change, whose `renderDrawer()` rebuilds the body under the
  // form. Waiting for the new card is what makes it deterministic — the
  // card cannot appear until that reply has been applied and rendered.
  const nudgeTitle = "SH-281 relation kind — the nudge";
  const slug = await projectSlug(request, "Alpha Project");
  const nudge = await request.post(`/api/repos/${slug}/story`, {
    headers: {
      "X-Storyhook": "1",
      "X-Storyhook-Token": requiredEnv("DASHBOARD_TOKEN"),
      "Content-Type": "application/json",
    },
    data: { title: nudgeTitle },
  });
  expect(nudge.ok()).toBe(true);
  await expect(
    page.locator('.column[data-state="todo"] .card', { hasText: nudgeTitle }),
  ).toBeVisible();

  await expect(kind).toHaveValue("blocked-by");
  await page
    .locator("#drawer-body .inline-add button", { hasText: "Add" })
    .click();

  // The relation actually recorded, not merely that *a* relation was: the
  // whole defect is that the Relationships list looked right while the edge
  // underneath it was the wrong kind.
  const row = page.locator(".rel-row", { hasText: blockerId });
  await expect(row).toBeVisible();
  await expect(row.locator(".rel-kind")).toHaveText("blocked-by");

  await page.locator("#drawer-close").click();
  await expect(page.locator("#drawer")).not.toHaveClass(/open/);

  // And the consequence the board draws from it: only `blocked-by` names
  // the blocker in the badge (`openBlockers`, `blockedFlag` -- SH-309
  // moved this out of `.card-blockers`, which now holds only the
  // cleared-blocker dwell).
  const workerCard = page.locator(".card", { hasText: workerTitle });
  await expect(workerCard.locator(".flag-blocked .rel-id")).toHaveText(
    blockerId,
  );

  // SH-407: the blocked-by edge display-promoted the worker into "blocked",
  // out of "todo" -- `workerCard` above is deliberately unscoped by column
  // for that reason, but cleanup still needs to find it where it now sits.
  await deleteBlockedStory(page, workerTitle);
});
