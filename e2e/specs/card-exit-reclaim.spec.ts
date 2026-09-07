import type { Page } from "@playwright/test";
import { expect, onAFrozenClock, openProject, seedToken, test } from "./support";

/**
 * SH-400: filtering a card out starts a deferred removal on its keyed DOM
 * node. Clearing the filter before that removal completes reclaims the same
 * node through `populateCard()`, but the old animation listener and 600 ms
 * fallback still own it and can remove the now-wanted card later.
 *
 * The clock makes the fallback boundary test-owned rather than a wall-clock
 * race. Search input remains the production entry point: no data response or
 * renderer is mocked, and Alpha's seeded card is never mutated in the store.
 */

const SEEDED_CARD_TITLE = "Wire up the auth flow";
const CARD_EXIT_FALLBACK_MS = 600;

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
  await onAFrozenClock(page, async () => {
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
