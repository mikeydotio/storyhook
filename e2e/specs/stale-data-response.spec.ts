import { test, expect } from "@playwright/test";
import type { Page } from "@playwright/test";
import type { HeldFetch } from "./support";
import {
  cleanUpCreatedStories,
  holdFetch,
  openProject,
  projectSlug,
  requiredEnv,
  seedToken,
} from "./support";

/**
 * SH-281: board snapshots are applied in the order they *arrive*, and the
 * dashboard has several in flight at once -- one per mutation
 * (`handleMutationSuccess`), one per SSE `repo-changed`
 * (`scheduleDataFetch`), one every 25s (`pollTick`). The daemon answers each
 * on whichever of its eight dispatcher threads is free (`src/daemon/serve.rs`),
 * so on a loaded machine a request issued *before* a write can be answered
 * *after* one issued behind it, and the older snapshot lands last.
 *
 * Nothing then re-renders for up to `SAFETY_POLL_INTERVAL_MS`: the SSE event
 * for that write has already been spent, so the board sits showing a state
 * the user has already been told is gone, for 25 seconds. That is what
 * `card-blockers.spec.ts` caught twice inside `make test` -- a blocked card
 * with no blockers row at all -- and never once in `make e2e`, which ran on a
 * quieter machine.
 *
 * These specs put that ordering where the test decides rather than where the
 * machine's load happens to put it. Nothing is faked: `holdBoardFetch` takes
 * the real snapshot from the real daemon at the moment the page asked for
 * it, and delivers those same bytes later.
 */

cleanUpCreatedStories("Alpha Project");

test.beforeEach(async ({ page }) => {
  await seedToken(page);
  await page.goto("/");
  await openProject(page, "Alpha Project");
});

/** One story as `GET .../data` reports it (`project_data_json`). */
interface BoardSnapshot {
  stories: Array<{ story: { id: string; title: string } }>;
}

/**
 * Holds the first board fetch whose snapshot satisfies `until`, and answers
 * every board fetch before it from the daemon as usual.
 *
 * `GET .../data` is the board's own endpoint and nothing else's, so matching
 * on the path alone is enough to name it. Everything else this needs -- why
 * the predicate rather than "the next fetch", why `route.fetch()` rather than
 * a delayed `continue()`, and what `seal()`/`deliver()` are for -- is
 * `holdFetch`'s own contract (`support.ts`), shared with every other spec
 * about a reply that arrives too late.
 */
function holdBoardFetch(
  page: Page,
  until: (snapshot: BoardSnapshot) => boolean,
  options?: { sealOnHold?: boolean },
): Promise<HeldFetch> {
  return holdFetch<BoardSnapshot>(
    page,
    (url) => /\/data$/.test(url.pathname),
    until,
    options,
  );
}

/** True once `title` is among the snapshot's stories. */
const holds = (title: string) => (snapshot: BoardSnapshot) =>
  snapshot.stories.some((view) => view.story.title === title);

async function createStory(page: Page, title: string) {
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

test("a board snapshot taken before a relation must not un-render it when it arrives late", async ({
  page,
}) => {
  const blockerTitle = "SH-281 stale snapshot — the blocker";
  const workerTitle = "SH-281 stale snapshot — the blocked story";
  const blockerCard = await createStory(page, blockerTitle);
  const blockerId = (await blockerCard.getAttribute("data-id"))!;

  // Both stories, and no relation between them: exactly the board this test
  // is about not reverting to.
  const stale = await holdBoardFetch(page, holds(workerTitle));
  await createStory(page, workerTitle);
  await stale.taken;

  await page.locator(".card", { hasText: workerTitle }).click();
  await expect(page.locator("#drawer")).toHaveClass(/open/);
  await page
    .locator('input[placeholder="Story ID (e.g. SH-2)"]')
    .fill(blockerId);
  await page
    .locator("#drawer-body .inline-add select")
    .selectOption("blocked-by");
  await page
    .locator("#drawer-body .inline-add button", { hasText: "Add" })
    .click();
  await expect(page.locator(".rel-row", { hasText: blockerId })).toBeVisible();
  await page.locator("#drawer-close").click();
  await expect(page.locator("#drawer")).not.toHaveClass(/open/);

  const workerCard = page.locator(".card", { hasText: workerTitle });
  const blockersRow = workerCard.locator(".card-blockers");
  await expect(blockersRow.locator(".rel-id")).toHaveText(blockerId);

  await stale.seal();
  await stale.deliver();

  // The card itself is unaffected by the older snapshot -- it exists in
  // both -- so `populateCard` rebuilds it in place, synchronously, with
  // whatever relationships the applied snapshot carries. No exit animation
  // stands between this assertion and the truth.
  await expect(blockersRow.locator(".rel-id")).toHaveText(blockerId);
});

test("a board snapshot in flight when a write lands must not un-render that write, with no newer reply to repair it", async ({
  page,
  request,
}) => {
  const blockerTitle = "SH-281 in-flight snapshot — the blocker";
  const workerTitle = "SH-281 in-flight snapshot — the blocked story";
  const blockerCard = await createStory(page, blockerTitle);
  const blockerId = (await blockerCard.getAttribute("data-id"))!;
  await createStory(page, workerTitle);

  // The difference between this spec and the first one is which half of the
  // guard can possibly answer. There, replies carrying the relation had
  // already been applied when the older snapshot arrived, so *ordering*
  // answered. Here the held snapshot is the newest request anyone has made:
  // it is issued after every reply the board has applied, and `sealOnHold`
  // stops anything landing behind it. Ordering has nothing to say. The only
  // reason it must not be applied is that a write happened while it was in
  // flight — which is the shape the failure actually takes on a loaded
  // machine. No overtaking required: just a request already in flight when
  // the user acted, and a confirmation slow enough to lose the race to it.
  const stale = await holdBoardFetch(page, holds(workerTitle), {
    sealOnHold: true,
  });
  // A write from outside this tab, purely to make the page ask again: the
  // board fetches on SSE `repo-changed`, and nothing else would issue a
  // request at a moment this test controls.
  const slug = await projectSlug(request, "Alpha Project");
  const nudge = await request.post(`/api/repos/${slug}/story`, {
    headers: {
      "X-Storyhook": "1",
      "X-Storyhook-Token": requiredEnv("DASHBOARD_TOKEN"),
      "Content-Type": "application/json",
    },
    data: { title: "SH-281 in-flight snapshot — the nudge" },
  });
  expect(nudge.ok()).toBe(true);
  await stale.taken;
  await stale.seal();

  await page.locator(".card", { hasText: workerTitle }).click();
  await expect(page.locator("#drawer")).toHaveClass(/open/);
  await page
    .locator('input[placeholder="Story ID (e.g. SH-2)"]')
    .fill(blockerId);
  await page
    .locator("#drawer-body .inline-add select")
    .selectOption("blocked-by");
  await page
    .locator("#drawer-body .inline-add button", { hasText: "Add" })
    .click();
  await expect(page.locator(".rel-row", { hasText: blockerId })).toBeVisible();
  await page.locator("#drawer-close").click();
  await expect(page.locator("#drawer")).not.toHaveClass(/open/);

  const workerCard = page.locator(".card", { hasText: workerTitle });
  const blockersRow = workerCard.locator(".card-blockers");
  await expect(blockersRow.locator(".rel-id")).toHaveText(blockerId);

  await stale.deliver();

  await expect(blockersRow.locator(".rel-id")).toHaveText(blockerId);
});

test("a board snapshot taken before a story existed must not un-render that story when it arrives late", async ({
  page,
}) => {
  const seedTitle = "SH-281 stale snapshot — the seed";
  const lateTitle = "SH-281 stale snapshot — the vanishing story";

  const stale = await holdBoardFetch(page, holds(seedTitle));
  await createStory(page, seedTitle);
  await stale.taken;

  await createStory(page, lateTitle);
  const populated = await page.locator("#filter-count").textContent();

  await stale.seal();
  await stale.deliver();

  // `#filter-count`, not the card: a card the applied snapshot no longer
  // contains is *animated* out (`.card.exiting`, 0.2s, with the real
  // `.remove()` deferred to `animationend`), so for a fifth of a second it
  // is still in the DOM and still visible -- a lagging indicator that would
  // let this assertion pass on a board already showing the wrong thing.
  // `renderView` writes this count straight from `state.data` in the same
  // turn the snapshot is applied.
  await expect(page.locator("#filter-count")).toHaveText(populated!);
  await expect(page.locator(".card", { hasText: lateTitle })).toBeVisible();
});

test("a board snapshot for the project the user has left must not be painted onto the one they opened", async ({
  page,
  request,
}) => {
  const markerTitle = "SH-281 stale snapshot — the wrong project's story";

  // Ordering alone does not answer this one. A reply for the project the
  // user left always carries a lower ticket than the reply for the project
  // they opened -- `apiBase()` reads `state.repoId` when the request is
  // *issued*, so no later request can be for the older project. It is the
  // reply for the newer project never being applied at all (it timed out,
  // it 500'd, or -- as here -- it never arrived) that leaves the older one
  // looking like the newest thing anybody has seen. Hence `sealOnHold`:
  // after this point nothing lands but the held snapshot itself.
  const stale = await holdBoardFetch(page, holds(markerTitle), {
    sealOnHold: true,
  });

  // Through the API rather than the create modal, because the modal's own
  // confirmation is a card, and a card needs a board fetch to land -- which
  // is precisely what this test has sealed off.
  const slug = await projectSlug(request, "Alpha Project");
  const created = await request.post(`/api/repos/${slug}/story`, {
    headers: {
      "X-Storyhook": "1",
      "X-Storyhook-Token": requiredEnv("DASHBOARD_TOKEN"),
      "Content-Type": "application/json",
    },
    data: { title: markerTitle },
  });
  expect(created.ok()).toBe(true);
  await stale.taken;
  await stale.seal();

  await page.locator("#projsel-btn").click();
  await page
    .locator("#projsel-menu .projsel-item", { hasText: "Beta Project" })
    .click();
  await expect(page.locator("#projsel-btn")).toContainText("BB · Beta Project");

  await stale.deliver();

  // No exit animation stands in the way this time: Alpha's stories would be
  // *entering* Beta's board, and an entering card is in the DOM from the
  // first frame.
  await expect(page.locator(".card", { hasText: markerTitle })).toHaveCount(0);
  await expect(page.locator("#projsel-btn")).toContainText("BB · Beta Project");
});
