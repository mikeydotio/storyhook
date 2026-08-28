import { test, expect } from "./support";
import type { Locator, Page } from "@playwright/test";
import {
  cleanUpCreatedStories,
  deleteStory,
  openProject,
  openStatusesEditor,
  seedToken,
} from "./support";

/**
 * SH-256: on a coarse pointer (a finger, not a mouse), no text-entry control
 * anywhere in the dashboard may compute a font-size under 16 CSS pixels --
 * the threshold below which iOS Safari zooms the viewport to whatever field
 * was just focused, and does not zoom back out when it blurs. Also: tap
 * targets carry `touch-action: manipulation`, so a quick double tap acts
 * instead of double-tap-zooming.
 *
 * This file runs under both mobile Playwright projects, `mobile-chromium`
 * and `mobile-webkit` (`playwright.config.ts`'s `MOBILE_SPECS` pattern,
 * matched by this file's own `.mobile.spec.ts` suffix) -- never under either
 * desktop project, because headless desktop Chromium and WebKit both report
 * `pointer: fine` and the very first test below would fail there by design.
 * `devices["Pixel 7"]` is Blink under mobile emulation; `devices["iPhone
 * 15"]` (SH-348) is WebKit under mobile emulation -- iOS Safari's 16px rule
 * is WebKit's own, and until SH-348 this file could only simulate a
 * coarse-pointer environment on the engine that doesn't have the rule.
 * What this file verifies, on either engine, is that the *mechanism* (the
 * `--control-font-*` tokens, raised under `@media (pointer: coarse)`) fires
 * in a genuinely coarse-pointer environment and reaches every control --
 * not that a phone would actually stay unzoomed, which only a real device
 * can prove (see the plan's verification section).
 *
 * The drawer and delete-modal tests create their own disposable story
 * rather than reaching for "Wire up the auth flow", Alpha Project's other
 * seeded fixture: `dispatch.spec.ts` runs a real dispatch against that
 * exact story (alphabetically before this file, so always first), which
 * moves it out of the `todo` column this file needs it in. A shared,
 * mutated-by-someone-else story is exactly what SH-245 already burned this
 * suite on once (see support.ts's own comment on `cleanUpCreatedStories`).
 */

/**
 * Input `type`s that never raise a keyboard and never make WebKit zoom.
 * An exclusion list, not an inclusion list, deliberately: `date`, `number`,
 * `email`, `tel`, `search`, `password` and every other text-shaped type
 * zoom exactly like `text`, and a new one should be caught by this sweep
 * the day it's introduced rather than the day someone remembers to list it.
 */
const NON_TEXT_INPUT_TYPES = [
  "button",
  "checkbox",
  "color",
  "file",
  "hidden",
  "image",
  "radio",
  "range",
  "reset",
  "submit",
];

interface MeasuredControl {
  describe: string;
  fontSizePx: number;
}

/**
 * Every text-entry control under `root`, with its computed font size.
 *
 * Filters on the `.type` IDL property, never a `[type=text]` CSS attribute
 * selector: `.drawer-title` (`web_dashboard.html`'s `buildTitleField`) is an
 * `<input>` with no `type` attribute at all, which the browser still
 * reports as `type === "text"` -- an attribute selector would silently
 * skip the field a reader edits most often.
 *
 * `renderedOnly: false` measures controls with no layout box too, which is
 * the point for the whole-document sweep (see the first test below): a
 * closed `.modal` is `display: none` via `[hidden]` but its font-size is
 * exactly as wrong when closed as when open, and `getComputedStyle`
 * resolves custom properties regardless of visibility.
 */
async function measureControls(
  root: Locator,
  renderedOnly: boolean,
): Promise<MeasuredControl[]> {
  return root.evaluate(
    (node, { renderedOnly, excluded }) => {
      const els = Array.from(
        node.querySelectorAll("input, select, textarea"),
      ) as Array<HTMLInputElement | HTMLSelectElement | HTMLTextAreaElement>;
      const out: { describe: string; fontSizePx: number }[] = [];
      for (const el of els) {
        if (el instanceof HTMLInputElement && excluded.includes(el.type)) {
          continue;
        }
        if (renderedOnly && el.getClientRects().length === 0) continue;
        const idPart = el.id ? `#${el.id}` : "";
        const classPart = el.className
          ? `.${String(el.className).split(/\s+/).join(".")}`
          : "";
        const placeholderPart =
          "placeholder" in el && el.placeholder ? ` [${el.placeholder}]` : "";
        out.push({
          describe: `${el.tagName.toLowerCase()}${idPart}${classPart}${placeholderPart}`,
          fontSizePx: parseFloat(getComputedStyle(el).fontSize),
        });
      }
      return out;
    },
    { renderedOnly, excluded: NON_TEXT_INPUT_TYPES },
  );
}

/**
 * Asserts every text-entry control under `root` computes at or above the
 * 16px threshold -- and that there were at least `atLeast` of them to make
 * that assertion about.
 *
 * The count check is not decoration: a renamed id, a surface that failed
 * to open, or a selector that stopped matching turns a font-size sweep
 * from a real assertion into a silently passing no-op, which is the one
 * failure mode this kind of test cannot survive.
 */
async function expectNoZoomingControls(
  root: Locator,
  surface: string,
  atLeast: number,
  renderedOnly = true,
): Promise<void> {
  const measured = await measureControls(root, renderedOnly);
  expect(
    measured.length,
    `${surface}: expected at least ${atLeast} text-entry control(s), found ` +
      `${measured.length} -- either the surface didn't render, or its ` +
      "controls moved, so this assertion would be measuring nothing",
  ).toBeGreaterThanOrEqual(atLeast);

  const tooSmall = measured.filter((c) => c.fontSizePx < 16);
  expect(
    tooSmall,
    `${surface}: these controls compute under 16 CSS pixels, so focusing ` +
      "one zooms the viewport on iOS Safari -- and the zoom is not undone " +
      "when the field blurs",
  ).toEqual([]);
}

cleanUpCreatedStories("Alpha Project");

test.beforeEach(async ({ page }: { page: Page }) => {
  await seedToken(page);
});

/**
 * Creates a story in Alpha Project's `todo` column, same as the local
 * helper drawer-detail-race.spec.ts and fixture-isolation.spec.ts each
 * define -- default state/type, so it lands in `todo` unblocked.
 */
async function createStory(page: Page, title: string): Promise<void> {
  await page.locator("#new-story-btn").click();
  await expect(page.locator("#create-modal")).toHaveClass(/open/);
  await page.locator("#create-title").fill(title);
  // Keep this fixture's priority explicit.
  await page.locator("#create-priority").selectOption("medium");
  await page.locator("#create-submit").click();
  await expect(page.locator("#create-modal")).not.toHaveClass(/open/);
  await expect(
    page.locator('.column[data-state="todo"] .card', { hasText: title }),
  ).toBeVisible();
}

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

test("every text-entry control the document ships with is at least 16px", async ({
  page,
}) => {
  await page.goto("/");
  // renderedOnly: false -- this is what covers the token modal and the
  // drop-blocked-reason modal without needing to contort either open on a
  // phone (the latter needs a drag onto a Blocked column).
  await expectNoZoomingControls(
    page.locator("body"),
    "the static document",
    9,
    false,
  );
});

test("the topbar's search field is at least 16px", async ({ page }) => {
  await page.goto("/");
  await openProject(page, "Alpha Project");

  await expectNoZoomingControls(page.locator(".topbar"), "the topbar", 1);

  // The filter bar is checkbox-only today (Show closed / Show archived /
  // Hide empty columns), so it should measure empty -- a text filter added
  // there in future must join this sweep, not slip past it silently.
  const filterBarControls = await measureControls(
    page.locator("#filter-bar"),
    true,
  );
  expect(
    filterBarControls,
    "the filter bar grew a text-entry control that isn't covered by this sweep yet",
  ).toEqual([]);
});

test("the create-story modal's controls are at least 16px", async ({
  page,
}) => {
  await page.goto("/");
  await openProject(page, "Alpha Project");

  await page.locator("#new-story-btn").click();
  await expect(page.locator("#create-modal")).toHaveClass(/open/);

  // Project (SH-439), Title, description, State/Type/Priority selects, and
  // the label combobox's own text input.
  await expectNoZoomingControls(
    page.locator("#create-modal"),
    "the create-story modal",
    7,
  );

  await page.locator("#create-discard").click();
  await expect(page.locator("#create-modal")).not.toHaveClass(/open/);
});

test("the story drawer's controls are at least 16px", async ({ page }) => {
  const title = "SH-256 zoom sweep — drawer";
  await page.goto("/");
  await openProject(page, "Alpha Project");
  await createStory(page, title);

  const detailLoaded = page.waitForResponse(
    (resp) =>
      /\/story\/[^/]+$/.test(new URL(resp.url()).pathname) &&
      resp.request().method() === "GET",
  );
  await page
    .locator('.column[data-state="todo"] .card', { hasText: title })
    .click();
  await expect(page.locator("#drawer")).toHaveClass(/open/);
  // The drawer renders once synchronously, then again when the detail GET
  // resolves (SH-218) -- measuring before that second render lands would
  // measure a body about to be replaced.
  await detailLoaded;

  // SH-217: .description-field is display:none outside edit mode, and
  // measureControls() skips anything with no client rects -- entering
  // edit mode first keeps it in the sweep rather than silently dropping
  // it, the same floor this measurement pinned before the read/edit
  // split existed.
  await page.locator(".description-view").click();

  // .drawer-title, 4 .field selects (State/Type/Priority/Assignee),
  // .description-field, the block-reason input, the label-add input, the
  // relation select + id input, and the comment textarea. Relationships
  // and Comments default open, and a freshly created story is unblocked,
  // so no expanding clicks are needed to see all of them.
  await expectNoZoomingControls(
    page.locator("#drawer-body"),
    "the story drawer",
    11,
  );

  await page.locator("#drawer-close").click();
  await expect(page.locator("#drawer")).not.toHaveClass(/open/);
  await deleteStory(page, title);
});

test("the delete-confirmation modal's id field is at least 16px", async ({
  page,
}) => {
  const title = "SH-256 zoom sweep — delete modal";
  await page.goto("/");
  await openProject(page, "Alpha Project");
  await createStory(page, title);

  await page
    .locator('.column[data-state="todo"] .card', { hasText: title })
    .click();
  await expect(page.locator("#drawer")).toHaveClass(/open/);
  await page.locator("#drawer-footer button", { hasText: "Delete" }).click();
  await expect(page.locator("#delete-modal")).toHaveClass(/open/);

  await expectNoZoomingControls(
    page.locator("#delete-modal"),
    "the delete-confirmation modal",
    1,
  );

  // Cancel here, to keep this test itself read-only; the story still gets
  // torn down below, through the same modal it just proved is sized right.
  await page.locator("#delete-modal-cancel").click();
  await expect(page.locator("#delete-modal")).not.toHaveClass(/open/);
  await expect(page.locator("#drawer")).toHaveClass(/open/);

  // deleteStory() assumes a clean starting state; the drawer Cancel left
  // open would otherwise block its own card click behind #drawer-backdrop
  // (same reasoning as delete-story.spec.ts's own Cancel test).
  await page.locator("#drawer-close").click();
  await expect(page.locator("#drawer")).not.toHaveClass(/open/);
  await deleteStory(page, title);
});

test("the settings project form's controls are at least 16px", async ({
  page,
}) => {
  await page.goto("/");
  await page.locator("#settings-btn").click();
  await expect(page.locator("#settings-view")).toBeVisible();

  // Path, display name, and story-id prefix.
  await expectNoZoomingControls(
    page.locator(".settings-form"),
    "the settings project form",
    3,
  );
});

test("the statuses editor's controls are at least 16px", async ({ page }) => {
  await page.goto("/");
  await openStatusesEditor(page, "Alpha Project");
  await expect(page.locator(".status-list")).toBeVisible();

  // Two roots rather than one row's worth: a floor, not an exact count, so
  // this stays correct however many statuses run-e2e.sh seeds Alpha with.
  // Read-only throughout -- focusing a control inside .status-row or
  // .status-add flips statusEditorIsBusy() and suppresses re-render, which
  // this spec has no reason to provoke.
  await expectNoZoomingControls(page.locator(".status-list"), "the statuses list", 3);
  await expectNoZoomingControls(page.locator(".status-add"), "the add-status row", 3);
});

test("tap targets carry touch-action: manipulation, and the page itself does not", async ({
  page,
}) => {
  await page.goto("/");
  await openProject(page, "Alpha Project");

  // This asserts the CSS *declaration*, not the gesture it produces:
  // Playwright's locator.click() injects mouse events even with
  // `hasTouch: true`, so no double-tap is actually performed here. The
  // behavioral claim -- that a double tap acts instead of zooming -- rests
  // on the CSS spec for `touch-action: manipulation`, not on this test.
  // (`.fdd-option`, the fourth selector in the rule, is a lazily-built
  // popover row not reliably present without opening a dropdown -- its
  // declaration is still pinned statically, in web_test.rs.)
  for (const selector of ["#new-story-btn", ".card", ".filter-toggle"]) {
    const locator = page.locator(selector).first();
    await expect(locator).toHaveCSS("touch-action", "manipulation");
  }

  await expect(page.locator("body")).toHaveCSS("touch-action", "auto");
  await expect(page.locator("html")).toHaveCSS("touch-action", "auto");
});
