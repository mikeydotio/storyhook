import { test, expect } from "./support";
import type { Page, Route } from "@playwright/test";
import {
  cleanUpCreatedStories,
  createStory,
  openProject,
  projectSlug,
  requiredEnv,
  seedToken,
  waitForDisplayedStoryBlockDeliveries,
} from "./support";

/**
 * SH-850: an in-progress story whose agent was lost (a reboot, a crashed tmux
 * server) offers Resume where it offered Dispatch; one whose agent still works
 * offers neither. The census (`GET /api/repos/{p}/agents`, `api::agents`) is
 * stubbed here, per test, because a real lost or live agent needs a real tmux
 * server and provider; the real endpoint's answer for a story claimed by hand
 * (no dispatch evidence: plain Dispatch) is what the untouched
 * `story-context-menu-dispatch.spec.ts` sees. Dispatch requests are stubbed
 * too: `dispatch.spec.ts` owns the real helper end to end.
 */

cleanUpCreatedStories("Alpha Project");

const DASHBOARD_TOKEN = requiredEnv("DASHBOARD_TOKEN");

type Agent = {
  story: string;
  state: "live" | "lost" | "unknown";
  provider: string | null;
  launch: Record<string, unknown> | null;
  detail: string;
};

/** The census the stubbed endpoint serves. Tests replace it; the route reads
 * it on every request, and counts the answers that named `watch`. */
let census: Agent[] = [];
let censusStatus = 200;
let watch = "";
let answersNamingWatch = 0;

async function stubCensus(page: Page) {
  await page.route("**/api/repos/*/agents", async (route: Route) => {
    if (census.some((agent) => agent.story === watch)) answersNamingWatch += 1;
    if (censusStatus !== 200) {
      await route.fulfill({ status: censusStatus, contentType: "text/plain", body: "census unavailable" });
      return;
    }
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({ result: "ok", agents: census }),
    });
  });
}

test.beforeEach(async ({ page }) => {
  census = [];
  censusStatus = 200;
  watch = "";
  answersNamingWatch = 0;
  await seedToken(page);
  await stubCensus(page);
  await page.goto("/");
});

function lost(id: string, launch: Record<string, unknown> | null): Agent {
  return {
    story: id,
    state: "lost",
    provider: "claude",
    launch,
    detail: `no window named ${id} on tmux server /private/tmp/tmux-501/default`,
  };
}

const RECORDED_LAUNCH = (id: string) => ({
  version: 1,
  project_slug: "alpha",
  story_id: id,
  provider: "claude",
  model: "sh850-recorded-model",
  effort: null,
  speed: null,
  autonomy: "auto",
  recorded_at: "2026-09-29T00:00:00Z",
});

/** Creates a story, claims it through the drawer, and waits until a census
 * answer naming `agents` for it has reached the board -- the move's own
 * change event is what refreshes the census. */
async function claimedStory(page: Page, title: string, agents: (id: string) => Agent[]) {
  const id = await createStory(page, title);
  census = agents(id);
  watch = id;
  const card = page.locator(".card", { hasText: title });
  await card.click();
  await expect(page.locator("#drawer")).toHaveClass(/open/);
  await page.locator("#drawer-body select").first().selectOption("in-progress");
  const moved = page.locator('.column[data-state="in-progress"] .card', { hasText: title });
  await expect(moved).toBeVisible();
  if (census.length) await expect.poll(() => answersNamingWatch).toBeGreaterThan(0);
  await page.locator("#drawer-close").click();
  return { id, card: moved };
}

async function menuFor(page: Page, card: import("@playwright/test").Locator) {
  await card.click({ button: "right" });
  const menu = page.locator(".ctxmenu");
  await expect(menu).toBeVisible();
  return menu;
}

function dispatchBody(id: string, state: string, intent: string, auto: boolean) {
  return JSON.stringify({
    result: "ok",
    dispatch: {
      handle: "stub-handle",
      project: "alpha",
      story: id,
      agent: "claude",
      auto,
      intent,
      state,
      started_at: "2026-01-01T00:00:00Z",
      finished_at: "2026-01-01T00:00:01Z",
      payload: state === "ok"
        ? { display: "Stubbed resume complete" }
        : { ok: false, reason: "agent-live", display: "a live agent still works in the worktree" },
      reason: state === "ok" ? undefined : "agent-live",
    },
  });
}

test("a lost agent offers Resume, prefilled from its launch record, and sends intent=resume", async ({ page }) => {
  await openProject(page, "Alpha Project");
  const { id, card } = await claimedStory(page, "SH-850 resume — lost with a record",
    (story) => [lost(story, RECORDED_LAUNCH(story))]);

  const menu = await menuFor(page, card);
  await expect(menu.getByRole("menuitem", { name: "Resume", exact: true })).toBeVisible();
  await expect(menu.getByRole("menuitem", { name: "Dispatch", exact: true })).toHaveCount(0);
  await menu.getByRole("menuitem", { name: "Resume", exact: true }).click();

  const modal = page.locator("#dispatch-modal");
  await expect(modal).toHaveClass(/open/);
  await expect(page.locator("#dispatch-modal-header")).toHaveText(`Resume ${id}`);
  await expect(page.locator("#dispatch-modal-submit")).toHaveText("Resume");
  const note = page.locator("#dispatch-resume-note");
  await expect(note).toBeVisible();
  await expect(note).toContainText("new agent session");
  await expect(note).toContainText("previous conversation is not restored");
  await expect(note).toContainText(`no window named ${id}`);
  await expect(page.locator("#dispatch-agent")).toHaveValue("claude");
  await expect(page.locator("#dispatch-auto")).toBeChecked();
  await expect(page.locator("#dispatch-model")).toHaveValue("sh850-recorded-model");
  await expect(page.locator("#dispatch-modal-submit")).toBeEnabled();

  const posted: string[] = [];
  await page.route("**/story/*/dispatch**", async (route) => {
    if (route.request().method() === "POST") posted.push(route.request().url());
    await route.fulfill({ status: 202, contentType: "application/json", body: dispatchBody(id, "ok", "resume", true) });
  });
  await page.locator("#dispatch-modal-submit").click();
  await expect.poll(() => posted.length).toBe(1);
  const query = new URL(posted[0]).searchParams;
  expect(query.get("intent")).toBe("resume");
  expect(query.get("agent")).toBe("claude");
  expect(query.get("auto")).toBe("1");
  expect(query.get("model")).toBe("sh850-recorded-model");
  const toast = page.locator("#toast-stack .toast", { hasText: `${id} resumed (auto)` });
  await expect(toast).toBeVisible();
  await expect(toast).toContainText("previous conversation was not restored");

  // A Resume never becomes the remembered choice for new work.
  await createStory(page, "SH-850 resume — dispatch after resume");
  await page.locator(".card", { hasText: "SH-850 resume — dispatch after resume" }).click({ button: "right" });
  await page.locator(".ctxmenu").getByRole("menuitem", { name: "Dispatch", exact: true }).click();
  await expect(page.locator("#dispatch-modal-header")).toHaveText("Dispatch story");
  await expect(page.locator("#dispatch-modal-submit")).toHaveText("Dispatch");
  await expect(page.locator("#dispatch-resume-note")).toBeHidden();
  await expect(page.locator("#dispatch-auto")).not.toBeChecked();
  await expect(page.locator("#dispatch-model")).not.toHaveValue("sh850-recorded-model");
  await page.locator("#dispatch-modal-cancel").click();
});

test("a lost agent with no launch record says the settings are unknown", async ({ page }) => {
  await openProject(page, "Alpha Project");
  const { card } = await claimedStory(page, "SH-850 resume — lost without a record",
    (story) => [lost(story, null)]);
  const menu = await menuFor(page, card);
  await menu.getByRole("menuitem", { name: "Resume", exact: true }).click();
  await expect(page.locator("#dispatch-resume-note")).toContainText("no record of how this story was last launched");
  await page.locator("#dispatch-modal-cancel").click();
});

test("a Full Auto lane's record resumes as an ordinary autonomous session, and says so", async ({ page }) => {
  await openProject(page, "Alpha Project");
  const { card } = await claimedStory(page, "SH-850 resume — last ran in Full Auto",
    (story) => [lost(story, { ...RECORDED_LAUNCH(story), autonomy: "full-auto" })]);
  const menu = await menuFor(page, card);
  await menu.getByRole("menuitem", { name: "Resume", exact: true }).click();
  await expect(page.locator("#dispatch-auto")).toBeChecked();
  await expect(page.locator("#dispatch-resume-note")).toContainText("ordinary autonomous session");
  await page.locator("#dispatch-modal-cancel").click();
});

test("a live agent offers neither Resume nor Dispatch, and the drawer agrees", async ({ page }) => {
  await openProject(page, "Alpha Project");
  const { card } = await claimedStory(page, "SH-850 resume — agent still working", (story) => [{
    story, state: "live", provider: "claude", launch: null, detail: "pane %7 in window X still runs a process",
  }]);
  const menu = await menuFor(page, card);
  await expect(menu.locator(".ctxmenu-item", { hasText: /^(Resume|Dispatch)$/ })).toHaveCount(0);
  // Copy ID/URL/Description, Set Status, Set Priority, Reset, Drop, Delete.
  await expect(menu.locator(".ctxmenu-item")).toHaveCount(8);
  await expect(menu.locator(".ctxmenu-sep")).toHaveCount(2);
  await page.keyboard.press("Escape");
  await card.click();
  await expect(page.locator("#drawer")).toHaveClass(/open/);
  await expect(page.locator("#dispatch-btn")).toHaveCount(0);
  await page.locator("#drawer-close").click();
});

test("an unknown agent offers Resume disabled, with the census's reason", async ({ page }) => {
  await openProject(page, "Alpha Project");
  const { card } = await claimedStory(page, "SH-850 resume — agent unknown", (story) => [{
    story, state: "unknown", provider: null, launch: null, detail: "cannot query recorded tmux server",
  }]);
  const menu = await menuFor(page, card);
  const resume = menu.locator(".ctxmenu-item", { hasText: /^Resume/ });
  await expect(resume).toHaveAttribute("aria-disabled", "true");
  await expect(resume).toHaveAttribute("title", /cannot query recorded tmux server/);
  await page.keyboard.press("Escape");
});

test("a held story's Resume is disabled until it is unblocked, in the menu and the drawer", async ({ page, request }) => {
  await openProject(page, "Alpha Project");
  const { id, card } = await claimedStory(page, "SH-850 resume — held after a reboot",
    (story) => [lost(story, RECORDED_LAUNCH(story))]);
  const slug = await projectSlug(request, "Alpha Project");
  const headers = {
    "X-Storyhook": "1",
    "X-Storyhook-Token": DASHBOARD_TOKEN,
    "Content-Type": "application/json",
  };
  const block = await request.post(`/api/repos/${slug}/story/${id}/block`, {
    headers,
    data: { reason: "Full Auto: window-gone on lane 1" },
  });
  expect(block.ok()).toBe(true);
  await waitForDisplayedStoryBlockDeliveries(page, id);

  // The hold reaches the board through the block's own change event.
  await expect(async () => {
    const menu = await menuFor(page, card);
    const resume = menu.locator(".ctxmenu-item", { hasText: /^Resume/ });
    await expect(resume).toHaveAttribute("aria-disabled", "true", { timeout: 1000 });
    await expect(resume).toHaveAttribute("title", /Unblock the story first/, { timeout: 1000 });
    await page.keyboard.press("Escape");
  }).toPass();
  await card.click();
  await expect(page.locator("#dispatch-btn")).toBeDisabled();
  await expect(page.locator("#dispatch-btn")).toHaveText("Resume");
  await page.locator("#drawer-close").click();

  const clear = await request.post(`/api/repos/${slug}/story/${id}/unblock`, { headers, data: {} });
  expect(clear.ok()).toBe(true);
  await waitForDisplayedStoryBlockDeliveries(page, id);
});

test("a failed census falls back to Dispatch, which the daemon guards itself", async ({ page }) => {
  await openProject(page, "Alpha Project");
  censusStatus = 500;
  const { card } = await claimedStory(page, "SH-850 resume — census failed", () => []);
  await expect(async () => {
    const menu = await menuFor(page, card);
    await expect(menu.getByRole("menuitem", { name: "Dispatch", exact: true })).toBeVisible({ timeout: 1000 });
    await page.keyboard.press("Escape");
  }).toPass();
});

test("a refused resume is a durable notice that names the resume", async ({ page }) => {
  await openProject(page, "Alpha Project");
  const { id, card } = await claimedStory(page, "SH-850 resume — refused",
    (story) => [lost(story, { ...RECORDED_LAUNCH(story), autonomy: "attended" })]);
  await page.route("**/story/*/dispatch**", async (route) => {
    await route.fulfill({ status: 202, contentType: "application/json", body: dispatchBody(id, "refused", "resume", false) });
  });
  const menu = await menuFor(page, card);
  await menu.getByRole("menuitem", { name: "Resume", exact: true }).click();
  await expect(page.locator("#dispatch-auto")).not.toBeChecked();
  await page.locator("#dispatch-modal-submit").click();
  const notice = page.locator("#toast-stack .toast", { hasText: `${id} resume refused` });
  await expect(notice).toBeVisible();
  await expect(notice).toContainText("agent-live");
});
