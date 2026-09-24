import {
  test, expect, cleanUpCreatedStories, openProject, projectSlug,
  requiredEnv, seedToken, awaitSettled,
} from "./support";
import { LONG_NAME, useLongProjectName, headerGeometry, expectHeaderContained } from "./header-geometry";

// Drain catalog reads before fixture deletion can trigger another refresh,
// and before context teardown disposes responses still owned by a handler.
test.afterEach(async ({ page }) => {
  await page.unrouteAll({ behavior: "wait" });
});
cleanUpCreatedStories("Alpha Project");

test.beforeEach(async ({ page, request }) => {
  const slug = await projectSlug(request, "Alpha Project");
  const created = await request.post(`/api/repos/${slug}/story`, {
    headers: { "X-Storyhook": "1", "X-Storyhook-Token": requiredEnv("DASHBOARD_TOKEN") },
    data: { title: "SH-741 toolbar draft", draft: true },
  });
  expect(created.ok(), await created.text()).toBe(true);
  await useLongProjectName(page);
  await seedToken(page);
  await page.setViewportSize({ width: 1440, height: 1100 });
  await page.goto("/");
  await openProject(page, LONG_NAME);
  await expect(page.locator("#drafts-btn-text")).toHaveText("1 Drafts");
  await expect(page.locator(".engine-run-btn")).toHaveText("Auto: Stopped");
});

for (const scale of [100, 200]) {
  for (const width of [769, 800, 900, 1024, 1280, 1440]) {
    test(`desktop toolbar fits at ${width}px with ${scale}% text`, async ({ page }) => {
      await page.locator("#filter-toggle-btn").click();
      await expect(page.locator("#filter-panel")).toBeVisible();
      // User text enlargement changes rem sizes without changing layout rules.
      await page.addStyleTag({ content: `html { font-size: ${scale}%; }` });
      await page.setViewportSize({ width, height: 1100 });
      const before = await expectHeaderContained(page);
      const board = page.locator("#board-view");
      await awaitSettled(board);
      for (const edge of ["right", "left"]) {
        await board.evaluate((node, edge) => {
          node.scrollLeft = edge === "right" ? node.scrollWidth : 0;
        }, edge);
        expect(await headerGeometry(page)).toEqual(before);
        const column = edge === "right" ? board.locator(".column").last() : board.locator(".column").first();
        const bounds = await column.boundingBox();
        expect(bounds).not.toBeNull();
        expect(bounds!.x).toBeGreaterThanOrEqual(-1);
        expect(bounds!.x + bounds!.width).toBeLessThanOrEqual(width + 1);
      }
      await page.locator("#drafts-btn").focus();
      await expectHeaderContained(page);
      await page.keyboard.press("Enter");
      await expect(page.locator("#drafts-modal")).toHaveClass(/open/);
      await page.locator("#drafts-close").click();
      await page.locator(".engine-run-btn").click();
      await expect(page.locator("#engine-modal")).toHaveClass(/open/);
      await page.keyboard.press("Escape");
      await page.locator("#filter-clear").click();
      await expectHeaderContained(page);
    });
  }
}

test("long failure labels fit and the compact breakpoint restores desktop controls", async ({ page }) => {
  await page.route("**/api/repos/*/engine", route => route.abort("failed"));
  await page.route("**/api/events", route => route.abort("failed"));
  await page.reload();
  await expect(page.locator(".engine-run-btn")).toHaveText("Auto: Unavailable");
  await expect(page.locator("#conn-text")).toHaveText("Disconnected");
  await page.addStyleTag({ content: "html { font-size: 200%; }" });
  await page.setViewportSize({ width: 769, height: 1100 });
  await expectHeaderContained(page);
  await page.addStyleTag({ content: "html { font-size: 100%; }" });
  await page.setViewportSize({ width: 768, height: 1100 });
  await expect(page.locator("#more-btn")).toBeVisible();
  await expect(page.locator("#drafts-btn")).toBeHidden();
  await page.setViewportSize({ width: 769, height: 1100 });
  await expect(page.locator("#drafts-btn")).toBeVisible();
  await expectHeaderContained(page);
});
