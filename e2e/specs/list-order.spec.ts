import { test, expect } from "./support";
import { openProject, projectSlug, seedToken } from "./support";

/**
 * SH-450's browser half. The server-side service/API tests own computation
 * of the dependency-aware `next_ids`; this spec injects a deliberately
 * disagreeing order so only the List renderer and comparator are under test.
 * The fixture is response-only and writes nothing to Alpha Project.
 */

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
  await page.route(
    (url) => url.pathname === `/api/repos/${encodeURIComponent(slug)}/data`,
    async (route) => {
      const response = await route.fetch();
      const data: {
        stories?: Array<Record<string, unknown>>;
        next_ids?: string[];
      } = await response.json();
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
      await route.fulfill({ response, json: data });
    },
  );
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
  const ascending = await positions();
  expect(ascending.first).toBeLessThan(ascending.second);
  expect(ascending.second).toBeLessThan(ascending.unranked);

  await header.click();
  await expect(page.locator("#sort-order")).toHaveText("▼");
  const descending = await positions();
  expect(descending.second).toBeLessThan(descending.first);
  expect(descending.first).toBeLessThan(descending.unranked);
});
