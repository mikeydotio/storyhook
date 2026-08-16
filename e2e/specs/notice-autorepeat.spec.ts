import { test, expect } from "@playwright/test";
import type { Page } from "@playwright/test";
import {
  cleanUpCreatedStories,
  createStory,
  openProject,
  raiseNotice,
  seedToken,
} from "./support";

/**
 * SH-339 — a held key never clears more than the one notice it was pressed on.
 *
 * A fifth notice-dock file, and the split follows the one the other four
 * already drew: `notification-contract.spec.ts` owns semantics and timing,
 * `notice-dock-geometry.spec.ts` rects and hit tests, `notice-announcement.spec
 * .ts` what a live region says, `notice-focus-indicator.spec.ts` colour. This
 * file's subject is **activation** — which key events do and do not reach a
 * dismiss control's default action — which is a sixth thing none of those
 * assert, and which fails for reasons none of them would catch.
 *
 * ## The defect
 *
 * A `<button>` runs its activation behaviour on keydown for Enter, so a held
 * Enter fires one click per OS auto-repeat. That was harmless until SH-326:
 * before it, the first click destroyed the button and the repeats landed on
 * `<body>`. SH-326's heir policy moves focus to the next notice's dismiss
 * control, which gives the repeats somewhere to land, so one held key walks the
 * entire stack. Measured before the fix: five durable **error** notices, one
 * held Enter, **zero** left. Those notices are the only record of what failed —
 * the daemon's finished `DispatchRecord`s are on no route, this page never
 * reads them, and they evict after 30 minutes or 32 records.
 *
 * Reverting the heir was never a candidate; it reinstates a filed defect (focus
 * stranded on `<body>`, SC 2.4.3). The fix is `refuseAutoRepeatActivation`, one
 * delegated `keydown` listener on `#notice-dock` that cancels Enter when
 * `event.repeat` is true. Council verdict, unanimous 3-0:
 * `.council/sh339-notice-dismiss-autorepeat/DECISION.md`.
 *
 * ## Why half these tests are over-reach checks
 *
 * A guard that suppressed too much would satisfy the headline assertion
 * perfectly — "Enter never works at all" also leaves four notices standing —
 * so the mutation check has to run in **both** directions or it pins nothing.
 * Four of the six tests here exist for that: discrete presses still clear the
 * stack; a held Space still dismisses exactly one (a button activates on Space
 * *keyup*, so its repeat never activated anything and must not start being
 * cancelled); a held ArrowDown still scrolls an overflowing `#toast-scroll`
 * (which is why the guard names Enter instead of testing `event.repeat` alone —
 * a bare check would cancel the scrolling the dock's conditional `tabindex`
 * exists to serve, taking it from the very people this story is written for);
 * and a synthesised `click()` with no keydown still dismisses, which is the
 * path assistive technology actually uses.
 *
 * ## What is NOT claimed here
 *
 * **Blink only.** The suite is Chromium-only (SH-335 is filed against exactly
 * that), so nothing here says how Gecko or WebKit behave. Where an engine or
 * input stack does not set `KeyboardEvent.repeat` at all — an input path that
 * delivers repeats as discrete keydown/keyup pairs — the guard is inert, which
 * is the pre-fix behaviour and never worse than it.
 *
 * **Playwright sets the `repeat` bit itself** on a second `keyboard.down` of a
 * key already down. So what these tests prove is Blink's half: that an
 * un-prevented repeat keydown activates a button, and that cancelling it stops
 * that. That a *physically* held key sets the flag rests on the UI Events spec
 * and on a hand check, and is not evidence this suite can produce — the
 * SH-322/SH-327 precedent, applied to an input event instead of an utterance.
 */

cleanUpCreatedStories("Alpha Project");

test.beforeEach(async ({ page }) => {
  // Clipboard permission is deliberately NOT granted: `copyText`'s `.catch`
  // branch is what makes every Copy-* notice a durable ERROR, which is the
  // variant this story is about. Load-bearing, so each test asserts the
  // `.error` class rather than trusting it — a permission default that changed
  // under us would otherwise silently reduce these to tests of self-clearing
  // notices, which carry no dismiss control at all.
  await seedToken(page);
});

/** Holds `key` down: one deliberate press, then `repeats` auto-repeats, then
 * release. Playwright reports the first `down` with `repeat: false` and every
 * subsequent one with `repeat: true`, which is the sequence a held key produces
 * and the only part of it this harness can speak to (see the file header). */
async function holdKey(page: Page, key: string, repeats: number): Promise<void> {
  await page.keyboard.down(key);
  for (let i = 0; i < repeats; i++) await page.keyboard.down(key);
  await page.keyboard.up(key);
}

/** Raises `count` durable error notices on a fresh story and returns its title. */
async function raiseDurableErrors(page: Page, label: string, count: number): Promise<void> {
  await page.goto("/");
  await openProject(page, "Alpha Project");
  const title = `${label} ${Date.now()}`;
  await createStory(page, title);
  for (let i = 0; i < count; i++) {
    await raiseNotice(page, title, i % 3);
    await expect(page.locator("#toast-stack .toast")).toHaveCount(i + 1);
  }
  // The variant is the subject, not an incidental: a durable notice is the only
  // kind that carries a `.toast-dismiss` for a key to land on.
  await expect(page.locator("#toast-stack .toast.error")).toHaveCount(count);
  await expect(page.locator("#toast-stack .toast-dismiss")).toHaveCount(count);
}

// ============================================================
// The defect itself
// ============================================================

test("a held Enter on a toast's dismiss control clears exactly one notice", async ({ page }) => {
  await raiseDurableErrors(page, "SH-339 — held Enter on a toast", 5);

  await page.locator("#toast-stack .toast-dismiss").first().focus();
  await holdKey(page, "Enter", 8);

  // Four, not zero. Eight repeats had four further notices to walk through and
  // reached none of them.
  await expect(page.locator("#toast-stack .toast")).toHaveCount(4);

  // SH-326 is intact underneath: the one dismissal that did land still handed
  // focus to the heir CONTROL — not the body, and not the notice container,
  // which is the landing SH-338 measured a focus ring on.
  const focused = page.locator("#toast-stack .toast-dismiss:focus");
  await expect(focused).toHaveCount(1);
  await expect(page.locator("#toast-stack .toast-dismiss").first()).toBeFocused();

  // And it announced once, with the count a single dismissal leaves behind. A
  // guard that suppressed the deliberate press too would leave five notices and
  // an empty status; a guard that leaked one repeat would say "3 remaining".
  await expect(page.locator("#notice-dock-status")).toHaveText("Notice dismissed. 4 remaining.");
});

test("a held Enter on a dispatch-history row clears exactly one row", async ({ page }) => {
  await page.goto("/");
  await stubEveryDispatchAsAutoRefusal(page);
  for (let i = 0; i < 3; i++) await raiseHistoryRow(page, `SH-339 — held Enter on a row ${i}`);

  const rows = page.locator("#dispatch-history .dispatch-history-row");
  await expect(rows).toHaveCount(3);

  await page.locator("#dispatch-history .dispatch-history-dismiss").first().focus();
  await holdKey(page, "Enter", 8);

  await expect(rows).toHaveCount(2);
  await expect(page.locator("#dispatch-history .dispatch-history-dismiss").first()).toBeFocused();
  await expect(page.locator("#notice-dock-status")).toHaveText(
    "Dispatch result dismissed. 2 remaining.",
  );
});

// ============================================================
// Over-reach: the same guard must not suppress anything else
// ============================================================

test("discrete Enter presses still clear the whole stack", async ({ page }) => {
  await raiseDurableErrors(page, "SH-339 — discrete presses", 5);

  await page.locator("#toast-stack .toast-dismiss").first().focus();
  // Press and release, five times. Every one of these is `repeat: false`, so
  // every one must land. Without this assertion "Enter never works" would pass
  // the test above.
  for (let i = 0; i < 5; i++) {
    await page.keyboard.press("Enter");
    await expect(page.locator("#toast-stack .toast")).toHaveCount(4 - i);
  }

  await expect(page.locator("#toast-stack .toast")).toHaveCount(0);
});

test("a held Space still dismisses exactly one notice", async ({ page }) => {
  await raiseDurableErrors(page, "SH-339 — held Space", 3);

  await page.locator("#toast-stack .toast-dismiss").first().focus();
  await holdKey(page, " ", 8);

  // One, and one is the *pre-existing* behaviour rather than something the
  // guard produces: a button activates on Space keyup, so a held Space has
  // always fired exactly once. Recorded as a measurement so the guard's
  // Enter-only scope is evidence rather than an assumption — if Space is ever
  // added to the predicate, this fails.
  await expect(page.locator("#toast-stack .toast")).toHaveCount(2);
});

test("a held ArrowDown still scrolls an overflowing notice scroller", async ({ page }) => {
  await raiseDurableErrors(page, "SH-339 — held ArrowDown", 12);

  const scroll = page.locator("#toast-scroll");
  const overflows = await scroll.evaluate((n) => n.scrollHeight > n.clientHeight + 1);
  expect(overflows, "twelve notices should overflow the bounded scroller").toBe(true);
  expect(await scroll.evaluate((n) => n.scrollTop)).toBe(0);

  await page.locator("#toast-stack .toast-dismiss").first().focus();
  await holdKey(page, "ArrowDown", 8);

  // This is why the guard names Enter rather than testing `event.repeat` alone.
  // A bare repeat check would cancel this too, and held-arrow scrolling is the
  // affordance `syncNoticeDock`'s conditional `tabindex` on this element exists
  // to provide.
  expect(await scroll.evaluate((n) => n.scrollTop)).toBeGreaterThan(0);
  await expect(page.locator("#toast-stack .toast")).toHaveCount(12);
});

test("a synthesised click with no keydown still dismisses", async ({ page }) => {
  await raiseDurableErrors(page, "SH-339 — synthesised click", 3);

  // The path assistive technology actually uses: AT dispatches a click, never a
  // key event, so it cannot reach a keydown handler at all. Asserted rather
  // than reasoned about, because "the AT path is untouched" is the claim on
  // which this guard's admissibility rests — SH-326's council rejected
  // `event.detail === 0` precisely because it caught this population.
  await page.locator("#toast-stack .toast-dismiss").first().dispatchEvent("click");

  await expect(page.locator("#toast-stack .toast")).toHaveCount(2);
});

// ============================================================
// Fixtures
// ============================================================

/** A terminal `--auto` refusal for whatever story is dispatched, so a single
 * route covers every row this file raises. `notice-announcement.spec.ts` keys
 * its own copy to one story id; nothing here reads the id back, so this one
 * echoes the request's own story instead of taking a parameter. */
async function stubEveryDispatchAsAutoRefusal(page: Page): Promise<void> {
  await page.route("**/dispatch**", async (route) => {
    await route.fulfill({
      status: route.request().method() === "POST" ? 202 : 200,
      contentType: "application/json",
      body: JSON.stringify({
        result: "ok",
        dispatch: {
          handle: "stub-handle",
          project: "alpha",
          story: "SH-0",
          auto: true,
          state: "refused",
          started_at: "2026-01-01T00:00:00Z",
          finished_at: "2026-01-01T00:00:01Z",
          payload: { display: "[story] refused: that story is already in-progress" },
          reason: "claim-conflict",
        },
      }),
    });
  });
}

/** Raises one `--auto` refused dispatch, leaving a durable `#dispatch-history`
 * row. Home first, unconditionally: `openProject` hunts for a `.repo-card-name`
 * on the Home screen, and a second call starts from the board the previous
 * dispatch left the page on (`notice-announcement.spec.ts`'s own finding). */
async function raiseHistoryRow(page: Page, title: string): Promise<void> {
  const before = await page.locator("#dispatch-history .dispatch-history-row").count();
  await page.locator("#home-btn").click();
  await openProject(page, "Alpha Project");
  await createStory(page, title);
  await page.locator('.column[data-state="todo"] .card', { hasText: title }).click();
  await expect(page.locator("#drawer")).toHaveClass(/open/);
  await page.locator("#dispatch-auto-btn").click();
  await expect(page.locator("#dispatch-history .dispatch-history-row")).toHaveCount(before + 1);
  await page.locator("#drawer-close").click();
  await expect(page.locator("#drawer")).not.toHaveClass(/open/);
}
