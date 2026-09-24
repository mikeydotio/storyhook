import type { Page } from "@playwright/test";
import { test, expect, seedToken } from "./support";

// SH-762: the header's two layouts must partition every layout width,
// including widths that are not a whole CSS pixel. Only the
// `fractional-firefox` project (Gecko at `layout.css.devPixelsPerPx` 1.1)
// produces one, so this file runs there and nowhere else.

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

test("viewport 768 lays out at a fractional width between the two header layouts", async ({ page }) => {
  await seedToken(page);
  await page.goto("/");
  await expectFractionalBand(page);
});
