import { test, expect } from "@playwright/test";
import type { Locator, Page } from "@playwright/test";
import {
  cleanUpCreatedStories,
  deleteStory,
  openFilters,
  openProject,
  requiredEnv,
  seedToken,
} from "./support";

/**
 * SH-235: the dashboard's shape at phone and tablet widths, on top of
 * SH-256's zoom fixes (`zoom.mobile.spec.ts`). Runs under both mobile
 * Playwright projects, `mobile-chromium` and `mobile-webkit` (SH-348) -- see
 * that file's own comment for why a coarse-pointer, `hasTouch` environment
 * is what a `*.mobile.spec.ts` suffix buys on either engine.
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

cleanUpCreatedStories("Alpha Project");

test.beforeEach(async ({ page }) => {
  await seedToken(page);
});

/**
 * Creates a story in Alpha Project's `todo` column -- default state/type,
 * same helper shape as `zoom.mobile.spec.ts`'s own local `createStory`.
 */
async function createStory(page: Page, title: string): Promise<void> {
  await page.locator("#new-story-btn").click();
  await expect(page.locator("#create-modal")).toHaveClass(/open/);
  await page.locator("#create-title").fill(title);
  // SH-358: an unassessed priority raises a timer-driven warning toast this
  // file's own assertions do not expect.
  await page.locator("#create-priority").selectOption("medium");
  await page.locator("#create-submit").click();
  await expect(page.locator("#create-modal")).not.toHaveClass(/open/);
  await expect(
    page.locator('.column[data-state="todo"] .card', { hasText: title }),
  ).toBeVisible();
}

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
 * Any element clipped to a *1x1px* box (`.sr-only`, and the narrow-width
 * `.brand h1`/`.icon-compact-btn .btn-text`/`.conn-text` rules that hide a
 * label from sighted readers without removing it from the accessibility
 * tree, SH-235) is always ignored on top of that -- detected by the
 * computed box itself, not a class name, so it catches every element using
 * the same technique rather than only the ones tagged `.sr-only` by name.
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
      if (el.clientWidth <= 1 && el.clientHeight <= 1) continue;
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
        // SH-217: a link inside rendered markdown (a description or a
        // comment body) sits inline within a sentence or block of
        // running text -- WCAG 2.2 SC 2.5.8 explicitly exempts a target
        // in that position, and a 44px min-height floor on it would
        // wreck the paragraph's own line layout for no accessibility
        // gain (`.rel-id`, a standalone story reference and NOT inline
        // in prose, keeps its own floor unaffected).
        if (el.tagName === "A" && el.closest(".md")) continue;
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
  await openProject(page, "Alpha Project");

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
  await openProject(page, "Alpha Project");
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
 * SH-292: the Drafts popover names its project, and a project name is
 * unbounded user input landing in a `min(30rem, 92vw)` box.
 *
 * This overlay had no entry in this sweep at all until now, which is what the
 * story's council found when it checked the claim that the sweep already
 * covered the risk: the file opened `#create-modal`, `#drawer`, `#list-view`,
 * `#settings-view`, the toast stack and the dispatch history, and never this
 * one. So the argument for putting the name on its own line rather than in
 * `.modal-header` was resting on coverage that did not exist.
 *
 * Two different failures are asserted, because the sweep alone cannot catch
 * both. A plain `<div>` holding a long name *wraps*: it grows to two or three
 * lines, overflows nothing, and passes `expectNoClippedElements` while pushing
 * the list down and reading as a title. Truncation is therefore asserted
 * directly — the box is one line wide, its content is wider, and
 * `text-overflow` resolves to `ellipsis` — while the sweep catches the other
 * shape, a fixed width or a `nowrap` without an ellipsis that clips the name
 * off the edge instead.
 *
 * The name arrives by rewriting `GET /api/repos`'s reply rather than by
 * renaming the fixture: data, not behaviour, and nothing another spec runs
 * against changes.
 */
const LONG_PROJECT_NAME =
  "Storyhook Dashboard Marketing Website Redesign And Rollout Programme, " +
  "Phase Two (Northern Hemisphere)";

for (const width of SWEEP_WIDTHS) {
  test(`the Drafts popover elides a long project name at ${width}px`, async ({
    page,
  }) => {
    await page.setViewportSize({ width, height: SWEEP_HEIGHT });
    await page.route("**/api/repos", async (route) => {
      // The token explicitly: `route.fetch()` replays the request without the
      // cookie the page authenticates with, and the daemon answers a 401 whose
      // body is not JSON at all.
      const response = await route.fetch({
        headers: {
          ...route.request().headers(),
          "X-Storyhook-Token": requiredEnv("DASHBOARD_TOKEN"),
        },
      });
      const repos = (await response.json()) as Array<{
        name: string;
      }>;
      repos.forEach((repo) => {
        if (repo.name === "Alpha Project") repo.name = LONG_PROJECT_NAME;
      });
      await route.fulfill({ response, json: repos });
    });
    await page.goto("/");
    await openProject(page, LONG_PROJECT_NAME);
    await page.locator("#drafts-btn").click();
    await expect(page.locator("#drafts-modal")).toHaveClass(/open/);

    const subject = page.locator("#drafts-subject");
    const box = await subject.evaluate((el) => ({
      scrollWidth: el.scrollWidth,
      clientWidth: el.clientWidth,
      textOverflow: getComputedStyle(el).textOverflow,
      whiteSpace: getComputedStyle(el).whiteSpace,
    }));
    expect(
      box.scrollWidth,
      `#drafts-subject (${box.clientWidth}px wide, white-space: ` +
        `${box.whiteSpace}) is not truncating a ${LONG_PROJECT_NAME.length}-` +
        `character project name at ${width}px -- it wrapped to more lines ` +
        "instead, which turns a subject line into a title",
    ).toBeGreaterThan(box.clientWidth);
    expect(
      box.textOverflow,
      "#drafts-subject must truncate with an ellipsis, the treatment " +
        "`.projsel-label` and `.drafts-row-title` already use -- clipping a " +
        "name mid-glyph says nothing about where it was cut",
    ).toBe("ellipsis");

    await expectNoClippedElements(
      page.locator("#drafts-modal"),
      `the Drafts popover @ ${width}px`,
    );
    // SH-303: `.projsel-btn` used to drop to `max-width: none` under 768px,
    // so the same long name behind this popover rendered at full length and
    // took `document.documentElement.scrollWidth` to 799px against a 320px
    // viewport -- this call used to fail at all four widths on the topbar,
    // not on the popover in front of it. Fixed by giving that rule a
    // viewport-relative ceiling; see the topbar-scoped test below for the
    // one that pins the mechanism directly.
    await expectNoHorizontalOverflow(page, `the Drafts popover @ ${width}px`);
  });
}

/**
 * SH-303: the topbar itself, not the popover in front of it. `.projsel-btn`'s
 * `max-width: 14rem` (the constraint that gives `.projsel-label`'s
 * `text-overflow: ellipsis` something to elide against) used to drop to
 * `max-width: none` inside `@media (max-width: 768px)` -- exactly the widths
 * that can least afford an unconstrained box. A 100-character project name
 * then rendered at full length and carried the whole document past its own
 * viewport width (measured: `scrollWidth` 799px against a 320px
 * `clientWidth`).
 *
 * This sweep proves it on the board screen directly, where the defect lives,
 * rather than through the Drafts popover above it -- the two assertions
 * together (document-level overflow, and the label's own truncation
 * mechanism) are what the topbar's own `.projsel-btn`/`.projsel-label` pair
 * needs, the same two-assertion shape SH-292 used for `#drafts-subject`.
 */
for (const width of SWEEP_WIDTHS) {
  test(`the header project selector elides a long project name at ${width}px`, async ({
    page,
  }) => {
    await page.setViewportSize({ width, height: SWEEP_HEIGHT });
    await page.route("**/api/repos", async (route) => {
      // The token explicitly: `route.fetch()` replays the request without the
      // cookie the page authenticates with, and the daemon answers a 401 whose
      // body is not JSON at all.
      const response = await route.fetch({
        headers: {
          ...route.request().headers(),
          "X-Storyhook-Token": requiredEnv("DASHBOARD_TOKEN"),
        },
      });
      const repos = (await response.json()) as Array<{
        name: string;
      }>;
      repos.forEach((repo) => {
        if (repo.name === "Alpha Project") repo.name = LONG_PROJECT_NAME;
      });
      await route.fulfill({ response, json: repos });
    });
    await page.goto("/");
    await openProject(page, LONG_PROJECT_NAME);

    await expectNoHorizontalOverflow(
      page,
      `the board screen's topbar @ ${width}px`,
    );

    const label = page.locator("#projsel-label");
    const box = await label.evaluate((el) => ({
      scrollWidth: el.scrollWidth,
      clientWidth: el.clientWidth,
      textOverflow: getComputedStyle(el).textOverflow,
      whiteSpace: getComputedStyle(el).whiteSpace,
    }));
    expect(
      box.scrollWidth,
      `#projsel-label (${box.clientWidth}px wide, white-space: ` +
        `${box.whiteSpace}) is not truncating a ${LONG_PROJECT_NAME.length}-` +
        `character project name at ${width}px -- \`.projsel-btn\` has no ` +
        "ceiling for the ellipsis to elide against",
    ).toBeGreaterThan(box.clientWidth);
    expect(
      box.textOverflow,
      "#projsel-label must truncate with an ellipsis rather than clip a " +
        "name mid-glyph or let it push the topbar off-screen",
    ).toBe("ellipsis");
  });
}

/**
 * SH-235 (D5): a notice surface pinned to `right: 1rem` with a bare
 * `max-width` leaves no room for its own matching 1rem on the *left*, so on
 * any viewport narrower than max-width + 2rem the box itself (whether or not
 * it holds any content yet) runs off the left edge. `min(<rem>, calc(100vw -
 * 2rem))` caps it at the viewport minus both margins instead -- and this test
 * proves the browser actually resolves that `calc()`/`min()` pair to the
 * expected pixel value, which a text-level check of the CSS source
 * (`web_test.rs`) cannot: a nesting or rounding mistake in the expression
 * would still read as correct source text while resolving wrong.
 *
 * SH-323 moved which element owes that sum. `.toast-stack` and
 * `.dispatch-history` are no longer positioned; both are children of
 * `.notice-dock`, the single box that touches the viewport edge, and each is
 * bounded by `100%` of whatever the dock resolved to. So the dock is what is
 * measured here.
 *
 * The stacks are deliberately NOT measured the same way, and the reason is a
 * property of `getComputedStyle` rather than a gap in coverage: a computed
 * `max-width` of `min(22rem, 100%)` keeps its percentage unresolved, so
 * `parseFloat` on it yields `NaN` -- a measurement that cannot fail loudly is
 * worse than none. Their bound is pinned as source text in `web_test.rs`
 * instead, and structurally: a child cannot exceed a parent it is `100%` of.
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

  for (const [selector, remCeiling] of [[".notice-dock", 26]] as const) {
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

/**
 * SH-235 (D3, WCAG 2.2 SC 2.5.8): every `button`, link and `select` across
 * the dashboard's surfaces measures at least 44 CSS px on both axes under
 * a coarse pointer -- the fingertip-comfortable size this suite holds tap
 * targets to (`--tap-min`'s coarse value; 24px is the floor everywhere
 * else, asserted for the fine-pointer default in `web_test.rs`).
 *
 * Split into two tests sharing one walk (`sweepTapTargets`) rather than one
 * combined selector, because a `select`'s tap target depends on a sizing
 * property (`height`) `button, a[href]` never needed: WebKit ignores
 * `min-height` on a default-appearance `<select>` (SH-377, found via this
 * exact sweep under `mobile-webkit` when the `select` half of this test was
 * still quarantined there), so `web_dashboard.html` sizes every `select` with
 * an explicit `height` instead. That fix is the same property on every
 * engine and needs no per-engine carve-out any more -- the two tests stay
 * split anyway, on purpose, so a future `select` regression can never take
 * button/link coverage down with it by sharing one selector.
 *
 * Scoped to `button, a[href], select` -- native `input[type=checkbox]`
 * boxes are excluded on purpose: each one here is wrapped by a `<label>`
 * that is itself the real target (clicking anywhere on the label toggles
 * it), and that label is what's measured and fixed, not the 13px checkbox
 * square nested inside it. Measuring the checkbox too would be a false
 * positive against a target SC 2.5.8 already exempts (a smaller control
 * inside an equivalent, larger one).
 */
async function sweepTapTargets(page: Page, selector: string): Promise<void> {
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto("/");

  await expectNoSmallTargets(page.locator("body"), "the home screen", selector);

  await openProject(page, "Alpha Project");
  await expectNoSmallTargets(
    page.locator("body"),
    "the board screen (filters collapsed)",
    selector,
  );
  // SH-235: the filter panel's own dropdowns, checkboxes and sort buttons
  // default collapsed and are excluded from the sweep above (a hidden
  // element's box is 0x0, filtered out by findSmallTargets on purpose --
  // see its own comment) -- open it so this sweep actually measures them
  // too, not just the always-visible summary row.
  await openFilters(page);
  await expectNoSmallTargets(page.locator("body"), "the board screen (filters open)", selector);

  await page.locator('#view-toggle button[data-view="list"]').click();
  await expect(page.locator("#list-body tr").first()).toBeVisible();
  await expectNoSmallTargets(page.locator("body"), "the list screen", selector);

  await page.locator("#list-body tr").first().click();
  await expect(page.locator("#drawer")).toHaveClass(/open/);
  await expectNoSmallTargets(page.locator("#drawer"), "the story drawer", selector);
  await page.locator("#drawer-close").click();
  await expect(page.locator("#drawer")).not.toHaveClass(/open/);

  await page.locator("#new-story-btn").click();
  await expect(page.locator("#create-modal")).toHaveClass(/open/);
  await expectNoSmallTargets(page.locator("#create-modal"), "the create-story modal", selector);
  await page.locator("#create-discard").click();
  await expect(page.locator("#create-modal")).not.toHaveClass(/open/);

  await page.locator("#settings-btn").click();
  await expect(page.locator("#settings-view")).toBeVisible();
  await expectNoSmallTargets(page.locator("body"), "the settings screen", selector);

  await page
    .locator(".settings-table tr", { hasText: "Alpha Project" })
    .locator("button", { hasText: "Statuses" })
    .click();
  await expect(page.locator(".status-list")).toBeVisible();
  await expectNoSmallTargets(page.locator(".settings"), "the statuses editor", selector);
}

test("every button and link meets the coarse-pointer tap-target minimum", async ({ page }) => {
  await sweepTapTargets(page, "button, a[href]");
});

test("every select meets the coarse-pointer tap-target minimum", async ({ page }) => {
  await sweepTapTargets(page, "select");
});

/** Asserts `findSmallTargets` reports nothing under `root` for `selector`,
 * at the coarse-pointer 44px minimum. */
async function expectNoSmallTargets(
  root: Locator,
  surface: string,
  selector: string,
): Promise<void> {
  const small = await findSmallTargets(root, selector, 44);
  expect(
    small,
    `${surface}: these tap targets measure under the 44px coarse-pointer minimum`,
  ).toEqual([]);
}

/**
 * SH-235 (D4, the plan's own "chrome budget"). The plan's first cut set an
 * arbitrary 25% ceiling before real measurement; this test is what
 * replaced it with a measured one, and it's the reason the topbar was
 * compacted at all -- the filter bar's disclosure alone left the topbar
 * (unrelated to the disclosure, but sharing its "chrome eats the screen"
 * root cause) taking 251px by itself at 375px wide, four buttons' worth of
 * prose text and a full wordmark included.
 *
 * 25% (167px) turned out unreachable without cutting something a reader
 * actually needs on a phone -- the search bar or the Board/List toggle,
 * both of which stay full-width/full-text on purpose. Four irreducible
 * control groups (identity+project, search, view toggle, actions) each
 * anchored by a coarse-pointer 44px tap target is a real floor, not a
 * tuning parameter: three stacked rows of it is ~170px before the filter
 * bar's own 61px collapsed row is added.
 *
 * What the topbar compaction actually bought (measured, this test's own
 * numbers): 312px (disclosure only, unfixed topbar) -> 232px (icon-only
 * Home/Settings/Drafts, the wordmark and connection status text hidden --
 * visually, not from the accessibility tree -- and brand/view-toggle
 * merged onto one row). 40% (267px) is the guard here: comfortably above
 * the measured, now-optimized 232px, well below the pre-compaction 312px,
 * so a real regression -- a row that stops merging, a label that stops
 * hiding -- still fails this test without chasing a number the topbar's
 * actual content can't reach.
 */
test("the topbar and collapsed filter bar together stay within a measured chrome budget", async ({
  page,
}) => {
  const height = 667; // iPhone SE-class height, the tightest common phone
  await page.setViewportSize({ width: 375, height });
  await page.goto("/");
  await openProject(page, "Alpha Project");
  await expect(page.locator("#filter-panel")).toBeHidden();

  const chromeBottom = await page
    .locator("#filter-bar")
    .evaluate((el) => el.getBoundingClientRect().bottom);
  const budget = height * 0.4;
  expect(
    chromeBottom,
    `the topbar + collapsed filter bar take up ${chromeBottom}px of a ` +
      `${height}px screen -- over the ${budget}px (40%) budget`,
  ).toBeLessThanOrEqual(budget);
});

/**
 * SH-235 (D9): HTML5 native drag-and-drop (`card.draggable` + `dragstart`
 * in `buildCard`) never fires on touch at all -- before this, the only way
 * to move or act on a card without a mouse was an undocumented long-press
 * raising the same context menu a right-click does. `.card-actions-btn`
 * (board) and `.row-actions-btn` (list) make that path visible, and this
 * proves it's genuinely the *same* menu -- not a second one that could
 * silently drift out of step with the first as items are added/removed.
 *
 * Both buttons are `display: none` outside `pointer: coarse` (see their
 * own CSS) -- this file only runs under the two mobile projects
 * (`mobile-chromium`, `mobile-webkit`), where that media query matches on
 * either engine, so no extra gating is needed to reach them here.
 */
test("the card and list-row actions menus have the same items as right-click", async ({
  page,
}) => {
  const title = "SH-235 actions-menu parity";
  await page.goto("/");
  await openProject(page, "Alpha Project");
  await createStory(page, title);

  const card = page.locator('.column[data-state="todo"] .card', {
    hasText: title,
  });
  await card.click({ button: "right" });
  await expect(page.locator(".ctxmenu")).toBeVisible();
  const rightClickItems = await page
    .locator(".ctxmenu .ctxmenu-item")
    .allTextContents();
  await page.keyboard.press("Escape");
  await expect(page.locator(".ctxmenu")).not.toBeVisible();

  await card.locator(".card-actions-btn").click();
  await expect(page.locator(".ctxmenu")).toBeVisible();
  const cardActionsItems = await page
    .locator(".ctxmenu .ctxmenu-item")
    .allTextContents();
  expect(
    cardActionsItems,
    "the card's (...) menu must offer exactly what right-click offers",
  ).toEqual(rightClickItems);
  await page.keyboard.press("Escape");
  await expect(page.locator(".ctxmenu")).not.toBeVisible();

  await page.locator('#view-toggle button[data-view="list"]').click();
  const row = page.locator("tr[data-id]", { hasText: title });
  await row.click({ button: "right" });
  await expect(page.locator(".ctxmenu")).toBeVisible();
  const rowRightClickItems = await page
    .locator(".ctxmenu .ctxmenu-item")
    .allTextContents();
  await page.keyboard.press("Escape");
  await expect(page.locator(".ctxmenu")).not.toBeVisible();

  await row.locator(".row-actions-btn").click();
  await expect(page.locator(".ctxmenu")).toBeVisible();
  const rowActionsItems = await page
    .locator(".ctxmenu .ctxmenu-item")
    .allTextContents();
  expect(
    rowActionsItems,
    "the list row's (...) menu must offer exactly what right-click offers",
  ).toEqual(rowRightClickItems);
  // Right-click already proved this menu has real items (not an empty
  // shell) -- both parity checks above would pass trivially against two
  // empty arrays otherwise.
  expect(rowActionsItems.length).toBeGreaterThan(0);
  await page.keyboard.press("Escape");
  await expect(page.locator(".ctxmenu")).not.toBeVisible();

  // deleteStory() targets the board's own card markup -- switch back from
  // list view first, or the card it's looking for is (correctly) hidden.
  await page.locator('#view-toggle button[data-view="board"]').click();
  await expect(page.locator("#board-view")).toBeVisible();
  await deleteStory(page, title);
});

/**
 * The card's own trade-off (documented in buildCard and
 * docs/spec/responsive-dashboard.md): `.card` is `role="button"`, so a
 * nested interactive element is ARIA-presentational regardless of markup
 * -- `.card-actions-btn` ships `tabIndex: -1` on purpose, deliberately
 * unreachable by Tab, with Shift+F10 / the Menu key left as the keyboard
 * path to the same menu. The list row carries no such constraint (a `<tr>`
 * is not `role="button"`), so `.row-actions-btn` is a real, natively
 * tabbable button.
 */
test("the card's actions button is deliberately not a Tab stop; the list row's is", async ({
  page,
}) => {
  const title = "SH-235 actions-menu tab reachability";
  await page.goto("/");
  await openProject(page, "Alpha Project");
  await createStory(page, title);

  await expect(
    page.locator(".card", { hasText: title }).locator(".card-actions-btn"),
  ).toHaveAttribute("tabindex", "-1");

  await page.locator('#view-toggle button[data-view="list"]').click();
  const rowActionsBtn = page
    .locator("tr[data-id]", { hasText: title })
    .locator(".row-actions-btn");
  await expect(rowActionsBtn).not.toHaveAttribute("tabindex", "-1");
  await rowActionsBtn.focus();
  await expect(rowActionsBtn).toBeFocused();

  // deleteStory() targets the board's own card markup -- switch back from
  // list view first, or the card it's looking for is (correctly) hidden.
  await page.locator('#view-toggle button[data-view="board"]').click();
  await expect(page.locator("#board-view")).toBeVisible();
  await deleteStory(page, title);
});

/**
 * SH-235 (D8): `.column`'s fixed `flex: 0 0 18rem` (288px) is nearly the
 * entire screen on the narrowest supported phones -- at 320px wide it
 * leaves only a 32px (10%) sliver of the next column, not enough to read
 * as "there's more this way" rather than "this is the last column".
 * `min(18rem, 85vw)` keeps 288px everywhere that already peeks
 * comfortably (375px+) and only shrinks the column where it wouldn't
 * peek at all.
 */
test("the next board column peeks on the narrowest supported phone", async ({
  page,
}) => {
  await page.setViewportSize({ width: 320, height: 844 });
  await page.goto("/");
  await openProject(page, "Alpha Project");
  // Alpha's own fixture carries five states (todo/in-progress/blocked/
  // done/review, column-visibility.spec.ts's own comment) -- a second
  // column to peek at is never in question here.
  await expect(page.locator(".column")).toHaveCount(5);

  const width = await page
    .locator(".column")
    .first()
    .evaluate((el) => el.getBoundingClientRect().width);
  expect(
    width,
    `a board column measures ${width}px at a 320px viewport -- expected ` +
      "min(18rem, 85vw) = 272px, not the unshrunk 288px ceiling",
  ).toBeCloseTo(272, 0);

  const firstRight = await page
    .locator(".column")
    .first()
    .evaluate((el) => el.getBoundingClientRect().right);
  expect(
    firstRight,
    "the first column's own right edge should leave room for the next " +
      "column to start peeking in before the 320px viewport ends",
  ).toBeLessThan(320);
  await expect(page.locator(".column").nth(1)).toBeInViewport({ ratio: 0 });
});

/** Confirms the fix above is additive, not a universal shrink -- a wider
 * phone (390px, comfortably above the 85vw crossover) keeps the original
 * 288px column width unchanged. */
test("a wider phone keeps the board column at its original 18rem width", async ({
  page,
}) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto("/");
  await openProject(page, "Alpha Project");
  await expect(page.locator(".column").first()).toBeVisible();

  const width = await page
    .locator(".column")
    .first()
    .evaluate((el) => el.getBoundingClientRect().width);
  expect(width).toBeCloseTo(288, 0);
});
