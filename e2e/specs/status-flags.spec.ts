import { test, expect } from "@playwright/test";
import {
  cleanUpCreatedStories,
  deleteStory,
  openProject,
  seedToken,
} from "./support";

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
 * Also exercises SH-309: the badge's `blockedFlag()`-derived text. Before
 * SH-309 the badge tested only `st.awaiting`, so a card blocked by an open
 * `blocked-by` or `obviated-by` relationship read "● blocked (no reason)"
 * even though the relationship it was missing already sat one row below it
 * (`.card-blockers`, SH-203). This file now owns every badge-text case
 * (`.flag-blocked`'s rendered sentence); `card-blockers.spec.ts` keeps the
 * cleared-blocker dwell, which SH-309 left in `.card-blockers` alone.
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

/** Records `kind` from the currently-open drawer's story to `otherId`, via
 * the Relationships section's inline-add form -- the same three-locator
 * dance `card-blockers.spec.ts` and `drawer-detail-race.spec.ts` each use
 * inline. Assumes a drawer is already open (`openDrawer()`). */
async function addRelation(
  page: import("@playwright/test").Page,
  kind: string,
  otherId: string,
) {
  await page
    .locator('input[placeholder="Story ID (e.g. SH-2)"]')
    .fill(otherId);
  await page.locator("#drawer-body .inline-add select").selectOption(kind);
  await page
    .locator("#drawer-body .inline-add button", { hasText: "Add" })
    .click();
  await expect(page.locator(".rel-row", { hasText: otherId })).toBeVisible();
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
  // SH-309: an awaiting reason with no other cause now quotes the reason
  // in the parenthetical rather than a bare "● blocked" -- the same rule
  // that fixed the filed bug (SH-307/SH-308) applies here too, since a
  // typed reason is itself a cause the badge used to hide as effectively
  // as a missing one.
  await expect(card.locator(".flag-blocked")).toHaveText(
    '● blocked ("e2e: exercising the blocked flag")',
  );

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

test("a card blocked by another story names the blocker in its badge, hyperlinked (SH-307/SH-308)", async ({
  page,
}) => {
  const blockerTitle = "SH-309 blocked badge — the blocker";
  const workerTitle = "SH-309 blocked badge — the blocked story";
  await createStory(page, blockerTitle);
  const blockerId = (await page
    .locator('.column[data-state="todo"] .card', { hasText: blockerTitle })
    .getAttribute("data-id"))!;
  await createStory(page, workerTitle);

  await openDrawer(page, workerTitle);
  await addRelation(page, "blocked-by", blockerId);
  await page.locator("#drawer-close").click();
  await expect(page.locator("#drawer")).not.toHaveClass(/open/);

  const workerCard = page.locator('.column[data-state="todo"] .card', {
    hasText: workerTitle,
  });
  const badge = workerCard.locator(".flag-blocked");
  await expect(badge).toHaveText("● blocked (" + blockerId + ")");
  // The live blocker moved out of .card-blockers and into the badge
  // (SH-309) -- it must not still be printed twice on the same card.
  await expect(workerCard.locator(".card-blockers")).toHaveCount(0);

  // The one behavior no static assertion reaches: the ref's click opens
  // the BLOCKER's drawer, not the card's own -- storyRef()'s
  // stopPropagation() now sits inside .card-flags, a place it has never
  // sat before, nested inside the whole-card click target from buildCard().
  await badge.locator(".rel-id").click();
  await expect(page.locator("#drawer")).toHaveClass(/open/);
  await expect(page.locator("#drawer-id")).toHaveText(blockerId);
  await page.locator("#drawer-close").click();
  await expect(page.locator("#drawer")).not.toHaveClass(/open/);

  await deleteStory(page, workerTitle);
  await deleteStory(page, blockerTitle);
});

test("a card blocked by two open stories lists both in its badge, comma-joined", async ({
  page,
}) => {
  const blockerATitle = "SH-309 comma blockers — blocker A";
  const blockerBTitle = "SH-309 comma blockers — blocker B";
  const workerTitle = "SH-309 comma blockers — the blocked story";
  await createStory(page, blockerATitle);
  const blockerAId = (await page
    .locator('.column[data-state="todo"] .card', { hasText: blockerATitle })
    .getAttribute("data-id"))!;
  await createStory(page, blockerBTitle);
  const blockerBId = (await page
    .locator('.column[data-state="todo"] .card', { hasText: blockerBTitle })
    .getAttribute("data-id"))!;
  await createStory(page, workerTitle);

  await openDrawer(page, workerTitle);
  await addRelation(page, "blocked-by", blockerAId);
  await addRelation(page, "blocked-by", blockerBId);
  await page.locator("#drawer-close").click();
  await expect(page.locator("#drawer")).not.toHaveClass(/open/);

  const workerCard = page.locator('.column[data-state="todo"] .card', {
    hasText: workerTitle,
  });
  // Exact-string, not "contains both ids": ordering comes from the store's
  // own `ORDER BY relation, other_no` (ascending, numeric), so a blocker
  // created first sorts first -- a dropped or reordered comma is a defect
  // this assertion is the only thing that would catch.
  await expect(workerCard.locator(".flag-blocked")).toHaveText(
    "● blocked (" + blockerAId + ", " + blockerBId + ")",
  );

  await deleteStory(page, workerTitle);
  await deleteStory(page, blockerATitle);
  await deleteStory(page, blockerBTitle);
});

test("a card blocked by both an open story and an awaiting reason names both causes", async ({
  page,
}) => {
  const blockerTitle = "SH-309 both causes — the blocker";
  const workerTitle = "SH-309 both causes — the blocked story";
  await createStory(page, blockerTitle);
  const blockerId = (await page
    .locator('.column[data-state="todo"] .card', { hasText: blockerTitle })
    .getAttribute("data-id"))!;
  await createStory(page, workerTitle);

  await openDrawer(page, workerTitle);
  await addRelation(page, "blocked-by", blockerId);
  await page
    .locator('input[placeholder="Reason for blocking…"]')
    .fill("waiting on legal");
  await page.locator("#drawer-body button", { hasText: "Block" }).click();
  await expect(page.locator(".banner-blocked")).toBeVisible();
  await page.locator("#drawer-close").click();
  await expect(page.locator("#drawer")).not.toHaveClass(/open/);

  // `story block` sets `awaiting` without moving the story's state
  // (`route_block_story`), so the card stays in "todo".
  const workerCard = page.locator('.column[data-state="todo"] .card', {
    hasText: workerTitle,
  });
  await expect(workerCard.locator(".flag-blocked")).toHaveText(
    '● blocked (' + blockerId + ', "waiting on legal")',
  );

  await deleteStory(page, workerTitle);
  await deleteStory(page, blockerTitle);
});

test("a card obviated by another story names it as obviated in the badge", async ({
  page,
}) => {
  const obviatorTitle = "SH-309 obviated badge — the obviator";
  const workerTitle = "SH-309 obviated badge — the obviated story";
  await createStory(page, obviatorTitle);
  const obviatorId = (await page
    .locator('.column[data-state="todo"] .card', { hasText: obviatorTitle })
    .getAttribute("data-id"))!;
  await createStory(page, workerTitle);

  await openDrawer(page, workerTitle);
  await addRelation(page, "obviated-by", obviatorId);
  await page.locator("#drawer-close").click();
  await expect(page.locator("#drawer")).not.toHaveClass(/open/);

  const workerCard = page.locator('.column[data-state="todo"] .card', {
    hasText: workerTitle,
  });
  await expect(workerCard.locator(".flag-blocked")).toHaveText(
    "● blocked (obviated by " + obviatorId + ")",
  );

  await deleteStory(page, workerTitle);
  await deleteStory(page, obviatorTitle);
});
