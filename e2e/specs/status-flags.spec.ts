import { test, expect } from "@playwright/test";
import { deleteStory, seedToken } from "./support";

/**
 * Exercises SH-168: the board and list views no longer decorate a "ready"
 * story with a green badge or border — ready is the default state and
 * needs no visual call-out. "Blocked" stays the one visually-flagged
 * exception, in both the board card flag and the list-row left border.
 *
 * Scope decided by council vote (`.council/sh168-ready-label-scope/`,
 * unanimous in the runoff): the board card's `.flag-ready` badge and the
 * list-row's green `border-left` both fall — both are steady-state
 * per-render decorations of the same kind. The `flash-ready` transition
 * pulse (a diff-triggered, self-removing animation, mirroring the untouched
 * `flash-blocked`/`flash-priority` paths) is out of scope and untouched.
 * The CLI's `story report --html` static report is a separate feature and
 * is also untouched.
 *
 * This spec creates and deletes its own stories rather than touching the
 * "Alpha Project" fixture, whose exact two-story shape other specs
 * (filter-persistence.spec.ts, column-visibility.spec.ts) assert on
 * byte-for-byte per run-e2e.sh's own comment.
 */

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

async function openDrawer(
  page: import("@playwright/test").Page,
  title: string,
) {
  // openDrawer() in the dashboard renders once from already-cached summary
  // data, then fires a `GET .../story/<id>` whose resolution re-renders the
  // whole drawer body from scratch — including a fresh, empty block-reason
  // input. Waiting for that response here closes the race: without it, a
  // fill() landing before this second render is silently wiped, and the
  // subsequent click hits the new empty input's Block button, which no-ops.
  const detailLoaded = page.waitForResponse(
    (resp) =>
      /\/story\/[^/]+$/.test(new URL(resp.url()).pathname) &&
      resp.request().method() === "GET",
  );
  await page
    .locator('.column[data-state="todo"] .card', { hasText: title })
    .click();
  await expect(page.locator("#drawer")).toHaveClass(/open/);
  await detailLoaded;
}

async function blockStory(
  page: import("@playwright/test").Page,
  title: string,
  reason: string,
) {
  await openDrawer(page, title);
  await page
    .locator('input[placeholder="Reason for blocking…"]')
    .fill(reason);
  await page.locator("#drawer-body button", { hasText: "Block" }).click();
  await expect(page.locator(".banner-blocked")).toBeVisible();
  await page.locator("#drawer-close").click();
  await expect(page.locator("#drawer")).not.toHaveClass(/open/);
}

test("a ready card carries no flag badge on the board", async ({ page }) => {
  const title = "SH-168 status flags — ready card";
  await createStory(page, title);

  const card = page.locator('.column[data-state="todo"] .card', {
    hasText: title,
  });
  await expect(card.locator(".card-flags .flag")).toHaveCount(0);

  await deleteStory(page, title);
});

test("a blocked card still carries the red blocked flag badge on the board", async ({
  page,
}) => {
  const title = "SH-168 status flags — blocked card";
  await createStory(page, title);
  await blockStory(page, title, "e2e: exercising the blocked flag");

  const card = page.locator('.column[data-state="todo"] .card', {
    hasText: title,
  });
  await expect(card.locator(".flag-blocked")).toHaveText("● blocked");

  await deleteStory(page, title);
});

test("a ready row has no colored left border in the list view", async ({
  page,
}) => {
  const title = "SH-168 status flags — ready row";
  await createStory(page, title);

  await page.locator('#view-toggle button[data-view="list"]').click();
  await expect(page.locator("#list-view")).toBeVisible();

  const row = page.locator("#list-body tr", { hasText: title });
  await expect(row).toHaveCSS("border-left-width", "0px");

  await page.locator('#view-toggle button[data-view="board"]').click();
  await deleteStory(page, title);
});

test("a blocked row keeps its red left border in the list view", async ({
  page,
}) => {
  const title = "SH-168 status flags — blocked row";
  await createStory(page, title);
  await blockStory(page, title, "e2e: exercising the blocked row border");

  await page.locator('#view-toggle button[data-view="list"]').click();
  await expect(page.locator("#list-view")).toBeVisible();

  const row = page.locator("#list-body tr", { hasText: title });
  await expect(row).toHaveCSS("border-left-width", "3px");

  await page.locator('#view-toggle button[data-view="board"]').click();
  await deleteStory(page, title);
});
