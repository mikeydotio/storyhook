import { test, expect } from "./support";
import type { Page } from "@playwright/test";
import {
  cleanUpCreatedStories,
  createStory,
  fullKeyboardAccess,
  keepNotices,
  measureFocusIndicator,
  openProject,
  raiseDurableNotices,
  seedToken,
  tabOnto,
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

/** SH-338's own doc comment on why themes are re-reads, not re-setups, and why
 * Tab (not `.focus()`) reaches every control here, now lives on `tabOnto` and
 * `THEMES` in `support.ts` -- lifted there ahead of a second caller (SH-360). */

test.beforeEach(async ({ page, context, browserName }) => {
  // The Copy-* paths raise an ERROR notice via `copyText`'s `.catch` branch
  // without clipboard permission. An error is durable either way, so this is
  // not load-bearing here the way it is in the geometry spec — but a notice
  // whose variant is an accident is a notice whose `border-left-color` is an
  // accident too, and this file measures colour.
  //
  // WebKit gets `clipboard-read` alone: its Playwright permission map has no
  // `clipboard-write` entry at all (`grantPermissions` throws "Unknown
  // permission: clipboard-write" there — SH-335). Every other engine keeps
  // both.
  const permissions =
    browserName === "webkit" ? ["clipboard-read"] : ["clipboard-read", "clipboard-write"];
  await context.grantPermissions(permissions);
  await seedToken(page);
});

/** Two durable dispatch-history rows, from two distinct stubbed outcomes.
 *
 * Keyed on the story id in the request path so both rows are real, separate
 * outcomes rather than one row rendered twice — the same stub
 * `notice-dock-geometry.spec.ts` uses, and `refused` because SH-304 narrowed
 * this surface to the outcomes that leave no other trace. */
async function raiseDispatchHistoryRows(page: Page, ids: string[]): Promise<void> {
  await page.route("**/story/*/dispatch**", async (route) => {
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

test("a keyboard-reached toast dismiss draws a measured ring in every theme", async ({
  page,
  browserName,
}) => {
  // `tabOnto()` needs real Tab traversal to reach a `<button>`, which WebKit
  // skips unless this machine has Full Keyboard Access on (`AppleKeyboardUIMode
  // >= 2`) -- real Safari's own out-of-box behavior (SH-335 -- story show
  // SH-335 carries the verdict). Fully load-bearing on `chromium`, and on a
  // `webkit` this machine has configured for full keyboard access.
  test.skip(
    browserName === "webkit" && !fullKeyboardAccess(),
    "WebKit's Tab order skips buttons/links unless AppleKeyboardUIMode>=2 (SH-335)",
  );
  await page.goto("/");
  await openProject(page, "Alpha Project");
  await keepNotices(page);
  await openProject(page, "Alpha Project");

  const title = "SH-338 — toast ring";
  await createStory(page, title);
  // Two, so `#toast-dismiss-all` is above the threshold and can seed the walk.
  await raiseDurableNotices(page, title, 2);

  await measureFocusIndicator(page, ".toast-dismiss:focus", "a toast's ×", () =>
    tabOnto(page, "#toast-dismiss-all", ".toast-dismiss"),
  );
});

test("the heir of a keyboard dismissal draws the same ring", async ({ page, browserName }) => {
  // Same gate as the test above: `tabOnto()` needs WebKit to put buttons in
  // the Tab order, which only happens with Full Keyboard Access on (SH-335).
  test.skip(
    browserName === "webkit" && !fullKeyboardAccess(),
    "WebKit's Tab order skips buttons/links unless AppleKeyboardUIMode>=2 (SH-335)",
  );
  await page.goto("/");
  await openProject(page, "Alpha Project");
  await keepNotices(page);
  await openProject(page, "Alpha Project");

  const title = "SH-338 — the heir's ring";
  await createStory(page, title);
  await raiseDurableNotices(page, title, 3);

  // Named as ":focus", not as an identity. SH-326 chooses the heir at dismissal
  // time and `notice-dock-geometry.spec.ts` already pins *which* control that
  // is; what this file adds is that wherever focus landed, it is visible. The
  // guard against a vacuous pass is the class check: a landing on `<body>` or on
  // the anchor would not match `.toast-dismiss`.
  await measureFocusIndicator(page, ".toast-dismiss:focus", "the heir", async () => {
    await tabOnto(page, "#toast-dismiss-all", ".toast-dismiss");
    await page.keyboard.press("Enter");
    await expect(page.locator("#toast-stack .toast")).toHaveCount(2);
    const landed = await page.evaluate(
      () => !!(document.activeElement as HTMLElement | null)?.classList.contains("toast-dismiss"),
    );
    expect(landed, "the heir of a keyboard dismissal must be another toast's ×").toBe(true);
  });
});

test("a dispatch-history dismiss draws a measured ring, reached and inherited", async ({
  page,
  browserName,
}) => {
  // Same gate as the first test in this file: `tabOnto()` needs WebKit to
  // put buttons in the Tab order, which only happens with Full Keyboard
  // Access on (SH-335).
  test.skip(
    browserName === "webkit" && !fullKeyboardAccess(),
    "WebKit's Tab order skips buttons/links unless AppleKeyboardUIMode>=2 (SH-335)",
  );
  await page.goto("/");
  await openProject(page, "Alpha Project");

  const first = await createStory(page, "SH-338 — history ring A");
  const second = await createStory(page, "SH-338 — history ring B");
  await raiseDispatchHistoryRows(page, [first, second]);
  await expect(page.locator("#dispatch-history .dispatch-history-row")).toHaveCount(2);

  await measureFocusIndicator(page, ".dispatch-history-dismiss:focus", "a history row's ×", () =>
    tabOnto(page, "#dispatch-history-dismiss-all", ".dispatch-history-dismiss"),
  );

  // The same inheritance SH-326 wired on this surface. Measured here too rather
  // than assumed from the toast test: the two controls share one CSS rule today,
  // and this is the assertion that fails if someone ever splits it. Full
  // four-theme coverage on the heir too -- SH-338 spot-checked it under a
  // single theme; measureFocusIndicator's unification closes that gap for
  // free rather than reintroducing a bespoke single-theme check here.
  await measureFocusIndicator(page, ".dispatch-history-dismiss:focus", "the history heir", async () => {
    await page.keyboard.press("Enter");
    await expect(page.locator("#dispatch-history .dispatch-history-row")).toHaveCount(1);
    const landed = await page.evaluate(
      () =>
        !!(document.activeElement as HTMLElement | null)?.classList.contains(
          "dispatch-history-dismiss",
        ),
    );
    expect(landed, "the heir of a keyboard dismissal must be another row's ×").toBe(true);
  });
});
