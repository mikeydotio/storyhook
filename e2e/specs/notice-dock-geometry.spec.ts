import { test, expect } from "@playwright/test";
import type { Page } from "@playwright/test";
import { cleanUpCreatedStories, openProject, seedToken } from "./support";

/**
 * SH-323 — the notice dock's *geometry*, as measured properties.
 *
 * A file of its own rather than more of `notification-contract.spec.ts`, which
 * owns notice **semantics and timing** (which outcomes are durable, when a clock
 * runs) and whose two real-clock canaries should not be disturbed. What is
 * asserted here is rects, hit tests and scroll state: where the notices are,
 * what they cover, and whether a reader can reach them. Those two files fail for
 * different reasons on purpose.
 *
 * `notification-contract.spec.ts` passing untouched is itself a pin of this
 * work, and is stated here so that whoever breaks it knows it was load-bearing:
 * every selector in it resolves through `#toast-stack`, which keeps its id, its
 * `role="status"` and its `aria-live="polite"` under everything below.
 *
 * ## Why these are hit tests and rect arithmetic rather than CSS assertions
 *
 * The council that settled this work (`.council/sh-323-notice-stack-occlusion-
 * and-growth/DECISION.md`) rejected "the CSS says so" as a pin, for a reason its
 * own evidence supplies: a static token offset *reads* correct and resolves
 * wrong. `1.5rem + var(--tap-min)` predicts a 48px header band against a real
 * `.topbar` bottom of 62 / 103.5 / 145 / 145 at 1280 / 768 / 390 / 320, because
 * the row is sized by `.search-input`'s own box and `.btn`'s padding plus font
 * metrics — `--tap-min`'s 24px floor never participates — and because the topbar
 * wraps below 768px. Only a measurement catches that.
 *
 * The same reasoning picks the *instrument*. A centre hit-test is not enough:
 * `#settings-btn` spans y 12.75–48.25 against a stack beginning at y 16, so
 * 3.25px of it stays visible, and a centre probe scores that sliver and total
 * occlusion identically. Where this file asks "is the control covered", it asks
 * for **rect-intersection area exactly 0** and keeps the hit test as a second,
 * cruder statement.
 */

cleanUpCreatedStories("Alpha Project");

test.beforeEach(async ({ page, context }) => {
  // Without this the Copy-* paths take `copyText`'s `.catch` branch and raise an
  // ERROR notice instead of a success. That is not a cosmetic difference: an
  // error is durable and carries a `.toast-dismiss`, so a test meaning to
  // exercise self-clearing notices would silently exercise durable ones. It cost
  // this story's own investigation one measurement that reported the exact
  // opposite of the truth before the variant was read off the node.
  await context.grantPermissions(["clipboard-read", "clipboard-write"]);
  await seedToken(page);
});

/** Turns durable notices on through the real Settings control.
 *
 * Through the page rather than by seeding `localStorage`: SH-322 shipped this as
 * a mechanism the user can reach, and a seeded key would satisfy the storage
 * half of that claim while proving no mechanism exists at all. */
async function keepNotices(page: Page): Promise<void> {
  await page.locator("#settings-btn").click();
  await expect(page.locator("#settings-view")).toBeVisible();
  await page.locator("#toggle-keep-notices").click();
  await expect(page.locator("#toggle-keep-notices")).toBeChecked();
  await page.locator("#home-btn").click();
}

/** Raises one durable notice per call through the story context menu, using a
 * different copy target each time so the notices are distinguishable by text.
 *
 * The three labels are the whole reason this path is used rather than three
 * Copy-IDs: `copyText` names the target in its headline, so "ID copied", "URL
 * copied" and "Description copied" are three notices an order assertion can tell
 * apart. Three identical "ID copied" notices could not pin an order at all. */
const COPY_TARGETS = ["Copy ID", "Copy URL", "Copy Description"] as const;
const COPY_HEADLINES = ["ID copied", "URL copied", "Description copied"] as const;

async function raiseNotice(page: Page, title: string, which: number): Promise<void> {
  const card = page.locator('.column[data-state="todo"] .card', { hasText: title });
  await card.click({ button: "right" });
  await page.locator(".ctxmenu-item", { hasText: COPY_TARGETS[which] }).click();
}

async function createStory(page: Page, title: string): Promise<string> {
  await page.locator("#new-story-btn").click();
  await expect(page.locator("#create-modal")).toHaveClass(/open/);
  await page.locator("#create-title").fill(title);
  await page.locator("#create-description").fill("a description, so Copy Description has something to copy");
  await page.locator("#create-submit").click();
  await expect(page.locator("#create-modal")).not.toHaveClass(/open/);
  const card = page.locator('.column[data-state="todo"] .card', { hasText: title });
  await expect(card).toBeVisible();
  return (await card.getAttribute("data-id"))!;
}

/** The top edge of every element matching `selector`, in DOM order. */
async function domOrderTops(page: Page, selector: string): Promise<number[]> {
  return page.$$eval(selector, (nodes) =>
    nodes.map((n) => (n as HTMLElement).getBoundingClientRect().top),
  );
}

test("the newest notice is first in the DOM and first on the screen", async ({
  page,
}) => {
  await page.goto("/");
  await openProject(page, "Alpha Project");
  await keepNotices(page);
  await openProject(page, "Alpha Project");

  const title = "SH-323 — notice order";
  await createStory(page, title);

  for (let i = 0; i < COPY_TARGETS.length; i++) {
    await raiseNotice(page, title, i);
    await expect(page.locator("#toast-stack .toast")).toHaveCount(i + 1);
  }

  // DOM order is newest-first. This is the half that a screen reader and the
  // tab sequence both follow.
  const texts = await page.$$eval("#toast-stack .toast .notice-headline", (nodes) =>
    nodes.map((n) => n.textContent),
  );
  expect(texts).toEqual([...COPY_HEADLINES].reverse());

  // ...and visual order agrees with it. Asserted as geometry rather than by
  // reading the CSS, because `flex-direction` is exactly the property that can
  // make these two disagree — which is the defect this ordering exists to avoid,
  // and which `#dispatch-history` had until this story.
  const tops = await domOrderTops(page, "#toast-stack .toast");
  expect(tops).toEqual([...tops].sort((a, b) => a - b));
});

test("the newest dispatch-history row is first in the DOM and first on the screen", async ({
  page,
}) => {
  await page.goto("/");
  await openProject(page, "Alpha Project");

  const first = await createStory(page, "SH-323 — history order A");
  const second = await createStory(page, "SH-323 — history order B");

  // Keyed on the story id in the request path (`/story/<id>/dispatch`), so both
  // rows are real, distinct outcomes rather than one row rendered twice.
  await page.route("**/dispatch**", async (route) => {
    const url = route.request().url();
    const which = url.includes(first) ? first : second;
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

  for (const id of [first, second]) {
    await page.locator(`.card[data-id="${id}"]`).click();
    await expect(page.locator("#drawer")).toHaveClass(/open/);
    await page.locator("#dispatch-auto-btn").click();
    await expect(
      page.locator("#dispatch-history .dispatch-history-row", { hasText: id }),
    ).toBeVisible();
    await page.locator("#drawer-close").click();
    await expect(page.locator("#drawer")).not.toHaveClass(/open/);
  }

  const rows = page.locator("#dispatch-history .dispatch-history-row");
  await expect(rows).toHaveCount(2);
  // `second` was dispatched last, so it is the newest and must be first.
  await expect(rows.nth(0)).toContainText(second);
  await expect(rows.nth(1)).toContainText(first);

  // The pin that fails today: `.dispatch-history` is `column-reverse`, so its
  // DOM order is the exact reverse of its visual order. That mismatch was
  // harmless while the panel had `overflow: visible` and no scroller — nothing
  // depended on which edge `scrollTop 0` meant. This story gives it a scroller,
  // which is what makes it load-bearing, so the reversal goes rather than being
  // pinned in the one browser this suite can drive.
  const tops = await domOrderTops(page, "#dispatch-history .dispatch-history-row");
  expect(tops).toEqual([...tops].sort((a, b) => a - b));
});
