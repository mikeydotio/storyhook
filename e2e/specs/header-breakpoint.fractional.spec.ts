import type { Page } from "@playwright/test";
import {
  test, expect, cleanUpCreatedStories, openProject, projectSlug,
  requiredEnv, seedToken,
} from "./support";
import { LONG_NAME, useLongProjectName, expectHeaderContained } from "./header-geometry";

// SH-762: the header's two layouts must partition every layout width,
// including widths that are not a whole CSS pixel. Only the
// `fractional-firefox` project (Gecko at `layout.css.devPixelsPerPx` 1.1)
// produces one, so this file runs there and nowhere else.

// Drain catalog reads before fixture deletion can trigger another refresh,
// and before context teardown disposes responses still owned by a handler.
test.afterEach(async ({ page }) => {
  await page.unrouteAll({ behavior: "wait" });
});
cleanUpCreatedStories("Alpha Project");

/** Where the engine itself places the layout width, read through media
 * evaluation rather than `innerWidth` (rounded to an integer) or
 * `visualViewport.width` (which excludes classic scrollbars that media
 * queries include). */
async function mediaWidthBand(page: Page) {
  return page.evaluate(() => ({
    atMost768: matchMedia("(max-width: 768px)").matches,
    atMost768AndAHalf: matchMedia("(max-width: 768.5px)").matches,
  }));
}

/** Fails loudly when this engine no longer lays viewport 768 out inside
 * (768, 768.5] CSS px -- the band a `(min-width: 769px)` query misses. */
async function expectFractionalBand(page: Page) {
  await page.setViewportSize({ width: 768, height: 1100 });
  expect(
    await mediaWidthBand(page),
    "viewport 768 must lay out strictly between 768 and 769 CSS px in this project",
  ).toEqual({ atMost768: false, atMost768AndAHalf: true });
}

/** Which of the header's two rule sets the stylesheet applied, and which
 * layout the script chose. The compact grid is the only rule that makes the
 * header a grid; the SH-741 containment is the only rule that lets
 * `.topbar-right` and `.filter-summary` wrap (both default to nowrap). */
async function headerLayout(page: Page) {
  return page.evaluate(() => {
    const style = (selector: string) => getComputedStyle(document.querySelector(selector)!);
    return {
      compactGrid: style("#dashboard-header").display === "grid",
      desktopContainment: style(".topbar-right").flexWrap === "wrap"
        && style(".filter-summary").flexWrap === "wrap",
      scriptCompact: !document.getElementById("more-btn")!.hidden,
    };
  });
}

test.beforeEach(async ({ page, request }) => {
  const slug = await projectSlug(request, "Alpha Project");
  const created = await request.post(`/api/repos/${slug}/story`, {
    headers: { "X-Storyhook": "1", "X-Storyhook-Token": requiredEnv("DASHBOARD_TOKEN") },
    data: { title: "SH-762 fractional header draft", draft: true },
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

test("viewport 768 lays out at a fractional width between the two header layouts", async ({ page }) => {
  await expectFractionalBand(page);
});

// 767 -> 767.25 (compact), 768 -> 768.167 (the band), 769 -> 769.083 (desktop).
for (const [width, compact] of [[767, true], [768, false], [769, false]] as const) {
  test(`exactly one header layout applies at viewport ${width}`, async ({ page }) => {
    if (width === 768) await expectFractionalBand(page);
    await page.setViewportSize({ width, height: 1100 });
    expect((await mediaWidthBand(page)).atMost768).toBe(compact);
    // Polled: the script's side follows compactHeaderQuery's change event,
    // which the engine dispatches on its next rendering update.
    await expect.poll(() => headerLayout(page)).toEqual({
      compactGrid: compact,
      desktopContainment: !compact,
      scriptCompact: compact,
    });
  });
}

for (const scale of [100, 200]) {
  test(`the desktop toolbar fits at the fractional width with ${scale}% text`, async ({ page }) => {
    await page.locator("#filter-toggle-btn").click();
    await expect(page.locator("#filter-panel")).toBeVisible();
    // User text enlargement changes rem sizes without changing layout rules.
    await page.addStyleTag({ content: `html { font-size: ${scale}%; }` });
    await expectFractionalBand(page);
    await expectHeaderContained(page);
  });
}
