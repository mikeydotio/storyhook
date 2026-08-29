import { test, expect } from "./support";
import {
  cleanUpCreatedStories,
  createStory,
  openProject,
  seedToken,
} from "./support";

/**
 * SH-448: the list is a dense metadata surface. Its title is prose and may
 * wrap; labels are discrete items and may move between flex lines; every
 * other value is an atomic token. In particular, a browser's ordinary break
 * opportunity after the hyphen in `in-progress` must never split the state.
 *
 * `tests/web_test.rs` pins the CSS/render mechanism. This spec forces actual
 * pressure and measures line boxes in Chromium and WebKit so a declaration
 * that does not produce the promised layout cannot pass on source text alone.
 */

cleanUpCreatedStories("Alpha Project");

test.beforeEach(async ({ page }) => {
  await seedToken(page);
  await page.setViewportSize({ width: 640, height: 844 });
  await page.goto("/");
  await openProject(page, "Alpha Project");
});

/** Number of distinct rendered lines occupied by all non-empty text nodes. */
async function textLineCount(locator: import("@playwright/test").Locator) {
  return locator.evaluate((element) => {
    const walker = document.createTreeWalker(element, NodeFilter.SHOW_TEXT);
    const tops: number[] = [];
    let node: Node | null;
    while ((node = walker.nextNode())) {
      if (!node.textContent?.trim()) continue;
      const range = document.createRange();
      range.selectNodeContents(node);
      for (const rect of range.getClientRects()) {
        if (rect.width > 0 && rect.height > 0) tops.push(rect.top);
      }
    }
    return new Set(tops.map((top) => Math.round(top * 10) / 10)).size;
  });
}

test("only titles and spaces between intact label chips wrap in list rows", async ({
  page,
}) => {
  const title =
    "SH-448 this deliberately long story title is the only list text allowed to wrap internally";
  const id = await createStory(page, title);
  const labels = [
    "layout-alpha",
    "layout-beta",
    "layout-gamma",
    "layout-delta",
    "layout-epsilon",
    "layout-zeta",
  ];

  await page.locator('.column[data-state="todo"] .card', { hasText: title }).click();
  await expect(page.locator("#drawer")).toHaveClass(/open/);
  const labelInput = page.locator('#drawer [data-field="label-add"]');
  for (const label of labels) {
    await labelInput.fill(label);
    await labelInput.press("Enter");
    await expect(page.locator("#drawer .label-chip", { hasText: label })).toBeVisible();
  }
  await page.locator("#drawer-body select").first().selectOption("in-progress");
  await expect(
    page.locator('.column[data-state="in-progress"] .card', { hasText: title }),
  ).toBeVisible();
  await page.locator("#drawer-close").click();
  await expect(page.locator("#drawer")).not.toHaveClass(/open/);

  await page.locator('#view-toggle button[data-view="list"]').click();
  await expect(page.locator("#list-view")).toBeVisible();
  const row = page.locator(`tr[data-id="${id}"]`);
  await expect(row).toBeVisible();

  const titleCell = row.locator(".col-title");
  expect(
    await textLineCount(titleCell),
    "the pressured story title should demonstrate the one permitted internal text wrap",
  ).toBeGreaterThan(1);

  const metadata = row.locator("td:not(.col-title):not(.col-labels):not(.col-actions)");
  const metadataLines = await metadata.evaluateAll((cells) =>
    cells.map((cell) => {
      const walker = document.createTreeWalker(cell, NodeFilter.SHOW_TEXT);
      const textNodes: Array<{ text: string; lines: number }> = [];
      let node: Node | null;
      while ((node = walker.nextNode())) {
        if (!node.textContent?.trim()) continue;
        const range = document.createRange();
        range.selectNodeContents(node);
        const rects = Array.from(range.getClientRects()).filter(
          (rect) => rect.width > 0 && rect.height > 0,
        );
        textNodes.push({ text: node.textContent.trim(), lines: rects.length });
      }
      return {
        text: cell.textContent?.trim(),
        whiteSpace: getComputedStyle(cell).whiteSpace,
        textNodes,
      };
    }),
  );
  for (const metric of metadataLines) {
    expect(metric.whiteSpace, `${metric.text} did not inherit the list's nowrap default`).toBe(
      "nowrap",
    );
    for (const node of metric.textNodes) {
      expect(node.lines, `${node.text} split across rendered lines`).toBe(1);
    }
  }
  await expect(row.locator(".state-pill")).toHaveText("in-progress");

  const chips = row.locator(".list-labels .chip");
  await expect(chips).toHaveCount(labels.length);
  const chipMetrics = await chips.evaluateAll((nodes) =>
    nodes.map((node) => {
      const range = document.createRange();
      range.selectNodeContents(node);
      const rects = Array.from(range.getClientRects()).filter(
        (rect) => rect.width > 0 && rect.height > 0,
      );
      return {
        text: node.textContent,
        lines: new Set(rects.map((rect) => Math.round(rect.top * 10) / 10)).size,
        top: Math.round(node.getBoundingClientRect().top * 10) / 10,
      };
    }),
  );
  for (const chip of chipMetrics) {
    expect(chip.lines, `${chip.text} split inside its chip`).toBe(1);
  }
  expect(
    new Set(chipMetrics.map((chip) => chip.top)).size,
    "the fixture must force the label group onto multiple flex lines",
  ).toBeGreaterThan(1);

  const overflow = await page.locator(".list-table-wrap").evaluate((wrap) => ({
    overflowX: getComputedStyle(wrap).overflowX,
    scrollWidth: wrap.scrollWidth,
    clientWidth: wrap.clientWidth,
    pageWidth: document.documentElement.scrollWidth,
    viewportWidth: window.innerWidth,
  }));
  expect(overflow.overflowX).toBe("auto");
  expect(overflow.scrollWidth).toBeGreaterThan(overflow.clientWidth);
  expect(overflow.pageWidth).toBeLessThanOrEqual(overflow.viewportWidth);
});
