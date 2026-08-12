import { test, expect } from "@playwright/test";
import { seedToken } from "./support";

/**
 * SH-256: on a coarse pointer (a finger, not a mouse), no text-entry control
 * anywhere in the dashboard may compute a font-size under 16 CSS pixels --
 * the threshold below which iOS Safari zooms the viewport to whatever field
 * was just focused, and does not zoom back out when it blurs. Also: tap
 * targets carry `touch-action: manipulation`, so a quick double tap acts
 * instead of double-tap-zooming.
 *
 * This file runs only under the `mobile-chromium` Playwright project
 * (`playwright.config.ts`'s `MOBILE_SPECS` pattern, matched by this file's
 * own `.mobile.spec.ts` suffix) -- never under the desktop `chromium`
 * project, because headless desktop Chromium reports `pointer: fine` and
 * the very test below would fail there by design. `devices["Pixel 7"]` is
 * Blink under mobile emulation, not WebKit: `make e2e-install` installs
 * chromium only, so a WebKit device descriptor would fail with a
 * missing-browser error on every machine. iOS Safari's 16px rule is
 * WebKit's; what this file verifies is that the *mechanism* (the
 * `--control-font-*` tokens, raised under `@media (pointer: coarse)`) fires
 * in a genuinely coarse-pointer environment and reaches every control --
 * not that a phone would actually stay unzoomed, which only a real device
 * can prove (see the plan's verification section).
 *
 * This first commit adds only the harness -- the mobile-chromium project
 * and this one environment check -- ahead of the CSS fix itself, so if
 * Blink's mobile emulation ever turned out not to report `pointer: coarse`,
 * that would be discovered in a commit whose entire subject is the harness,
 * not tangled into the fix's own diff. It stays in the tree afterward as
 * the guard that a future config edit cannot silently drop mobile emulation
 * and leave the rest of this file's sweep passing against a desktop
 * pointer.
 */

test.beforeEach(async ({ page }) => {
  await seedToken(page);
});

test("the mobile project really is a coarse-pointer environment", async ({
  page,
}) => {
  await page.goto("/");
  const env = await page.evaluate(() => ({
    pointerCoarse: matchMedia("(pointer: coarse)").matches,
    anyPointerCoarse: matchMedia("(any-pointer: coarse)").matches,
    hoverNone: matchMedia("(hover: none)").matches,
    innerWidth: window.innerWidth,
    maxTouchPoints: navigator.maxTouchPoints,
  }));
  // Every field is reported so a failure here names its own next step (see
  // the plan's "If T1 fails" section) instead of a bare boolean mismatch.
  expect(env, JSON.stringify(env)).toMatchObject({ pointerCoarse: true });
  expect(env.innerWidth).toBeLessThanOrEqual(768);
});
