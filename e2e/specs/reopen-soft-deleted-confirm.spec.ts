import { test, expect } from "@playwright/test";
import { seedToken, requiredEnv } from "./support";

/**
 * Exercises SH-210: an unforced `POST .../story/{id}/reopen` of a
 * soft-deleted story answers 409 with an `UndeletePlan` (SH-154's
 * `reopen_plan`) -- this checks the dashboard draws a proper undelete
 * confirm from that plan instead of the bare "Conflict" toast `toastError`
 * alone used to leave the user with: no explanation, no way forward short
 * of the CLI's `--force`.
 *
 * Reaching that 409 from the drawer's own "Reopen" button needs a specific
 * race: `StoryService::delete` only accepts an *open* story -- it closes and
 * soft-deletes in one atomic step, and refuses (as not-found) a story that
 * is already closed (`service/story.rs`'s own comment: "An already-archived
 * story is *not found* rather than *closed*"). So a client can only ever
 * observe the *result* of a delete as an already-closed, already-deleted
 * story -- never a plain close followed later by a delete. The one place
 * that result reaches the client without the deletion being filtered out is
 * `openDrawer()`'s single-story detail fetch (`GET .../story/<id>`, unlike
 * the board's own `/data`, does not exclude deleted stories) -- and the
 * drawer footer's Reopen-button logic reads only `superstate`, never
 * `deleted`. So: open the drawer while the story is still open (its detail
 * fetch in flight), delete it out from under that in-flight fetch, and let
 * the fetch resolve -- the footer now shows "Reopen" for a story that is
 * actually soft-deleted, exactly SH-210's bug condition.
 *
 * The detail fetch is delayed with `page.route()` to make that race
 * deterministic rather than relying on real network timing, the same
 * technique `drawer-detail-race.spec.ts` (SH-218) uses for its own race.
 * `EventSource` is stubbed out before the page loads so a live
 * `repo-changed` push -- which the out-of-band delete below genuinely
 * triggers -- can't resync `state.data.stories` (dropping the now-deleted
 * story) and close the drawer during that manufactured delay; this is the
 * app's own documented "no push support" fallback (`connectEvents()`'s
 * first line), not a special test hook.
 *
 * This spec creates and deletes its own stories rather than touching the
 * "Alpha Project" fixture, whose exact two-story shape other specs
 * (filter-persistence.spec.ts, column-visibility.spec.ts) assert on
 * byte-for-byte per run-e2e.sh's own comment.
 */

test.beforeEach(async ({ page }) => {
  await page.addInitScript(() => {
    // Plain assignment, not `delete` -- `EventSource` is a non-configurable
    // own property of `window` in Chromium, so `delete window.EventSource`
    // silently no-ops and the live push stays wired up.
    // @ts-expect-error -- simulating a client with no SSE push support, the
    // same fallback path a browser without EventSource takes for real.
    window.EventSource = undefined;
  });
  await seedToken(page);
  await page.goto("/");
  await page.locator(".repo-card-name", { hasText: "Alpha Project" }).click();
  await expect(page.locator("#board-view")).toBeVisible();
});

/** Creates `title` via the "+ New" modal and returns the project id and
 * story id straight off the create response -- read off the request rather
 * than hardcoded, since nothing in this file has access to the
 * closure-scoped `state.repoId`/`state.drawerId` the page itself keeps. */
async function createStory(
  page: import("@playwright/test").Page,
  title: string,
): Promise<{ repoId: string; storyId: string }> {
  const created = page.waitForResponse(
    (resp) =>
      /\/api\/repos\/[^/]+\/story$/.test(new URL(resp.url()).pathname) &&
      resp.request().method() === "POST",
  );
  await page.locator("#new-story-btn").click();
  await expect(page.locator("#create-modal")).toHaveClass(/open/);
  await page.locator("#create-title").fill(title);
  await page.locator("#create-submit").click();
  await expect(page.locator("#create-modal")).not.toHaveClass(/open/);
  await expect(
    page.locator('.column[data-state="todo"] .card', { hasText: title }),
  ).toBeVisible();

  const resp = await created;
  const match = new URL(resp.url()).pathname.match(
    /\/api\/repos\/([^/]+)\/story$/,
  );
  if (!match) throw new Error("could not parse repo id from the create response");
  const body = await resp.json();
  return { repoId: decodeURIComponent(match[1]), storyId: body.story.story.id };
}

/** Delays every `GET .../story/<id>` detail fetch -- the same helper
 * `drawer-detail-race.spec.ts` uses to make its own race deterministic. */
async function delayDetailFetch(page: import("@playwright/test").Page) {
  await page.route(/\/story\/[^/]+$/, async (route) => {
    if (route.request().method() !== "GET") {
      await route.continue();
      return;
    }
    await new Promise((resolve) => setTimeout(resolve, 400));
    await route.continue();
  });
}

/** Soft-deletes `storyId` via a direct API call, standing in for another
 * session's write while `title`'s own detail fetch is deliberately delayed
 * (see this file's header comment). Requires the story to currently be
 * open -- `StoryService::delete` refuses an already-closed one. */
async function deleteOutOfBand(
  request: import("@playwright/test").APIRequestContext,
  repoId: string,
  storyId: string,
  reason: string,
) {
  const res = await request.delete(
    `/api/repos/${encodeURIComponent(repoId)}/story/${encodeURIComponent(storyId)}`,
    {
      headers: {
        "X-Storyhook": "1",
        "X-Storyhook-Token": requiredEnv("DASHBOARD_TOKEN"),
      },
      data: { reason },
    },
  );
  if (!res.ok()) {
    throw new Error(
      `out-of-band delete of ${storyId} failed: ${res.status()} ${await res.text()}`,
    );
  }
}

/** Opens `title`'s drawer (from "todo") and waits for its detail fetch to
 * resolve -- by the time this returns, the race set up by the caller has
 * already played out and the footer reflects it. */
async function openDrawerAndAwaitDetail(
  page: import("@playwright/test").Page,
  title: string,
) {
  const detailLoaded = page.waitForResponse(
    (resp) =>
      /\/api\/repos\/[^/]+\/story\/[^/]+$/.test(new URL(resp.url()).pathname) &&
      resp.request().method() === "GET",
  );
  await page
    .locator('.column[data-state="todo"] .card', { hasText: title })
    .click();
  await expect(page.locator("#drawer")).toHaveClass(/open/);
  await detailLoaded;
}

async function deleteStory(
  page: import("@playwright/test").Page,
  title: string,
  column: string,
) {
  // Both callers below reach this right after an Undelete that leaves the
  // drawer open on this same story -- close it first so the card underneath
  // is clickable (an open drawer's backdrop intercepts pointer events).
  if (await page.locator("#drawer").evaluate((el) => el.classList.contains("open"))) {
    await page.locator("#drawer-close").click();
    await expect(page.locator("#drawer")).not.toHaveClass(/open/);
  }
  const card = page.locator(`.column[data-state="${column}"] .card`, {
    hasText: title,
  });
  await card.click();
  await expect(page.locator("#drawer")).toHaveClass(/open/);
  await page.locator("#drawer-footer button", { hasText: "Delete" }).click();
  await page
    .locator('input[placeholder="Reason for deletion (required)"]')
    .fill("e2e cleanup");
  await page
    .locator("#drawer-footer button", { hasText: "Confirm delete" })
    .click();
  await expect(card).not.toBeVisible();
}

test("Reopen on a story deleted out from under the drawer shows an undelete confirm, and Cancel backs out of it", async ({
  page,
  request,
}) => {
  const title = "SH-210 reopen confirm — cancel";
  const { repoId, storyId } = await createStory(page, title);
  await delayDetailFetch(page);

  // Both fire while the story is still open: the click starts the (delayed)
  // detail fetch, then the delete races it to completion first.
  const opened = openDrawerAndAwaitDetail(page, title);
  await deleteOutOfBand(request, repoId, storyId, "e2e out-of-band delete");
  await opened;
  // The delay was only needed to win that race; leaving it wired up would
  // make every later detail fetch in this test (including cleanup's) replay
  // SH-218's stale-node race against whatever clicks first.
  await page.unroute(/\/story\/[^/]+$/);

  // The footer now shows the ordinary closed-story "Reopen" button for a
  // story the server already knows is soft-deleted -- SH-210's bug
  // condition.
  const reopenBtn = page.locator("#drawer-footer button", { hasText: "Reopen" });
  await expect(reopenBtn).toBeVisible();
  await reopenBtn.click();

  await expect(page.locator("#drawer-footer")).toContainText(
    `${title} was deleted (e2e out-of-band delete)`,
  );
  await expect(page.locator(".toast.error")).toHaveCount(0);

  await page.locator("#drawer-footer button", { hasText: "Cancel" }).click();
  await expect(reopenBtn).toBeVisible();
  await expect(page.locator("#drawer-footer")).not.toContainText("was deleted");

  // Cleanup: reopen for real (the story is still soft-deleted) so it lands
  // back in "todo" for the ordinary delete flow to remove.
  await reopenBtn.click();
  await page.locator("#drawer-footer button", { hasText: "Undelete" }).click();
  await deleteStory(page, title, "todo");
});

test("confirming Undelete reopens the story into the project's default open state", async ({
  page,
  request,
}) => {
  const title = "SH-210 reopen confirm — undelete";
  const { repoId, storyId } = await createStory(page, title);
  await delayDetailFetch(page);

  const opened = openDrawerAndAwaitDetail(page, title);
  await deleteOutOfBand(request, repoId, storyId, "e2e out-of-band delete");
  await opened;
  // The delay was only needed to win that race; leaving it wired up would
  // make every later detail fetch in this test (including cleanup's) replay
  // SH-218's stale-node race against whatever clicks first.
  await page.unroute(/\/story\/[^/]+$/);

  await page.locator("#drawer-footer button", { hasText: "Reopen" }).click();
  await expect(page.locator("#drawer-footer")).toContainText("was deleted");

  await page.locator("#drawer-footer button", { hasText: "Undelete" }).click();
  await expect(
    page.locator(".toast.success", { hasText: `${storyId} undeleted` }),
  ).toBeVisible();

  // `StoryService::reopen` moves an undeleted story to the project's
  // default open state -- back on the board, not left sitting wherever it
  // was last closed.
  await expect(
    page.locator('.column[data-state="todo"] .card', { hasText: title }),
  ).toBeVisible();

  await deleteStory(page, title, "todo");
});
