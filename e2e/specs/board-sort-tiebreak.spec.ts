import { installBoardFixture } from "../board-fixture";
import { test, expect } from "./support";
import { openProject, projectSlug, seedToken } from "./support";

/** SH-336 transport/rendering contract after SH-894: sort choices must reach
 * the server and the browser must preserve its returned same-second ordering.
 * The explicit fixture orders below do not prove a comparator. The equivalent
 * disagreeing head_global_seq/story-number cases are covered by board API Rust
 * comparator tests, where ordering now happens before pagination. */

test.beforeEach(async ({ page }) => {
  await seedToken(page);
  await page.goto("/");
});

const OLDER_TITLE = "SH-336 tiebreak test — written first";
const NEWER_TITLE = "SH-336 tiebreak test — written last";
const TIED_AT = "2026-01-01T00:00:00Z";

/** Load a real template before routing: metadata and empty columns cannot
 * provide the first story that the old exhaustive response guaranteed. */
async function injectTiedTodoCards(
  page: import("@playwright/test").Page,
  slug: string,
): Promise<void> {
  await installBoardFixture(page, slug, async (data, options) => {
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
      // authoritative backend tiebreak were bypassed, its numeric fallback
      // would produce the opposite order. Rust pins that computation; these
      // same identities pin preservation of its wire order in the browser.
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
    if (options.sort === "updated") {
      const expected = options.dir === 1 ? ["SH-90002", "SH-90001"] : ["SH-90001", "SH-90002"];
      const injected = new Map(data.stories.filter((view: any) => expected.includes(view.story.id)).map((view: any) => [view.story.id, view]));
      data.stories = data.stories.filter((view: any) => !expected.includes(view.story.id)).concat(expected.map(id => injected.get(id)));
    }
  });
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

test('"Modified" requests and renders both server tiebreak directions', async ({
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
  await expect.poll(() => ourColumnTitles(page, "todo")).toEqual([
    NEWER_TITLE,
    OLDER_TITLE,
  ]);

  await selectColumnSort(page, "todo", "Modified ↑");
  await expect(columnSortBtn(page, "todo")).toHaveAttribute(
    "title",
    "Sort: Modified ↑",
  );
  await expect.poll(() => ourColumnTitles(page, "todo")).toEqual([
    OLDER_TITLE,
    NEWER_TITLE,
  ]);
});

test("the List view requests and renders both server Updated tiebreak directions", async ({
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
  await expect.poll(orderedTitles).toEqual([NEWER_TITLE, OLDER_TITLE]);
  const descending = await orderedTitles();
  expect(descending[0]).toContain(NEWER_TITLE);
  expect(descending[1]).toContain(OLDER_TITLE);

  await header.click();
  await expect(arrow).toHaveText("▲");
  await expect.poll(orderedTitles).toEqual([OLDER_TITLE, NEWER_TITLE]);
  const ascending = await orderedTitles();
  expect(ascending[0]).toContain(OLDER_TITLE);
  expect(ascending[1]).toContain(NEWER_TITLE);
});
