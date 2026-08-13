import { test, expect } from "@playwright/test";
import {
  cleanUpCreatedStories,
  deleteStory,
  openProject,
  projectSlug,
  requiredEnv,
  seedToken,
} from "./support";

/**
 * Exercises SH-197's roving tabindex: before this, board cards were plain
 * `<div>`s and list rows plain `<tr>`s with a click handler and no
 * `tabindex` at all -- Tab skipped every story, and since `contextmenu`
 * (Shift+F10 / the Menu key) only ever fires on a focused element, the
 * context menu the rest of this story adds would otherwise be unreachable
 * from the keyboard. `syncRoving`/`bindRovingKeys` (`src/web_dashboard.html`)
 * keep exactly one card (or row) at `tabIndex=0` at a time -- the WAI-ARIA
 * grid pattern -- with arrow keys moving it and Enter/Space activating it.
 *
 * This spec creates and deletes its own stories rather than touching the
 * "Alpha Project" fixture, whose exact two-story shape other specs
 * (filter-persistence.spec.ts, column-visibility.spec.ts) assert on
 * byte-for-byte per run-e2e.sh's own comment.
 */

cleanUpCreatedStories("Alpha Project");

const DASHBOARD_TOKEN = requiredEnv("DASHBOARD_TOKEN");

test.beforeEach(async ({ page }) => {
  await seedToken(page);
  await page.goto("/");
  await openProject(page, "Alpha Project");
});

/** `priority` defaults to unset. Alpha's own seeded stories (ready, default
 * priority) sit in "todo" alongside anything this spec creates there, so a
 * test that cares which card sorts *first* under the board's default
 * priority-descending sort needs to ask for "critical" explicitly. */
async function createStory(
  page: import("@playwright/test").Page,
  title: string,
  priority?: string,
) {
  await page.locator("#new-story-btn").click();
  await expect(page.locator("#create-modal")).toHaveClass(/open/);
  await page.locator("#create-title").fill(title);
  if (priority) await page.locator("#create-priority").selectOption(priority);
  await page.locator("#create-submit").click();
  await expect(page.locator("#create-modal")).not.toHaveClass(/open/);
  const card = page.locator('.column[data-state="todo"] .card', {
    hasText: title,
  });
  await expect(card).toBeVisible();
  return card;
}

/** Moves `title`'s story to `stateSlug` via the drawer's own State select
 * (the first `<select>` in `#drawer-body`'s field grid) rather than
 * drag-and-drop, which Playwright can't simulate as an HTML5 native drag.
 * Finds the card by title alone, not scoped to any one column, since this
 * is also used to move a story back out of wherever an earlier call left it. */
async function moveToState(
  page: import("@playwright/test").Page,
  title: string,
  stateSlug: string,
) {
  await page.locator(".card", { hasText: title }).click();
  await expect(page.locator("#drawer")).toHaveClass(/open/);
  await page.locator("#drawer-body select").first().selectOption(stateSlug);
  await expect(
    page.locator('.column[data-state="' + stateSlug + '"] .card', {
      hasText: title,
    }),
  ).toBeVisible();
  await page.locator("#drawer-close").click();
  await expect(page.locator("#drawer")).not.toHaveClass(/open/);
}

test("exactly one card holds the roving tabindex, and it is keyboard-focusable", async ({
  page,
}) => {
  // Not a blind Tab press from wherever focus happens to land after the
  // create modal closes -- the DOM's tab order between here and the first
  // card also crosses the topbar and filter bar, none of which is this
  // story's concern. What SH-197 actually promises is the roving-tabindex
  // invariant itself: exactly one card is ever a Tab stop, and it can
  // receive real focus -- checked directly instead.
  const a = "SH-197 keyboard — Tab reaches one card A";
  const b = "SH-197 keyboard — Tab reaches one card B";
  await createStory(page, a);
  await createStory(page, b);

  // Holds regardless of how many other stories are on the board (Alpha's
  // own seeded fixtures included) -- the rest are reachable by arrow key,
  // never by Tab.
  await expect(page.locator('.card[tabindex="0"]')).toHaveCount(1);

  await page.locator('.card[tabindex="0"]').focus();
  await expect(page.locator('.card[tabindex="0"]')).toBeFocused();

  await deleteStory(page, a);
  await deleteStory(page, b);
});

test("arrow keys move focus within a column and across columns, Enter opens the drawer", async ({
  page,
}) => {
  const leftTitle = "SH-197 keyboard — column left";
  const rightTitle = "SH-197 keyboard — column right";
  // "critical" so leftCard sorts first in "todo" under the board's default
  // priority-descending sort regardless of Alpha's own ambient seeded
  // stories -- ArrowLeft below clamps to the SAME INDEX in the neighbouring
  // column (per boardRovingMove's own spec), not "the same card", so index
  // 0 has to unambiguously mean leftCard for this test to be deterministic.
  const leftCard = await createStory(page, leftTitle, "critical");
  await createStory(page, rightTitle);
  await moveToState(page, rightTitle, "in-progress");

  await leftCard.focus();
  await expect(leftCard).toBeFocused();

  const rightCard = page.locator('.column[data-state="in-progress"] .card', {
    hasText: rightTitle,
  });
  await page.keyboard.press("ArrowRight");
  await expect(rightCard).toBeFocused();

  await page.keyboard.press("ArrowLeft");
  await expect(leftCard).toBeFocused();

  await page.keyboard.press("Enter");
  await expect(page.locator("#drawer")).toHaveClass(/open/);
  await expect(page.locator("#drawer-id")).toHaveText(/./);

  await page.keyboard.press("Escape");
  await expect(page.locator("#drawer")).not.toHaveClass(/open/);

  // deleteStory only looks in "todo" -- rightTitle's story is still in
  // "in-progress" from moveToState above.
  await moveToState(page, rightTitle, "todo");
  await deleteStory(page, leftTitle);
  await deleteStory(page, rightTitle);
});

test("the roving tab stop survives a live update pushed by another client", async ({
  page,
  request,
}) => {
  const title = "SH-197 keyboard — survives a live update";
  const card = await createStory(page, title);
  const id = await card.getAttribute("data-id");
  expect(id).toBeTruthy();
  const slug = await projectSlug(request, "Alpha Project");

  await card.focus();
  await expect(card).toBeFocused();
  const styleBefore = await card.getAttribute("style");

  // A mutation from outside this tab -- not a click in the page -- so the
  // resulting re-render is a genuine SSE-pushed `renderBoard()`, the exact
  // path `syncRoving`'s `focusedId` parameter exists for.
  const resp = await request.post(`/api/repos/${slug}/story/${id}/priority`, {
    headers: {
      "X-Storyhook": "1",
      "X-Storyhook-Token": DASHBOARD_TOKEN,
      "Content-Type": "application/json",
    },
    data: { priority: "high" },
  });
  expect(resp.ok()).toBe(true);

  // `--card-accent` changes with priority -- proof a real re-render landed
  // before checking that focus survived it, not that nothing happened.
  await expect.poll(() => card.getAttribute("style")).not.toBe(styleBefore);

  await expect(card).toBeFocused();
  await expect(card).toHaveAttribute("tabindex", "0");

  await deleteStory(page, title);
});

test("a card removed by another client moves the roving stop to a surviving neighbor, not <body>", async ({
  page,
  request,
}) => {
  // Deliberately an API call from outside the page, not the on-page delete
  // modal: clicking through that modal's own Cancel/Confirm buttons moves
  // real DOM focus onto them well before the card is actually removed, so
  // it would no longer be the focused element by removal time -- exercising
  // nothing. The reclaim this test is for is the multi-client case: a card
  // that still holds this tab's focus disappearing for a reason that never
  // touched this tab's own focus at all, e.g. another tab deleting it.
  const first = "SH-197 keyboard — external delete, survivor A";
  const second = "SH-197 keyboard — external delete, survivor B";
  const firstCard = await createStory(page, first);
  // "critical" so `second` deterministically sorts first among survivors
  // once `first` is gone -- the fallback `syncRoving` picks (`nodes[0]`)
  // is "whichever card sorts first", not "the other card this test made",
  // and Alpha's own ambient seeded stories would otherwise win that sort.
  const secondCard = await createStory(page, second, "critical");
  const id = await firstCard.getAttribute("data-id");
  const slug = await projectSlug(request, "Alpha Project");

  await firstCard.focus();
  await expect(firstCard).toBeFocused();

  const resp = await request.delete(`/api/repos/${slug}/story/${id}`, {
    headers: {
      "X-Storyhook": "1",
      "X-Storyhook-Token": DASHBOARD_TOKEN,
      "Content-Type": "application/json",
    },
    data: { reason: "e2e: external delete while focused" },
  });
  expect(resp.ok()).toBe(true);

  await expect(firstCard).not.toBeVisible();
  await expect(secondCard).toHaveAttribute("tabindex", "0");
  await expect(secondCard).toBeFocused();

  await deleteStory(page, second);
});

test("List view: Up/Down move focus between rows, Enter opens the drawer", async ({
  page,
}) => {
  const first = "SH-197 keyboard — list row A";
  const second = "SH-197 keyboard — list row B";
  await createStory(page, first);
  await createStory(page, second);

  await page.locator('#view-toggle button[data-view="list"]').click();
  await expect(page.locator("#list-view")).toBeVisible();

  // Row order is NOT assumed to match creation order: List view's default
  // sort is "updated" descending, not creation order, so whether `first`
  // or `second` (or an ambient Alpha row) lands on top isn't this test's
  // concern -- only that Up/Down move the roving stop between whichever
  // rows are actually physically adjacent.
  const rows = page.locator("#list-body tr[data-id]");
  const rowCount = await rows.count();
  expect(rowCount).toBeGreaterThanOrEqual(2);
  const topRow = rows.nth(0);
  const nextRow = rows.nth(1);

  await topRow.focus();
  await expect(topRow).toBeFocused();
  await page.keyboard.press("ArrowDown");
  await expect(nextRow).toBeFocused();
  await page.keyboard.press("ArrowUp");
  await expect(topRow).toBeFocused();

  await page.keyboard.press("Enter");
  await expect(page.locator("#drawer")).toHaveClass(/open/);
  await page.locator("#drawer-close").click();
  await expect(page.locator("#drawer")).not.toHaveClass(/open/);

  await page.locator('#view-toggle button[data-view="board"]').click();
  await deleteStory(page, first);
  await deleteStory(page, second);
});
