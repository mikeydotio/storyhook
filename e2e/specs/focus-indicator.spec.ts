import { test, expect } from "./support";
import {
  cleanUpCreatedStories,
  COPY_TARGETS,
  createStory,
  keepNotices,
  measureFocusIndicator,
  openProject,
  raiseNotice,
  seedToken,
  tabOnto,
} from "./support";

/**
 * SH-360 -- the dashboard's other seven focus indicators, measured.
 *
 * SH-338 built `measureFocusIndicator` (lifted into `support.ts` ahead of
 * this file) and pointed it at exactly two controls: `.toast-dismiss` and
 * `.dispatch-history-dismiss`. `src/web_dashboard.html` declares seven more
 * author-declared focus indicators that nothing measured before this file:
 *
 *   .card, tbody tr, .description-view, .notice-scroll  -- outline kind
 *   .search-input, .drawer-title, .description-field    -- border-swap kind
 *
 * Every one of them is measured here, in all four theme resolutions, the
 * same instrument SH-338 built (real Tab presses, composited backdrop,
 * `getComputedStyle`, never a hand-computed number). `.btn` and
 * `.ctxmenu-item` take the user agent's own ring deliberately and
 * page-wide; they declare no focus rule at all, so they are out of this
 * story's scope and structurally invisible to the derived fence in
 * `tests/dashboard_focus_coverage.rs`.
 *
 * ## Stated limits
 *
 * This instrument enumerates DECLARED focus rules -- it cannot find SH-338's
 * own defect class, a focusable control with no rule at all, because such a
 * control simply never appears in the derived set this file's coverage is
 * checked against. Closing that gap is a different instrument (a live
 * DOM/tabindex walk) and a different story.
 *
 * The backdrop model composites ancestor BACKGROUNDS only -- no sibling or
 * descendant paint that happens to sit under a ring (`tbody td`'s own
 * bottom border sits under `tbody tr`'s inset ring; still clears comfortably,
 * but the model doesn't know that), and no `box-shadow` (`.card` casts one
 * into the exact band its outset ring occupies). A pixel-accurate model
 * needs a screenshot decoder, a new dependency this suite does not carry.
 *
 * `data-theme` is unreachable in the shipped product -- nothing sets it. The
 * two attribute blocks are measured anyway because they are a second
 * hand-maintained copy of the same palette (SH-338's own reasoning, restated
 * on `THEMES` in `support.ts`).
 *
 * SC 2.4.13 (AAA) is RECORDED for the border-swap trio, never enforced: the
 * tightest case is `.description-field`'s focused border against its own
 * unfocused border, ~3.03:1 in dark theme -- 1% of headroom on a clause the
 * story already declined to gate (the same trio's 1px thickness, also SC
 * 2.4.13). A future drop below 3 there is its own AAA finding, not a
 * failure of this instrument.
 */

cleanUpCreatedStories("Alpha Project");

test.beforeEach(async ({ page }) => {
  await seedToken(page);
});

test("the board's own focus indicators, in every theme", async ({ page }) => {
  await page.goto("/");
  await openProject(page, "Alpha Project");

  // Topbar DOM order: brand -> #projsel-btn -> #subtitle (a bare <span>,
  // not a tab stop) -> .search-wrap (its <svg> is pointer-events: none and
  // not tabbable either) -> #search-input. One real Tab away.
  await measureFocusIndicator(page, ".search-input:focus", "the search box", () =>
    tabOnto(page, "#projsel-btn", ".search-input"),
  );

  // Alpha's two seeded stories are enough; syncRoving keeps exactly one
  // card at tabindex="0" (SH-197), the one this walk lands on. The bare
  // `.card` selector (not `.card[tabindex='0']`) is deliberate: it is the
  // CSS rule's own base identity, which is what
  // tests/dashboard_focus_coverage.rs derives its coverage set from --
  // `:focus`/`Element.matches()` already disambiguate to whichever one
  // element actually holds focus, so no extra qualifier is needed.
  await measureFocusIndicator(page, ".card:focus", "a board card", () =>
    tabOnto(page, "#search-input", ".card"),
  );
});

test("the list view's own focus indicator, in every theme", async ({ page }) => {
  await page.goto("/");
  await openProject(page, "Alpha Project");
  await page.locator('#view-toggle [data-view="list"]').click();

  // Same roving-tabindex mechanism as the board, bound separately for the
  // list (syncRoving(body, "tr[data-id]", ...)). "tbody tr", not a bare
  // qualifier, matches the CSS rule's own base identity -- see the board
  // test's comment above for why no extra qualifier is needed.
  await measureFocusIndicator(page, "tbody tr:focus", "a list row", () =>
    tabOnto(page, "#search-input", "tbody tr"),
  );
});

test("the drawer's three focus indicators, in every theme", async ({ page }) => {
  await page.goto("/");
  await openProject(page, "Alpha Project");
  await page.locator(".card").first().click();
  await expect(page.locator("#drawer")).toHaveClass(/open/);

  // .drawer-title is the FIRST child appended to #drawer-body, immediately
  // after #drawer-close in the drawer's own DOM order.
  await measureFocusIndicator(page, ".drawer-title:focus", "the drawer title", () =>
    tabOnto(page, "#drawer-close", ".drawer-title"),
  );

  // The same precedent description-edit-mode.spec.ts:167 uses to reach the
  // description view: one Tab from the field grid's fourth <select>.
  await measureFocusIndicator(page, ".description-view:focus", "the description view", () =>
    tabOnto(page, "#drawer-body select >> nth=3", ".description-view"),
  );

  // enterEdit()'s own path: Enter on the already-focused description view
  // opens the editor and focuses the textarea programmatically. Reached
  // this way (not by Tab) deliberately -- SH-217's own mechanism -- and
  // still asserted as :focus-visible, universally: a <textarea> matches
  // :focus-visible on ANY focus, programmatic included, in every engine
  // this suite runs, unlike a <button> or a bare-div `[tabindex]`.
  await measureFocusIndicator(page, ".description-field:focus", "the description field", () =>
    page.keyboard.press("Enter"),
  );

  // Escape reverts rather than saves (nothing was typed, so either is
  // harmless, but this is the documented no-op exit -- description-edit-
  // mode.spec.ts:149 uses the same key for the same reason) before closing,
  // so this test leaves Alpha's fixture story exactly as it found it.
  await page.keyboard.press("Escape");
  await page.locator("#drawer-close").click();
  await expect(page.locator("#drawer")).not.toHaveClass(/open/);
});

/** A hard ceiling on how many notices this test will raise chasing overflow
 * -- `notice-dock-geometry.spec.ts:488` needs 24 at this suite's default
 * viewport, so 30 leaves margin without hunting for a smaller number that
 * would only be true of today's `--notice-row-min`/dock geometry. */
const MAX_NOTICES_FOR_OVERFLOW = 30;

test("the notice scroller's focus indicator, in every theme", async ({ page }) => {
  test.setTimeout(60_000); // matches notice-dock-geometry.spec.ts:418's own
  // budget for the same ~24-notice cost.

  await page.goto("/");
  await openProject(page, "Alpha Project");
  await keepNotices(page);
  await openProject(page, "Alpha Project");

  const title = "SH-360 — notice scroller ring";
  await createStory(page, title);

  // Raised until #toast-scroll actually overflows -- syncNoticeDock() adds
  // tabindex="0" only then -- rather than hard-coding the 24 that happens
  // to be true today, so this test keeps working if the dock's geometry or
  // --notice-row-min ever changes.
  const scroll = page.locator("#toast-scroll");
  let raised = 0;
  while ((await scroll.getAttribute("tabindex")) !== "0") {
    if (raised >= MAX_NOTICES_FOR_OVERFLOW) {
      const box = await scroll.evaluate((el) => ({ scrollHeight: el.scrollHeight, clientHeight: el.clientHeight }));
      throw new Error(
        `#toast-scroll never overflowed after ${raised} notices ` +
          `(scrollHeight=${box.scrollHeight}, clientHeight=${box.clientHeight})`,
      );
    }
    await raiseNotice(page, title, raised % COPY_TARGETS.length);
    await expect(page.locator("#toast-stack .toast")).toHaveCount(raised + 1);
    raised++;
  }

  // `.notice-scroll`, matching the CSS rule's own base identity -- see the
  // board test's comment for why no `#toast-scroll` qualifier is needed:
  // once tabOnto lands focus on it, it is the only `.notice-scroll` that
  // matches `:focus`.
  await measureFocusIndicator(page, ".notice-scroll:focus", "the notice scroller", () =>
    tabOnto(page, "#toast-dismiss-all", ".notice-scroll"),
  );
});
