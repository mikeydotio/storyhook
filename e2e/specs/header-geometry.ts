import type { Page } from "@playwright/test";
import { expect, requiredEnv, awaitSettled } from "./support";

/** A project name long enough to press every header group against its row (SH-741). */
export const LONG_NAME = "Alpha Project with a very long unbroken identifier ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";

/** Serves "Alpha Project" under `LONG_NAME` without renaming the shared seed. */
export async function useLongProjectName(page: Page): Promise<void> {
  await page.route("**/api/repos", async route => {
    const response = await route.fetch({
      headers: { ...route.request().headers(), "X-Storyhook-Token": requiredEnv("DASHBOARD_TOKEN") },
    });
    const repos = await response.json();
    const alpha = repos.find((repo: { name: string }) => repo.name === "Alpha Project");
    alpha.name = LONG_NAME;
    await route.fulfill({ response, json: repos });
  });
}

/** Observe before any locator action can scroll an overflowing control into view. */
export async function headerGeometry(page: Page) {
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
      // The layout the stylesheet applied, not `innerWidth <= 768`: an
      // integer innerWidth of 768 can be a 768.167px desktop layout (SH-762).
      compact: getComputedStyle(header).display === "grid",
      header: rect(header), controls,
    };
  });
}

/** Controls must fit their row and viewport without intersecting a neighbor. */
export async function expectHeaderContained(page: Page) {
  await awaitSettled(page.locator("#dashboard-header"));
  const geometry = await headerGeometry(page);
  const diagnostic = JSON.stringify(geometry);
  expect(geometry.controls.length).toBeGreaterThan(10);
  for (const control of geometry.controls) {
    expect(control.left, diagnostic).toBeGreaterThanOrEqual(-1);
    expect(control.right, diagnostic).toBeLessThanOrEqual(geometry.viewport + 1);
    // The compact header uses display:contents instead of desktop row boxes.
    const row = geometry.compact ? geometry.header : control.row;
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
