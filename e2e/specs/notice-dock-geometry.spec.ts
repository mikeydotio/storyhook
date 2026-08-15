import { test, expect } from "@playwright/test";
import type { Page } from "@playwright/test";
import { cleanUpCreatedStories, onAFrozenClock, openProject, seedToken } from "./support";

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

/** Waits until no backdrop is still on screen.
 *
 * Closing an overlay is a 0.18s opacity transition, and `.backdrop` carries no
 * `pointer-events: none` — so for those 180ms a full-viewport `position: fixed;
 * inset: 0` element is still the thing under the cursor everywhere. A hit test
 * taken in that window reports the backdrop and says nothing about the dock.
 * Found by this file's own failure message naming `div#modal-backdrop.backdrop`,
 * which is why `centreOwner` returns the occluder rather than a boolean. */
async function awaitNoOverlay(page: Page): Promise<void> {
  await page.waitForFunction(
    () => document.querySelectorAll(".backdrop:not([hidden])").length === 0,
    undefined,
    { timeout: 5000 },
  );
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
  await awaitNoOverlay(page);
  return (await card.getAttribute("data-id"))!;
}

/** The top edge of every element matching `selector`, in DOM order. */
async function domOrderTops(page: Page, selector: string): Promise<number[]> {
  return page.$$eval(selector, (nodes) =>
    nodes.map((n) => (n as HTMLElement).getBoundingClientRect().top),
  );
}

/** The area, in px², where `#notice-dock` overlaps `selector`.
 *
 * The primary instrument, and a centre hit-test is deliberately not it. With one
 * notice standing, `#settings-btn` spans y 12.75–48.25 against a stack starting
 * at y 16 — so 3.25px of it stays visible, and `elementFromPoint` at its centre
 * reports a toast either way. A probe that scores "a sliver survives" and
 * "entirely buried" identically cannot tell a fix from a near-miss, and would
 * equally bless a dock that cleared every centre while eating a button's lower
 * third. Zero shared area is the property; the hit test is kept beside it as a
 * cruder second statement. */
async function overlapArea(page: Page, selector: string): Promise<number> {
  return page.evaluate((sel) => {
    const dock = document.getElementById("notice-dock");
    const target = document.querySelector(sel);
    if (!dock || !target) return -1;
    const a = dock.getBoundingClientRect();
    const b = target.getBoundingClientRect();
    const w = Math.max(0, Math.min(a.right, b.right) - Math.max(a.left, b.left));
    const h = Math.max(0, Math.min(a.bottom, b.bottom) - Math.max(a.top, b.top));
    return w * h;
  }, selector);
}

/** What `selector`'s own centre hit-tests to: `"self"`, or a description of
 * whatever is on top of it.
 *
 * Returns the occluder rather than a boolean on purpose. A bare `false` sends
 * the next reader hunting for what covered the control, which is the position
 * this story's own investigation started from; naming it in the failure message
 * is the difference between a five-minute diagnosis and a thirty-minute one. */
async function centreOwner(page: Page, selector: string): Promise<string> {
  return page.evaluate((sel) => {
    const target = document.querySelector(sel) as HTMLElement | null;
    if (!target) return "missing";
    const b = target.getBoundingClientRect();
    const hit = document.elementFromPoint(b.x + b.width / 2, b.y + b.height / 2) as HTMLElement | null;
    if (!hit) return `nothing (rect ${JSON.stringify(b)})`;
    if (hit === target || target.contains(hit)) return "self";
    const id = hit.id ? `#${hit.id}` : "";
    const cls = hit.className && typeof hit.className === "string" ? `.${hit.className.split(" ").join(".")}` : "";
    return `${hit.tagName.toLowerCase()}${id}${cls}`;
  }, selector);
}

/** Raises `count` further durable notices, asserting the running total after
 * each one.
 *
 * Counted from whatever is already standing rather than from zero: a helper that
 * assumed an empty stack would silently pass while measuring the wrong pile the
 * first time a caller topped one up. Asserting after every raise is what keeps a
 * geometry assertion from racing a stack that is still growing under it. */
async function raiseDurableNotices(page: Page, title: string, count: number): Promise<void> {
  const start = await page.locator("#toast-stack .toast").count();
  for (let i = 0; i < count; i++) {
    await raiseNotice(page, title, i % COPY_TARGETS.length);
    await expect(page.locator("#toast-stack .toast")).toHaveCount(start + i + 1);
  }
}

/** The topbar controls the dock must never overlap. `#conn-dot` is a `<span>`
 * that takes no focus, so no criterion reaches it — it is here because it was
 * measured covered, and a notice sitting on the connection indicator is a defect
 * whether or not a criterion names it. */
const TOPBAR_CONTROLS = [
  "#settings-btn",
  "#new-story-btn",
  "#drafts-btn",
  "#home-btn",
  "#projsel-btn",
  "#conn-dot",
  "#filter-toggle-btn",
];

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

test("no topbar control is overlapped, at any notice count", async ({ page }) => {
  await page.goto("/");
  await openProject(page, "Alpha Project");
  await keepNotices(page);
  await openProject(page, "Alpha Project");

  const title = "SH-323 — topbar clearance";
  await createStory(page, title);

  // N = 0 included on purpose: an empty dock is still a `position: fixed` box
  // with a height, and "it only covers things once a notice arrives" would be a
  // fix that depends on the stack being empty to be correct.
  for (const n of [0, 1, 3, 12]) {
    const standing = await page.locator("#toast-stack .toast").count();
    if (n > standing) await raiseDurableNotices(page, title, n - standing);
    await expect(page.locator("#toast-stack .toast")).toHaveCount(n);
    for (const control of TOPBAR_CONTROLS) {
      expect(await overlapArea(page, control), `${control} at ${n} notices`).toBe(0);
      expect(await centreOwner(page, control), `${control} centre at ${n} notices`).toBe("self");
    }
  }
});

test("the drawer's Close and its footer buttons are not overlapped", async ({
  page,
}) => {
  await page.goto("/");
  await openProject(page, "Alpha Project");
  await keepNotices(page);
  await openProject(page, "Alpha Project");

  const title = "SH-323 — drawer clearance";
  await createStory(page, title);
  await raiseDurableNotices(page, title, 3);

  await page.locator(".card", { hasText: title }).click();
  await expect(page.locator("#drawer")).toHaveClass(/open/);
  // Settled, not merely open. Measured 30ms after `.open` lands, `#drawer-close`
  // reads x=1601 — off-screen, mid-transition — and a hit test there proves
  // nothing at all. This is the wait, and it is a property rather than a delay.
  await page.waitForFunction(
    () =>
      document.getElementById("drawer")!.getBoundingClientRect().right <=
      window.innerWidth + 0.5,
    undefined,
    { timeout: 5000 },
  );

  expect(await overlapArea(page, "#drawer-close")).toBe(0);
  expect(await centreOwner(page, "#drawer-close")).toBe("self");

  const footerButtons = await page.locator("#drawer-footer button").count();
  expect(footerButtons).toBeGreaterThan(0);
  for (let i = 0; i < footerButtons; i++) {
    const sel = `#drawer-footer button:nth-of-type(${i + 1})`;
    expect(await overlapArea(page, sel), sel).toBe(0);
  }
});

test("the band is measured, not a constant: the dock follows the chrome", async ({
  page,
}) => {
  await page.goto("/");
  await openProject(page, "Alpha Project");
  await keepNotices(page);
  await openProject(page, "Alpha Project");

  const title = "SH-323 — the band is measured";
  await createStory(page, title);
  await raiseDurableNotices(page, title, 2);

  const dockTop = () =>
    page.evaluate(() => document.getElementById("notice-dock")!.getBoundingClientRect().top);
  const topbarBottom = () =>
    page.evaluate(() => document.querySelector(".topbar")!.getBoundingClientRect().bottom);
  const before = await dockTop();
  const chromeBefore = await topbarBottom();

  // The pin no hand-derived formula can pass — including one that is correct
  // today. A token offset tuned to the four measured widths still fails here,
  // because this changes the chrome's height without changing the viewport.
  await page.evaluate(() => {
    const spacer = document.createElement("div");
    spacer.id = "sh323-spacer";
    spacer.style.height = "120px";
    spacer.style.width = "1px";
    document.querySelector(".topbar-right")!.appendChild(spacer);
  });
  // The dock must move by exactly what the CHROME moved, which is not the
  // spacer's own 120px: `.topbar` is `align-items: center` around a row that
  // already had height, so a 120px child grows the bar by 120 less whatever the
  // row already was. Asserting "moved by 120" would be asserting the fixture's
  // arithmetic; asserting "moved by however much the topbar moved" is the
  // property — the dock tracks the chrome, whatever the chrome does.
  await expect.poll(topbarBottom, { timeout: 5000 }).toBeGreaterThan(chromeBefore + 1);
  const chromeDelta = (await topbarBottom()) - chromeBefore;
  expect(chromeDelta).toBeGreaterThan(50);
  await expect.poll(dockTop, { timeout: 5000 }).toBeGreaterThan(before + chromeDelta - 2);
  expect(await dockTop()).toBeLessThan(before + chromeDelta + 2);
  for (const control of TOPBAR_CONTROLS) {
    expect(await overlapArea(page, control), `${control} with a taller topbar`).toBe(0);
  }

  await page.evaluate(() => document.getElementById("sh323-spacer")!.remove());
  await expect.poll(dockTop, { timeout: 5000 }).toBeLessThan(before + 1);
});

test("the pile is bounded, loses nothing, and every notice can be reached", async ({
  page,
}) => {
  test.setTimeout(120_000);
  await page.goto("/");
  await openProject(page, "Alpha Project");
  await keepNotices(page);
  await openProject(page, "Alpha Project");

  const title = "SH-323 — bounded and reachable";
  await createStory(page, title);
  await raiseDurableNotices(page, title, 24);

  const dock = await page.evaluate(() => {
    const d = document.getElementById("notice-dock")!.getBoundingClientRect();
    const scroll = document.getElementById("toast-scroll")!;
    return {
      top: d.top,
      bottom: d.bottom,
      innerHeight: window.innerHeight,
      scrollHeight: scroll.scrollHeight,
      clientHeight: scroll.clientHeight,
    };
  });
  // Bounded: the whole dock is inside the viewport, where 24 notices used to
  // reach y1733 in a 720px window.
  expect(dock.bottom).toBeLessThanOrEqual(dock.innerHeight);
  expect(dock.top).toBeGreaterThanOrEqual(0);
  // ...and genuinely scrollable, so the bound is doing something.
  expect(dock.scrollHeight).toBeGreaterThan(dock.clientHeight);
  // Nothing was silently deleted to achieve it. A count cap was the rejected
  // alternative, and this is what says it was not quietly adopted.
  await expect(page.locator("#toast-stack .toast")).toHaveCount(24);

  // Reachable, not merely present: the property the story reported broken was
  // that 13 of 24 were dismissable only blind. Each one is scrolled to and must
  // then own its own dismiss button's centre.
  for (let i = 0; i < 24; i++) {
    const dismiss = page.locator("#toast-stack .toast .toast-dismiss").first();
    await dismiss.scrollIntoViewIfNeeded();
    const owns = await page.evaluate(() => {
      const btn = document.querySelector("#toast-stack .toast .toast-dismiss") as HTMLElement;
      const b = btn.getBoundingClientRect();
      const hit = document.elementFromPoint(b.x + b.width / 2, b.y + b.height / 2);
      return !!hit && (hit === btn || btn.contains(hit));
    });
    expect(owns, `notice ${i + 1} of 24 must own its dismiss button's centre`).toBe(true);
    await dismiss.click();
    await expect(page.locator("#toast-stack .toast")).toHaveCount(23 - i);
  }
});

test("the scroller is a tab stop exactly while it can scroll", async ({ page }) => {
  await page.goto("/");
  await openProject(page, "Alpha Project");
  await keepNotices(page);
  await openProject(page, "Alpha Project");

  const title = "SH-323 — conditional tab stop";
  await createStory(page, title);

  // One notice: nothing to scroll to, so no tab stop. A permanent tabindex here
  // would be a stop that leads nowhere in the overwhelmingly common case.
  await raiseDurableNotices(page, title, 1);
  expect(await page.locator("#toast-scroll").getAttribute("tabindex")).toBeNull();

  // Enough to overflow: the region becomes operable, and a keyboard really moves
  // it. `End` is pressed once and the result is POLLED rather than read once,
  // because Chromium animates keyboard-initiated scrolling: three consecutive
  // instantaneous reads of this value, after End, PageDown and ArrowDown, all
  // returned 0 while the scroll was in fact under way, and only a later read saw
  // it land. A single read here would pass or fail on how quickly the browser
  // answered a round trip, which is the definition of a flaky pin.
  await raiseDurableNotices(page, title, 23);
  expect(await page.locator("#toast-scroll").getAttribute("tabindex")).toBe("0");
  await page.locator("#toast-scroll").focus();
  expect(
    await page.evaluate(() => document.activeElement === document.getElementById("toast-scroll")),
    "the scroller must actually hold focus before a key press means anything",
  ).toBe(true);
  const before = await page.evaluate(() => document.getElementById("toast-scroll")!.scrollTop);
  await page.keyboard.press("End");
  await expect
    .poll(
      () => page.evaluate(() => document.getElementById("toast-scroll")!.scrollTop),
      { timeout: 5000 },
    )
    .toBeGreaterThan(before);

  // Both directions, so neither a dead tab stop nor an unreachable region can
  // ship: dismissing back down to one removes the attribute again.
  const dismissAll = page.locator("#toast-stack .toast .toast-dismiss");
  while ((await page.locator("#toast-stack .toast").count()) > 1) {
    await dismissAll.first().scrollIntoViewIfNeeded();
    await dismissAll.first().click();
  }
  await expect(page.locator("#toast-stack .toast")).toHaveCount(1);
  expect(await page.locator("#toast-scroll").getAttribute("tabindex")).toBeNull();
});

test("the empty band passes clicks through to what is under it", async ({
  page,
}) => {
  await page.goto("/");
  await openProject(page, "Alpha Project");
  await keepNotices(page);
  await openProject(page, "Alpha Project");

  const title = "SH-323 — the dock is not a shield";
  await createStory(page, title);
  await raiseDurableNotices(page, title, 1);

  // `pointer-events: none` on the wrapper, asserted rather than assumed — the
  // assertion the version SH-322 rejected never had. The dock spans the whole
  // band, so without this the fix would trade occluding the topbar for
  // occluding the board.
  const hitInEmptyBand = await page.evaluate(() => {
    const d = document.getElementById("notice-dock")!.getBoundingClientRect();
    const stack = document.getElementById("toast-stack")!.getBoundingClientRect();
    // Well below the single notice, still inside the dock's own box.
    const y = Math.min(d.bottom - 4, stack.bottom + 40);
    const hit = document.elementFromPoint(d.left + d.width / 2, y);
    return {
      insideDock: !!hit && !!hit.closest("#notice-dock"),
      tag: hit ? hit.tagName : null,
    };
  });
  expect(hitInEmptyBand.insideDock).toBe(false);
});

test("Dismiss all appears at two notices, counts them, and is not a notice", async ({
  page,
}) => {
  await page.goto("/");
  await openProject(page, "Alpha Project");
  await keepNotices(page);
  await openProject(page, "Alpha Project");

  const title = "SH-323 — dismiss all";
  await createStory(page, title);
  const bar = page.locator("#toast-bar");

  // One notice already carries its own dismiss button, so a second control
  // would be doing the first one's job.
  await raiseDurableNotices(page, title, 1);
  await expect(bar).toBeHidden();

  await raiseDurableNotices(page, title, 1);
  await expect(bar).toBeVisible();
  await expect(page.locator("#toast-count")).toHaveText("2");

  await raiseDurableNotices(page, title, 1);
  await expect(page.locator("#toast-count")).toHaveText("3");

  // The count is in the ACCESSIBLE NAME, not merely on screen. A visible
  // "Dismiss all 3" announced as "Dismiss all" is an SC 2.5.3 failure, and so is
  // a fixed aria-label that says something the user cannot read — speech-input
  // users say what they see.
  await expect(
    page.getByRole("button", { name: "Dismiss all 3" }),
  ).toBeVisible();

  // And the control is outside every live region, so it is never announced as
  // notice content. Asserted against the accessibility tree rather than the CSS.
  const placement = await page.evaluate(() => {
    const btn = document.getElementById("toast-dismiss-all")!;
    const stack = document.getElementById("toast-stack")!;
    return {
      insideStack: stack.contains(btn),
      insideAnyLiveRegion: !!btn.closest('[aria-live], [role="status"], [role="alert"]'),
      // Nothing but notices may ever be inserted into the announced region.
      stackHoldsOnlyToasts: Array.from(stack.children).every((n) =>
        n.classList.contains("toast"),
      ),
    };
  });
  expect(placement.insideStack).toBe(false);
  expect(placement.insideAnyLiveRegion).toBe(false);
  expect(placement.stackHoldsOnlyToasts).toBe(true);

  // Dismissing back below the threshold hides it again.
  await page.locator("#toast-stack .toast .toast-dismiss").first().click();
  await page.locator("#toast-stack .toast .toast-dismiss").first().click();
  await expect(page.locator("#toast-stack .toast")).toHaveCount(1);
  await expect(bar).toBeHidden();
});

test("Dismiss all works from the keyboard and does not strand focus", async ({
  page,
}) => {
  await page.goto("/");
  await openProject(page, "Alpha Project");
  await keepNotices(page);
  await openProject(page, "Alpha Project");

  const title = "SH-323 — dismiss all by keyboard";
  await createStory(page, title);
  await raiseDurableNotices(page, title, 3);

  await page.locator("#toast-dismiss-all").focus();
  await page.keyboard.press("Enter");
  await expect(page.locator("#toast-stack .toast")).toHaveCount(0);

  // Focus is asserted as a PROPERTY, not as an identity: the anchor may change
  // (SH-326 will extend this helper), and a test naming one element would have
  // to be rewritten rather than reused. What must stay true is that focus is
  // somewhere real, visible, reachable and continuous.
  const landed = await page.evaluate(() => {
    const el = document.activeElement as HTMLElement | null;
    if (!el || el === document.body) return { ok: false, why: "body or nothing" };
    if (el.closest("[inert]")) return { ok: false, why: "inside an inert subtree" };
    const r = el.getBoundingClientRect();
    const onScreen = r.bottom > 0 && r.top < window.innerHeight && r.right > 0 && r.left < window.innerWidth;
    return { ok: onScreen, why: onScreen ? "" : "off-screen", id: el.id };
  });
  expect(landed.ok, `focus after Dismiss all: ${landed.why}`).toBe(true);

  // ...and Tab from there still reaches the page's own controls.
  let reached = false;
  for (let i = 0; i < 30 && !reached; i++) {
    await page.keyboard.press("Tab");
    reached = await page.evaluate(
      () => !!document.activeElement?.closest(".topbar"),
    );
  }
  expect(reached, "Tab from the post-dismissal anchor must reach the topbar").toBe(true);
});

test("Dismiss all cancels the clocks of the notices it removes", async ({
  page,
}) => {
  await page.clock.install();
  await page.goto("/");
  await openProject(page, "Alpha Project");

  const title = "SH-323 — bulk clear cancels clocks";
  await createStory(page, title);

  // A MIXED pile, which is the only way a bulk clear meets a running clock on a
  // real path: the bar is keyed on durable notices, so a clocked-only stack
  // never offers the control at all. Two durable ones make it appear; the
  // preference is then turned off and a third notice arrives with a live timer.
  await keepNotices(page);
  await openProject(page, "Alpha Project");
  await raiseDurableNotices(page, title, 2);
  await expect(page.locator("#toast-bar")).toBeVisible();

  await page.locator("#settings-btn").click();
  await page.locator("#toggle-keep-notices").click();
  await expect(page.locator("#toggle-keep-notices")).not.toBeChecked();
  await page.locator("#home-btn").click();
  await openProject(page, "Alpha Project");

  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(e.message));

  await onAFrozenClock(page, async () => {
    await raiseNotice(page, title, 0);
    await expect(page.locator("#toast-stack .toast")).toHaveCount(3);
    // The third has a clock and no dismiss button of its own; the other two have
    // buttons and no clock. The count names only the durable pair.
    await expect(page.locator("#toast-stack .toast .toast-dismiss")).toHaveCount(2);
    await expect(page.locator("#toast-count")).toHaveText("2");

    await page.locator("#toast-dismiss-all").click();
    await expect(page.locator("#toast-stack .toast")).toHaveCount(0);

    // Thirty seconds of fake time: a timer that survived its node would fire
    // here, against an element that no longer exists.
    await page.clock.runFor(30_000);
    await expect(page.locator("#toast-stack .toast")).toHaveCount(0);

    // And the document-level listener went with it. A notice removed by a bulk
    // clear never reaches `depart()`, so without an explicit teardown its
    // `visibilitychange` listener would outlive the node for the whole session.
    await page.evaluate(() => document.dispatchEvent(new Event("visibilitychange")));
    await expect(page.locator("#toast-stack .toast")).toHaveCount(0);
  });

  expect(errors, "a cancelled clock must not fire against a removed node").toEqual([]);
});
