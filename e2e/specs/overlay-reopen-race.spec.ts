import { test, expect } from "@playwright/test";
import type { Page } from "@playwright/test";
import { openProject, seedToken } from "./support";

/**
 * SH-302 — an overlay reopened inside its own fade-out must keep its backdrop.
 *
 * Every overlay in `web_dashboard.html` closes the same way: drop `.open` so
 * the backdrop can fade (`transition: opacity 0.18s`), then hide it on a
 * timer once the fade has finished. That timer belongs to the close that
 * scheduled it, and a reopen inside its window undoes that close — so the
 * write it is still going to perform lands on a surface that is now open,
 * leaving `<div class="backdrop open" hidden>`: invisible, unclickable, and
 * still the thing every click on the page hits.
 *
 * SH-284 patched the drafts popover's timer to re-read the popover's state
 * when it fires. That is the right question asked of a signal that arrives
 * late: `openDraftsModal()` sets `hidden = false` synchronously and adds
 * `.open` a frame later, so a timer landing inside that frame sees a popover
 * that is already open and reads it as closed. The other overlays never asked
 * at all. Both shapes are the same defect — a stale write nobody cancelled —
 * and both are fixed by cancelling it.
 *
 * The three overlays below are the three mechanisms, not a sample: a
 * late-signal guard (drafts), no guard (create modal), and a guard on a
 * synchronously-set variable that was already correct and must stay correct
 * once its bespoke check is gone (drawer). The remaining four are held to
 * routing through the same two functions by
 * `tests/web_test.rs::every_backdrop_is_shown_and_hidden_through_the_helpers`,
 * which is what makes covering three of seven honest here — reaching the
 * other four means archiving a column or dragging a card into Blocked, and
 * every one of them would be reproducing the same race a third and fourth
 * time.
 *
 * Read-only against the shared fixtures: nothing here creates, moves or
 * deletes a story, so `tests/e2e_fixture_hygiene.rs`'s cleanup registration
 * does not apply and Alpha's two-story shape is undisturbed.
 */

/** Alpha's first seeded story — opened, closed, and left exactly as found. */
const ALPHA_CARD_TITLE = "Wire up the auth flow";

/**
 * How long {@link blockTheRenderer} holds an animation frame. Comfortably
 * past the longest hide delay in the file (the drawer's 180ms), because the
 * whole point is a frame that lands *after* the timer rather than before it.
 */
const RAF_DEFERRAL_MS = 400;

/** Long enough for both the hide timer and a deferred frame to have run.
 * Every assertion below waits this out first: against the defect the backdrop
 * is genuinely still visible until the stale timer fires, so an assertion
 * made any earlier passes on broken code. */
const SETTLE_MS = RAF_DEFERRAL_MS + 200;

/**
 * Models a renderer too busy to paint: animation frames still arrive, just
 * later than the 150ms timer they race.
 *
 * This is what a loaded machine does to the window under test, and doing it
 * with a timer makes it a test rather than an anecdote — the same reasoning
 * `board-readiness.spec.ts` gives for slowing `/data` with a route instead of
 * with load. A board's first full render is exactly the work that blocks the
 * frame in practice, which is why the reported sighting was a board rendering
 * under `cargo build`.
 */
async function blockTheRenderer(page: Page): Promise<void> {
  await page.evaluate((ms) => {
    window.requestAnimationFrame = ((callback: FrameRequestCallback) =>
      window.setTimeout(
        () => callback(performance.now()),
        ms,
      )) as unknown as typeof window.requestAnimationFrame;
  }, RAF_DEFERRAL_MS);
}

interface Overlay {
  /** How the test names it. */
  readonly name: string;
  /** The surface that carries `.open`. */
  readonly surface: string;
  /** The backdrop that must not be `hidden` while the surface is open. */
  readonly backdrop: string;
  /** Whether animation frames are deferred past the hide timer. */
  readonly renderer: "blocked" | "live";
  /** Puts the overlay on screen, through a real user action. */
  readonly open: (page: Page) => Promise<void>;
  /** Ids of the controls that close it and reopen it, clicked in a single
   * tick by {@link reopenInOneTick}. */
  readonly closeThenOpen: readonly [string, string];
}

const OVERLAYS: readonly Overlay[] = [
  {
    // The reported defect. Its timer already re-reads the popover's state;
    // what it cannot do is read a class that has not been set yet.
    name: "the drafts popover",
    surface: "drafts-modal",
    backdrop: "drafts-backdrop",
    renderer: "blocked",
    open: (page) => page.locator("#drafts-btn").click(),
    closeThenOpen: ["drafts-close", "drafts-btn"],
  },
  {
    // Its timer asks nothing at all, so this needs no blocked renderer and no
    // load: Discard, then New Story, inside a fifth of a second. A user with
    // a fast hand and an idle machine reaches this one.
    name: "the create modal",
    surface: "create-modal",
    backdrop: "modal-backdrop",
    renderer: "live",
    open: (page) => page.locator("#new-story-btn").click(),
    closeThenOpen: ["create-discard", "new-story-btn"],
  },
  {
    // The one that was already right, under the most adversarial conditions
    // available: its timer reads `state.drawerId`, which `openDrawer()` sets
    // synchronously, so no frame can arrive late enough to fool it. That
    // bespoke check is what the shared cancellation replaces, and this is the
    // assertion that says the replacement lost nothing.
    name: "the drawer",
    surface: "drawer",
    backdrop: "drawer-backdrop",
    renderer: "blocked",
    open: (page) =>
      page.locator(".card-title", { hasText: ALPHA_CARD_TITLE }).click(),
    closeThenOpen: ["drawer-close", "drawer-close-reopen-card"],
  },
];

/**
 * Clicks the close control and then the open control in one JavaScript tick.
 *
 * Driven one Playwright action at a time this is 10-300ms of round trips,
 * which straddles the 150ms timer and makes the test pass or fail on machine
 * speed; in one tick the timer is guaranteed to fire after the reopen, which
 * is the state under test. `board-readiness.spec.ts`'s SH-284 regression test
 * compresses three clicks the same way, for the same reason.
 *
 * Every handler invoked here is the one a user's click invokes. The
 * background is `inert` while an overlay is up, but the close runs first and
 * `releaseOverlay()` lifts that synchronously, so the reopen lands on a live
 * control — no synthetic dispatch, no inertness bypassed.
 */
async function reopenInOneTick(
  page: Page,
  [closeId, openId]: readonly [string, string],
  cardTitle: string,
): Promise<void> {
  await page.evaluate(
    ([close, open, title]) => {
      const control = (id: string): HTMLElement => {
        // The drawer has no button that reopens it -- a card does, and which
        // card is a title rather than an id.
        if (id === "drawer-close-reopen-card") {
          const card = Array.from(
            document.querySelectorAll<HTMLElement>(".card-title"),
          ).find((node) => (node.textContent ?? "").includes(title));
          if (!card) throw new Error(`no card titled "${title}" on the board`);
          return card;
        }
        const node = document.getElementById(id);
        if (!node) throw new Error(`#${id} is not in the document`);
        return node;
      };
      control(close).click();
      control(open).click();
    },
    [closeId, openId, cardTitle] as const,
  );
}

test.describe("an overlay reopened inside its own fade-out", () => {
  test.beforeEach(async ({ page }) => {
    await seedToken(page);
    await page.goto("/");
    await openProject(page, "Alpha Project");
  });

  for (const overlay of OVERLAYS) {
    test(`${overlay.name} keeps its backdrop`, async ({ page }) => {
      const surface = page.locator(`#${overlay.surface}`);
      const backdrop = page.locator(`#${overlay.backdrop}`);

      await overlay.open(page);
      await expect(surface).toHaveClass(/open/);
      await expect(backdrop).toBeVisible();

      if (overlay.renderer === "blocked") await blockTheRenderer(page);
      await reopenInOneTick(page, overlay.closeThenOpen, ALPHA_CARD_TITLE);

      // Waits for the reopen to land -- under a blocked renderer that is a
      // deferred frame away, and the settle below has to start after it.
      await expect(surface).toHaveClass(/open/);
      await page.waitForTimeout(SETTLE_MS);

      // The invariant, stated where it breaks: an open overlay's backdrop is
      // never `hidden`. Asserted on the backdrop rather than through a
      // failing click because `toBeVisible()` names the actual defect, where
      // a blocked click only reports a timeout on the control it wanted.
      await expect(surface).toHaveClass(/open/);
      await expect(backdrop).toBeVisible();
    });
  }

  /**
   * Two closes in a row leave one hide pending, not two.
   *
   * The reason this is its own test: a fix that only cancels on the way *in*
   * still passes every assertion above, and still loses. Closing twice would
   * leave two timers running against one backdrop with only the later of them
   * recorded, so the reopen cancels that one and the first fires anyway —
   * hiding a popover that is open, from a close two closes ago.
   *
   * That is not a contrived double-close, and it is not a narrow window
   * either. The shared Escape handler calls every `close*` function on every
   * press, and `updateDraftsButton()` calls `closeDraftsModal()` on every
   * navigation to a non-repo screen — so `page.goto("/")` plus `openProject()`
   * reaches two-in-a-row before the user has touched anything. Measured:
   * with the close-path cancellation removed and everything else here left
   * alone, the first test above fails on its *opening* assertion, with the
   * backdrop already `hidden` the first ordinary time the popover is
   * clicked open. Three of these four tests go red on that one omission.
   *
   * Driven through the X and then the backdrop, which are two real listeners
   * for the same function, rather than through a synthesised keypress.
   */
  test("the drafts popover keeps its backdrop after two closes, not one", async ({
    page,
  }) => {
    const surface = page.locator("#drafts-modal");
    const backdrop = page.locator("#drafts-backdrop");

    await page.locator("#drafts-btn").click();
    await expect(surface).toHaveClass(/open/);

    await blockTheRenderer(page);
    await page.evaluate(() => {
      const click = (id: string) => {
        const node = document.getElementById(id);
        if (!node) throw new Error(`#${id} is not in the document`);
        node.click();
      };
      click("drafts-close");
      click("drafts-backdrop");
      click("drafts-btn");
    });

    await expect(surface).toHaveClass(/open/);
    await page.waitForTimeout(SETTLE_MS);

    await expect(surface).toHaveClass(/open/);
    await expect(backdrop).toBeVisible();
  });
});
