import type { APIRequestContext, Page, Request } from "@playwright/test";
import { BASE_EXPECT_TIMEOUT_MS, gracedOperationBudget } from "../load-grace";
import {
  test, expect, cleanUpCreatedStories, clickHeaderAction, heldReadDeadlineMs,
  latch, openFilters, openProject, projectSlug, requiredEnv, seedToken,
} from "./support";

// SH-894: use the real daemon's pages, withholding only delivery. The probe
// copies closure state out of the served document; production gains no globals
// or test-only branch, and the tests cannot mutate the board through the probe.
type Options = { limit?: number; column?: string; cursor?: string; text?: string; drafts?: boolean };
type Card = { story: { id: string; title: string; [key: string]: unknown }; is_summary: boolean };
type Board = {
  stories: Card[]; refs: Record<string, unknown>;
  counts: { columns: Record<string, number>; total: number; drafts: number };
  page: { total: number; next_cursor: string | null; revision: string; offset: number };
};
type Probe = {
  repo: string; epoch: number; rows: Card[]; refs: Record<string, unknown>;
  pages: Record<string, { rows: Card[]; refs: Record<string, unknown>; loading: boolean; error: string | null }>;
  requests: number; queued: number; drawerRead: boolean;
  drawerId: string | null; drawerDetail: Card | null; drawerRefs: Record<string, unknown>;
  draft: { rows: Card[]; project: { id: string } } | null;
  draftOpen: { id: string; project: string } | null; draftDetailRead: boolean;
  retryClicks: number;
};
const releases = new Set<() => void>();
const headers = () => ({ "X-Storyhook": "1", "X-Storyhook-Token": requiredEnv("DASHBOARD_TOKEN") });
const boardUrl = (slug: string, options: Options) =>
  `/api/repos/${encodeURIComponent(slug)}/board?options=${encodeURIComponent(JSON.stringify(options))}`;
function optionsOf(url: URL): Options | null {
  if (!url.pathname.endsWith("/board")) return null;
  return JSON.parse(url.searchParams.get("options") || "{}");
}
async function probe(page: Page): Promise<Probe> {
  return page.evaluate(() => (window as unknown as { __boardProbe: Probe }).__boardProbe);
}
async function installProbe(page: Page) {
  await page.route(url => url.pathname === "/", async route => {
    const response = await route.fetch();
    const html = await response.text();
    const anchor = "})();\n</script>";
    expect(html.split(anchor)).toHaveLength(2);
    const instrument = `var boardProbeRetryClicks = 0;
    document.addEventListener('click', function(event) {
      if (event.isTrusted && event.target.closest &&
          event.target.closest('[data-page-key="todo"] .board-page-retry')) boardProbeRetryClicks++;
    }, true);
    Object.defineProperty(window, '__boardProbe', {get: function() {
      return JSON.parse(JSON.stringify({repo: state.repoId, epoch: boardEpoch,
        rows: state.data ? state.data.stories : [], refs: state.data ? state.data.refs : {},
        pages: boardPages, requests: boardRequests.length, queued: boardQueue.length,
        drawerRead: !!drawerRead, drawerId: state.drawerId,
        drawerDetail: state.drawerDetail, drawerRefs: drawerRefs, draft: draftWindow,
        draftOpen: draftOpenOwner, draftDetailRead: !!draftDetailRead,
        retryClicks: boardProbeRetryClicks}));
    }});\n`;
    await route.fulfill({ response, body: html.replace(anchor, instrument + anchor) });
  });
}

// Unlike holdFetch, this helper deliberately permits XHR cancellation. Its
// delivery barrier accepts either browser completion or cancellation, rather
// than requiring a successful response after abort. All held routes are
// released on failure.
async function holdOne(page: Page, matches: (url: URL) => boolean) {
  const taken = latch(), gate = latch(), delivered = latch(), browserDone = latch();
  let claimed = false;
  let measured: { responseBytes: number; summaryBytes: number; summaryCount: number } | null = null;
  releases.add(gate.release);
  await page.route(matches, async route => {
    if (claimed || route.request().method() !== "GET") return route.fallback();
    claimed = true;
    const originalRequest = route.request();
    const settled = (request: typeof originalRequest) => {
      if (request !== originalRequest) return;
      page.off("requestfinished", settled);
      page.off("requestfailed", settled);
      browserDone.release();
    };
    page.on("requestfinished", settled);
    page.on("requestfailed", settled);
    try {
      const response = await route.fetch({ headers: { ...route.request().headers(), ...headers() } });
      expect(response.ok(), `Held real response: ${response.status()}`).toBe(true);
      const bytes = await response.body();
      const body = JSON.parse(bytes.toString());
      measured = { responseBytes: bytes.byteLength,
        summaryBytes: Buffer.byteLength(JSON.stringify(body.stories || [])),
        summaryCount: (body.stories || []).length };
      taken.release();
      await gate.held;
      await route.fulfill({ response });
    } finally { delivered.release(); }
  });
  return {
    taken: taken.held,
    measurement: () => measured,
    deliver: async () => {
      gate.release();
      await delivered.held;
      await browserDone.held;
      releases.delete(gate.release);
      await page.evaluate(() => new Promise(resolve => setTimeout(resolve, 0)));
    },
  };
}
async function create(request: APIRequestContext, slug: string, title: string, extra = {}) {
  const response = await request.post(`/api/repos/${encodeURIComponent(slug)}/story`, {
    headers: headers(), data: { title, priority: "medium", ...extra },
  });
  expect(response.ok(), await response.text()).toBe(true);
}
async function readBoard(request: APIRequestContext, slug: string, options: Options): Promise<Board> {
  const response = await request.get(boardUrl(slug, options), { headers: headers() });
  expect(response.ok(), await response.text()).toBe(true);
  return response.json();
}
async function idle(page: Page) {
  await expect.poll(async () => {
    const p = await probe(page);
    return p.requests + p.queued;
  }).toBe(0);
}
async function setTodoVisible(page: Page, visible: boolean) {
  await openFilters(page);
  const checkbox = page.locator('#fdd-columns input[value="todo"]');
  if (!await checkbox.isVisible()) await page.locator("#fdd-columns .fdd-btn").click();
  await checkbox.setChecked(visible);
}
async function switchProject(page: Page, name: string) {
  await page.locator("#projsel-btn").click();
  await page.locator("#projsel-menu .projsel-item", { hasText: name }).click();
}
async function expectEvicted(page: Page, ids: string[]) {
  const p = await probe(page);
  const retained = [p.rows, p.refs, p.pages];
  for (const id of ids) {
    // Match the complete JSON string/key, so AA-5 cannot match AA-53.
    expect(JSON.stringify(retained), `No retained page/cache entry for ${id}`).not.toContain(JSON.stringify(id));
    await expect(page.locator(`#board-view [data-id="${id}"], #list-body [data-id="${id}"], #mobile-list-body [data-id="${id}"]`)).toHaveCount(0);
  }
}

function historyCharacters(rows: Card[]): number {
  return rows.reduce((total, row) => total + ["description", "comments", "attachments", "referenced_by_commits"]
    .reduce((chars, key) => chars + (row.story[key] == null ? 0 : JSON.stringify(row.story[key]).length), 0), 0);
}
async function residentMetrics(page: Page) {
  const current = await probe(page);
  return {
    mergedRows: current.rows.length,
    residentPageRows: Object.values(current.pages).reduce((count, column) => count + column.rows.length, 0),
    residentSummaryBytes: Buffer.byteLength(JSON.stringify(current.rows)),
    residentHistoryCharacters: historyCharacters(current.rows),
    liveReads: current.requests, queuedReads: current.queued,
    dom: await page.evaluate(() => ({
      boardCards: document.querySelectorAll("#board-view .card").length,
      inactiveDesktopListRows: document.querySelectorAll("#list-body [data-id]").length,
      inactiveMobileListRows: document.querySelectorAll("#mobile-list-body [data-id]").length,
      hiddenCards: Array.from(document.querySelectorAll(".card")).filter(node => !node.getClientRects().length).length,
    })),
  };
}

test.beforeEach(async ({ page }) => {
  await seedToken(page);
  await installProbe(page);
});
test.afterEach(async ({ page }) => {
  for (const release of releases) release();
  releases.clear();
  await page.unrouteAll({ behavior: "wait" });
});
cleanUpCreatedStories("Alpha Project");
cleanUpCreatedStories("Beta Project");

test("canceling a pending draft open prevents its late detail from opening an editor in another project", async ({ page, request }) => {
  const slug = await projectSlug(request, "Alpha Project");
  const title = "SH-894 canceled draft opening";
  await create(request, slug, title, { draft: true, description: "Private full draft detail" });
  const drafts = await readBoard(request, slug, { drafts: true, limit: 50 });
  const id = drafts.stories.find(row => row.story.title === title)!.story.id;
  const detail = await holdOne(page, url => url.pathname.endsWith(`/story/${id}`));
  await page.goto(`/?boardFetchTimeoutMs=${heldReadDeadlineMs()}&apiGetTimeoutMs=${heldReadDeadlineMs()}`);
  await openProject(page, "Alpha Project");
  await clickHeaderAction(page, "drafts-btn");
  await page.locator(".drafts-row", { hasText: title }).click();
  await detail.taken;
  await expect(page.locator(".draft-open-cancel")).toBeVisible();
  expect((await probe(page)).draftOpen?.id).toBe(id);
  await page.locator("#drafts-close").click();
  expect((await probe(page)).draftOpen).toBeNull();
  expect((await probe(page)).draftDetailRead).toBe(false);
  await switchProject(page, "Beta Project");
  await detail.deliver();
  await expect(page.locator("#create-modal")).not.toHaveClass(/open/);
  await expect(page.locator("#drafts-list")).toBeEmpty();
  expect((await probe(page)).draftOpen).toBeNull();
  expect((await probe(page)).repo).toBe(await projectSlug(request, "Beta Project"));
});

test("loaded drawer errors remain visible, recover on Retry and preserve dirty edits after deletion", async ({ page, request }) => {
  const slug = await projectSlug(request, "Alpha Project");
  const title = "SH-894 dirty detail availability";
  await create(request, slug, title, { description: "Full detail before the failure." });
  const row = (await readBoard(request, slug, { limit: 50, text: title })).stories[0];
  const id = row.story.id;
  await page.goto("/");
  await openProject(page, "Alpha Project");
  await page.locator(".card", { hasText: title }).click();
  const comment = page.locator('#drawer-body textarea[data-field="comment"]');
  await expect(comment).toBeVisible();
  await idle(page);
  await comment.fill("Keep these unsent words for copying.");
  let unavailable = true;
  await page.route(url => url.pathname.endsWith(`/story/${id}`), route => {
    if (route.request().method() !== "GET" || !unavailable) return route.fallback();
    return route.fulfill({ status: 503, contentType: "application/json", body: '{"error":"temporary read failure"}' });
  });
  const nudge = await request.post(`/api/repos/${slug}/story/${id}/comment`, {
    headers: headers(), data: { text: "Trigger a real SSE detail refresh." },
  });
  expect(nudge.ok()).toBe(true);
  await expect(page.locator(".drawer-detail-status")).toContainText("out of date");
  await expect(comment).toHaveValue("Keep these unsent words for copying.");
  await expect(page.locator(".drawer-detail-retry")).toBeEnabled();
  unavailable = false;
  await page.locator(".drawer-detail-retry").click();
  await expect(page.locator(".drawer-detail-status")).toHaveCount(0);
  await expect(comment).toBeEditable();
  await expect(comment).toHaveValue("Keep these unsent words for copying.");

  const deleted = await request.delete(`/api/repos/${slug}/story/${id}`, { headers: headers(), data: { force: true } });
  expect(deleted.ok(), await deleted.text()).toBe(true);
  await expect(page.locator(".drawer-detail-status")).toContainText("Unsaved edits are kept here for copying");
  await expect(comment).toHaveValue("Keep these unsent words for copying.");
  await expect(comment).not.toBeEditable();
  await expect(page.locator('#drawer-body button:enabled:not(.drawer-detail-retry), #drawer-footer button:enabled')).toHaveCount(0);
  await expect(page.locator(".drawer-detail-retry")).toBeEnabled();

  // Read-only alone prevents textarea edits, but cannot prove that the drawer's
  // capture guard canceled Enter before a downstream submit handler saw it.
  // Observe trusted events before that guard, then inspect their final state.
  const keys = await comment.evaluateHandle(node => {
    const field = node as HTMLTextAreaElement;
    const events: KeyboardEvent[] = [];
    let downstreamEnters = 0;
    const capture = (event: KeyboardEvent) => {
      if (event.target === field && ["enter", "arrowleft", "c"].includes(event.key.toLowerCase())) events.push(event);
    };
    const downstreamSubmit = (event: KeyboardEvent) => {
      if (event.key === "Enter") downstreamEnters++;
    };
    document.addEventListener("keydown", capture, true);
    field.addEventListener("keydown", downstreamSubmit);
    return {
      snapshot: () => ({ downstreamEnters, events: events.map(event => ({
        key: event.key.toLowerCase(), repeat: event.repeat, trusted: event.isTrusted,
        prevented: event.defaultPrevented, copyModifier: event.ctrlKey || event.metaKey,
      })) }),
      cleanup: () => {
        document.removeEventListener("keydown", capture, true);
        field.removeEventListener("keydown", downstreamSubmit);
        events.length = 0;
      },
    };
  });
  let clientMutations = 0;
  const observeMutation = (sent: Request) => {
    if (!["GET", "HEAD"].includes(sent.method())) clientMutations++;
  };
  page.on("request", observeMutation);
  try {
    await comment.focus();
    await page.keyboard.down("Enter");
    await page.keyboard.down("Enter");
    await page.keyboard.down("Enter");
    await page.keyboard.up("Enter");
    const enters = await keys.evaluate(witness => witness.snapshot());
    expect(enters.events.map(event => ({ key: event.key, repeat: event.repeat, trusted: event.trusted, prevented: event.prevented }))).toEqual([
      { key: "enter", repeat: false, trusted: true, prevented: true },
      { key: "enter", repeat: true, trusted: true, prevented: true },
      { key: "enter", repeat: true, trusted: true, prevented: true },
    ]);
    expect(enters.downstreamEnters).toBe(0);
    await expect(comment).toHaveValue("Keep these unsent words for copying.");

    // Copy remains available without depending on clipboard permissions or
    // contents. ArrowLeft proves native caret movement still works, too.
    await comment.evaluate(node => (node as HTMLTextAreaElement).setSelectionRange(5, 9));
    await page.keyboard.press("ControlOrMeta+c");
    expect(await comment.evaluate(node => [(node as HTMLTextAreaElement).selectionStart, (node as HTMLTextAreaElement).selectionEnd])).toEqual([5, 9]);
    await page.keyboard.press("ArrowLeft");
    expect(await comment.evaluate(node => [(node as HTMLTextAreaElement).selectionStart, (node as HTMLTextAreaElement).selectionEnd])).toEqual([5, 5]);
    const allowed = await keys.evaluate(witness => witness.snapshot());
    expect(allowed.events.slice(3)).toEqual([
      { key: "c", repeat: false, trusted: true, prevented: false, copyModifier: true },
      { key: "arrowleft", repeat: false, trusted: true, prevented: false, copyModifier: false },
    ]);
    expect(allowed.downstreamEnters).toBe(0);
    expect(clientMutations).toBe(0);
  } finally {
    page.off("request", observeMutation);
    await keys.evaluate(witness => witness.cleanup());
    await keys.dispose();
  }

  const dismissed = latch();
  page.once("dialog", async dialog => { await dialog.dismiss(); dismissed.release(); });
  await page.locator("#drawer-close").click();
  await dismissed.held;
  await expect(comment).toHaveValue("Keep these unsent words for copying.");
  page.once("dialog", dialog => dialog.accept());
  await page.locator("#drawer-close").click();
  expect((await probe(page)).drawerDetail).toBeNull();
  expect((await probe(page)).drawerRefs).toEqual({});
  await expect(page.locator("#drawer-body")).toBeEmpty();
});

test("a deleted clean drawer evicts full detail and history and shows an unavailable state", async ({ page, request }) => {
  const slug = await projectSlug(request, "Alpha Project");
  const title = "SH-894 clean detail eviction";
  const history = "SH-894 private history must be evicted";
  await create(request, slug, title, { description: history });
  const id = (await readBoard(request, slug, { limit: 50, text: title })).stories[0].story.id;
  await page.goto("/");
  await openProject(page, "Alpha Project");
  await page.locator(".card", { hasText: title }).click();
  await expect(page.locator(".description-view")).toContainText(history);
  const deleted = await request.delete(`/api/repos/${slug}/story/${id}`, { headers: headers(), data: { force: true } });
  expect(deleted.ok(), await deleted.text()).toBe(true);
  await expect(page.locator(".drawer-detail-status")).toContainText("no longer available");
  expect((await probe(page)).drawerDetail).toBeNull();
  expect((await probe(page)).drawerRefs).toEqual({});
  await expect(page.locator("#drawer-body")).not.toContainText(history);
  await expect(page.locator("#drawer-body input, #drawer-body textarea, #drawer-body select")).toHaveCount(0);
  await expect(page.locator("#drawer-footer")).toBeEmpty();
  await expect(page.locator(".drawer-detail-retry")).toBeEnabled();
});

test("a late older column reply cannot duplicate or move back a story from a newer column page", async ({ page, request }) => {
  const slug = await projectSlug(request, "Alpha Project");
  const title = "SH-894 cross-column revision witness";
  await create(request, slug, title);
  const id = (await readBoard(request, slug, { limit: 50, text: title })).stories[0].story.id;
  const oldTodo = await holdOne(page, url => optionsOf(url)?.column === "todo");
  const afterMove = latch(), subsequentReads = latch();
  releases.add(afterMove.release);
  releases.add(subsequentReads.release);
  await page.route(url => optionsOf(url)?.column === "in-progress", async route => {
    await afterMove.held;
    await route.fallback();
  });
  // Freeze later polls/SSE repair reads. Otherwise a wrong intermediate
  // duplicate could be repaired before a retrying assertion observed it.
  const seen = new Set<string>();
  await page.route(url => optionsOf(url) !== null, async route => {
    const options = optionsOf(new URL(route.request().url()))!;
    const key = options.limit === 0 ? "metadata" : options.column || "$list";
    if (seen.has(key)) await subsequentReads.held;
    else seen.add(key);
    await route.fallback();
  });
  await page.goto(`/?boardFetchTimeoutMs=${heldReadDeadlineMs()}`);
  await openProject(page, "Alpha Project");
  await oldTodo.taken;
  const moved = await request.post(`/api/repos/${slug}/story/${id}/move`, {
    headers: headers(), data: { state: "in-progress" },
  });
  expect(moved.ok(), await moved.text()).toBe(true);
  afterMove.release();
  const destination = page.locator(`.column[data-state="in-progress"] .card[data-id="${id}"]`);
  await expect(destination).toHaveCount(1);
  await oldTodo.deliver();
  await expect.poll(async () => (await probe(page)).pages.todo.loading).toBe(false);
  const settled = await probe(page);
  expect(settled.rows.filter(row => row.story.id === id)).toHaveLength(1);
  expect(Object.values(settled.pages).flatMap(column => column.rows).filter(row => row.story.id === id)).toHaveLength(1);
  expect(settled.pages.todo.rows.some(row => row.story.id === id)).toBe(false);
  await expect(page.locator(`#board-view .card[data-id="${id}"]`)).toHaveCount(1);
  await expect(destination).toHaveCount(1);
});
test("bounded daemon summaries replace page windows and preserve scroll across a real refresh", async ({ page, request }, testInfo) => {
  const slug = await projectSlug(request, "Alpha Project");
  const prefix = "SH-894 bounded page";
  const history = "History must stay in detail. ".repeat(1200);
  // Four local writes at a time bound fixture setup without creating one
  // browser request per story or changing the harness's worker policy.
  for (let start = 0; start < 55; start += 4) {
    await Promise.all(Array.from({ length: Math.min(4, 55 - start) }, (_, n) =>
      create(request, slug, `${prefix} ${String(start + n).padStart(2, "0")}`, { description: history })));
  }
  const first = await readBoard(request, slug, { column: "todo", limit: 50, text: prefix });
  expect(first.stories).toHaveLength(50);
  expect(first.page.total).toBe(55);
  expect(first.counts.columns.todo).toBe(55);
  expect(first.page.next_cursor).toBeTruthy();
  const commented = await request.post(`/api/repos/${slug}/story/${first.stories[0].story.id}/comment`, {
    headers: headers(), data: { text: history },
  });
  expect(commented.ok(), await commented.text()).toBe(true);
  const stale = await request.get(boardUrl(slug, {
    column: "todo", limit: 50, text: prefix, cursor: first.page.next_cursor!,
  }), { headers: headers() });
  expect(stale.status()).toBe(409);
  expect((await stale.json()).restart).toBe(true);
  const metadata = await readBoard(request, slug, { limit: 0, text: prefix });
  expect(metadata.stories).toHaveLength(0);
  expect(metadata.counts.total).toBe(55);
  const response = await request.get(boardUrl(slug, { column: "todo", limit: 50, text: prefix }), { headers: headers() });
  expect(response.ok()).toBe(true);
  const bytes = await response.body();
  expect(bytes.byteLength).toBeLessThanOrEqual(1024 * 1024);
  const fresh = JSON.parse(bytes.toString()) as Board;
  expect(Buffer.byteLength(JSON.stringify(fresh.stories))).toBeLessThanOrEqual(256 * 1024);
  for (const card of fresh.stories) {
    expect(card.is_summary).toBe(true);
    expect(Buffer.byteLength(JSON.stringify(card))).toBeLessThanOrEqual(16 * 1024);
    for (const key of ["description", "comments", "attachments", "referenced_by_commits"]) {
      expect(card.story).not.toHaveProperty(key);
    }
  }
  expect(bytes.toString()).not.toContain("History must stay in detail");
  const detail = await request.get(`/api/repos/${slug}/story/${first.stories[0].story.id}`, { headers: headers() });
  expect(detail.ok()).toBe(true);
  expect(await detail.text()).toContain("History must stay in detail");
  // Measure the still-supported legacy endpoint against the same fixture.
  // It has the whole project (including two baseline stories), whereas this
  // summary request selects the 55 test stories and returns only its first 50.
  const legacy = await request.get(`/api/repos/${slug}/data`, { headers: headers() });
  expect(legacy.ok()).toBe(true);
  const legacyBytes = await legacy.body();
  const legacyRows = JSON.parse(legacyBytes.toString()).stories as Card[];

  await page.goto("/");
  await openProject(page, "Alpha Project");
  await page.locator("#search-input").fill(prefix);
  const cards = page.locator('.column[data-state="todo"] .card');
  await expect(cards).toHaveCount(50);
  await expect(cards.first()).toContainText(prefix);
  const firstWindow = await residentMetrics(page);
  const firstIds = await cards.evaluateAll(nodes => nodes.map(node => node.getAttribute("data-id")!));
  await page.locator('[data-page-key="todo"] .board-page-next').click();
  await expect(cards).toHaveCount(5);
  await expectEvicted(page, firstIds);
  expect((await probe(page)).rows).toHaveLength(5);
  const nextWindow = await residentMetrics(page);
  await page.locator('[data-page-key="todo"] .board-page-previous').click();
  await expect(cards).toHaveCount(50);
  expect(await cards.evaluateAll(nodes => nodes.map(node => node.getAttribute("data-id")))).toEqual(firstIds);
  await idle(page);
  const scroller = page.locator('.column[data-state="todo"] .column-cards');
  const scrollBefore = await scroller.evaluate(node => {
    node.scrollTop = Math.min(node.clientHeight, node.scrollHeight - node.clientHeight);
    return { top: node.scrollTop, height: node.clientHeight, extent: node.scrollHeight };
  });
  expect(scrollBefore.top).toBeGreaterThan(0);
  expect(scrollBefore.extent).toBeGreaterThan(scrollBefore.height);
  const preservedCard = await cards.nth(20).elementHandle();
  expect(preservedCard).not.toBeNull();
  const beforeRefresh = await readBoard(request, slug, { column: "todo", limit: 50, text: prefix });
  const refreshed = page.waitForResponse(async reply => {
    const options = optionsOf(new URL(reply.url()));
    return options?.column === "todo" && options.text === prefix && reply.ok()
      && (await reply.json()).page.revision !== beforeRefresh.page.revision;
  });
  // Comments do not alter this column's priority ordering. The real daemon's
  // SSE notification, rather than a direct call to fetchData, drives the read.
  const nudge = await request.post(`/api/repos/${slug}/story/${firstIds[0]}/comment`, {
    headers: headers(), data: { text: "SH-894 scroll refresh witness" },
  });
  expect(nudge.ok()).toBe(true);
  await (await refreshed).finished();
  await idle(page);
  const sameNode = await preservedCard!.evaluate(node => node.isConnected &&
    document.querySelector(`.column[data-state="todo"] .card[data-id="${node.getAttribute("data-id")}"]`) === node);
  const scrollAfter = await scroller.evaluate(node => node.scrollTop);
  expect(sameNode).toBe(true);
  expect(Math.abs(scrollAfter - scrollBefore.top)).toBeLessThanOrEqual(1);
  expect(await cards.evaluateAll(nodes => nodes.map(node => node.getAttribute("data-id")))).toEqual(firstIds);
  await preservedCard!.dispose();
  const measurements = {
    fixtureStories: 55, pageResponseBytes: bytes.byteLength,
    pageSummaryBytes: Buffer.byteLength(JSON.stringify(fresh.stories)), summaryCount: fresh.stories.length,
    legacyWholeProjectResponseBytes: legacyBytes.byteLength, legacyWholeProjectRows: legacyRows.length,
    legacyHistoryCharacters: historyCharacters(legacyRows), firstWindow, nextWindow,
    afterRefresh: await residentMetrics(page), scrollBefore, scrollAfter, sameNode,
  };
  console.log("SH894_BOARD_WINDOW_METRICS " + JSON.stringify(measurements));
  await testInfo.attach("board-bounded-window-measurements", {
    contentType: "application/json", body: JSON.stringify(measurements, null, 2),
  });
});

test("one column paints before another reply arrives with at most two board reads", async ({ page }, testInfo) => {
  const todo = await holdOne(page, url => optionsOf(url)?.column === "todo");
  const held = await holdOne(page, url => optionsOf(url)?.column === "in-progress");
  await page.goto(`/?boardFetchTimeoutMs=${heldReadDeadlineMs()}`);
  await openProject(page, "Alpha Project");
  await Promise.all([todo.taken, held.taken]);
  // Both first reads have genuine replies but neither can finish yet. An
  // eager request for every column would violate this bound deterministically,
  // without relying on Playwright's requestfinished event delivery order.
  const waiting = await probe(page);
  expect(waiting.requests).toBe(2);
  expect(waiting.queued).toBeGreaterThan(0);
  await expect(page.locator('.column[data-state="todo"] .card')).toHaveCount(0);
  await expect(page.locator('.column[data-state="todo"] .column-empty, .column[data-state="in-progress"] .column-empty')).toHaveCount(0);
  await todo.deliver();
  await expect(page.locator('.column[data-state="todo"] .card')).toHaveCount(2);
  expect((await probe(page)).pages["in-progress"].loading).toBe(true);
  expect((await probe(page)).requests).toBeLessThanOrEqual(2);
  await expect(page.locator('[data-page-key="in-progress"]')).toContainText("Loading");
  const measurements = {
    beforeDelivery: { liveReads: waiting.requests, queuedReads: waiting.queued, residentRows: waiting.rows.length },
    firstCompletedVisiblePage: "todo", stillHeldPage: "in-progress",
    stillHeldLoading: (await probe(page)).pages["in-progress"].loading,
    completedResponse: todo.measurement(), heldResponse: held.measurement(),
    afterFirstPaint: await residentMetrics(page),
  };
  console.log("SH894_BOARD_FIRST_PAINT_METRICS " + JSON.stringify(measurements));
  await testInfo.attach("board-progressive-first-paint-measurements", {
    contentType: "application/json", body: JSON.stringify(measurements, null, 2),
  });
  await held.deliver();
  await idle(page);
});

test("hide and reveal repeatedly evicts rows and ignores a canceled late column reply", async ({ page }) => {
  await page.goto(`/?boardFetchTimeoutMs=${heldReadDeadlineMs()}`);
  await openProject(page, "Alpha Project");
  await idle(page);
  const ids = (await probe(page)).rows.map(row => row.story.id);
  expect(ids).toHaveLength(2);
  await setTodoVisible(page, false);
  await expectEvicted(page, ids);
  await idle(page);
  const held = await holdOne(page, url => optionsOf(url)?.column === "todo");
  await setTodoVisible(page, true);
  await held.taken;
  await setTodoVisible(page, false);
  await idle(page);
  await expectEvicted(page, ids);
  const epoch = (await probe(page)).epoch;
  await held.deliver();
  expect((await probe(page)).epoch).toBe(epoch);
  await expectEvicted(page, ids);
  await setTodoVisible(page, true);
  await expect(page.locator('.column[data-state="todo"] .card')).toHaveCount(2);
  await setTodoVisible(page, false);
  await expectEvicted(page, ids);
});

test("filter, view and project changes discard incompatible pages and inactive DOM", async ({ page, request }) => {
  await page.goto("/");
  await openProject(page, "Alpha Project");
  await idle(page);
  const ids = (await probe(page)).rows.map(row => row.story.id);
  await page.locator('[data-view="list"]').click();
  await idle(page);
  await expect(page.locator("#board-view .card")).toHaveCount(0);
  expect(Object.keys((await probe(page)).pages)).toEqual(["$list"]);
  await page.locator("#search-input").fill("SH-894 no matching category");
  await idle(page);
  await expectEvicted(page, ids);
  await page.locator("#search-input").fill("");
  await page.locator('[data-view="board"]').click();
  await expect(page.locator('.column[data-state="todo"] .card')).toHaveCount(2);
  await expect(page.locator("#list-body [data-id], #mobile-list-body [data-id]")).toHaveCount(0);
  await switchProject(page, "Beta Project");
  await idle(page);
  expect((await probe(page)).repo).toBe(await projectSlug(request, "Beta Project"));
  await expectEvicted(page, ids);
  await expect(page.locator(".card", { hasText: "Draft the release notes" })).toBeVisible();
});

test("a failed page offers Retry without presenting a false empty column", async ({ page }) => {
  const unrelated = await holdOne(page, url => optionsOf(url)?.column === "in-progress");
  await page.route(url => optionsOf(url)?.column === "todo", async route => {
    // The read-only probe observes the trusted click in the capture phase,
    // before Retry issues its XHR. Background polls cannot heal this failure
    // while Playwright is still finding/stabilizing the actual click target.
    if ((await probe(page)).retryClicks === 0) {
      return route.fulfill({ status: 503, contentType: "application/json", body: '{"error":"temporary read failure"}' });
    }
    return route.fallback();
  });
  await page.goto(`/?boardFetchTimeoutMs=${heldReadDeadlineMs()}`);
  await openProject(page, "Alpha Project");
  await unrelated.taken;
  const controls = page.locator('[data-page-key="todo"]');
  await expect(controls.locator(".board-page-retry")).toBeVisible();
  await expect(controls).not.toContainText("No matching stories");
  await expect(page.locator('.column[data-state="todo"] .column-empty')).toHaveCount(0);
  expect((await probe(page)).pages.todo.error).toBeTruthy();
  const retry = await controls.locator(".board-page-retry").elementHandle();
  expect(retry).not.toBeNull();
  await unrelated.deliver();
  await idle(page);
  expect(await retry!.evaluate(node => node.isConnected &&
    document.querySelector('[data-page-key="todo"] .board-page-retry') === node)).toBe(true);
  await retry!.dispose();
  expect((await probe(page)).retryClicks).toBe(0);
  await controls.locator(".board-page-retry").click({ timeout: gracedOperationBudget(BASE_EXPECT_TIMEOUT_MS) });
  expect((await probe(page)).retryClicks).toBe(1);
  await expect(page.locator('.column[data-state="todo"] .card')).toHaveCount(2);
  await expect(controls.locator(".board-page-retry")).toHaveCount(0);
});

test("draft project changes and close evict summaries including a canceled late reply", async ({ page, request }) => {
  const alpha = await projectSlug(request, "Alpha Project"), beta = await projectSlug(request, "Beta Project");
  await create(request, alpha, "SH-894 Alpha private draft", { draft: true });
  await create(request, beta, "SH-894 Beta private draft", { draft: true });
  const catalog = await request.get("/api/repos?board=1", { headers: headers() });
  expect(catalog.ok()).toBe(true);
  const projects = (await catalog.json()).filter((project: { id: string }) => [alpha, beta].includes(project.id));
  expect(projects).toHaveLength(2);
  for (const project of projects) {
    expect(project.drafts).toEqual([]);
    expect(typeof project.draft_count).toBe("number");
  }
  await page.goto(`/?boardFetchTimeoutMs=${heldReadDeadlineMs()}`);
  await openProject(page, "Alpha Project");
  await clickHeaderAction(page, "drafts-btn");
  await expect(page.locator("#drafts-list")).toContainText("SH-894 Alpha private draft");
  const held = await holdOne(page, url => optionsOf(url)?.drafts === true && url.pathname.includes(`/${beta}/`));
  await page.locator('select[aria-label="Draft project"]').selectOption(beta);
  await held.taken;
  expect(JSON.stringify((await probe(page)).draft)).not.toContain("SH-894 Alpha private draft");
  await page.locator("#drafts-close").click();
  expect((await probe(page)).draft).toBeNull();
  await expect(page.locator("#drafts-list")).toBeEmpty();
  await held.deliver();
  expect((await probe(page)).draft).toBeNull();
  await expect(page.locator("#drafts-list")).toBeEmpty();
  await clickHeaderAction(page, "drafts-btn");
  await expect(page.locator("#drafts-list")).toContainText("SH-894 Alpha private draft");
});

test("unsent drawer edits survive held detail refresh, hidden category and canceled navigation", async ({ page, request }) => {
  const slug = await projectSlug(request, "Alpha Project");
  const title = "SH-894 preserve drawer edits";
  await create(request, slug, title, { description: "A full detail description." });
  const data = await readBoard(request, slug, { column: "todo", limit: 50, text: title });
  const id = data.stories[0].story.id;
  const detailMatch = (url: URL) => url.pathname.endsWith(`/story/${id}`);
  const initial = await holdOne(page, detailMatch);
  await page.goto(`/?boardFetchTimeoutMs=${heldReadDeadlineMs()}&apiGetTimeoutMs=${heldReadDeadlineMs()}`);
  await openProject(page, "Alpha Project");
  await page.locator(".card", { hasText: title }).click();
  await initial.taken;
  await expect(page.locator('#drawer-body textarea[data-field="comment"]')).toHaveCount(0);
  await initial.deliver();
  const comment = page.locator('#drawer-body textarea[data-field="comment"]');
  await expect(comment).toBeVisible();
  await idle(page);
  const refreshed = await holdOne(page, detailMatch);
  const mutation = await request.post(`/api/repos/${slug}/story/${id}/comment`, {
    headers: headers(), data: { text: "External activity triggers a real refresh." },
  });
  expect(mutation.ok()).toBe(true);
  await refreshed.taken;
  await comment.fill("Unsent words must survive the older detail reply.");
  await setTodoVisible(page, false);
  await expectEvicted(page, [id]);
  await refreshed.deliver();
  await expect(comment).toHaveValue("Unsent words must survive the older detail reply.");
  // The open, visible detail is the intentional exception to page eviction.
  expect((await probe(page)).drawerDetail?.story.id).toBe(id);
  const dismissed = latch();
  page.once("dialog", async dialog => {
    expect(dialog.message()).toContain("Discard the unsaved edits");
    await dialog.dismiss(); dismissed.release();
  });
  await switchProject(page, "Beta Project");
  await dismissed.held;
  expect((await probe(page)).repo).toBe(slug);
  await expect(comment).toHaveValue("Unsent words must survive the older detail reply.");
  await comment.fill("");
  await page.locator("#drawer-close").click();
  const closed = await probe(page);
  expect(closed.drawerId).toBeNull();
  expect(closed.drawerDetail).toBeNull();
  expect(closed.drawerRefs).toEqual({});
  expect(closed.drawerRead).toBe(false);
  await expect(page.locator("#drawer-body")).toBeEmpty();
  await expect(page.locator("#drawer-footer")).toBeEmpty();
});
