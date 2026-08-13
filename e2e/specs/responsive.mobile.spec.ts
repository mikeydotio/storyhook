import { test, expect } from "@playwright/test";
import type { Locator, Page } from "@playwright/test";
import { seedToken } from "./support";

/**
 * SH-235: the dashboard's shape at phone and tablet widths, on top of
 * SH-256's zoom fixes (`zoom.mobile.spec.ts`). Runs only under the
 * `mobile-chromium` Playwright project -- see that file's own comment for
 * why a coarse-pointer, `hasTouch` environment is what a `*.mobile.spec.ts`
 * suffix buys, and why Blink under emulation (not WebKit) is what
 * `make e2e-install` can actually run everywhere.
 *
 * This file grows one commit at a time alongside SH-235's fixes: each fix
 * lands its own RED-then-GREEN test here, beside the mechanism it guards.
 * This first commit is the harness -- the generic helpers every later test
 * reuses -- plus one sweep that already passes, so the harness itself is
 * proven before anything depends on it.
 */

/** Widths this file sweeps: the two narrowest common iPhones (SE, and the
 * 14/15/16 family), an Android mid-range width, and the `max-width: 768px`
 * layout breakpoint itself -- the one width where the desktop and mobile
 * CSS rules are both live candidates, so a boundary bug shows up here
 * first. */
const SWEEP_WIDTHS = [320, 375, 390, 768];
const SWEEP_HEIGHT = 844;

test.beforeEach(async ({ page }) => {
  await seedToken(page);
});

/**
 * Fails if the page itself scrolls horizontally at the current viewport --
 * the coarsest possible "something doesn't fit" signal, and the one a
 * reader notices first (a sideways-scrollable app reads as broken before
 * any one element is inspected).
 */
async function expectNoHorizontalOverflow(
  page: Page,
  surface: string,
): Promise<void> {
  const box = await page.evaluate(() => ({
    scrollWidth: document.documentElement.scrollWidth,
    clientWidth: document.documentElement.clientWidth,
  }));
  expect(
    box.scrollWidth,
    `${surface}: document.documentElement.scrollWidth (${box.scrollWidth}) ` +
      `exceeds clientWidth (${box.clientWidth}) -- something is running off ` +
      "the right edge of the viewport",
  ).toBeLessThanOrEqual(box.clientWidth);
}

interface ClippedElement {
  describe: string;
  scrollWidth: number;
  clientWidth: number;
  overflowX: string;
}

/**
 * Every element under `root` whose content overflows its own box while that
 * overflow is neither reachable (`overflow-x: auto`/`scroll`) nor a
 * deliberate truncation (`text-overflow: ellipsis`) -- the general shape of
 * the bug SH-235 was filed over: `.list-table-wrap` clipping a 630px table
 * inside a 348px box (`overflow-x: hidden`, no ellipsis, no scrollbar, ~45%
 * of every row simply gone). Whatever element clips next, this catches it
 * the same way, without needing its own bespoke assertion.
 *
 * `ignoreSelectors` excludes elements that are off-screen by design rather
 * than by bug -- e.g. the closed `.drawer`, parked off the right edge by
 * `transform: translateX(100%)` until it opens -- and everything inside
 * them, so a false positive there can never mask a real one elsewhere.
 * `.sr-only` is always ignored on top of that: it is the standard
 * visually-hidden-but-announced pattern (`width: 1px; overflow: hidden;
 * clip: rect(0,0,0,0)`), deliberately "clipped" for sighted readers by
 * design, not a bug this sweep exists to catch.
 */
async function findClippedElements(
  root: Locator,
  ignoreSelectors: string[],
): Promise<ClippedElement[]> {
  return root.evaluate((node, ignore) => {
    const desc = (el: Element) =>
      el.tagName.toLowerCase() +
      (el.id ? `#${el.id}` : "") +
      (typeof el.className === "string" && el.className.trim()
        ? `.${el.className.trim().split(/\s+/).join(".")}`
        : "");
    const ignored: Element[] = [];
    [...ignore, ".sr-only"].forEach((sel) =>
      node.querySelectorAll(sel).forEach((el) => ignored.push(el)),
    );
    const isIgnored = (el: Element) =>
      ignored.some((anc) => anc === el || anc.contains(el));
    const out: {
      describe: string;
      scrollWidth: number;
      clientWidth: number;
      overflowX: string;
    }[] = [];
    for (const el of Array.from(node.querySelectorAll("*"))) {
      if (isIgnored(el)) continue;
      if (el.scrollWidth <= el.clientWidth + 1) continue;
      const cs = getComputedStyle(el);
      if (cs.overflowX === "auto" || cs.overflowX === "scroll") continue;
      if (cs.textOverflow === "ellipsis") continue;
      out.push({
        describe: desc(el),
        scrollWidth: el.scrollWidth,
        clientWidth: el.clientWidth,
        overflowX: cs.overflowX,
      });
    }
    return out;
  }, ignoreSelectors);
}

/** Asserts `findClippedElements` reports nothing under `root`. */
async function expectNoClippedElements(
  root: Locator,
  surface: string,
  ignoreSelectors: string[] = [],
): Promise<void> {
  const clipped = await findClippedElements(root, ignoreSelectors);
  expect(
    clipped,
    `${surface}: these elements clip their content instead of scrolling to ` +
      "it or truncating it with an ellipsis",
  ).toEqual([]);
}

interface MeasuredTarget {
  describe: string;
  width: number;
  height: number;
}

/**
 * Every element under `root` matching `selector` whose rendered box is
 * smaller than `minPx` on either axis -- WCAG 2.2 SC 2.5.8's Target Size
 * (Minimum), 24 CSS px, or the coarse-pointer 44px this suite holds tap
 * targets to (see `--tap-min` in `web_dashboard.html`). Zero-size boxes
 * (`display: none`, an unopened popover) are excluded -- a hidden control
 * cannot be mis-tapped.
 */
async function findSmallTargets(
  root: Locator,
  selector: string,
  minPx: number,
): Promise<MeasuredTarget[]> {
  return root.evaluate(
    (node, { selector, minPx }) => {
      const desc = (el: Element) =>
        el.tagName.toLowerCase() +
        (el.id ? `#${el.id}` : "") +
        (typeof el.className === "string" && el.className.trim()
          ? `.${el.className.trim().split(/\s+/).join(".")}`
          : "");
      const out: { describe: string; width: number; height: number }[] = [];
      for (const el of Array.from(node.querySelectorAll(selector))) {
        const r = el.getBoundingClientRect();
        if (r.width === 0 && r.height === 0) continue;
        if (r.width < minPx || r.height < minPx) {
          out.push({
            describe: desc(el),
            width: Math.round(r.width * 10) / 10,
            height: Math.round(r.height * 10) / 10,
          });
        }
      }
      return out;
    },
    { selector, minPx },
  );
}

for (const width of SWEEP_WIDTHS) {
  test(`the home screen never scrolls horizontally at ${width}px`, async ({
    page,
  }) => {
    await page.setViewportSize({ width, height: SWEEP_HEIGHT });
    await page.goto("/");
    // At least one seeded project, so this is measuring a rendered screen
    // rather than an empty one that would pass by having nothing to overflow.
    await expect(page.locator(".repo-card-name").first()).toBeVisible();
    await expectNoHorizontalOverflow(page, `home @ ${width}px`);
    await expectNoClippedElements(page.locator("body"), `home @ ${width}px`, [
      ".drawer",
    ]);
  });
}

/**
 * SH-235 (D1/D6): a phone browser's URL bar shrinks the *visible* viewport
 * below `window.innerHeight`'s own largest-possible value -- headless
 * Blink has no such toolbar, so it cannot reproduce the gap `100dvh` exists
 * to close (see the `dvh`/`vh` fallback pair's own comment in
 * `web_dashboard.html`, and `web_test.rs`'s structural test for that
 * mechanism). What this test can still prove, even without a toolbar: the
 * app shell and an open modal both fit fully within whatever viewport
 * height they're given, all the way down to a height a squeezed phone
 * screen might plausibly present -- so a regression that pins either to a
 * literal pixel value (rather than the shell/viewport relationship) fails
 * here regardless of `dvh` support.
 */
test("the app shell and an open modal fit inside a squeezed viewport height", async ({
  page,
}) => {
  const width = 390;
  const height = 560; // roughly an iPhone's visible height with the URL bar shown
  await page.setViewportSize({ width, height });
  await page.goto("/");
  await page.locator(".repo-card-name", { hasText: "Alpha Project" }).click();
  await expect(page.locator("#board-view")).toBeVisible();

  const appBottom = await page
    .locator(".app")
    .evaluate((el) => el.getBoundingClientRect().bottom);
  expect(
    appBottom,
    `the app shell's bottom edge (${appBottom}) must not exceed the ` +
      `viewport height (${height})`,
  ).toBeLessThanOrEqual(height);

  await page.locator("#new-story-btn").click();
  await expect(page.locator("#create-modal")).toHaveClass(/open/);
  const modalBottom = await page
    .locator("#create-modal")
    .evaluate((el) => el.getBoundingClientRect().bottom);
  expect(
    modalBottom,
    `the create-story modal's bottom edge (${modalBottom}) must not exceed ` +
      `the viewport height (${height}) -- its footer buttons would sit ` +
      "behind the browser chrome otherwise",
  ).toBeLessThanOrEqual(height);

  await page.locator("#create-discard").click();
  await expect(page.locator("#create-modal")).not.toHaveClass(/open/);
});
