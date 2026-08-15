import { test, expect } from "@playwright/test";
import type { Page } from "@playwright/test";
import {
  cleanUpCreatedStories,
  COPY_HEADLINES,
  COPY_TARGETS,
  createStory,
  keepNotices,
  onAFrozenClock,
  openProject,
  raiseDurableNotices,
  raiseNotice,
  seedToken,
} from "./support";

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

/** Asserts that focus landed somewhere a user can actually be, without naming
 * which element.
 *
 * A PROPERTY, not an identity. SH-323 wrote this inline against "Dismiss all"
 * and predicted the anchor would move; SH-326 made that true by giving every
 * per-notice dismissal the same last-resort landing, so the block is a shared
 * helper now and **both paths call it**. That is the point of sharing it rather
 * than copying it: the single and bulk landings cannot drift apart unnoticed,
 * because one assertion covers both.
 *
 * `!closest("#notice-dock")` is SH-326's addition and it is the clause that
 * discriminates. Every rejected candidate in that council — `#toast-stack` given
 * `tabindex="-1"`, `#toast-scroll`, the dock itself — satisfies "not body" while
 * parking the user in a `pointer-events: none` region with no focus indicator,
 * last in the document, whose next Tab leaves for the browser chrome. Without
 * this line the test blesses the thing the decision rejected. */
async function expectFocusLanded(page: Page, after: string): Promise<void> {
  const landed = await page.evaluate(() => {
    const el = document.activeElement as HTMLElement | null;
    if (!el || el === document.body) return { ok: false, why: "body or nothing" };
    if (el.closest("[inert]")) return { ok: false, why: "inside an inert subtree" };
    if (el.closest("#notice-dock")) return { ok: false, why: "inside the notice dock" };
    const r = el.getBoundingClientRect();
    const onScreen = r.bottom > 0 && r.top < window.innerHeight && r.right > 0 && r.left < window.innerWidth;
    return { ok: onScreen, why: onScreen ? "" : "off-screen", id: el.id };
  });
  expect(landed.ok, `focus after ${after}: ${landed.why}`).toBe(true);

  // ...and Tab from there still reaches the page's own controls.
  let reached = false;
  for (let i = 0; i < 30 && !reached; i++) {
    await page.keyboard.press("Tab");
    reached = await page.evaluate(
      () => !!document.activeElement?.closest(".topbar"),
    );
  }
  expect(reached, `Tab from the anchor after ${after} must reach the topbar`).toBe(true);
}

/** The id of whatever holds focus, or a description of why nothing does.
 *
 * Used to assert that two different gestures land on the SAME element, which is
 * the property that keeps one dismissal and a bulk clear from teaching a user
 * two different places to expect focus. */
async function focusedIdentity(page: Page): Promise<string> {
  return page.evaluate(() => {
    const el = document.activeElement as HTMLElement | null;
    if (!el) return "nothing";
    if (el === document.body) return "body";
    return el.id ? `#${el.id}` : el.tagName.toLowerCase();
  });
}

/** Dismisses a notice WITHOUT the browser focusing the button first.
 *
 * `element.click()` dispatches the activation and moves no focus in any engine,
 * which is the state a real pointer click leaves behind **on WebKit** — this
 * file's own source records the measurement (`src/web_dashboard.html`, the
 * `armedDeleteSlug` comment: `document.activeElement` is `BODY` after a real
 * click on a `<button>`).
 *
 * It **emulates** that state; it does not prove it. `playwright.config.ts`
 * installs chromium and mobile-chromium and nothing else, so the engine this
 * guard exists for is not under test here at all (SH-335). Said plainly rather
 * than left to be inferred: a pin whose comment implies coverage it does not
 * have is the SH-306 shape. */
async function dismissWithoutFocusing(page: Page, selector: string): Promise<void> {
  await page.evaluate((sel) => {
    (document.querySelector(sel) as HTMLElement).click();
  }, selector);
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

  await expectFocusLanded(page, "Dismiss all");
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

// ============================================================
// SH-326 — where focus goes when ONE notice is dismissed
// ============================================================
//
// The bulk case above is SH-323's. This is the per-notice one, and the council
// that settled it (`.council/sh326-notice-dismiss-focus-landing/DECISION.md`,
// unanimous 3-0) resolved the question the story was filed to answer — where
// focus goes when the dismissed notice was the LAST one — by finding that it did
// not need a new answer. `focusAfterNoticeRemoval()` is not a rival policy; it is
// the last clause of one rule:
//
//   Capture `document.activeElement` before the mutation. After it: if focus was
//   not in this notice region, or focus still lives somewhere real, do nothing.
//   Otherwise focus the heir. If there is no heir, `focusAfterNoticeRemoval()`.
//
// The gate is a STATE question, never a modality question, and that is the part
// a future author is most likely to "simplify". `event.detail === 0` is not a
// keyboard test: assistive technology dispatches synthesised clicks with
// `detail === 0`, so it fires for AT-driven pointer users and not for real ones.
// And this file's own source records that WebKit focuses no `<button>` on click
// at all, so "was this a keyboard activation" is unanswerable while "was the
// focused element destroyed" is directly observable — SH-226's doctrine one
// layer down.

test("a dismissed notice hands focus to the notice that took its place", async ({
  page,
}) => {
  await page.goto("/");
  await openProject(page, "Alpha Project");
  await keepNotices(page);
  await openProject(page, "Alpha Project");

  const title = "SH-326 — the heir";
  await createStory(page, title);
  await raiseDurableNotices(page, title, 3);

  // Prepended, so DOM order is newest first: Description, URL, ID.
  const headlines = await page.$$eval("#toast-stack .toast .notice-headline", (ns) =>
    ns.map((n) => n.textContent),
  );
  expect(headlines).toEqual(["Description copied", "URL copied", "ID copied"]);

  // The MIDDLE notice, and that is the point of this test rather than a detail
  // of it. Dismissing the first or the last cannot tell next-first from
  // previous-first apart — at the top there is no previous, at the bottom no
  // next, so both rules pick the same survivor and a reversed implementation
  // passes. Only a notice with a neighbour on each side puts the direction
  // under test. (Found by mutation-checking this pin, which the first version
  // survived.)
  await page.locator("#toast-stack .toast .toast-dismiss").nth(1).focus();
  await page.keyboard.press("Enter");
  await expect(page.locator("#toast-stack .toast")).toHaveCount(2);

  // The NEXT sibling, not merely "a neighbour". Notices are prepended, so the
  // following sibling is the older notice that moves up into the vacated pixels
  // — focus follows position, not age. "ID copied" was below the dismissed
  // notice; "Description copied" was above it and must not win.
  const heir = await page.evaluate(() => {
    const el = document.activeElement as HTMLElement | null;
    return {
      isDismiss: !!el?.classList.contains("toast-dismiss"),
      headline: el?.closest(".toast")?.querySelector(".notice-headline")?.textContent ?? null,
    };
  });
  expect(heir.isDismiss, "focus must be on a dismiss button, not merely somewhere").toBe(true);
  expect(heir.headline).toBe("ID copied");

  // The ergonomic claim, asserted as behaviour rather than left in prose: the
  // pile clears with repeated Enter and no Tab in between. Before this fix each
  // press cost a full traversal back from the top of the document, because the
  // dock is last in the DOM.
  await page.keyboard.press("Enter");
  await expect(page.locator("#toast-stack .toast")).toHaveCount(1);
  await page.keyboard.press("Enter");
  await expect(page.locator("#toast-stack .toast")).toHaveCount(0);
});

test("the last notice in DOM order hands focus upward instead", async ({ page }) => {
  await page.goto("/");
  await openProject(page, "Alpha Project");
  await keepNotices(page);
  await openProject(page, "Alpha Project");

  const title = "SH-326 — the heir, upward";
  await createStory(page, title);
  await raiseDurableNotices(page, title, 3);

  // The oldest notice, at the bottom: there is no next sibling to inherit from.
  await page.locator("#toast-stack .toast .toast-dismiss").last().focus();
  await page.keyboard.press("Enter");
  await expect(page.locator("#toast-stack .toast")).toHaveCount(2);

  const headline = await page.evaluate(
    () =>
      (document.activeElement as HTMLElement | null)
        ?.closest(".toast")
        ?.querySelector(".notice-headline")?.textContent ?? null,
  );
  expect(headline, "the notice above must inherit when there is none below").toBe("URL copied");
});

test("the last notice standing lands focus exactly where a bulk clear does", async ({
  page,
}) => {
  await page.goto("/");
  await openProject(page, "Alpha Project");
  await keepNotices(page);
  await openProject(page, "Alpha Project");

  const title = "SH-326 — the anchor";
  await createStory(page, title);

  // One notice, dismissed by keyboard: the case the story was filed about.
  await raiseDurableNotices(page, title, 1);
  await page.locator("#toast-stack .toast .toast-dismiss").focus();
  await page.keyboard.press("Enter");
  await expect(page.locator("#toast-stack .toast")).toHaveCount(0);
  // Sampled BEFORE the property check, which tabs up to thirty times to prove
  // the anchor is not a dead end and therefore moves focus off it.
  const afterSingle = await focusedIdentity(page);
  await expectFocusLanded(page, "dismissing the last notice");

  // ...and the same anchor the bulk path uses. Asserted as an EQUALITY between
  // two gestures rather than as a named element, because the anchor is allowed
  // to move — what may never happen is the two teaching a user two different
  // places to expect focus for what is, to them, the same act.
  await raiseDurableNotices(page, title, 3);
  await page.locator("#toast-dismiss-all").focus();
  await page.keyboard.press("Enter");
  await expect(page.locator("#toast-stack .toast")).toHaveCount(0);
  const afterBulk = await focusedIdentity(page);

  expect(afterSingle, "one dismissal and a bulk clear must land on the same element").toBe(afterBulk);
});

test("a dismissal that took no focus moves none", async ({ page }) => {
  await page.goto("/");
  await openProject(page, "Alpha Project");
  await keepNotices(page);
  await openProject(page, "Alpha Project");

  const title = "SH-326 — nothing is stolen";
  await createStory(page, title);
  await raiseDurableNotices(page, title, 2);

  // The whole reason the gate exists. Without it, `focusAfterNoticeRemoval()`
  // on a per-notice handler would yank a user out of the field they are typing
  // in every time a notice was dismissed by pointer — a worse defect than the
  // stranding this story repairs, and the one objection SH-322's council raised
  // against moving focus here at all.
  await page.locator("#search-input").focus();
  await dismissWithoutFocusing(page, "#toast-stack .toast .toast-dismiss");
  await expect(page.locator("#toast-stack .toast")).toHaveCount(1);

  expect(await focusedIdentity(page)).toBe("#search-input");
});

test("focus is rescued when the control holding it is hidden rather than removed", async ({
  page,
}) => {
  await page.goto("/");
  await openProject(page, "Alpha Project");
  await keepNotices(page);
  await openProject(page, "Alpha Project");

  const title = "SH-326 — the vanishing bar";
  await createStory(page, title);
  await raiseDurableNotices(page, title, 2);
  await expect(page.locator("#toast-bar")).toBeVisible();

  // A second way one dismissal strands focus, which "was the focused node
  // removed?" alone does not catch: `#toast-dismiss-all` is not removed, it is
  // hidden with its bar when the durable count falls below two. The node is
  // still connected and still has a `focus` method — but the browser has already
  // dropped focus to `<body>`, so the rule has to ask where focus IS now, not
  // whether the element it was on still exists.
  await page.locator("#toast-dismiss-all").focus();
  await dismissWithoutFocusing(page, "#toast-stack .toast .toast-dismiss");
  await expect(page.locator("#toast-stack .toast")).toHaveCount(1);
  await expect(page.locator("#toast-bar")).toBeHidden();

  const rescued = await page.evaluate(() => {
    const el = document.activeElement as HTMLElement | null;
    return {
      onBody: el === document.body || !el,
      isDismiss: !!el?.classList.contains("toast-dismiss"),
      inHidden: !!el?.closest("[hidden]"),
    };
  });
  expect(rescued.onBody, "the vanishing bar must not strand focus on body").toBe(false);
  expect(rescued.inHidden, "focus must not be left inside a hidden subtree").toBe(false);
  expect(rescued.isDismiss).toBe(true);
});

test("each dismissal is announced, counting only the notices that need dismissing", async ({
  page,
}) => {
  await page.clock.install();
  await page.goto("/");
  await openProject(page, "Alpha Project");

  const title = "SH-326 — the announcement";
  await createStory(page, title);

  // A MIXED pile on purpose. Two durable notices plus one clocked one means the
  // two plausible counts differ — three toasts, two dismiss buttons — so an
  // implementation that counted `.toast` instead of the durable control would
  // pass every other assertion here and fail only this one. The visible
  // "Dismiss all N" already publishes the durable number; a spoken count that
  // disagreed with it in front of one reader is the SH-136 shape.
  await keepNotices(page);
  await openProject(page, "Alpha Project");
  await raiseDurableNotices(page, title, 2);

  await page.locator("#settings-btn").click();
  await page.locator("#toggle-keep-notices").click();
  await expect(page.locator("#toggle-keep-notices")).not.toBeChecked();
  await page.locator("#home-btn").click();
  await openProject(page, "Alpha Project");

  await onAFrozenClock(page, async () => {
    await raiseNotice(page, title, 0);
    await expect(page.locator("#toast-stack .toast")).toHaveCount(3);
    await expect(page.locator("#toast-stack .toast .toast-dismiss")).toHaveCount(2);

    await page.locator("#toast-stack .toast .toast-dismiss").first().click();
    await expect(page.locator("#notice-dock-status")).toHaveText("Notice dismissed. 1 remaining.");

    // A removal from an `aria-live="polite"` region is announced by nothing —
    // default `aria-relevant` is `additions text` — and the heir's accessible
    // name is character-for-character the destroyed button's. Without this line
    // a successful dismissal is indistinguishable from a dead key.
    await page.locator("#toast-stack .toast .toast-dismiss").first().click();
    await expect(page.locator("#notice-dock-status")).toHaveText(
      "Notice dismissed. No notices remaining.",
    );

    // What a screen reader actually utters is a hand check, not this test's
    // claim (the SH-322/SH-327 precedent). What is pinned here is the mechanism:
    // the text, the count, and that the announcement sits outside every live
    // region so it is never itself announced as a notice.
    const placement = await page.evaluate(() => {
      const status = document.getElementById("notice-dock-status")!;
      return {
        insideToastStack: !!status.closest("#toast-stack"),
        insideDispatchHistory: !!status.closest("#dispatch-history"),
      };
    });
    expect(placement.insideToastStack).toBe(false);
    expect(placement.insideDispatchHistory).toBe(false);
  });
});

test("a bulk clear that took no focus moves none either", async ({ page }) => {
  await page.goto("/");
  await openProject(page, "Alpha Project");
  await keepNotices(page);
  await openProject(page, "Alpha Project");

  const title = "SH-326 — the bulk guard";
  await createStory(page, title);
  await raiseDurableNotices(page, title, 2);
  await expect(page.locator("#toast-bar")).toBeVisible();

  // The defect SH-323 shipped and SH-326's council found: these two handlers
  // called `focusAfterNoticeRemoval()` unconditionally, which is safe only when
  // the bar button is focused by construction — i.e. only for a keyboard press.
  // A pointer press on WebKit focuses no `<button>`, so `document.activeElement`
  // was still the field the reader was typing in, and focus and caret went to
  // the drawer. Emulated here, not proven: chromium-only suite (SH-335).
  await page.locator("#search-input").focus();
  await dismissWithoutFocusing(page, "#toast-dismiss-all");
  await expect(page.locator("#toast-stack .toast")).toHaveCount(0);

  expect(await focusedIdentity(page)).toBe("#search-input");
});

test("a dismissal does not rehome focus that something else destroyed", async ({
  page,
}) => {
  await page.goto("/");
  await openProject(page, "Alpha Project");
  await keepNotices(page);
  await openProject(page, "Alpha Project");

  const title = "SH-326 — attribution";
  await createStory(page, title);
  await raiseDurableNotices(page, title, 2);

  // **This test asserts that focus is left on `<body>`, which looks like it is
  // asserting the defect. It is not.** The rule repairs the focus a notice
  // dismissal destroyed — no more. Here something else destroys it in the same
  // turn, and the dismissal must not claim the repair: focus movement
  // attributable to the wrong cause is what produces a bug report nobody can
  // reproduce a year later, because the gesture blamed for it is innocent.
  //
  // Written because the mutation battery found it missing. Deleting the region
  // gate reddened nothing: in every other pin the second clause ("focus still
  // lives somewhere real") answered first, so the gate looked load-bearing and
  // was untested. Only a survivor that is destroyed by a third party in the
  // same turn separates the two clauses.
  await page.locator("#search-input").focus();
  await page.evaluate(() => {
    document.getElementById("search-input")!.remove();
    (document.querySelector("#toast-stack .toast .toast-dismiss") as HTMLElement).click();
  });
  await expect(page.locator("#toast-stack .toast")).toHaveCount(1);

  expect(
    await page.evaluate(() => document.activeElement === document.body),
    "a dismissal must not adopt a stranding it did not cause",
  ).toBe(true);
});
