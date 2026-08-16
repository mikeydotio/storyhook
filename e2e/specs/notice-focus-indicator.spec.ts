import { test, expect } from "@playwright/test";
import type { Page } from "@playwright/test";
import {
  cleanUpCreatedStories,
  createStory,
  keepNotices,
  openProject,
  raiseDurableNotices,
  seedToken,
} from "./support";

/**
 * SH-338 — the notice dock's focus indicators, as a measured contrast ratio.
 *
 * A third notice-dock file, and the split is the same one
 * `notice-dock-geometry.spec.ts` already drew against
 * `notification-contract.spec.ts`: semantics and timing there, rects and hit
 * tests in geometry, **colour** here. These fail for different reasons on
 * purpose, and this one is the only one that fails when a token changes value.
 *
 * ## What is under test
 *
 * `.toast-dismiss` and `.dispatch-history-dismiss` are the two per-notice
 * dismiss controls, and since SH-326 they are also a focus **landing site**:
 * dismissing one notice moves focus to the next notice's `×`. The ring is then
 * the only signal that focus moved at all, on the exact gesture that fix exists
 * to serve — so a focus fix whose focus indicator is unmeasured is half a fix.
 * Both are asserted here: the control a keyboard user *reaches*, and the control
 * that *inherits*.
 *
 * ## Why this measures rather than asserts the CSS
 *
 * `notice-dock-geometry.spec.ts`'s doc comment argues the general case — a
 * static token offset reads correct and resolves wrong — and it applies with
 * more force to colour than to geometry, because a colour token is *four*
 * declarations in this file (`:root`, the `prefers-color-scheme: dark` media
 * block, and one `[data-theme]` block each) and nothing but a measurement can
 * tell which one a given viewer resolved. `getComputedStyle` is read for the
 * resolved `outline-color`, and the backdrop is **composited from the real
 * ancestor chain** rather than assumed to be `--bg-raised`: the button's own
 * background is `transparent`, so what is actually behind the ring is whatever
 * the notice row happens to paint, which is the thing that can change without
 * anyone editing this rule.
 *
 * ## The two numbers, and where they come from
 *
 * **3:1** is WCAG 2.2 SC 1.4.11 (Non-text Contrast, AA), which is the criterion
 * the story cites. **2 CSS pixels** is SC 2.4.13's minimum perimeter thickness;
 * it is asserted because contrast alone blesses a hairline nobody can see, and
 * because 2px is what `.notice-scroll`, `.card`, `tbody tr` and
 * `.description-view` already use — the value is this file's own convention, not
 * a number invented here.
 *
 * What the fix actually measures, recorded so a future reader knows the
 * headroom rather than only that the floor held: `--accent` on `--bg-raised` is
 * **5.12:1** light (`#3b5bfd` on `#ffffff`) and **4.82:1** dark (`#5b7cff` on
 * `#161922`). Both numbers are the test's own output, not arithmetic done here,
 * and neither is asserted — a palette change that kept 3:1 is allowed to move
 * them.
 *
 * A user-agent `outline-style: auto` ring fails rather than passing. That is
 * deliberate and it is the defect as filed: Chromium's default ring may well be
 * perfectly visible, but it exposes no author-declared colour, so its contrast
 * against this page's backdrop is *unknown* rather than merely unstated — and an
 * unknown is exactly what this project's own doctrine refuses to read as a pass.
 *
 * ## Four themes, not two
 *
 * The `prefers-color-scheme` pair is the only one a user can reach today;
 * nothing in the page sets `data-theme`. The attribute pair is measured anyway,
 * with the media query set to the *opposite* scheme, because those blocks are a
 * second hand-maintained copy of the same tokens and the cross-setting is what
 * proves each measurement read the copy it names. Duplicated constants drifting
 * apart unobserved is a failure this project has already paid for four times
 * (SH-136, SH-258, SH-198, SH-260/276); here it costs one `page.evaluate` to
 * fence.
 */

cleanUpCreatedStories("Alpha Project");

test.beforeEach(async ({ page, context }) => {
  // The Copy-* paths raise an ERROR notice via `copyText`'s `.catch` branch
  // without clipboard permission. An error is durable either way, so this is
  // not load-bearing here the way it is in the geometry spec — but a notice
  // whose variant is an accident is a notice whose `border-left-color` is an
  // accident too, and this file measures colour.
  await context.grantPermissions(["clipboard-read", "clipboard-write"]);
  await seedToken(page);
});

/** A colour with a straight (non-premultiplied) alpha, in sRGB 0-255. */
type Rgba = { r: number; g: number; b: number; a: number };

/** Everything the browser will say about one element's focus indicator. */
type IndicatorProbe = {
  focusVisible: boolean;
  outlineStyle: string;
  outlineWidth: string;
  outlineColor: string;
  /** `background-color` from the element outward to `<html>`, in that order. */
  backgrounds: string[];
  /** For failure messages: what the probe actually landed on. */
  what: string;
};

/** Reads the rendered indicator off whatever `selector` resolves to.
 *
 * `":focus"` is a legal argument and is how the inheritance tests name their
 * subject: the heir is chosen by `adjacentNoticeControl` at dismissal time, so
 * naming it by identity here would restate the implementation instead of asking
 * the page where focus actually is. */
async function probeIndicator(page: Page, selector: string): Promise<IndicatorProbe | null> {
  return page.evaluate((sel) => {
    const el = document.querySelector(sel) as HTMLElement | null;
    if (!el) return null;
    const cs = getComputedStyle(el);
    const backgrounds: string[] = [];
    for (let n: Element | null = el; n; n = n.parentElement) {
      backgrounds.push(getComputedStyle(n).backgroundColor);
    }
    const cls = typeof el.className === "string" && el.className ? `.${el.className.split(" ").join(".")}` : "";
    return {
      focusVisible: el.matches(":focus-visible"),
      outlineStyle: cs.outlineStyle,
      outlineWidth: cs.outlineWidth,
      outlineColor: cs.outlineColor,
      backgrounds,
      what: `${el.tagName.toLowerCase()}${el.id ? `#${el.id}` : ""}${cls}`,
    };
  }, selector);
}

/** Parses a computed colour string. Chromium serialises every computed
 * `<color>` as `rgb(r, g, b)` or `rgba(r, g, b, a)`, so the numbers are taken
 * positionally; anything else is a change in the platform rather than in this
 * page, and throwing names it instead of silently scoring it as black. */
function parseColor(css: string): Rgba {
  const nums = css.match(/-?[\d.]+/g);
  if (!nums || nums.length < 3) throw new Error(`unparseable computed colour: ${css}`);
  return {
    r: Number(nums[0]),
    g: Number(nums[1]),
    b: Number(nums[2]),
    a: nums.length > 3 ? Number(nums[3]) : 1,
  };
}

/** `fg` painted over `bg`, source-over. */
function over(fg: Rgba, bg: Rgba): Rgba {
  const a = fg.a + bg.a * (1 - fg.a);
  if (a === 0) return { r: 0, g: 0, b: 0, a: 0 };
  const mix = (f: number, b: number) => (f * fg.a + b * bg.a * (1 - fg.a)) / a;
  return { r: mix(fg.r, bg.r), g: mix(fg.g, bg.g), b: mix(fg.b, bg.b), a };
}

/** What is actually painted behind the outline: the ancestor chain composited
 * from the canvas up.
 *
 * Starts on opaque white because that is the initial canvas colour when nothing
 * in the chain is opaque. In this page `body` always is, so the seed never
 * decides an assertion — it is there so a page that changed that fact would
 * still produce a defined number rather than a transparent one. */
function backdropOf(backgrounds: string[]): Rgba {
  let base: Rgba = { r: 255, g: 255, b: 255, a: 1 };
  for (let i = backgrounds.length - 1; i >= 0; i--) {
    base = over(parseColor(backgrounds[i]), base);
  }
  return base;
}

/** WCAG 2.x relative luminance. */
function relativeLuminance({ r, g, b }: Rgba): number {
  const lin = (v: number) => {
    const c = v / 255;
    return c <= 0.03928 ? c / 12.92 : Math.pow((c + 0.055) / 1.055, 2.4);
  };
  return 0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b);
}

/** WCAG 2.x contrast ratio between two opaque colours. */
function contrastRatio(a: Rgba, b: Rgba): number {
  const la = relativeLuminance(a);
  const lb = relativeLuminance(b);
  return (Math.max(la, lb) + 0.05) / (Math.min(la, lb) + 0.05);
}

function show({ r, g, b, a }: Rgba): string {
  return a === 1 ? `rgb(${r}, ${g}, ${b})` : `rgba(${r}, ${g}, ${b}, ${a})`;
}

/** WCAG SC 1.4.11's floor for a non-text indicator. */
const MIN_CONTRAST = 3;
/** WCAG SC 2.4.13's floor for indicator thickness, in CSS pixels. */
const MIN_OUTLINE_PX = 2;

/** The whole assertion: `selector` is focus-visible, draws an author-declared
 * ring at least `MIN_OUTLINE_PX` thick, and that ring clears `MIN_CONTRAST`
 * against the backdrop the page actually painted behind it.
 *
 * **Soft, and that is the point of calling it in a loop.** A hard assertion
 * stops at the first theme that fails, so a ring broken in three of the four
 * palettes reports as one failure, gets "fixed", and fails again on the next
 * run — twice. The four measurements are independent questions about
 * independent copies of the same token, so all four are asked and all four are
 * reported; Playwright still fails the test at the end. Only the null check is
 * hard, because there is nothing to measure past it. */
async function expectMeasuredFocusRing(page: Page, selector: string, where: string): Promise<void> {
  const probe = await probeIndicator(page, selector);
  expect(probe, `${where}: nothing matches ${selector}`).not.toBeNull();
  const p = probe!;

  expect.soft(
    p.focusVisible,
    `${where}: ${p.what} holds focus but does not match :focus-visible, so no ring is drawn at all`,
  ).toBe(true);

  expect.soft(
    ["none", "hidden", "auto"],
    `${where}: ${p.what} draws outline-style: ${p.outlineStyle}. ` +
      `"auto" is the user agent's own ring, which declares no colour this page can measure — ` +
      `its contrast is unknown rather than merely unstated, which is the defect SH-338 was filed for. ` +
      `"none"/"hidden" is no ring. Declare one, as .notice-scroll does.`,
  ).not.toContain(p.outlineStyle);

  const width = parseFloat(p.outlineWidth);
  expect.soft(
    width,
    `${where}: ${p.what}'s ring is ${p.outlineWidth}; SC 2.4.13 wants at least ${MIN_OUTLINE_PX} CSS px`,
  ).toBeGreaterThanOrEqual(MIN_OUTLINE_PX);

  // Skipped when there is no author colour to composite: `outline-style: auto`
  // has already been reported above, and scoring the user agent's placeholder
  // colour would add a second, misleading number to that report.
  if (p.outlineStyle === "auto") return;
  const backdrop = backdropOf(p.backgrounds);
  const ink = over(parseColor(p.outlineColor), backdrop);
  const ratio = contrastRatio(ink, backdrop);
  expect.soft(
    ratio,
    `${where}: ${p.what}'s ring is ${show(ink)} on ${show(backdrop)} — ` +
      `${ratio.toFixed(2)}:1, below SC 1.4.11's ${MIN_CONTRAST}:1`,
  ).toBeGreaterThanOrEqual(MIN_CONTRAST);
}

/** The four ways this page can resolve its palette.
 *
 * The `[data-theme]` pair deliberately sets the media query to the *opposite*
 * scheme: without that, an attribute measurement could be reading the media
 * block's copy of the tokens and nobody would know. */
const THEMES: { name: string; apply: (page: Page) => Promise<void> }[] = [
  {
    name: "prefers-color-scheme: light",
    apply: async (page) => {
      await page.emulateMedia({ colorScheme: "light" });
      await page.evaluate(() => document.documentElement.removeAttribute("data-theme"));
    },
  },
  {
    name: "prefers-color-scheme: dark",
    apply: async (page) => {
      await page.emulateMedia({ colorScheme: "dark" });
      await page.evaluate(() => document.documentElement.removeAttribute("data-theme"));
    },
  },
  {
    name: 'data-theme="light" over a dark media query',
    apply: async (page) => {
      await page.emulateMedia({ colorScheme: "dark" });
      await page.evaluate(() => document.documentElement.setAttribute("data-theme", "light"));
    },
  },
  {
    name: 'data-theme="dark" over a light media query',
    apply: async (page) => {
      await page.emulateMedia({ colorScheme: "light" });
      await page.evaluate(() => document.documentElement.setAttribute("data-theme", "dark"));
    },
  },
];

/** Walks focus onto `selector` with real Tab presses, starting from `from`.
 *
 * Tab rather than `.focus()`, and it is not fussiness: `:focus-visible` is a
 * statement about *how* focus arrived, so a programmatic focus can leave the
 * ring undrawn on exactly the control this file is measuring. The seed focus on
 * `from` is programmatic, but nothing is measured there — the first Tab after it
 * is what establishes keyboard modality for everything that follows. */
async function tabOnto(page: Page, from: string, selector: string): Promise<void> {
  await page.locator(from).focus();
  for (let i = 0; i < 20; i++) {
    await page.keyboard.press("Tab");
    const there = await page.evaluate(
      (sel) => !!(document.activeElement as HTMLElement | null)?.matches(sel),
      selector,
    );
    if (there) return;
  }
  const stuck = await page.evaluate(() => {
    const el = document.activeElement as HTMLElement | null;
    return el ? `${el.tagName.toLowerCase()}${el.id ? `#${el.id}` : ""}` : "nothing";
  });
  throw new Error(`20 Tabs from ${from} never reached ${selector}; focus is on ${stuck}`);
}

/** Two durable dispatch-history rows, from two distinct stubbed outcomes.
 *
 * Keyed on the story id in the request path so both rows are real, separate
 * outcomes rather than one row rendered twice — the same stub
 * `notice-dock-geometry.spec.ts` uses, and `refused` because SH-304 narrowed
 * this surface to the outcomes that leave no other trace. */
async function raiseDispatchHistoryRows(page: Page, ids: string[]): Promise<void> {
  await page.route("**/dispatch**", async (route) => {
    const url = route.request().url();
    const which = ids.find((id) => url.includes(id)) ?? ids[0];
    await route.fulfill({
      status: route.request().method() === "POST" ? 202 : 200,
      contentType: "application/json",
      body: JSON.stringify({
        result: "ok",
        dispatch: {
          handle: "stub-" + which,
          project: "alpha",
          story: which,
          auto: true,
          state: "refused",
          reason: "claim-conflict",
          started_at: "2026-01-01T00:00:00Z",
          finished_at: "2026-01-01T00:00:01Z",
          payload: { display: "[story] refused: already in-progress" },
        },
      }),
    });
  });

  for (const id of ids) {
    await page.locator(`.card[data-id="${id}"]`).click();
    await expect(page.locator("#drawer")).toHaveClass(/open/);
    await page.locator("#dispatch-auto-btn").click();
    await expect(
      page.locator("#dispatch-history .dispatch-history-row", { hasText: id }),
    ).toBeVisible();
    await page.locator("#drawer-close").click();
    await expect(page.locator("#drawer")).not.toHaveClass(/open/);
  }
}

test("a keyboard-reached toast dismiss draws a measured ring in every theme", async ({ page }) => {
  await page.goto("/");
  await openProject(page, "Alpha Project");
  await keepNotices(page);
  await openProject(page, "Alpha Project");

  const title = "SH-338 — toast ring";
  await createStory(page, title);
  // Two, so `#toast-dismiss-all` is above the threshold and can seed the walk.
  await raiseDurableNotices(page, title, 2);

  await tabOnto(page, "#toast-dismiss-all", ".toast-dismiss");

  for (const theme of THEMES) {
    await theme.apply(page);
    await expectMeasuredFocusRing(page, ".toast-dismiss:focus", `a toast's × under ${theme.name}`);
  }
});

test("the heir of a keyboard dismissal draws the same ring", async ({ page }) => {
  await page.goto("/");
  await openProject(page, "Alpha Project");
  await keepNotices(page);
  await openProject(page, "Alpha Project");

  const title = "SH-338 — the heir's ring";
  await createStory(page, title);
  await raiseDurableNotices(page, title, 3);

  await tabOnto(page, "#toast-dismiss-all", ".toast-dismiss");
  await page.keyboard.press("Enter");
  await expect(page.locator("#toast-stack .toast")).toHaveCount(2);

  // Named as ":focus", not as an identity. SH-326 chooses the heir at dismissal
  // time and `notice-dock-geometry.spec.ts` already pins *which* control that
  // is; what this file adds is that wherever focus landed, it is visible. The
  // guard against a vacuous pass is the class check: a landing on `<body>` or on
  // the anchor would not match `.toast-dismiss`.
  const landed = await page.evaluate(
    () => !!(document.activeElement as HTMLElement | null)?.classList.contains("toast-dismiss"),
  );
  expect(landed, "the heir of a keyboard dismissal must be another toast's ×").toBe(true);

  for (const theme of THEMES) {
    await theme.apply(page);
    await expectMeasuredFocusRing(page, ".toast-dismiss:focus", `the heir under ${theme.name}`);
  }
});

test("a dispatch-history dismiss draws a measured ring, reached and inherited", async ({ page }) => {
  await page.goto("/");
  await openProject(page, "Alpha Project");

  const first = await createStory(page, "SH-338 — history ring A");
  const second = await createStory(page, "SH-338 — history ring B");
  await raiseDispatchHistoryRows(page, [first, second]);
  await expect(page.locator("#dispatch-history .dispatch-history-row")).toHaveCount(2);

  await tabOnto(page, "#dispatch-history-dismiss-all", ".dispatch-history-dismiss");
  for (const theme of THEMES) {
    await theme.apply(page);
    await expectMeasuredFocusRing(
      page,
      ".dispatch-history-dismiss:focus",
      `a history row's × under ${theme.name}`,
    );
  }

  // The same inheritance SH-326 wired on this surface. Measured here too rather
  // than assumed from the toast test: the two controls share one CSS rule today,
  // and this is the assertion that fails if someone ever splits it.
  await page.keyboard.press("Enter");
  await expect(page.locator("#dispatch-history .dispatch-history-row")).toHaveCount(1);
  const landed = await page.evaluate(
    () =>
      !!(document.activeElement as HTMLElement | null)?.classList.contains(
        "dispatch-history-dismiss",
      ),
  );
  expect(landed, "the heir of a keyboard dismissal must be another row's ×").toBe(true);
  await expectMeasuredFocusRing(
    page,
    ".dispatch-history-dismiss:focus",
    "the history heir",
  );
});
