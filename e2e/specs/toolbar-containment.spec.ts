import type { Page } from "@playwright/test";
import {
  test, expect, cleanUpCreatedStories, openProject, projectSlug,
  requiredEnv, seedToken, awaitSettled,
} from "./support";

// Drain catalog reads before fixture deletion can trigger another refresh,
// and before context teardown disposes responses still owned by a handler.
test.afterEach(async ({ page }) => {
  await page.unrouteAll({ behavior: "wait" });
});
cleanUpCreatedStories("Alpha Project");
const LONG_NAME = "Alpha Project with a very long unbroken identifier ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";

/** Observe before any locator action can scroll an overflowing control into view. */
async function headerGeometry(page: Page) {
  return page.evaluate(() => {
    const header = document.getElementById("dashboard-header")!;
    const rect = (node: Element) => {
      const r = node.getBoundingClientRect();
      return { left: r.left, right: r.right, top: r.top, bottom: r.bottom };
    };
    const controls = Array.from(header.querySelectorAll(
      "button, .search-input, .filter-toggle, .filter-count, .connection",
    )).filter(node => {
      const r = node.getBoundingClientRect();
      return r.width > 0 && r.height > 0 && getComputedStyle(node).visibility !== "hidden";
    }).map(node => ({
      id: node.id || node.getAttribute("aria-label") || node.textContent,
      ...rect(node),
      row: rect(node.closest(".filter-panel, .filter-summary, .topbar")!),
    }));
    return {
      viewport: innerWidth, scroll: scrollX, extent: document.documentElement.scrollWidth,
      header: rect(header), controls,
    };
  });
}

/** Controls must fit their row and viewport without intersecting a neighbor. */
async function expectContained(page: Page) {
  await awaitSettled(page.locator("#dashboard-header"));
  const geometry = await headerGeometry(page);
  const diagnostic = JSON.stringify(geometry);
  expect(geometry.controls.length).toBeGreaterThan(10);
  for (const control of geometry.controls) {
    expect(control.left, diagnostic).toBeGreaterThanOrEqual(-1);
    expect(control.right, diagnostic).toBeLessThanOrEqual(geometry.viewport + 1);
    // The compact header uses display:contents instead of desktop row boxes.
    const row = geometry.viewport <= 768 ? geometry.header : control.row;
    expect(control.left, diagnostic).toBeGreaterThanOrEqual(row.left - 1);
    expect(control.right, diagnostic).toBeLessThanOrEqual(row.right + 1);
    expect(control.top, diagnostic).toBeGreaterThanOrEqual(row.top - 1);
    expect(control.bottom, diagnostic).toBeLessThanOrEqual(row.bottom + 1);
  }
  for (let i = 0; i < geometry.controls.length; i++) {
    for (const other of geometry.controls.slice(i + 1)) {
      const control = geometry.controls[i];
      const overlapX = Math.min(control.right, other.right) - Math.max(control.left, other.left);
      const overlapY = Math.min(control.bottom, other.bottom) - Math.max(control.top, other.top);
      expect(overlapX > 1 && overlapY > 1, diagnostic).toBe(false);
    }
  }
  expect(geometry.scroll, diagnostic).toBe(0);
  expect(geometry.extent, diagnostic).toBeLessThanOrEqual(geometry.viewport);
  return geometry;
}

test.beforeEach(async ({ page, request }) => {
  const slug = await projectSlug(request, "Alpha Project");
  const created = await request.post(`/api/repos/${slug}/story`, {
    headers: { "X-Storyhook": "1", "X-Storyhook-Token": requiredEnv("DASHBOARD_TOKEN") },
    data: { title: "SH-741 toolbar draft", draft: true },
  });
  expect(created.ok(), await created.text()).toBe(true);
  await page.route("**/api/repos", async route => {
    const response = await route.fetch({
      headers: { ...route.request().headers(), "X-Storyhook-Token": requiredEnv("DASHBOARD_TOKEN") },
    });
    const repos = await response.json();
    const alpha = repos.find((repo: { name: string }) => repo.name === "Alpha Project");
    alpha.name = LONG_NAME;
    await route.fulfill({ response, json: repos });
  });
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
      const before = await expectContained(page);
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
      await expectContained(page);
      await page.keyboard.press("Enter");
      await expect(page.locator("#drafts-modal")).toHaveClass(/open/);
      await page.locator("#drafts-close").click();
      await page.locator(".engine-run-btn").click();
      await expect(page.locator("#engine-modal")).toHaveClass(/open/);
      await page.keyboard.press("Escape");
      await page.locator("#filter-clear").click();
      await expectContained(page);
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
  await expectContained(page);
  await page.addStyleTag({ content: "html { font-size: 100%; }" });
  await page.setViewportSize({ width: 768, height: 1100 });
  await expect(page.locator("#more-btn")).toBeVisible();
  await expect(page.locator("#drafts-btn")).toBeHidden();
  await page.setViewportSize({ width: 769, height: 1100 });
  await expect(page.locator("#drafts-btn")).toBeVisible();
  await expectContained(page);
});
