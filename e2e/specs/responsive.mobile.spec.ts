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

/**
 * SH-235 (D2): `.list-table-wrap` is narrower than the table it holds at
 * phone widths -- seven columns (ID, Title, State, Priority, Labels,
 * Assignee, Updated) don't fit 350-ish CSS px. `overflow: hidden` there
 * doesn't shrink the table, it clips it: Labels, Assignee and Updated
 * render past the wrap's own right edge and are removed from the box
 * entirely, not merely scrolled out of view -- the story's own "text is
 * clipped", measured (390px viewport: table scrollWidth 630px inside
 * clientWidth 348px, ~45% of every row gone).
 *
 * `overflow-x: auto` doesn't make the table narrower either; the fix here
 * is reachability, not a redesign -- every column is still present,
 * reading a value the wrap has means a horizontal scroll rather than
 * `display: none`.
 */
test("the list table scrolls sideways to its far columns instead of clipping them", async ({
  page,
}) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto("/");
  await page.locator(".repo-card-name", { hasText: "Alpha Project" }).click();
  await expect(page.locator("#board-view")).toBeVisible();
  await page.locator('#view-toggle button[data-view="list"]').click();
  await expect(page.locator("#list-view")).toBeVisible();
  await expect(page.locator("#list-body tr").first()).toBeVisible();

  const wrap = page.locator(".list-table-wrap");
  const before = await wrap.evaluate((el) => ({
    overflowX: getComputedStyle(el).overflowX,
    scrollWidth: el.scrollWidth,
    clientWidth: el.clientWidth,
    scrollLeft: el.scrollLeft,
  }));
  // The premise: the table really is wider than the wrap at this width.
  // If it weren't, scrolling it wouldn't prove anything either way.
  expect(
    before.scrollWidth,
    `the table (${before.scrollWidth}px) is expected to be wider than ` +
      `.list-table-wrap (${before.clientWidth}px) at 390px -- if it isn't, ` +
      "this test can no longer tell a scrollable table from a clipped one",
  ).toBeGreaterThan(before.clientWidth);
  expect(
    before.overflowX,
    ".list-table-wrap must be horizontally scrollable (overflow-x: auto), " +
      "not overflow: hidden, so the columns past its right edge stay reachable",
  ).toBe("auto");

  // Reachability, not just the declaration: actually scroll, and confirm
  // a far column (Updated, the last one) becomes visible.
  await wrap.evaluate((el) => {
    el.scrollLeft = el.scrollWidth;
  });
  const after = await wrap.evaluate((el) => el.scrollLeft);
  expect(
    after,
    "scrolling .list-table-wrap did not move it -- the far columns are " +
      "still unreachable",
  ).toBeGreaterThan(0);
  await expect(
    page.locator("thead th", { hasText: "Updated" }),
  ).toBeInViewport();

  await expectNoClippedElements(page.locator("#list-view"), "the list view @ 390px");
});

/**
 * SH-235 (D5): `.toast-stack` and `.dispatch-history` are both
 * `position: fixed; right: 1rem` with a bare `max-width` -- 22rem (352px)
 * and 26rem (416px). Neither box leaves room for its own matching 1rem on
 * the *left*, so on any viewport narrower than max-width + 2rem, the box
 * itself (whether or not it holds any content yet) runs off the left edge.
 * `min(<rem>, calc(100vw - 2rem))` is meant to cap it at the viewport
 * minus both margins instead -- this test proves the browser actually
 * resolves that `calc()`/`min()` pair to the expected pixel value, which a
 * text-level check of the CSS source (`web_test.rs`) cannot: a nesting or
 * rounding mistake in the expression would still read as correct source
 * text while resolving wrong.
 */
test("toast and dispatch-history overlays never exceed a narrow viewport", async ({
  page,
}) => {
  const width = 320;
  await page.setViewportSize({ width, height: 844 });
  await page.goto("/");

  const remPx = await page.evaluate(
    () => parseFloat(getComputedStyle(document.documentElement).fontSize),
  );
  const expectedMaxWidth = width - 2 * remPx; // the two 1rem margins

  for (const [selector, remCeiling] of [
    [".toast-stack", 22],
    [".dispatch-history", 26],
  ] as const) {
    const computed = await page
      .locator(selector)
      .evaluate((el) => parseFloat(getComputedStyle(el).maxWidth));
    expect(
      computed,
      `${selector}'s computed max-width (${computed}px) must not exceed ` +
        `the viewport minus its own left+right margins (${expectedMaxWidth}px)`,
    ).toBeLessThanOrEqual(expectedMaxWidth + 0.5);
    // At 320px the `calc(100vw - 2rem)` branch of `min()` must be the one
    // that actually wins -- otherwise this test would pass trivially by
    // measuring the untouched `<rem>` ceiling instead of the fix.
    expect(
      computed,
      `${selector}'s computed max-width (${computed}px) should be capped ` +
        `well under its ${remCeiling}rem ceiling at a 320px viewport -- ` +
        "if it isn't, the viewport-relative branch of min() never won",
    ).toBeLessThan(remCeiling * remPx);
  }
});
