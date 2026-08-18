import { test, expect } from "./support";
import {
  cleanUpCreatedStories,
  openProject,
  projectSlug,
  requiredEnv,
  resolvedTokenColor,
  seedToken,
} from "./support";

/**
 * Exercises SH-203 consumer 2's cleared-blocker dwell: when a blocker
 * closes, its light turns green (stateColor()'s CLOSED anchor -- see
 * story-status-light.spec.ts for the semantic-vs-positional palette proof)
 * and the entry dwells on the card for a few seconds before being removed
 * -- the point of the dwell being that the reader sees *which* blocker
 * cleared, not merely that the list got shorter.
 *
 * SH-309 moved the *live* half of "every OPEN story still blocking this
 * one" out of `.card-blockers` and into the blocked badge
 * (`.flag-blocked`, `status-flags.spec.ts`'s own file) -- the two used to
 * print the same blocking story's id on the same card twice. This file's
 * scope narrowed to match: it now owns only the dwell, the moment after a
 * blocker closes where the badge has already vanished (the story left
 * `blocked_ids` the instant its last blocker cleared) but the cleared
 * entry still needs somewhere to keep being visible.
 *
 * The blocker is closed through the API, not a UI click in this page --
 * the same reasoning card-keyboard.spec.ts's "survives a live update"
 * test gives: the resulting re-render has to be a genuine SSE-pushed one,
 * the exact path the cleared-blocker ledger (recordClearedBlockers(),
 * populateCard()) exists for, not an optimistic local update that would
 * exercise a completely different code path.
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

async function createStory(
  page: import("@playwright/test").Page,
  title: string,
) {
  await page.locator("#new-story-btn").click();
  await expect(page.locator("#create-modal")).toHaveClass(/open/);
  await page.locator("#create-title").fill(title);
  await page.locator("#create-submit").click();
  await expect(page.locator("#create-modal")).not.toHaveClass(/open/);
  const card = page.locator('.column[data-state="todo"] .card', {
    hasText: title,
  });
  await expect(card).toBeVisible();
  return card;
}

/** Deletes `title`'s "todo"-column story through the drawer, the same shape
 * every other spec's local `deleteStory` uses -- support.ts's own shared
 * helper. Not used for the blocker: a CLOSED story is already `archived`
 * at the store row level (`StoryClosedAndArchived` -- distinct from the
 * dashboard's own `hidden_at`/"Archive" action), so `DELETE` answers 404
 * for one until it's reopened first. `cleanUpCreatedStories`'s own
 * afterEach already does exactly that reopen-then-delete dance for any
 * stray it finds (its own doc comment names the same 404), so the blocker
 * -- left CLOSED on purpose, to prove the dwell -- is swept there rather
 * than duplicating that dance here. */
async function deleteStory(
  page: import("@playwright/test").Page,
  title: string,
) {
  const card = page.locator('.column[data-state="todo"] .card', {
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

test("a card names its open blocker in the badge; closing the blocker turns the badge into a green dwell chip, then drops it", async ({
  page,
  request,
}) => {
  const blockerTitle = "SH-203 card blockers — the blocker";
  const workerTitle = "SH-203 card blockers — the blocked story";
  const blockerCard = await createStory(page, blockerTitle);
  const blockerId = (await blockerCard.getAttribute("data-id"))!;
  await createStory(page, workerTitle);

  // blocked-by, from the worker's own drawer -- openBlockers() reads the
  // worker's OWN relationships, so the edge has to be recorded in this
  // direction (RelationService writes the inverse "blocks" onto the
  // blocker automatically; either direction of `/relate` call would do,
  // this one just matches which story's drawer is already open).
  await page.locator(".card", { hasText: workerTitle }).click();
  await expect(page.locator("#drawer")).toHaveClass(/open/);
  await page.locator('input[placeholder="Story ID (e.g. SH-2)"]').fill(blockerId);
  await page.locator("#drawer-body .inline-add select").selectOption("blocked-by");
  await page
    .locator("#drawer-body .inline-add button", { hasText: "Add" })
    .click();
  const relRow = page.locator(".rel-row", { hasText: blockerId });
  await expect(relRow).toBeVisible();
  // The *kind*, not merely that some row names the blocker. A drawer
  // rebuild used to reset this form's relation select to its first option
  // (SH-281), and a row asserting only "AA-13 appears here" let a
  // `relates-to` edge through to be discovered five lines below as a card
  // mysteriously missing its blockers row — a failure that names neither
  // the control that changed nor the write that went wrong.
  await expect(relRow.locator(".rel-kind")).toHaveText("blocked-by");
  await page.locator("#drawer-close").click();
  await expect(page.locator("#drawer")).not.toHaveClass(/open/);

  const workerCard = page.locator(".card", { hasText: workerTitle });
  // SH-309: the live blocker is named in the badge now, not a separate
  // .card-blockers row (status-flags.spec.ts owns the badge's text in
  // full; this spec only needs enough of it to set up the dwell below).
  const badge = workerCard.locator(".flag-blocked");
  await expect(badge).toHaveText("● blocked (" + blockerId + ")");
  await expect(workerCard.locator(".card-blockers")).toHaveCount(0);
  // The blocker is still in "todo" -- an OPEN, non-active, non-blocked
  // state, stateColor()'s deliberately quiet default.
  await expect(badge.locator(".story-light")).toHaveCSS(
    "background-color",
    await resolvedTokenColor(page, "--fg-faint"),
  );

  // Closes the blocker from outside this tab, forcing a genuine SSE-pushed
  // re-render rather than an optimistic local one -- moveStory()'s own
  // request shape (POST .../move, { state: targetSlug }).
  const slug = await projectSlug(request, "Alpha Project");
  const resp = await request.post(
    `/api/repos/${slug}/story/${blockerId}/move`,
    {
      headers: {
        "X-Storyhook": "1",
        "X-Storyhook-Token": DASHBOARD_TOKEN,
        "Content-Type": "application/json",
      },
      data: { state: "done" },
    },
  );
  expect(resp.ok()).toBe(true);

  // The handoff SH-309 introduces: the worker has no other cause, so the
  // instant its last blocker clears it leaves blocked_ids and the badge
  // disappears entirely -- while .card-blockers, which now exists purely
  // to keep the cleared entry visible for its dwell, appears in the same
  // render carrying it, still lit and marked cleared. Still lit, still
  // green -- the dwell's whole point is that the reader sees *this*
  // blocker turn green, not merely that the badge vanished.
  await expect(workerCard.locator(".flag-blocked")).toHaveCount(0, {
    timeout: 8000,
  });
  const blockersRow = workerCard.locator(".card-blockers");
  await expect(blockersRow.locator(".rel-id")).toHaveText(blockerId);
  await expect(blockersRow.locator(".story-ref.blocker-cleared")).toHaveCount(
    1,
  );
  await expect(blockersRow.locator(".story-light")).toHaveCSS(
    "background-color",
    await resolvedTokenColor(page, "--success"),
  );

  // After the dwell (BLOCKER_CLEARED_DWELL_MS, 4s) the whole blockers row
  // is gone -- populateCard() only appends .card-blockers at all when
  // there's still a dwelling entry to show.
  await expect(workerCard.locator(".card-blockers")).toHaveCount(0, {
    timeout: 8000,
  });

  // The blocker is left CLOSED -- cleanUpCreatedStories' afterEach reopens
  // then deletes it, per this file's own deleteStory() comment.
  await deleteStory(page, workerTitle);
});
