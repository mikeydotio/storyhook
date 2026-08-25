import { test, expect } from "./support";
import { openProject, projectSlug, requiredEnv, seedToken } from "./support";

/**
 * SH-336's own tiebreak, proven directly rather than inferred from a
 * boundary condition. `board-sort.spec.ts`'s "Modified" test proves the
 * sort reads `updated_at`, not `created_at`, and for that it needs the two
 * to *diverge* — `waitUntilStoreClockPasses` exists to keep them apart.
 * This spec proves the opposite half: what happens when two stories
 * genuinely tie on `updated_at`, which is this tracker's normal workload
 * (agents writing in bursts), not an edge case.
 *
 * The store cannot be told to make two real writes land in the same second
 * on demand — that would be exactly the race SH-245/SH-318/SH-329 already
 * removed from this suite. So the tie is forced in the payload instead:
 * `injectTiedTodoCards` clones a real story-view from the fixture project's
 * own `/data` response (guaranteeing every field the renderer needs is
 * correctly shaped) and pushes two copies back in with a shared
 * `updated_at` and two distinct, real-looking `head_global_seq` values.
 * Nothing is written to the store — this proves the **browser
 * comparator**, not that the daemon emits the field (`web_test.rs::
 * web_serve_api_data_carries_head_global_seq` covers that half).
 *
 * Both directions are asserted, deliberately: a single-direction check
 * cannot tell "reads head_global_seq" apart from "happens to agree with
 * insertion order", and cannot prove the tiebreak follows `dir` at all.
 *
 * This file does not touch "Alpha Project"'s own fixture stories or create
 * any of its own — `route.fulfill` only changes what is served to this
 * page, never the real backend — so there is nothing for
 * `cleanUpCreatedStories` to sweep.
 */

test.beforeEach(async ({ page }) => {
  await seedToken(page);
  await page.goto("/");
});

const OLDER_TITLE = "SH-336 tiebreak test — written first";
const NEWER_TITLE = "SH-336 tiebreak test — written last";
const TIED_AT = "2026-01-01T00:00:00Z";

/**
 * Registered before the navigation that triggers the first `/data` fetch
 * (the same discipline `board-readiness.spec.ts`'s `slowData()` uses),
 * since a route registered after the page has already loaded would never
 * see a request to intercept — nothing here causes the client to refetch on
 * its own once it holds data.
 */
async function injectTiedTodoCards(
  page: import("@playwright/test").Page,
  slug: string,
): Promise<void> {
  await page.route(
    (url) => url.pathname === `/api/repos/${encodeURIComponent(slug)}/data`,
    async (route) => {
      const response = await route.fetch();
      const data: { stories?: Array<Record<string, unknown>> } =
        await response.json();
      const template = (data.stories ?? [])[0];
      if (!template) {
        throw new Error(
          "injectTiedTodoCards: the fixture project has no story to clone from",
        );
      }
      const cards = [
        // Story number is the DISAGREEING id/write-order pair on purpose:
        // OLDER_TITLE gets the *higher* story number and the *lower*
        // head_global_seq (written first), NEWER_TITLE the reverse. If the
        // real tiebreak (head_global_seq) were ever mistakenly bypassed, the
        // comparator's residual `byNumber` fallback would produce the
        // OPPOSITE order from the one asserted below — so this fixture
        // cannot pass by accident the way same-direction ids would.
        { id: "SH-90002", title: OLDER_TITLE, headGlobalSeq: 10 },
        { id: "SH-90001", title: NEWER_TITLE, headGlobalSeq: 20 },
      ];
      for (const card of cards) {
        const clone = JSON.parse(JSON.stringify(template)) as {
          story: Record<string, unknown>;
          head_global_seq?: number;
          is_ready?: boolean;
          is_blocked?: boolean;
        };
        clone.story.id = card.id;
        clone.story.title = card.title;
        clone.story.state = "todo";
        clone.story.superstate = "OPEN";
        clone.story.updated_at = TIED_AT;
        clone.head_global_seq = card.headGlobalSeq;
        clone.is_ready = false;
        clone.is_blocked = false;
        (data.stories ??= []).push(clone);
      }
      await route.fulfill({ response, json: data });
    },
  );
}

function columnSortBtn(page: import("@playwright/test").Page, slug: string) {
  return page.locator(`.column[data-state="${slug}"] .column-sort-btn`);
}

function columnSortMenu(page: import("@playwright/test").Page, slug: string) {
  return page.locator(`.ctxmenu[aria-label="Sort ${slug}"]`);
}

async function selectColumnSort(
  page: import("@playwright/test").Page,
  slug: string,
  label: string,
) {
  await columnSortBtn(page, slug).click();
  const menu = columnSortMenu(page, slug);
  await expect(menu).toBeVisible();
  await menu.locator(".ctxmenu-item", { hasText: label }).click();
  await expect(menu).not.toBeVisible();
}

async function ourColumnTitles(
  page: import("@playwright/test").Page,
  slug: string,
) {
  const titles = page.locator(`.column[data-state="${slug}"] .card .card-title`);
  return (await titles.allTextContents()).filter((t) =>
    t.startsWith("SH-336 tiebreak test"),
  );
}

test('"Modified" breaks a same-second tie by write order, and follows the sort direction', async ({
  page,
  request,
}) => {
  const slug = await projectSlug(request, "Alpha Project");
  await injectTiedTodoCards(page, slug);
  await openProject(page, "Alpha Project");

  await selectColumnSort(page, "todo", "Modified ↓");
  await expect(columnSortBtn(page, "todo")).toHaveAttribute(
    "title",
    "Sort: Modified ↓",
  );
  expect(await ourColumnTitles(page, "todo")).toEqual([
    NEWER_TITLE,
    OLDER_TITLE,
  ]);

  await selectColumnSort(page, "todo", "Modified ↑");
  await expect(columnSortBtn(page, "todo")).toHaveAttribute(
    "title",
    "Sort: Modified ↑",
  );
  expect(await ourColumnTitles(page, "todo")).toEqual([
    OLDER_TITLE,
    NEWER_TITLE,
  ]);
});

test("the List view's Updated column breaks a same-second tie by write order, and follows the sort direction", async ({
  page,
  request,
}) => {
  const slug = await projectSlug(request, "Alpha Project");
  await injectTiedTodoCards(page, slug);
  await openProject(page, "Alpha Project");

  await page.locator('#view-toggle button[data-view="list"]').click();
  await expect(page.locator("#list-view")).toBeVisible();

  // SH-450 puts Order after ID, so `populateListRow` now puts the title in
  // the row's third `<td>` (no class of its own).
  async function orderedTitles(): Promise<string[]> {
    const rows = page
      .locator("tr[data-id]")
      .filter({ hasText: "SH-336 tiebreak test" });
    return rows.locator("td:nth-child(3)").allTextContents();
  }

  const header = page.locator('thead th[data-col="updated"]');
  const arrow = page.locator("#sort-updated");

  // Normalize to descending regardless of whatever direction persisted
  // filters left the Updated column in — clicking twice from either state
  // lands on descending, since a click on the already-active column just
  // flips the sign.
  await header.click();
  await header.click();
  if ((await arrow.textContent()) !== "▼") {
    await header.click();
  }
  await expect(arrow).toHaveText("▼");
  const descending = await orderedTitles();
  expect(descending[0]).toContain(NEWER_TITLE);
  expect(descending[1]).toContain(OLDER_TITLE);

  await header.click();
  await expect(arrow).toHaveText("▲");
  const ascending = await orderedTitles();
  expect(ascending[0]).toContain(OLDER_TITLE);
  expect(ascending[1]).toContain(NEWER_TITLE);
});
