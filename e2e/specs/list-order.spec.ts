import { installBoardFixture } from "../board-fixture";
import { test, expect } from "./support";
import { openProject, projectSlug, seedToken } from "./support";

/** SH-450's browser transport/display half. The server owns dependency
 * ordering and unranked-last comparison before pagination; Rust tests pin that
 * computation. This explicit ordered fixture checks both requested directions,
 * unchanged returned order, and one-based global rank labels. */

test.beforeEach(async ({ page }) => {
  await seedToken(page);
  await page.goto("/");
});

const FIRST = "SH-450 list order — first";
const SECOND = "SH-450 list order — second";
const UNRANKED = "SH-450 list order — unranked";

async function injectExecutionOrder(
  page: import("@playwright/test").Page,
  slug: string,
): Promise<void> {
  await installBoardFixture(page, slug, async (data, options) => {
    const template = (data.stories ?? [])[0];
    if (!template) {
      throw new Error("injectExecutionOrder: fixture has no story to clone");
    }
    const rows = [
      { id: "SH-90003", title: UNRANKED },
      { id: "SH-90002", title: SECOND },
      { id: "SH-90001", title: FIRST },
    ];
    for (const injected of rows) {
      const clone = JSON.parse(JSON.stringify(template)) as {
        story: Record<string, unknown>;
        is_ready?: boolean;
        is_blocked?: boolean;
      };
      clone.story.id = injected.id;
      clone.story.title = injected.title;
      clone.story.state = "todo";
      clone.story.superstate = "OPEN";
      clone.is_ready = injected.id !== "SH-90003";
      clone.is_blocked = false;
      (data.stories ??= []).push(clone);
    }
    data.next_ids = ["SH-90001", "SH-90002"];
    if (options.sort === "next") {
      const expected = options.dir === 1 ? ["SH-90001", "SH-90002", "SH-90003"] : ["SH-90002", "SH-90001", "SH-90003"];
      const injected = new Map(data.stories.filter((view: any) => expected.includes(view.story.id)).map((view: any) => [view.story.id, view]));
      data.stories = expected.map(id => injected.get(id)).concat(data.stories.filter((view: any) => !expected.includes(view.story.id)));
    }
  });
}

test("List shows one-based execution ranks and keeps unranked rows last in both sort directions", async ({
  page,
  request,
}) => {
  const slug = await projectSlug(request, "Alpha Project");
  await injectExecutionOrder(page, slug);
  await openProject(page, "Alpha Project");
  await page.locator('#view-toggle button[data-view="list"]').click();
  await expect(page.locator("#list-view")).toBeVisible();

  const row = (title: string) => page.locator("tr[data-id]", { hasText: title });
  await expect(row(FIRST).locator(".col-order")).toHaveText("1");
  await expect(row(SECOND).locator(".col-order")).toHaveText("2");
  await expect(row(UNRANKED).locator(".col-order")).toHaveText("—");

  const positions = async () => {
    const titles = await page
      .locator("#list-body tr td:nth-child(3)")
      .allTextContents();
    return {
      first: titles.indexOf(FIRST),
      second: titles.indexOf(SECOND),
      unranked: titles.indexOf(UNRANKED),
    };
  };

  const header = page.locator('thead th[data-col="order"]');
  await header.click();
  await expect(page.locator("#sort-order")).toHaveText("▲");
  await expect.poll(async () => { const p = await positions(); return p.first >= 0 && p.first < p.second && p.second < p.unranked; }).toBe(true);
  const ascending = await positions();
  expect(ascending.first).toBeLessThan(ascending.second);
  expect(ascending.second).toBeLessThan(ascending.unranked);

  await header.click();
  await expect(page.locator("#sort-order")).toHaveText("▼");
  await expect.poll(async () => { const p = await positions(); return p.second >= 0 && p.second < p.first && p.first < p.unranked; }).toBe(true);
  const descending = await positions();
  expect(descending.second).toBeLessThan(descending.first);
  expect(descending.first).toBeLessThan(descending.unranked);
});
