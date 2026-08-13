import { test, expect } from "@playwright/test";
import type { Page, Request } from "@playwright/test";
import {
  cleanUpCreatedStories,
  latch,
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

interface HeldFetch {
  /** Resolves once the daemon has answered the held request -- i.e. once
   * the snapshot the page will eventually receive has been taken. */
  taken: Promise<void>;
  /**
   * Refuses every *later* board fetch, and resolves once the ones already in
   * flight have landed.
   *
   * Without this the spec proves nothing: a mutation is followed by its own
   * fetch and, `FETCH_DEBOUNCE_MS` later, the SSE-driven one, so a board
   * this test un-renders would simply be re-rendered by the next reply to
   * arrive, and a retrying assertion would never see the gap. Refusing them
   * models the production case exactly -- there, the fetches that could
   * repair the board have *already been spent* on the write, and the next
   * one is a safety poll 25 seconds out, far beyond any assertion budget.
   */
  seal: () => Promise<void>;
  /** Delivers the held snapshot to the page, and resolves once the page's
   * own `onreadystatechange` has had its turn on the renderer's task
   * queue. */
  deliver: () => Promise<void>;
}

/**
 * Holds the first board fetch whose snapshot satisfies `until`, and answers
 * every board fetch before it from the daemon as usual.
 *
 * The predicate is what makes the spec deterministic. A mutation produces
 * *two* fetches (its own and the SSE-driven one), and the previous
 * mutation's second fetch can still be to come when this arms -- so "the
 * next fetch" is not a fixed snapshot, and a spec that assumed it was would
 * sometimes hold one taken before the story it is about even exists. The
 * test says which snapshot it needs instead, and the harness waits for it.
 *
 * `route.fetch()` rather than a delayed `route.continue()`: the point is a
 * snapshot *taken* before a later write and *delivered* after it, and
 * `continue()` would send the request only once released, by which time the
 * daemon would answer with the write already in it -- a test that exercises
 * nothing (`latch()`'s own doc comment names the same failure shape).
 */
async function holdBoardFetch(
  page: Page,
  until: (snapshot: BoardSnapshot) => boolean,
  options?: { sealOnHold?: boolean },
): Promise<HeldFetch> {
  const taken = latch();
  const gate = latch();
  const isBoardFetch = (request: Request) => /\/data$/.test(request.url());
  let heldRequest: Request | null = null;
  let sealed = false;

  // Board fetches still in flight. `seal()` drops the held one (which has no
  // reply until `deliver()`) and waits for the rest to empty, because a reply
  // already on the wire when the seal goes up would repair the board just as
  // effectively as one issued after it.
  const outstanding = new Set<Request>();
  page.on("request", (request) => {
    if (isBoardFetch(request)) outstanding.add(request);
  });
  const settled = (request: Request) => outstanding.delete(request);
  page.on("requestfinished", settled);
  page.on("requestfailed", settled);

  await page.route(/\/data$/, async (route) => {
    if (heldRequest) {
      // Refused, not held, once sealed: an aborted fetch leaves `state.data`
      // untouched (`fetchData`'s own `onerror` sets the connection flag and
      // nothing else), which is precisely a board with no repair on the way.
      if (sealed) await route.abort();
      else await route.continue();
      return;
    }
    // The bearer token by header, because the page's own credential is the
    // `HttpOnly` cookie (SH-255) and `route.fetch()` does not carry the
    // browser context's jar -- it answers 401, whose body the page then
    // fails to parse, so the snapshot is never applied and the test passes
    // having proved nothing. That vacuous pass is what this closes; the
    // daemon accepts either channel (`src/api/admission.rs`), and the bytes
    // are still the daemon's own.
    const response = await route.fetch({
      headers: {
        ...route.request().headers(),
        "X-Storyhook-Token": requiredEnv("DASHBOARD_TOKEN"),
      },
    });
    if (!response.ok()) {
      throw new Error(
        `holdBoardFetch: the daemon answered ${response.status()} for a ` +
          "snapshot this spec depends on — it would prove nothing",
      );
    }
    if (!until((await response.json()) as BoardSnapshot)) {
      await route.fulfill({ response });
      return;
    }
    heldRequest = route.request();
    // Sealing here rather than from the test body closes a window the test
    // body cannot: a reply that lands between the hold and a later `seal()`
    // is applied, and the held snapshot is then older than it -- so the
    // *ordering* half of the guard would answer, and a spec meant to
    // exercise the other half would pass without ever reaching it.
    if (options && options.sealOnHold) sealed = true;
    taken.release();
    await gate.held;
    await route.fulfill({ response });
  });

  return {
    taken: taken.held,
    seal: async () => {
      sealed = true;
      if (heldRequest) outstanding.delete(heldRequest);
      await expect.poll(() => outstanding.size).toBe(0);
    },
    deliver: async () => {
      // Identity, not the URL: every board fetch in this page shares one
      // URL, so a URL match could be satisfied by an unrelated reply that
      // happened to be in flight, and the assertion below would then read
      // the DOM before the held body ever reached it.
      const arrived = page.waitForResponse((r) => r.request() === heldRequest);
      gate.release();
      await arrived;
      // The XHR's completion task was queued on the renderer before this
      // `evaluate` could be, and a renderer runs its task queue in order, so
      // a macrotask that resolves here is proof the page has already done
      // whatever it intends to do with that body.
      await page.evaluate(() => new Promise((r) => setTimeout(r, 0)));
    },
  };
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
