import { test, expect } from "./support";
import type { Locator, Page } from "@playwright/test";
import {
  cleanUpCreatedStories,
  deleteStory,
  openProject,
  onAFrozenClock,
  seedToken,
} from "./support";

/**
 * Exercises SH-424: `populateCard()` updates the two classes it derives from
 * current story state without replacing classes owned by drag, entrance,
 * FLIP, or change-flash lifecycles. The witness uses a second story's real
 * create flow to force an unrelated `/data` render over the retained card.
 */

cleanUpCreatedStories("Alpha Project");

test.beforeEach(async ({ page }) => {
  await page.clock.install();
  await seedToken(page);
  await page.goto("/");
  await openProject(page, "Alpha Project");
});

/** Hold the independent CSS and JavaScript lifecycle owners across driver delays. */
async function withTransientTimeHeld(page: Page, target: Locator, body: () => Promise<void>): Promise<void> {
  const node = await target.elementHandle();
  if (!node) throw new Error("the transient-class target is missing");
  const style = await page.addStyleTag({ content: ".card[data-sh812-hold] { animation-play-state: paused !important; }" });
  try {
    await node.evaluate(element => element.setAttribute("data-sh812-hold", ""));
    await onAFrozenClock(page, body);
  } finally {
    await node.evaluate(element => element.removeAttribute("data-sh812-hold"));
    await style.evaluate(element => (element as HTMLStyleElement).remove());
  }
}

async function createStory(
  page: import("@playwright/test").Page,
  title: string,
  frozen = false,
) {
  await page.locator("#new-story-btn").click();
  // The modal opens on its next frame; lifecycle cleanup remains 320 ms away.
  if (frozen) await page.clock.runFor(16);
  await expect(page.locator("#create-modal")).toHaveClass(/open/);
  await page.locator("#create-title").fill(title);
  await page.locator("#create-priority").selectOption("medium");
  await page.locator("#create-submit").click();
  await expect(page.locator("#create-modal")).not.toHaveClass(/open/);
  await expect(
    page.locator('.column[data-state="todo"] .card', { hasText: title }),
  ).toBeVisible();
}

test("an unrelated render preserves a card's transient classes (SH-424)", async ({
  page,
}) => {
  const targetTitle = "SH-424 transient class target";
  const triggerTitle = "SH-424 unrelated render trigger";
  const transientClasses = [
    "dragging",
    "entering",
    "moving",
    "flash-priority",
    "future-transient",
  ];

  await createStory(page, targetTitle);
  const target = page.locator('.column[data-state="todo"] .card', {
    hasText: targetTitle,
  });
  await withTransientTimeHeld(page, target, async () => {
    await target.evaluate(
      (node, classes) => node.classList.add(...classes),
      transientClasses,
    );

    await target.evaluate(async (node) => {
      // A separate Web Animation must not be mistaken for a CSS class owner.
      const independent = node.animate([{ outlineOffset: "0px" }, { outlineOffset: "1px" }], {
        duration: 1000,
        iterations: Infinity,
      });
      try {
        const animations = node.getAnimations().filter(animation => animation instanceof CSSAnimation);
        if (!animations.length || animations.some(animation => animation.playState !== "paused")) {
          throw new Error(`the witnessed card's animation lifecycle is not held: ${JSON.stringify(
            animations.map(animation => ({ type: animation.constructor.name, state: animation.playState })),
          )}`);
        }
        const duration = Math.max(...animations.map(animation => Number(animation.effect!.getComputedTiming().endTime)));
        if (!(duration > 0 && Number.isFinite(duration))) throw new Error("card animation must have a finite duration");
        const witness = document.createElement("span");
        document.body.appendChild(witness);
        try {
          await witness.animate([{ opacity: 0 }, { opacity: 1 }], { duration: duration * 2 }).finished;
        } finally {
          witness.remove();
        }
      } finally {
        independent.cancel();
      }
    });

    await createStory(page, triggerTitle, true);

    await expect(target).toHaveClass(/\bcard\b/);
    await expect
      .poll(() =>
        target.evaluate((node, classes) =>
          classes.filter((name) => !node.classList.contains(name)),
          transientClasses,
        ),
      )
      .toEqual([]);

    await target.evaluate(
      (node, classes) => node.classList.remove(...classes),
      transientClasses,
    );
  });
  await deleteStory(page, triggerTitle);
  await deleteStory(page, targetTitle);
});
