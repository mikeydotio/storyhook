import type { Page } from "@playwright/test";
import { expect, onAFrozenClock, openProject, seedToken, test } from "./support";

/**
 * SH-400: filtering a card out starts a deferred removal on its keyed DOM
 * node. Clearing the filter before that removal completes reclaims the same
 * node through `populateCard()`, but the old animation listener and 600 ms
 * fallback still own it and can remove the now-wanted card later.
 *
 * The clock owns the fallback boundary; a scoped CSS pause owns the separate
 * animation timeline. Search input remains the production entry point: no
 * data response or renderer is mocked, and Alpha's seeded card is never
 * mutated in the store.
 */

const SEEDED_CARD_TITLE = "Wire up the auth flow";
const CARD_EXIT_FALLBACK_MS = 600;

/** Holds both exit completion paths until the test chooses one. */
async function withExitCompletionHeld(page: Page, body: () => Promise<void>): Promise<void> {
  // The JS clock does not pause CSS animations. Install the rule before an
  // exit starts, so no driver round trip can spend its real 200 ms lifetime.
  const style = await page.addStyleTag({ content: ".card.exiting { animation-play-state: paused !important; }" });
  try {
    await onAFrozenClock(page, body);
  } finally {
    await style.evaluate((node) => (node as HTMLStyleElement).remove());
  }
}

test.beforeEach(async ({ page }) => {
  await page.clock.install();
  await seedToken(page);
  await page.goto("/");
  await openProject(page, "Alpha Project");
});

async function expectReclaimedCardToSurvive(
  page: Page,
  hiddenQueries: readonly string[],
  completion: "animation" | "fallback" = "fallback",
): Promise<void> {
  const card = page.locator('.column[data-state="todo"] .card', {
    hasText: SEEDED_CARD_TITLE,
  });
  await expect(card).toBeVisible();
  const originalNode = await card.elementHandle();
  if (!originalNode) throw new Error("the seeded Alpha card has no element handle");

  const search = page.locator("#search-input");
  await withExitCompletionHeld(page, async () => {
    for (const query of hiddenQueries) {
      await search.fill(query);
      expect(
        await originalNode.evaluate((node) => ({
          connected: node.isConnected,
          exiting: node.classList.contains("exiting"),
        })),
      ).toEqual({ connected: true, exiting: true });
    }

    await search.fill("");
    expect(
      await originalNode.evaluate((node) => {
        const id = (node as HTMLElement).dataset.id;
        return {
          connected: node.isConnected,
          reclaimedByIdentity:
            node === document.querySelector(`.card[data-id="${id}"]`),
          exiting: node.classList.contains("exiting"),
        };
      }),
    ).toEqual({ connected: true, reclaimedByIdentity: true, exiting: false });

    if (completion === "animation") {
      await originalNode.evaluate((node) => {
        node.dispatchEvent(
          new AnimationEvent("animationend", {
            animationName: "card-exit",
            bubbles: true,
          }),
        );
      });
    } else {
      await page.clock.runFor(CARD_EXIT_FALLBACK_MS - 1);
      expect(await originalNode.evaluate((node) => node.isConnected)).toBe(true);
      await page.clock.runFor(1);
    }

    expect(
      await originalNode.evaluate((node) => {
        const style = getComputedStyle(node);
        return {
          connected: node.isConnected,
          visible:
            node.isConnected &&
            style.display !== "none" &&
            style.visibility !== "hidden" &&
            (node as HTMLElement).getClientRects().length > 0,
        };
      }),
    ).toEqual({ connected: true, visible: true });
  });
}

test("a card reclaimed before its exit fallback remains on the board", async ({
  page,
}) => {
  await expectReclaimedCardToSurvive(page, ["no Alpha story matches this query"]);
});

test("reclaim cancels every removal armed by repeated hidden renders", async ({
  page,
}) => {
  await expectReclaimedCardToSurvive(page, [
    "no Alpha story matches this first query",
    "no Alpha story matches this second query",
  ]);
});

test("a reclaimed card ignores its former exit animation completion", async ({
  page,
}) => {
  await expectReclaimedCardToSurvive(
    page,
    ["no Alpha story matches this query"],
    "animation",
  );
});

test("a descendant animation cannot complete the card's exit", async ({ page }) => {
  const card = page.locator('.column[data-state="todo"] .card', {
    hasText: SEEDED_CARD_TITLE,
  });
  await expect(card).toBeVisible();
  const originalNode = await card.elementHandle();
  if (!originalNode) throw new Error("the seeded Alpha card has no element handle");

  await originalNode.evaluate((node) => {
    const clearedBlocker = document.createElement("span");
    clearedBlocker.className = "blocker-cleared";
    const statusLight = document.createElement("span");
    statusLight.className = "story-light";
    clearedBlocker.appendChild(statusLight);
    node.appendChild(clearedBlocker);
  });

  await withExitCompletionHeld(page, async () => {
    await page.locator("#search-input").fill("no Alpha story matches this query");
    expect(
      await originalNode.evaluate((node) => ({
        connected: node.isConnected,
        exiting: node.classList.contains("exiting"),
      })),
    ).toEqual({ connected: true, exiting: true });

    await originalNode.evaluate((node) => {
      node.querySelector(".story-light")?.dispatchEvent(
        new AnimationEvent("animationend", {
          animationName: "pulse-success",
          bubbles: true,
        }),
      );
    });
    expect(await originalNode.evaluate((node) => node.isConnected)).toBe(true);

    await originalNode.evaluate((node) => {
      node.dispatchEvent(
        new AnimationEvent("animationend", {
          animationName: "card-exit",
          bubbles: true,
        }),
      );
    });
    expect(await originalNode.evaluate((node) => node.isConnected)).toBe(false);
  });
});

test("a held exit survives CSS time but its fallback removes it at the exact deadline", async ({ page }) => {
  const card = page.locator('.column[data-state="todo"] .card', { hasText: SEEDED_CARD_TITLE });
  await expect(card).toBeVisible();
  const originalNode = await card.elementHandle();
  if (!originalNode) throw new Error("the seeded Alpha card has no element handle");

  await withExitCompletionHeld(page, async () => {
    await page.locator("#search-input").fill("no Alpha story matches this query");
    const held = await originalNode.evaluate((node) => {
      const exit = node.getAnimations().find((animation) =>
        animation instanceof CSSAnimation && animation.animationName === "card-exit");
      return { connected: node.isConnected, state: exit?.playState };
    });
    expect(held).toEqual({ connected: true, state: "paused" });

    // A separate native animation witnesses elapsed CSS time while JS time
    // stays frozen. This forces the driver-delay shape without a guessed sleep.
    await originalNode.evaluate(async (node) => {
      const exit = node.getAnimations().find((animation) =>
        animation instanceof CSSAnimation && animation.animationName === "card-exit");
      if (!exit?.effect) throw new Error("the held card has no exit animation");
      const duration = Number(exit.effect.getComputedTiming().endTime);
      if (!(duration > 0 && Number.isFinite(duration))) throw new Error("exit duration must be finite and positive");
      const witness = document.createElement("span");
      document.body.appendChild(witness);
      try {
        await witness.animate([{ opacity: 0 }, { opacity: 1 }], { duration: duration * 2 }).finished;
      } finally {
        witness.remove();
      }
    });
    expect(await originalNode.evaluate((node) => node.isConnected)).toBe(true);
    await page.clock.runFor(CARD_EXIT_FALLBACK_MS - 1);
    expect(await originalNode.evaluate((node) => node.isConnected)).toBe(true);
    await page.clock.runFor(1);
    expect(await originalNode.evaluate((node) => node.isConnected)).toBe(false);
  });
});
