import { test, expect } from "./support";
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
async function createStory(page: Page, title: string): Promise<string> {
  await page.locator("#new-story-btn").click();
  await expect(page.locator("#create-modal")).toHaveClass(/open/);
  await page.locator("#create-title").fill(title);
  // Keep this fixture's priority explicit.
  await page.locator("#create-priority").selectOption("medium");
  await page.locator("#create-submit").click();
  await expect(page.locator("#create-modal")).not.toHaveClass(/open/);
  const card = page.locator('.column[data-state="todo"] .card', {
    hasText: title,
  });
  await expect(card).toBeVisible();
  return (await card.getAttribute("data-id"))!;
}

/** Adds one blocked-by edge through the open drawer's real relation form. */
async function addBlocker(page: Page, blockerId: string): Promise<void> {
  await page.locator('input[data-field="relationship-id"]').fill(blockerId);
  await page
    .locator('select[data-field="relationship-kind"]')
    .selectOption("blocked-by");
  await page
    .locator("#drawer-body .inline-add button", { hasText: "Add" })
    .click();
  await expect(page.locator(".rel-row", { hasText: blockerId })).toBeVisible();
}

/**
 * SH-451: the blocked badge is one flex item containing several storyRef()
 * children. At phone width, three coarse-pointer-floored references create
 * the same pressure as the reported WKT-36 screenshot: the badge must wrap
 * between references, never inside an id at its hyphen.
 */
test("a pressured blocked badge keeps every story id on one line", async ({
  page,
}) => {
  await page.setViewportSize({ width: 320, height: SWEEP_HEIGHT });
  await page.goto("/");
  await openProject(page, "Alpha Project");

  const blockerTitles = [
    "SH-451 intact id — blocker A",
    "SH-451 intact id — blocker B",
    "SH-451 intact id — blocker C",
  ];
  const blockerIds: string[] = [];
  for (const title of blockerTitles) {
    blockerIds.push(await createStory(page, title));
  }
  const workerTitle = "SH-451 intact id — pressured worker";
  await createStory(page, workerTitle);

  const detailLoaded = page.waitForResponse(
    (resp) =>
      /\/story\/[^/]+$/.test(new URL(resp.url()).pathname) &&
      resp.request().method() === "GET",
  );
  await page
    .locator('.column[data-state="todo"] .card', { hasText: workerTitle })
    .click();
  await expect(page.locator("#drawer")).toHaveClass(/open/);
  await detailLoaded;
  for (const blockerId of blockerIds) {
    await addBlocker(page, blockerId);
  }
  await page.locator("#drawer-close").click();
  await expect(page.locator("#drawer")).not.toHaveClass(/open/);

  // SH-487: all three blockers are ordinary open work, so the worker stays
  // in "todo" -- only the badge, which this test is about, is affected.
  const workerCard = page.locator('.column[data-state="todo"] .card', {
    hasText: workerTitle,
  });
  const badge = workerCard.locator(".flag-blocked");
  const refs = badge.locator(".story-ref");
  await expect(refs).toHaveCount(3);

  const metrics = await refs.evaluateAll((nodes) =>
    nodes.map((node) => {
      const id = node.querySelector<HTMLElement>(".rel-id")!;
      const range = document.createRange();
      range.selectNodeContents(id);
      return {
        id: id.textContent,
        textLines: range.getClientRects().length,
        fontSize: getComputedStyle(id).fontSize,
        badgeFontSize: getComputedStyle(node.closest(".flag-blocked")!).fontSize,
      };
    }),
  );
  for (const metric of metrics) {
    expect(metric.textLines, `${metric.id} split across lines`).toBe(1);
    expect(
      metric.fontSize,
      `${metric.id} should match the compact blocker badge typography`,
    ).toBe(metric.badgeFontSize);
  }
  const overflow = await workerCard.evaluate((node) => ({
    scrollWidth: node.scrollWidth,
    clientWidth: node.clientWidth,
  }));
  expect(
    overflow.scrollWidth,
    "keeping ids atomic must not make the blocked badge overflow its card",
  ).toBeLessThanOrEqual(overflow.clientWidth);

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
  /** How far under `minPx` the shorter axis fell, past the representation
   * bound. Always > 0 for a reported target -- it is the evidence for the
   * verdict, and the reason `width`/`height` are no longer rounded before
   * being reported (SH-420). */
  shortBy: number;
}

/**
 * Every element under `root` matching `selector` whose rendered box is
 * smaller than `minPx` on either axis -- WCAG 2.2 SC 2.5.8's Target Size
 * (Minimum), 24 CSS px, or the coarse-pointer 44px this suite holds tap
 * targets to (see `--tap-min` in `web_dashboard.html`). Zero-size boxes
 * (`display: none`, an unopened popover) are excluded -- a hidden control
 * cannot be mis-tapped.
 *
 * SH-420: the comparison is not a bare `<`. WebKit returns rect coordinates
 * as float32 (measured: `Math.fround(r.top) === r.top` for every select on
 * this surface), and `height` is `bottom - top`. When those two endpoints
 * land in different binades -- 468 in [256,512), 512 in [512,1024) -- they
 * round on grids that differ by a factor of two, and their difference misses
 * the specified height by up to one ulp. A control specified at exactly 44px
 * then measures 43.999969482421875, and `< 44` decides the gate on float
 * noise: swept across 64 consecutive sub-ulp offsets, an exact-44px control
 * read under the minimum at 16 of them, over it at 16, and exactly at it at
 * 32.
 *
 * So an element is flagged only when its shortfall exceeds the bound on that
 * error. A correctly-rounded float32 endpoint sits at most half an ulp from
 * the true value and `ulp(x) <= |x| * 2**-23`, so a difference of two of them
 * carries at most `(|a| + |b|) * 2**-24`. That is an upper bound on the
 * instrument's precision, derived from the coordinates the browser actually
 * reported -- not a chosen epsilon, and not a relaxation of the criterion.
 * At the coordinates above it is 5.8e-5 CSS px, roughly 1.8e-4 device px at
 * this profile's dpr of 3: four orders of magnitude below one device pixel,
 * and below anything that can be painted, let alone mis-tapped.
 *
 * `shortBy` travels with the report, and `width`/`height` are no longer
 * rounded to 1dp on the way out. That rounding is what let this gate print
 * `height: 44` directly beneath the words "measure under the 44px
 * coarse-pointer minimum" -- an error message contradicting its own verdict,
 * which under SH-306's doctrine is a gate a reader cannot act on.
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
      // float32 carries 24 significand bits (23 stored, plus the implicit
      // leading 1), so a correctly-rounded endpoint is within `|x| * 2**-24`
      // of the truth and a difference of two is within the sum.
      const FLOAT32_SIGNIFICAND_BITS = 24;
      const representationError = (a: number, b: number) =>
        (Math.abs(a) + Math.abs(b)) * Math.pow(2, -FLOAT32_SIGNIFICAND_BITS);
      const out: {
        describe: string;
        width: number;
        height: number;
        shortBy: number;
      }[] = [];
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
        const shortWidth = minPx - r.width - representationError(r.left, r.right);
        const shortHeight = minPx - r.height - representationError(r.top, r.bottom);
        if (shortWidth > 0 || shortHeight > 0) {
          out.push({
            describe: desc(el),
            width: r.width,
            height: r.height,
            shortBy: Math.max(shortWidth, shortHeight),
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
 * SH-442: every global draft row names its project, and a project name is
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
  test(`a global draft row elides a long project name at ${width}px`, async ({
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
    await page.locator("#new-story-btn").click();
    await page.locator("#create-title").fill(`Long-name draft ${width}`);
    await page.locator("#create-save-draft").click();
    await expect(page.locator("#create-modal")).not.toHaveClass(/open/);
    await page.locator("#drafts-btn").click();
    await expect(page.locator("#drafts-modal")).toHaveClass(/open/);

    const project = page.locator("#drafts-list .drafts-row-project");
    await expect(project).toBeVisible();
    await expect(
      project,
      ".drafts-row-project must truncate with an ellipsis, the treatment " +
        "`.projsel-label` and `.drafts-row-title` already use -- clipping a " +
        "name mid-glyph says nothing about where it was cut",
    ).toHaveCSS("text-overflow", "ellipsis");
    await expect
      .poll(
        async () =>
          page.evaluate(() => {
            const element = document.querySelector<HTMLElement>(
              "#drafts-list .drafts-row-project",
            );
            return (
              element?.isConnected === true &&
              element.scrollWidth > element.clientWidth
            );
          }),
        {
          message:
            `.drafts-row-project is not truncating a ` +
            `${LONG_PROJECT_NAME.length}-character project name at ` +
            `${width}px -- it wrapped to more lines instead, which turns ` +
            "a subject line into a title",
        },
      )
      .toBe(true);

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
    // Cleanup emits another catalog refresh. Drain the response-rewriting
    // handler before fixture teardown can dispose its fetched response.
    await page.unrouteAll({ behavior: "wait" });
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
 * needs, the same two-assertion shape SH-442 uses for `.drafts-row-project`.
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
 * SH-235 (D3, WCAG 2.2 SC 2.5.8): every `button`, link, number stepper and `select` across
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

test("every button, link and number stepper meets the coarse-pointer tap-target minimum", async ({ page }) => {
  await sweepTapTargets(page, "button, a[href], input[type=number]");
});

test("every select meets the coarse-pointer tap-target minimum", async ({ page }) => {
  await sweepTapTargets(page, "select");
});

/**
 * SH-420's behaviour fence for the representation bound, and the reason the
 * bound is falsifiable at all in this repo.
 *
 * The filed reproduction is gone: a full `mobile-webkit responsive.mobile`
 * run is green today, because ordinary layout drift moved the create modal's
 * third select off the binade boundary it happened to straddle when the bug
 * was reported. Waiting for it to drift back is not a test. So this
 * constructs the straddle instead, at the coordinates it was measured at --
 * a control whose box spans 468 to 512, the boundary between the 2**-15 and
 * 2**-14 float32 grids -- and walks the host through 64 consecutive sub-ulp
 * translations.
 *
 * Both directions are asserted, which is the whole point: the control sized
 * AT the minimum must never be reported at any of the 64 offsets, and a
 * control 1px under it must be reported at every one of them. A bound that
 * is too tight fails the first; one that swallowed real shortfalls, or that
 * had the wrong sign, fails the second.
 *
 * Against the bare `<` this replaced, the at-minimum control is reported at
 * 16 of the 64 offsets (and reads OVER the minimum at another 16), which is
 * this test's RED.
 */
test("a control sized exactly at the minimum is never reported, at any sub-pixel offset", async ({
  page,
}) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto("/");

  // 2**-15 is one ulp on the finer of the two grids the straddling box spans;
  // 64 steps of it walk two full ulps of the coarser one, so every rounding
  // phase either endpoint can take is visited.
  const ULP_AT_LOWER_BINADE = Math.pow(2, -15);
  const OFFSETS = 64;

  await page.evaluate((minPx) => {
    const host = document.createElement("div");
    host.id = "sh420-fixture";
    host.style.cssText =
      "position:fixed;left:0;top:0;width:390px;height:1px;pointer-events:none;";
    const make = (id: string, height: number) => {
      const el = document.createElement("select");
      el.id = id;
      // `top: 468px` puts the box at 468..(468+height), straddling 512 --
      // the boundary between the two float32 grids. `min-height: 0` so the
      // sheet's own `select` rule cannot raise the deliberately-short one.
      el.style.cssText =
        "position:absolute;left:0;top:468px;width:200px;min-height:0;height:" +
        height +
        "px;";
      return el;
    };
    host.appendChild(make("sh420-at-min", minPx));
    host.appendChild(make("sh420-under-min", minPx - 1));
    document.body.appendChild(host);
  }, COARSE_TAP_MIN);

  const host = page.locator("#sh420-fixture");
  const atMinReported: number[] = [];
  const underMinReported: number[] = [];
  for (let i = 0; i < OFFSETS; i++) {
    await host.evaluate((node, ty) => {
      (node as HTMLElement).style.transform = "translateY(" + ty + "px)";
    }, i * ULP_AT_LOWER_BINADE);
    const small = await findSmallTargets(host, "select", COARSE_TAP_MIN);
    const ids = small.map((t) => t.describe);
    if (ids.some((d) => d.includes("sh420-at-min"))) atMinReported.push(i);
    if (ids.some((d) => d.includes("sh420-under-min"))) underMinReported.push(i);
  }
  await host.evaluate((node) => node.remove());

  expect(
    atMinReported,
    "a control sized exactly at the " +
      COARSE_TAP_MIN +
      "px minimum was reported as under it at these sub-ulp offsets -- the " +
      "comparison is being decided by float32 noise rather than by anything a " +
      "finger could miss (SH-420)",
  ).toEqual([]);
  expect(
    underMinReported.length,
    "a control 1px under the " +
      COARSE_TAP_MIN +
      "px minimum must be reported at every offset; if it is not, the " +
      "representation bound has swallowed a real shortfall",
  ).toBe(OFFSETS);
});

/**
 * SH-420: the sweep's own positive control, planted on the surface the bug
 * was reported on -- inside the create modal, under its live
 * `translate(-50%, -50%)`, rather than in a synthetic host.
 *
 * `expectNoSmallTargets` can only ever assert an empty list, so on its own it
 * is indistinguishable from a walk that silently stopped finding anything.
 * This proves the walk still bites after the settle wait and the
 * representation bound were added: a control 1px short is caught, and the one
 * beside it sized exactly at the minimum is not.
 */
test("the tap-target sweep still catches a genuinely undersized control in the modal", async ({
  page,
}) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto("/");
  await openProject(page, "Alpha Project");
  await page.locator("#new-story-btn").click();
  await expect(page.locator("#create-modal")).toHaveClass(/open/);

  await page.evaluate((minPx) => {
    const body = document.querySelector("#create-modal .modal-body")!;
    const probes: [string, number][] = [
      ["sh420-probe-at-min", minPx],
      ["sh420-probe-under-min", minPx - 1],
    ];
    for (const [id, height] of probes) {
      const el = document.createElement("button");
      el.id = id;
      el.style.cssText = "min-height:0;height:" + height + "px;width:200px;";
      body.appendChild(el);
    }
  }, COARSE_TAP_MIN);

  const modal = page.locator("#create-modal");
  const minPx = await settleAndReadTapMin(modal, "the create-story modal");
  const reported = (await findSmallTargets(modal, "button", minPx)).map(
    (t) => t.describe,
  );

  expect(
    reported.filter((d) => d.includes("sh420-probe-under-min")).length,
    "a button 1px under the minimum, planted in the modal, must be reported",
  ).toBe(1);
  expect(
    reported.filter((d) => d.includes("sh420-probe-at-min")),
    "the button sized exactly at the minimum, planted beside it under the same " +
      "transform, must not be",
  ).toEqual([]);
});

/**
 * SH-420: the settle wait's own red-green, and the class it reaches that no
 * tolerance could.
 *
 * `.card.entering` runs `card-enter`, which interpolates from
 * `scale(0.97) translateY(-4px)`. A 44px tap target measured while its card
 * is entering reads 44 * 0.97 = 42.68 -- **1.3px** under the minimum. That is
 * forty thousand times the float32 residue the rest of this fix is about, so
 * no representation bound can absorb it; only measuring a settled card can.
 * It is a false red waiting to happen on the release tier, and the reason
 * settling is the larger half of SH-420 rather than a tidy-up beside it.
 *
 * The animation is deliberately slowed for the duration of this test rather
 * than raced at its natural 0.22s: a fixture that has to win a race against
 * a real animation is a fixture that passes vacuously whenever it loses.
 * Slowing it is the fixture's own fixed cost, paid once, and it makes both
 * directions deterministic -- the assertion below never has to guess whether
 * the card was still moving when the sweep ran, because it checks.
 */
test("a tap target inside a card that is still animating in is not reported", async ({
  page,
}) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto("/");
  await openProject(page, "Alpha Project");

  // Long enough that the sweep cannot accidentally start after the card has
  // settled, which is the only way this test could pass without meaning it.
  const SLOWED_ENTER_SECONDS = 3;
  await page.addStyleTag({
    content: `.card.entering { animation-duration: ${SLOWED_ENTER_SECONDS}s !important; }`,
  });

  const title = "SH-420 entering card";
  await createStory(page, title);

  const column = page.locator('.column[data-state="todo"]');
  // The premise, asserted rather than assumed: a card really is mid-flight,
  // and its targets really do measure under the minimum right now. Without
  // this, a future change that stopped animating cards would leave the test
  // passing while proving nothing.
  const enteringNow = await column.evaluate((node) =>
    node
      .getAnimations({ subtree: true })
      .filter((a) => a.playState === "running").length,
  );
  expect(
    enteringNow,
    "the entering card's animation must still be running when the sweep " +
      "starts, or this test proves nothing about settling",
  ).toBeGreaterThan(0);
  const midFlight = await findSmallTargets(column, "button, a[href]", COARSE_TAP_MIN);
  expect(
    midFlight.length,
    "a scaled-down card really should read as undersized mid-flight -- if it " +
      "does not, `card-enter` no longer scales and this test's premise is gone",
  ).toBeGreaterThan(0);

  // And the sweep, which settles first, sees none of it.
  await expectNoSmallTargets(column, "the board's todo column", "button, a[href]");

  await deleteStory(page, title);
});

/**
 * SH-420: the settle wait is scoped to the swept root's subtree, not to the
 * document. A toast, a card flash or a dispatch-history row animating
 * elsewhere on the page has nothing to do with whether the modal has stopped
 * moving, and a document-wide wait would hold the sweep up for it -- trading
 * a rare false red for a rare false hang.
 *
 * Asserted by state rather than by a clock: the decoy is still running after
 * the sweep returns. A document-wide wait could not produce that outcome --
 * it would have blocked until the decoy finished, or timed out.
 */
test("the settle wait ignores an animation running outside the swept surface", async ({
  page,
}) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto("/");
  await openProject(page, "Alpha Project");
  await page.locator("#new-story-btn").click();
  await expect(page.locator("#create-modal")).toHaveClass(/open/);

  // Long enough that it cannot plausibly finish during the sweep, and finite
  // so a failure leaves nothing running behind it.
  const DECOY_SECONDS = 600;
  await page.evaluate((seconds) => {
    const decoy = document.createElement("div");
    decoy.id = "sh420-decoy";
    decoy.style.cssText = "position:fixed;left:0;bottom:0;width:1px;height:1px;";
    document.body.appendChild(decoy);
    decoy.animate([{ opacity: 1 }, { opacity: 0 }], { duration: seconds * 1000 });
  }, DECOY_SECONDS);

  await expectNoSmallTargets(
    page.locator("#create-modal"),
    "the create-story modal (with an unrelated animation in flight)",
    "select",
  );

  const decoyStillRunning = await page.evaluate(
    () =>
      document
        .getElementById("sh420-decoy")!
        .getAnimations()
        .filter((a) => a.playState === "running").length,
  );
  expect(
    decoyStillRunning,
    "the sweep must not have waited for an animation outside the surface it " +
      "was measuring; if this is 0, the decoy ran to completion and the wait " +
      "is document-wide (SH-420)",
  ).toBe(1);
});

/**
 * The value `--tap-min` must resolve to under a coarse pointer. Kept as a
 * literal HERE on purpose, and only here: this is the promise itself, not a
 * tolerance around it. `tests/web_test.rs` greps the stylesheet for the two
 * declarations; `settleAndReadTapMin` below reads what a real coarse-pointer
 * engine actually computes, which the grep cannot do, and refuses to sweep
 * against anything else -- so weakening the token can never quietly weaken
 * this sweep instead of failing it.
 */
const COARSE_TAP_MIN = 44;

/**
 * Waits for `root` to stop moving, then reads the minimum to hold it to.
 *
 * SH-420: the sweep used to measure immediately after `toHaveClass(/open/)`,
 * which is a few milliseconds into a 0.15s transition -- so it walked a box
 * that was still in flight. `.modal` animates `translate(-50%, -46%)` to
 * `translate(-50%, -50%)`; `.drawer` animates `translateX(100%)` to
 * `translateX(0)` over 0.2s. Measured over six openings of the create modal
 * on `mobile-webkit` (before SH-439 added a fourth select, `#create-project`,
 * ahead of the original three and shifted every coordinate below it -- the
 * settled *property* this measurement demonstrates, not any one of its own
 * numbers, is what this comment documents): settled, every select's box sat
 * at the same coordinates run after run, every coordinate on the 1/128 grid,
 * every height exactly 44, zero variance. Mid-transition, not one coordinate
 * was dyadic, the values wandered run to run, and one iteration reproduced
 * SH-420's failure outright. An interpolated percentage is the only thing on
 * this surface that puts a non-dyadic offset into a float32 coordinate at all.
 *
 * The story had ruled this out on the grounds that the transform is a pure
 * translate and "a translation cannot change a descendant's measured
 * height". That is the wrong test: a translation is exactly what changes the
 * float32 residue of `top` and `bottom`, because it is what moves them onto
 * different rounding grids.
 *
 * This is the larger half of SH-420's fix, and it reaches a class no
 * tolerance could: `.card.entering` runs `card-enter`, which scales to 0.97,
 * so a 44px target measured inside an entering card reads ~42.68 -- 1.3px
 * under, which no float32-scale bound can absorb and which would land as a
 * false red on the release tier.
 *
 * **Scoped to `root`'s own subtree, never `document`** -- an unrelated toast
 * or card flash elsewhere on the page (0.18s-0.9s, all finite) would
 * otherwise hold up a sweep it has nothing to do with. `getAnimations` with
 * `subtree: true` includes the element itself, which is what matters here:
 * on every surface this sweeps, the transform in flight is on the swept root
 * (`#create-modal`, `#drawer`) rather than above it.
 *
 * **What this narrows, stated rather than discovered:** after settling, the
 * sweep asserts nothing about a target's size WHILE it animates. A control
 * that is undersized only mid-transition is out of coverage. That is the
 * promise this suite should be making -- a user taps a settled control --
 * but it is narrower than what the bare walk accidentally claimed.
 *
 * The residual race is real rather than hypothetical: the same measurement
 * caught five animations already running again on one iteration's settled
 * read, because the dashboard polls and re-animates cards. It is named in
 * the failure message so the next reader does not have to re-derive it.
 */
async function settleAndReadTapMin(
  root: Locator,
  surface: string,
): Promise<number> {
  await expect
    .poll(
      async () =>
        root.evaluate((node) =>
          node
            .getAnimations({ subtree: true })
            .filter((a) => a.playState === "running")
            .map((a) => {
              const effect = a.effect as KeyframeEffect | null;
              const target = effect && effect.target ? effect.target.tagName : "?";
              return `${(a as unknown as { animationName?: string }).animationName ||
                (a as unknown as { transitionProperty?: string }).transitionProperty ||
                "animation"} on ${target}`;
            }),
        ),
      {
        message:
          `${surface}: animations under this surface never settled, so the ` +
          "sweep would measure a moving box (SH-420). A live poll can restart " +
          "card animations at any moment -- if this is flaking rather than " +
          "hanging, that is the residual race, not a new defect.",
      },
    )
    .toEqual([]);

  const minPx = await root.evaluate(() =>
    parseFloat(
      getComputedStyle(document.documentElement).getPropertyValue("--tap-min"),
    ),
  );
  expect(
    minPx,
    "--tap-min must compute to the coarse-pointer minimum in a coarse-pointer " +
      "engine; the stylesheet declaring it is not evidence that it resolved",
  ).toBe(COARSE_TAP_MIN);
  return minPx;
}

/** Asserts `findSmallTargets` reports nothing under `root` for `selector`,
 * at the coarse-pointer minimum `--tap-min` actually resolves to, once
 * `root` has stopped moving (SH-420). */
async function expectNoSmallTargets(
  root: Locator,
  surface: string,
  selector: string,
): Promise<void> {
  const minPx = await settleAndReadTapMin(root, surface);
  const small = await findSmallTargets(root, selector, minPx);
  expect(
    small,
    `${surface}: these tap targets measure under the ${minPx}px coarse-pointer minimum`,
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
  // The configured state catalog can grow independently of this layout
  // contract. Wait for its second rendered column instead of coupling the
  // peek assertion to the fixture's exact number of states.
  const columns = page.locator(".column");
  await expect(columns.nth(1)).toBeVisible();

  const width = await columns
    .first()
    .evaluate((el) => el.getBoundingClientRect().width);
  expect(
    width,
    `a board column measures ${width}px at a 320px viewport -- expected ` +
      "min(18rem, 85vw) = 272px, not the unshrunk 288px ceiling",
  ).toBeCloseTo(272, 0);

  const firstRight = await columns
    .first()
    .evaluate((el) => el.getBoundingClientRect().right);
  expect(
    firstRight,
    "the first column's own right edge should leave room for the next " +
      "column to start peeking in before the 320px viewport ends",
  ).toBeLessThan(320);
  await expect(columns.nth(1)).toBeInViewport({ ratio: 0 });
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
