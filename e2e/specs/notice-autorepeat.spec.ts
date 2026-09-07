import { test, expect } from "./support";
import type { Page } from "@playwright/test";
import {
  cleanUpCreatedStories,
  createStory,
  dispatchStory,
  holdKey,
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
 * `event.repeat` is true. Council verdict, unanimous 3-0, recorded on SH-339.
 *
 * ## Why half these tests are over-reach checks
 *
 * A guard that suppressed too much would satisfy the headline assertion
 * perfectly — "Enter never works at all" also leaves four notices standing —
 * so the mutation check has to run in **both** directions or it pins nothing.
 * Four of the six tests here exist for that: discrete presses still clear the
 * stack; a held Space still dismisses exactly one (a button activates on Space
 * *keyup*, so its repeat never activated anything and must not start being
 * cancelled); a held ArrowDown on a dismiss button remains uncancelled (which
 * is why the guard names Enter instead of testing `event.repeat` alone — a bare
 * check would cancel every repeated keydown from that button);
 * and a synthesised `click()` with no keydown still dismisses, which is the
 * path assistive technology actually uses.
 *
 * ## The mutation battery, and the one that got through
 *
 * Every assertion was checked by breaking the thing it guards, because a pin
 * that has never failed is not evidence (the SH-326 precedent, where 2 of 5
 * survived). Four mutations, each with its kill predicted before it ran:
 *
 *   1. **Unbind the listener.** Predicted 1 and 2 fail. They did; 3-6 passed.
 *   2. **Drop the Enter check** (`if (!event.repeat) return`). Predicted 5
 *      fails. **All six passed** — see below.
 *   3. **Drop the repeat check** (Enter never activates). Predicted 3 fails. It
 *      did, and so did 1 and 2, which is right: with Enter inert the stack
 *      never loses its first notice either.
 *   4. **Add Space to the predicate.** Predicted 4 fails. All six passed — and
 *      unlike (2) this one is correct, see below.
 *
 * **(2) was a real hole and this file was rewritten twice to close it.** The
 * first ArrowDown test asserted `scrollTop > 0` after the held key. But a bare
 * `event.repeat` guard cancels only the *repeats*; the deliberate first keydown
 * still scrolls one step in Chromium, so the over-reach went unreported. A
 * one-step baseline killed that mutant there, but SH-374 exposed its own
 * engine assumption: WebKit does not scroll this ancestor when its descendant
 * button has focus. The test now observes each keydown at `window`, after the
 * dock's bubbling listener, and requires every repeated ArrowDown to remain
 * uncancelled. That pins the application boundary directly in both engines.
 *
 * **(4) is an equivalent mutant, not a gap.** Adding Space to the predicate
 * changes no observable behaviour on Blink, because a button activates on Space
 * *keyup* and the first Space keydown is never `repeat: true` — the repeats it
 * would newly cancel were doing nothing anyway. Worth recording because the
 * council reasoned that guarding Space "would risk cancelling the one activation
 * a held Space legitimately produces on release"; on Blink, measured, it does
 * not. The Enter-only scope stands on (2) — the ArrowDown regression — not on
 * this. Test 4 keeps its place regardless: it pins the pre-existing
 * one-dismissal-per-held-Space property, which a guard implemented one layer up
 * (suppressing the *click* rather than the keydown default) would break.
 *
 * ## What is NOT claimed here
 *
 * **Chromium and WebKit both run all six tests.** Nothing here says how Gecko
 * behaves. Where an engine or input stack does not set `KeyboardEvent.repeat`
 * at all — an input path that delivers repeats as discrete keydown/keyup pairs
 * — the guard is inert, which is the pre-fix behaviour and never worse than it.
 *
 * **Playwright sets the `repeat` bit itself** on a second `keyboard.down` of a
 * key already down. So the complete mutation battery proves in both driven
 * engines that an un-prevented repeat keydown activates a button, and that
 * cancelling it stops that. That a *physically* held key sets the flag rests on
 * the UI Events spec and on a hand check, and is not evidence this suite can
 * produce — the SH-322/SH-327 precedent, applied to an input event instead of
 * an utterance.
 */

cleanUpCreatedStories("Alpha Project");

test.beforeEach(async ({ page }) => {
  await seedToken(page);
});

/** Makes every Clipboard API write reject before application code loads.
 *
 * `raiseDurableErrors` needs `copyText`'s rejection branch, but the default
 * permission posture differs between Playwright's Chromium and WebKit builds.
 * Installing the browser boundary directly keeps the fixture deterministic
 * without granting, denying or otherwise depending on ambient permissions. */
async function installRejectingClipboard(page: Page): Promise<void> {
  await page.addInitScript(() => {
    Object.defineProperty(window.navigator, "clipboard", {
      configurable: true,
      value: {
        writeText: async () => {
          throw new DOMException("Clipboard write denied by test fixture", "NotAllowedError");
        },
      },
    });
  });
}

/** Raises `count` durable error notices on a fresh story and returns its title. */
async function raiseDurableErrors(page: Page, label: string, count: number): Promise<void> {
  await installRejectingClipboard(page);
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

test("a held ArrowDown on a dismiss control remains uncancelled", async ({ page }) => {
  await raiseDurableErrors(page, "SH-339 — held ArrowDown", 3);
  await page.locator("#toast-stack .toast-dismiss").first().focus();

  await page.evaluate(() => {
    const observed: Array<{ repeat: boolean; defaultPrevented: boolean }> = [];
    (window as unknown as { __arrowDownEvents: typeof observed }).__arrowDownEvents = observed;
    window.addEventListener("keydown", (event) => {
      if (event.key !== "ArrowDown") return;
      observed.push({ repeat: event.repeat, defaultPrevented: event.defaultPrevented });
    });
  });

  await holdKey(page, "ArrowDown", 8);

  const observed = await page.evaluate(
    () =>
      (window as unknown as {
        __arrowDownEvents: Array<{ repeat: boolean; defaultPrevented: boolean }>;
      }).__arrowDownEvents,
  );
  expect(observed.map((event) => event.repeat)).toEqual([false, ...Array(8).fill(true)]);
  expect(observed.map((event) => event.defaultPrevented)).toEqual(Array(9).fill(false));
  await expect(page.locator("#toast-stack .toast")).toHaveCount(3);
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
  await page.route("**/story/*/dispatch**", async (route) => {
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
  await dispatchStory(page, { auto: true });
  await expect(page.locator("#dispatch-history .dispatch-history-row")).toHaveCount(before + 1);
  await page.locator("#drawer-close").click();
  await expect(page.locator("#drawer")).not.toHaveClass(/open/);
}
