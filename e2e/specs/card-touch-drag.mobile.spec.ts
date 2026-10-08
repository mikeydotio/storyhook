import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import type { Page } from "@playwright/test";
import { test as dashboardTest, expect } from "./support";

// This spec owns an entirely synthetic HTTP fixture, so it needs neither a
// daemon token nor real-project healing. Keep the shared watchdog/assertion
// fixtures; replace only the two external-store fixtures for this spec.
const test = dashboardTest.extend({
  testToken: [async ({}, use) => { await use(); }, { auto: true }],
  fixtureHeal: [async ({}, use) => { await use(); }, { auto: true }],
});

// SH-893: actual dashboard HTML and real browser hit testing, with synthetic
// summaries and captured writes. No daemon, credentials, or private story data.
// Synthetic touch event sequences exercise both engines; Chromium's trusted CDP gesture
// below additionally checks browser scrolling/default-action negotiation.
test.use({ viewport: { width: 1024, height: 768 }, hasTouch: true, isMobile: true });
const origin = "http://storyhook-touch.test";
const states = ["todo", "in-progress", "blocked", "verifying", "done"];
async function board(page: Page, automations = false) {
  const writes: { method: string; path: string; body: any }[] = [];
  const rows = states.map((state, i) => ({ story: {
    id: `TT-${i + 1}`, title: `Touch story ${i + 1}`, state,
    superstate: state === "done" ? "CLOSED" : "OPEN", priority: "medium",
    story_type: "normal", labels: [], relationships: [], comments: [], attachments: [],
    description: "Synthetic detail", created_at: "2026-10-01T00:00:00Z",
    updated_at: "2026-10-01T00:00:00Z", awaiting: null,
  }, is_summary: true, is_ready: true, is_blocked: false }));
  const metadata = {
    meta: { states: states.map(slug => ({ slug, super_state: slug === "done" ? "CLOSED" : "OPEN" })),
      types: [{ slug: "normal", emoji: "📙" }], priorities: ["high", "medium", "low"], labels: [], relations: [],
      defaults: { state: "todo", story_type: "normal", priority: "medium" } },
    automations_enabled: automations, drafts: [], refs: {},
    summary: { total_open: 4, total_closed: 1, ready_count: 4, blocked_count: 0 },
    counts: { all: 5, total: 5, columns: Object.fromEntries(states.map(s => [s, 1])), drafts: 0 },
    verification_control: { state: "stopped" }, verifier: { control: "stopped", active: null, held_stories: [] },
  };
  let prefs = { view: "board", hiddenColumns: [], filtersOpen: false, repoId: "synthetic" };
  const html = readFileSync(resolve(__dirname, "../../src/web_dashboard.html"), "utf8");
  await page.route("**/*", async route => {
    const request = route.request(), url = new URL(request.url());
    const json = (body: unknown) => route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify(body) });
    if (url.pathname === "/") return route.fulfill({ contentType: "text/html", body: html });
    if (url.pathname === "/api/preferences") {
      if (request.method() === "PATCH") prefs = { ...prefs, ...request.postDataJSON() };
      return json(prefs);
    }
    if (url.pathname === "/api/repos") return json([{ id: "synthetic", name: "Touch fixture", prefix: "TT", available: true, visible: true, summary: metadata.summary }]);
    if (url.pathname.endsWith("/board")) {
      const options = JSON.parse(url.searchParams.get("options") || "{}");
      const selected = options.limit === 0 ? [] : rows.filter(v => !options.column || v.story.state === options.column);
      return json({ ...metadata, stories: selected, ready_ids: selected.map(v => v.story.id), blocked_ids: [], next_ids: [],
        page: { total: selected.length, next_cursor: null, revision: "1", offset: 0 } });
    }
    const match = url.pathname.match(/\/story\/(TT-\d+)(\/move)?$/);
    if (match) {
      const row = rows.find(v => v.story.id === match[1])!;
      if (request.method() !== "GET") {
        const body = request.postDataJSON();
        writes.push({ method: request.method(), path: url.pathname, body });
        Object.assign(row.story, body);
      }
      return json({ story: { ...row, is_summary: false } });
    }
    if (url.pathname === "/api/events") return route.fulfill({ contentType: "text/event-stream", body: ": fixture\n\n" });
    return json({});
  });
  await page.goto(origin + "/?project=synthetic");
  await expect(page.locator('.card[data-id="TT-1"]')).toBeVisible();
  return { writes, rows };
}
async function point(page: Page, selector: string) {
  const box = await page.locator(selector).boundingBox();
  if (!box) throw new Error("Missing touch target " + selector);
  return { x: box.x + box.width / 2, y: box.y + Math.min(30, box.height / 2) };
}
async function touch(page: Page, type: string, p: { x: number; y: number }, count = 1) {
  return page.evaluate(({ type, p, count }) => {
    const target = (window as any).__touchTarget || document.elementFromPoint(p.x, p.y)!;
    if (type === "touchstart") (window as any).__touchTarget = target;
    const t = { identifier: 1, target, clientX: p.x, clientY: p.y };
    const touches = type === "touchend" || type === "touchcancel" ? [] : [t];
    if (count === 2) touches.push({ identifier: 2, target, clientX: p.x + 10, clientY: p.y });
    // WebKit's Touch constructor is not public on desktop builds. Supply
    // the event's readonly fields explicitly; this is not trusted input.
    const event = new Event(type, { bubbles: true, cancelable: true });
    Object.defineProperties(event, {
      touches: { value: touches }, targetTouches: { value: touches }, changedTouches: { value: [t] },
    });
    target.dispatchEvent(event);
    if (!touches.length) delete (window as any).__touchTarget;
    return event.defaultPrevented;
  }, { type, p, count });
}
async function start(page: Page, id = "TT-1") {
  const p = await point(page, `.card[data-id="${id}"]`);
  await touch(page, "touchstart", p);
  await expect(page.locator(`.card[data-id="${id}"]`)).toHaveClass(/dragging/);
  return p;
}
async function drop(page: Page, slug: string) {
  const p = await point(page, `.column[data-state="${slug}"]`);
  expect(await touch(page, "touchmove", p)).toBe(true);
  await touch(page, "touchend", p);
}

test("held touch moves a card once without selecting text or opening its drawer", async ({ page }) => {
  const { writes } = await board(page);
  await start(page);
  await drop(page, "in-progress");
  await expect.poll(() => writes.length).toBe(1);
  expect(writes[0].body).toEqual({ state: "in-progress" });
  await expect(page.locator("#drawer")).not.toHaveClass(/open/);
  await expect(page.locator(".dragging, .touch-drag-preview, .drag-over")).toHaveCount(0);
  expect(await page.locator('.card[data-id="TT-1"]').evaluate(n => getComputedStyle(n).userSelect)).toBe("none");
});

test("quick movement remains scrolling and tapping still opens the drawer", async ({ page }) => {
  const { writes } = await board(page);
  const p = await point(page, '.card[data-id="TT-1"]');
  await touch(page, "touchstart", p);
  expect(await touch(page, "touchmove", { x: p.x, y: p.y + 35 })).toBe(false);
  await touch(page, "touchend", p);
  await expect(page.locator(".dragging, .touch-drag-preview")).toHaveCount(0);
  expect(writes).toEqual([]);
  await page.locator('.card[data-id="TT-1"]').tap();
  await expect(page.locator("#drawer")).toHaveClass(/open/);
});

test("cancel, multitouch, and leaving the board cancel a held drag", async ({ page }) => {
  const { writes } = await board(page);
  for (const cancel of ["touchcancel", "multitouch", "blur"]) {
    const p = await start(page);
    if (cancel === "multitouch") await touch(page, "touchstart", p, 2);
    else if (cancel === "blur") await page.evaluate(() => window.dispatchEvent(new Event("blur")));
    else await touch(page, cancel, p);
    await touch(page, "touchend", p);
    await expect(page.locator(".dragging, .touch-drag-preview, .drag-over")).toHaveCount(0);
  }
  await start(page);
  await page.locator('#view-toggle button[data-view="list"]').click();
  await expect(page.locator(".touch-drag-preview")).toHaveCount(0);
  expect(writes).toEqual([]);
});

test("touch drop shares Blocked reason and same-column no-op behavior", async ({ page }) => {
  const { writes } = await board(page);
  await start(page);
  await drop(page, "todo");
  expect(writes).toEqual([]);
  await start(page);
  await drop(page, "blocked");
  await expect(page.locator("#drop-blocked-reason-modal")).toHaveClass(/open/);
  expect(writes).toEqual([]);
  await page.locator("#drop-blocked-reason-input").fill("Waiting for review");
  await page.locator("#drop-blocked-reason-submit").click();
  await expect.poll(() => writes.length).toBe(1);
  expect(writes[0].body).toEqual({ state: "blocked", reason: "Waiting for review" });
});

test("edge scrolling reaches offscreen columns and preserves verifier override", async ({ page }) => {
  const { writes } = await board(page, true);
  await page.locator('.card[data-id="TT-4"]').scrollIntoViewIfNeeded();
  await start(page, "TT-4");
  const edge = await page.locator("#board-view").evaluate(n => {
    const r = n.getBoundingClientRect();
    return { x: r.right - 4, y: r.top + 50, initial: n.scrollLeft };
  });
  await touch(page, "touchmove", edge);
  await expect.poll(() => page.locator("#board-view").evaluate(n => n.scrollLeft)).toBeGreaterThan(edge.initial);
  await expect.poll(async () => (await page.locator('.column[data-state="done"]').boundingBox())!.x).toBeLessThan(800);
  const p = await point(page, '.column[data-state="done"]');
  await touch(page, "touchmove", p);
  await touch(page, "touchend", p);
  await expect(page.locator("#verify-override-modal")).toHaveClass(/open/);
  expect(writes).toEqual([]);
});


test("closed cards and interactive card actions never start a touch drag", async ({ page }) => {
  const { writes } = await board(page);
  await page.locator('.card[data-id="TT-5"]').scrollIntoViewIfNeeded();
  const p = await point(page, '.card[data-id="TT-5"]');
  await touch(page, "touchstart", p);
  await page.waitForTimeout(300); // Cross the product's 250ms hold threshold.
  await touch(page, "touchend", p);
  await expect(page.locator(".dragging, .touch-drag-preview")).toHaveCount(0);
  await page.locator('.card[data-id="TT-1"]').scrollIntoViewIfNeeded();
  const action = page.locator('.card[data-id="TT-1"] .card-actions-btn');
  await action.tap();
  await expect(page.locator('.ctxmenu[role="menu"]')).toBeVisible();
  expect(writes).toEqual([]);
});

test("mouse drag still uses the same destination after touch cancellation", async ({ page }) => {
  const { writes } = await board(page);
  const p = await start(page);
  await touch(page, "touchcancel", p);
  await page.locator('.card[data-id="TT-1"]').dragTo(page.locator('.column[data-state="in-progress"]'));
  await expect.poll(() => writes.length).toBe(1);
  expect(writes[0].body).toEqual({ state: "in-progress" });
});

test("trusted touch drag prevents native panning while an immediate swipe scrolls", async ({ page, browserName }) => {
  test.skip(browserName !== "chromium", "WebKit has no protocol touch-move injection; covered by TouchEvents and native tap above");
  const { writes } = await board(page);
  const cdp = await page.context().newCDPSession(page);
  const p = await point(page, '.card[data-id="TT-1"]');
  const target = await point(page, '.column[data-state="in-progress"]');
  const send = (type: string, point?: { x: number; y: number }) => cdp.send("Input.dispatchTouchEvent", {
    type, touchPoints: point ? [{ x: point.x, y: point.y, id: 1 }] : [],
  });
  try {
    await send("touchStart", p);
    await expect(page.locator('.card[data-id="TT-1"]')).toHaveClass(/dragging/);
    await send("touchMove", target);
    await send("touchEnd");
    await expect.poll(() => writes.length).toBe(1);
    expect(await page.locator("#board-view").evaluate(n => n.scrollLeft)).toBe(0);
    await expect(page.locator("#drawer")).not.toHaveClass(/open/);
    await send("touchStart", { x: 700, y: p.y });
    for (const x of [650, 580, 500, 420]) await send("touchMove", { x, y: p.y });
    await send("touchEnd");
    await expect.poll(() => page.locator("#board-view").evaluate(n => n.scrollLeft)).toBeGreaterThan(0);
    expect(writes).toHaveLength(1);
  } finally { await cdp.detach(); }
});


test("progressive refresh cancels a drag whose source state changed", async ({ page }) => {
  await page.clock.install();
  const { writes, rows } = await board(page);
  const p = await point(page, '.card[data-id="TT-1"]');
  await touch(page, "touchstart", p);
  await page.clock.runFor(300);
  await expect(page.locator('.card[data-id="TT-1"]')).toHaveClass(/dragging/);
  rows[0].story.state = "blocked";
  await page.clock.runFor(26000); // Drive the actual 25-second safety poll.
  await expect(page.locator('.column[data-state="blocked"] .card[data-id="TT-1"]')).toBeVisible();
  await page.clock.runFor(50);
  await expect(page.locator(".dragging, .touch-drag-preview, .drag-over")).toHaveCount(0);
  await touch(page, "touchend", await point(page, '.column[data-state="in-progress"]'));
  expect(writes).toEqual([]);
});

test("an in-flight move cannot be dragged a second time", async ({ page }) => {
  const { writes } = await board(page);
  let release!: () => void;
  const held = new Promise<void>(resolve => { release = resolve; });
  await page.route("**/story/TT-1/move", async route => {
    if (route.request().method() === "POST") await held;
    await route.fallback();
  });
  try {
    await start(page);
    await drop(page, "in-progress");
    const card = page.locator('.card[data-id="TT-1"]');
    await expect(card).toHaveClass(/pending/);
    const p = await point(page, '.card[data-id="TT-1"]');
    await touch(page, "touchstart", p);
    await page.waitForTimeout(300); // Cross the hold threshold while pending.
    await expect(page.locator(".dragging, .touch-drag-preview")).toHaveCount(0);
    await touch(page, "touchend", p);
    expect(writes).toEqual([]);
  } finally { release(); }
  await expect.poll(() => writes.length).toBe(1);
});

test("held drag scrolls a tall column and dropping outside the board does nothing", async ({ page }) => {
  const { writes, rows } = await board(page);
  for (let i = 0; i < 20; i++) rows.push({ ...rows[0], story: { ...rows[0].story, id: `TT-${100 + i}`, title: `Overflow card ${i}` } });
  await page.reload();
  await expect(page.locator('.column[data-state="todo"] .card')).toHaveCount(21);
  await start(page);
  const edge = await page.locator('.column[data-state="todo"] .column-cards').evaluate(n => {
    const r = n.getBoundingClientRect();
    return { x: r.left + r.width / 2, y: r.bottom - 4 };
  });
  await touch(page, "touchmove", edge);
  await expect.poll(() => page.locator('.column[data-state="todo"] .column-cards').evaluate(n => n.scrollTop)).toBeGreaterThan(0);
  await touch(page, "touchmove", { x: 10, y: 10 });
  await touch(page, "touchend", { x: 10, y: 10 });
  await expect(page.locator(".dragging, .touch-drag-preview, .drag-over")).toHaveCount(0);
  expect(writes).toEqual([]);
});
