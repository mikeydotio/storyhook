import type { Page, Route } from "@playwright/test";
import { test, expect } from "./support";
import {
  measureFocusIndicator,
  openProject,
  projectSlug,
  requiredEnv,
  seedToken,
} from "./support";

const ENGINE_PROJECT = "Engine Project";
const ENGINE_STORY_ID = requiredEnv("DASHBOARD_ENGINE_STORY_ID");
const DASHBOARD_TOKEN = requiredEnv("DASHBOARD_TOKEN");
const REAL_ENGINE_TIMEOUT = 45_000;
const MUTATION_HEADERS = {
  "Content-Type": "application/json",
  "X-Storyhook": "1",
  "X-Storyhook-Token": DASHBOARD_TOKEN,
};

type EngineLane = {
  index: number;
  state: string;
  story: string | null;
  dispatched_at: string | null;
  last_observed_at: string;
  outcome: string | null;
  outcome_detail: string | null;
};

type EngineQuarantine = {
  lane_index: number;
  story_id: string | null;
  kind: string;
  detail: string | null;
  pane_id: string | null;
  window_name: string | null;
  worktree_path: string | null;
  observed_at: string;
};

type EngineRun = {
  id: string;
  project: string;
  scope: { kind: "project" } | { kind: "epic"; story: string };
  lane_count: number;
  agent: "claude" | "codex";
  state: "running" | "paused" | "draining" | "halted" | "finished";
  lanes: EngineLane[];
  consecutive_hard_stops: number;
  recent_quarantines: EngineQuarantine[];
  stop_reason: string | null;
  acknowledged_at: string | null;
  created_at: string;
  updated_at: string;
};

function run(
  project: string,
  story = "AA-7",
  state: EngineRun["state"] = "running",
): EngineRun {
  const now = new Date();
  const dispatched = new Date(now.getTime() - 65_000).toISOString();
  return {
    id: `run-${project}`,
    project,
    scope: { kind: "project" },
    lane_count: 2,
    agent: "claude",
    state,
    lanes: [
      {
        index: 0,
        state: "working",
        story,
        dispatched_at: dispatched,
        last_observed_at: now.toISOString(),
        outcome: null,
        outcome_detail: null,
      },
      {
        index: 1,
        state: "idle",
        story: null,
        dispatched_at: null,
        last_observed_at: now.toISOString(),
        outcome: null,
        outcome_detail: null,
      },
    ],
    consecutive_hard_stops: 0,
    recent_quarantines: [],
    stop_reason: null,
    acknowledged_at: null,
    created_at: now.toISOString(),
    updated_at: now.toISOString(),
  };
}

function stoppedRun(
  project: string,
  id: string,
  state: EngineRun["state"],
  reason: string,
  createdAt: string,
): EngineRun {
  const stopped = run(project, "AA-1", state);
  stopped.id = id;
  stopped.stop_reason = reason;
  stopped.created_at = createdAt;
  stopped.updated_at = createdAt;
  stopped.lanes = stopped.lanes.map((lane) => ({
    ...lane,
    state: "idle",
    story: null,
    dispatched_at: null,
    outcome: null,
    outcome_detail: null,
  }));
  return stopped;
}

function quarantine(
  index: number,
  story: string | null,
  outcome: string,
  observedAt: string,
  detail: string | null = story,
  state = "quarantined",
): EngineLane {
  return {
    index,
    state,
    story,
    dispatched_at: null,
    last_observed_at: observedAt,
    outcome,
    outcome_detail: detail,
  };
}

async function fulfillRuns(route: Route, runs: EngineRun[]): Promise<void> {
  await route.fulfill({
    status: 200,
    contentType: "application/json",
    body: JSON.stringify({ result: "ok", runs }),
  });
}

function engineStoryLane(page: Page, story: string) {
  return page.locator(".engine-lane-story").filter({ hasText: story });
}

async function expectCoarseEngineTargets(page: Page, selector: string): Promise<void> {
  const undersized = await page.locator(selector).evaluateAll((nodes) => {
    if (!matchMedia("(pointer: coarse)").matches) return [];
    const minimum = parseFloat(
      getComputedStyle(document.documentElement).getPropertyValue("--tap-min"),
    );
    return nodes.flatMap((node) => {
      const rect = node.getBoundingClientRect();
      if (!rect.width || !rect.height) return [];
      // SH-420's float32 representation bound: a target at exactly the
      // minimum must not fail because its two endpoints rounded differently.
      const widthError = (Math.abs(rect.left) + Math.abs(rect.right)) * 2 ** -24;
      const heightError = (Math.abs(rect.top) + Math.abs(rect.bottom)) * 2 ** -24;
      return rect.width + widthError < minimum || rect.height + heightError < minimum
        ? [`${node.tagName.toLowerCase()}.${node.className}: ${rect.width}x${rect.height}`]
        : [];
    });
  });
  expect(undersized).toEqual([]);
}

async function installTestEventSource(page: Page): Promise<void> {
  await page.addInitScript(() => {
    class TestEventSource {
      onopen: (() => void) | null = null;
      onerror: (() => void) | null = null;
      listeners = new Map<string, Array<(event: MessageEvent) => void>>();

      constructor(_url: string) {
        (window as unknown as { __testEventSource: TestEventSource }).__testEventSource = this;
        setTimeout(() => this.onopen?.(), 0);
      }

      addEventListener(name: string, listener: (event: MessageEvent) => void) {
        const listeners = this.listeners.get(name) || [];
        listeners.push(listener);
        this.listeners.set(name, listeners);
      }

      emit(name: string, data: string) {
        for (const listener of this.listeners.get(name) || []) {
          listener(new MessageEvent(name, { data }));
        }
      }

      close() {}
    }

    (window as unknown as { EventSource: typeof TestEventSource }).EventSource = TestEventSource;
  });
}

test.beforeEach(async ({ page }) => {
  await seedToken(page);
});

test("engine alerts are non-dismissable queued dialogs with a live-run abandon action", async ({
  page,
}) => {
  const live = stoppedRun(
    "alpha",
    "run-live-older",
    "draining",
    "operator-stopped",
    "2026-08-30T08:30:00Z",
  );
  const halted = stoppedRun(
    "alpha",
    "run-halted-newer",
    "halted",
    "breaker-tripped",
    "2026-08-30T09:00:00Z",
  );
  const runs = [live, halted];
  const requests: Array<{ action: string; body: unknown }> = [];
  let releaseStop!: () => void;
  const stopGate = new Promise<void>((resolve) => {
    releaseStop = resolve;
  });

  await page.route("**/api/repos/*/engine/stop", async (route) => {
    requests.push({ action: "stop", body: route.request().postDataJSON() });
    await stopGate;
    live.state = "finished";
    live.stop_reason = "operator-stopped-now";
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({ result: "ok", run: live }),
    });
  });
  await page.route("**/api/repos/*/engine/ack", async (route) => {
    const body = route.request().postDataJSON() as { run: string };
    requests.push({ action: "ack", body });
    const acknowledged = runs.find((candidate) => candidate.id === body.run)!;
    acknowledged.acknowledged_at = "2026-08-30T09:05:00Z";
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({ result: "ok", run: acknowledged }),
    });
  });
  await page.route("**/api/repos/*/engine", (route) => fulfillRuns(route, runs));

  await page.goto("/");
  await openProject(page, "Alpha Project");

  const modal = page.locator("#engine-alert-modal");
  await expect(modal).toHaveClass(/open/);
  await expect(modal).toHaveAttribute("role", "dialog");
  await expect(modal).toHaveAttribute("aria-modal", "true");
  await expect(page.locator("#app")).toHaveAttribute("inert", "");
  await expect(page.locator("#engine-alert-title")).toBeFocused();
  await expect(modal).toContainText("run-halted-newer");
  await expect(page.locator(".engine-alert-abandon")).toBeHidden();

  await page.keyboard.press("Escape");
  await expect(modal).toHaveClass(/open/);
  await page.locator("#engine-alert-backdrop").click({ position: { x: 5, y: 5 } });
  await expect(modal).toHaveClass(/open/);

  // WebKit follows macOS Full Keyboard Access and may omit buttons from its
  // native Tab order. Programmatic focus puts the last control at the trap's
  // boundary; both directions must still wrap inside the dialog in every
  // engine, independent of that machine preference.
  await page.locator(".engine-alert-ack").focus();
  await page.keyboard.press("Tab");
  await expect(page.locator(".engine-alert-ack")).toBeFocused();
  await page.keyboard.press("Shift+Tab");
  await expect(page.locator(".engine-alert-ack")).toBeFocused();

  await measureFocusIndicator(
    page,
    ".engine-alert-title:focus",
    "the Full Auto alert heading",
    async () => page.locator("#engine-alert-title").focus(),
  );
  await page.locator(".engine-alert-ack").click();
  await expect(modal).toContainText("run-live-older");
  await expect(modal).not.toContainText("run-halted-newer");
  await expect(page.locator("#engine-alert-title")).toBeFocused();
  await expect(page.locator(".engine-alert-abandon")).toContainText("Abandon run");
  await expect(modal).toContainText("Immediately stops in-flight lane work");

  const tapMinimum = await page.evaluate(() =>
    Number.parseFloat(
      getComputedStyle(document.documentElement).getPropertyValue("--tap-min"),
    ),
  );
  for (const selector of [".engine-alert-abandon", ".engine-alert-ack"]) {
    const bounds = await page.locator(selector).boundingBox();
    expect(bounds, `${selector} must have rendered bounds`).not.toBeNull();
    expect(bounds!.width).toBeGreaterThanOrEqual(tapMinimum);
    expect(bounds!.height).toBeGreaterThanOrEqual(tapMinimum);
  }

  await page.evaluate(() => {
    const button = document.querySelector(".engine-alert-abandon") as HTMLButtonElement;
    button.click();
    button.click();
  });
  await expect.poll(() => requests.filter(({ action }) => action === "stop").length).toBe(1);
  await expect(page.locator(".engine-alert-abandon")).toBeDisabled();
  await expect(page.locator(".engine-alert-abandon")).toHaveText("Abandoning…");
  await expect(page.locator(".engine-alert-ack")).toBeDisabled();
  releaseStop();
  await expect(modal).not.toHaveClass(/open/);
  await expect(page.locator("#app")).not.toHaveAttribute("inert", "");
  await expect(page.locator("#projsel-btn")).toBeFocused();
  expect(requests).toEqual([
    { action: "ack", body: { run: "run-halted-newer" } },
    { action: "stop", body: { run: "run-live-older", now: true } },
    { action: "ack", body: { run: "run-live-older" } },
  ]);
});

test("running and paused alerts also offer Abandon", async ({ page }) => {
  let current = stoppedRun(
    "alpha",
    "run-running",
    "running",
    "breaker-warning",
    "2026-08-30T09:10:00Z",
  );
  await page.route("**/api/repos/*/engine", (route) => fulfillRuns(route, [current]));

  await page.goto("/");
  await openProject(page, "Alpha Project");
  await expect(page.locator(".engine-alert-abandon")).toBeVisible();

  current = stoppedRun(
    "alpha",
    "run-paused",
    "paused",
    "breaker-warning",
    "2026-08-30T09:15:00Z",
  );
  await page.reload();
  await expect(page.locator("#engine-alert-modal")).toContainText("run-paused");
  await expect(page.locator(".engine-alert-abandon")).toBeVisible();
});

test("an ambiguous Abandon stop retains the live alert and never acknowledges it", async ({
  page,
}) => {
  const live = stoppedRun(
    "alpha",
    "run-ambiguous-stop",
    "draining",
    "operator-stopped",
    "2026-08-30T09:20:00Z",
  );
  let acknowledgements = 0;
  await page.route("**/api/repos/*/engine/stop", (route) => route.abort("failed"));
  await page.route("**/api/repos/*/engine/ack", async (route) => {
    acknowledgements++;
    await route.abort("failed");
  });
  await page.route("**/api/repos/*/engine", (route) => fulfillRuns(route, [live]));

  await page.goto("/");
  await openProject(page, "Alpha Project");
  await page.locator(".engine-alert-abandon").click();

  await expect(page.locator("#toast-stack .toast.error")).toContainText(
    "storyhook could not confirm whether this reached the daemon",
  );
  await expect(page.locator("#engine-alert-modal")).toContainText("run-ambiguous-stop");
  await expect(page.locator("#engine-alert-modal")).toHaveClass(/open/);
  await expect(page.locator(".engine-alert-abandon")).toBeEnabled();
  expect(acknowledgements).toBe(0);
});

test("an ambiguous Abandon acknowledgement retains the stopped alert", async ({ page }) => {
  const live = stoppedRun(
    "alpha",
    "run-ambiguous-abandon-ack",
    "draining",
    "operator-stopped",
    "2026-08-30T09:25:00Z",
  );
  await page.route("**/api/repos/*/engine/stop", async (route) => {
    live.state = "finished";
    live.stop_reason = "operator-stopped-now";
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({ result: "ok", run: live }),
    });
  });
  await page.route("**/api/repos/*/engine/ack", (route) => route.abort("failed"));
  await page.route("**/api/repos/*/engine", (route) => fulfillRuns(route, [live]));

  await page.goto("/");
  await openProject(page, "Alpha Project");
  await page.locator(".engine-alert-abandon").click();

  await expect(page.locator("#toast-stack .toast.error")).toContainText(
    "storyhook could not confirm whether this reached the daemon",
  );
  await expect(page.locator("#engine-alert-modal")).toContainText("operator-stopped-now");
  await expect(page.locator("#engine-alert-modal")).toHaveClass(/open/);
  await expect(page.locator(".engine-alert-abandon")).toBeHidden();
  await expect(page.locator(".engine-alert-ack")).toBeEnabled();
});

test("Full Auto claims through the real daemon and leaves a durable acknowledged outcome", async ({
  page,
  request,
}) => {
  // This test waits on two real story.sh subprocesses: the engine dispatch and
  // stop-now's unclaim. Everything outside tmux is production code; the
  // runner's isolated fake tmux is the sole process-boundary double.
  test.setTimeout(2 * REAL_ENGINE_TIMEOUT + 30_000);

  await page.goto("/");
  await openProject(page, ENGINE_PROJECT);

  const lanes = page.locator("#engine-lanes");
  await expect(lanes).toBeEnabled();
  await expect(lanes).toHaveValue("1");
  await lanes.fill("2");
  await page.locator(".engine-run-btn").click();

  await expect(page.locator(".engine-state")).toHaveText("running", {
    timeout: REAL_ENGINE_TIMEOUT,
  });
  const claimedLane = engineStoryLane(page, ENGINE_STORY_ID);
  await expect(claimedLane).toHaveCount(1, { timeout: REAL_ENGINE_TIMEOUT });
  await expect(page.locator(".engine-lane-count")).toHaveText("2 lanes");

  const slug = await projectSlug(request, ENGINE_PROJECT);
  const statusResponse = await request.get(`/api/repos/${slug}/engine`, {
    headers: { "X-Storyhook-Token": DASHBOARD_TOKEN },
  });
  expect(statusResponse.status()).toBe(200);
  const statusBody = (await statusResponse.json()) as {
    result: string;
    runs: EngineRun[];
  };
  expect(statusBody.result).toBe("ok");
  const liveRun = statusBody.runs.find(
    (candidate) =>
      candidate.state === "running" &&
      candidate.lanes.some((lane) => lane.story === ENGINE_STORY_ID),
  );
  expect(
    liveRun,
    `expected one running engine lane for ${ENGINE_STORY_ID}: ${JSON.stringify(statusBody)}`,
  ).toBeDefined();

  const stopResponse = await request.post(`/api/repos/${slug}/engine/stop`, {
    headers: MUTATION_HEADERS,
    data: { run: liveRun!.id, now: true },
  });
  const stopText = await stopResponse.text();
  expect(stopResponse.status(), stopText).toBe(200);
  const stopBody = JSON.parse(stopText) as { result: string; run: EngineRun };
  expect(stopBody.result).toBe("ok");
  expect(stopBody.run.state).toBe("finished");
  expect(stopBody.run.stop_reason).toBe("operator-stopped-now");
  expect(stopBody.run.lanes.every((lane) => lane.state === "idle" && lane.story === null)).toBe(
    true,
  );

  const alert = page.locator("#engine-alert-modal");
  await expect(alert).toHaveClass(/open/, { timeout: REAL_ENGINE_TIMEOUT });
  await expect(alert).toContainText(liveRun!.id);
  await expect(alert).toContainText("operator-stopped-now");
  await expect(alert).toContainText("finished");
  await alert.locator(".engine-alert-ack").click();
  await expect(alert).not.toHaveClass(/open/);
});

test("unacknowledged runs advance newest-first with the last three linked quarantines", async ({
  page,
}) => {
  const older = stoppedRun(
    "alpha",
    "run-drained-older",
    "finished",
    "queue-drained",
    "2026-08-30T08:00:00Z",
  );
  const draining = stoppedRun(
    "alpha",
    "run-draining",
    "draining",
    "operator-stopped",
    "2026-08-30T08:30:00Z",
  );
  const halted = stoppedRun(
    "alpha",
    "run-halted-newer",
    "halted",
    "breaker-tripped",
    "2026-08-30T09:00:00Z",
  );
  halted.consecutive_hard_stops = 4;
  // One reusable lane can produce the whole breaker series. Keep a stale lane
  // outcome as a control: the durable run history must take precedence.
  halted.lanes = [quarantine(0, "AA-2", "stalled", "2026-08-30T09:01:00Z")];
  halted.recent_quarantines = [
    {
      lane_index: 0, story_id: "AA-1", kind: "agent-blocked", detail: null,
      pane_id: "%201", window_name: "AA-1", worktree_path: "/tmp/AA-1",
      observed_at: "2026-08-30T09:02:00Z",
    },
    {
      lane_index: 0, story_id: "AA-2", kind: "window-gone", detail: null,
      pane_id: "%202", window_name: "AA-2", worktree_path: "/tmp/AA-2",
      observed_at: "2026-08-30T09:03:00Z",
    },
    {
      lane_index: 0, story_id: "AA-1", kind: "interrupted", detail: null,
      pane_id: "%203", window_name: "AA-1", worktree_path: "/tmp/AA-1-retry",
      observed_at: "2026-08-30T09:04:00Z",
    },
  ];
  halted.lane_count = halted.lanes.length;
  const acknowledged = stoppedRun(
    "alpha",
    "run-already-acknowledged",
    "finished",
    "operator-stopped-now",
    "2026-08-30T10:00:00Z",
  );
  acknowledged.acknowledged_at = "2026-08-30T10:01:00Z";

  const alertRuns = [older, draining, halted, acknowledged];
  await page.route("**/api/repos/*/engine/ack", async (route) => {
    const body = route.request().postDataJSON() as { run: string };
    const current = alertRuns.find((run) => run.id === body.run)!;
    current.acknowledged_at = "2026-08-30T10:05:00Z";
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({ result: "ok", run: current }),
    });
  });
  await page.route("**/api/repos/*/engine", (route) => fulfillRuns(route, alertRuns));
  await page.goto("/");
  await openProject(page, "Alpha Project");

  const alert = page.locator("#engine-alert-modal");
  await expect(alert).toContainText("run-halted-newer");
  await expect(alert).toContainText("breaker-tripped");
  await expect(alert).toContainText("halted");
  await expect(alert).toContainText("4 consecutive hard stops");
  await expect(alert).not.toContainText("run-draining");
  await expect(alert).not.toContainText("run-already-acknowledged");

  const quarantines = alert.locator(".engine-quarantine-item");
  await expect(quarantines).toHaveCount(3);
  await expect(quarantines.nth(0)).toContainText("AA-1");
  await expect(quarantines.nth(0)).toContainText("interrupted");
  await expect(quarantines.nth(1)).toContainText("AA-2");
  await expect(quarantines.nth(1)).toContainText("window-gone");
  await expect(quarantines.nth(2)).toContainText("AA-1");
  await expect(quarantines.nth(2)).toContainText("agent-blocked");
  await expect(alert.locator(".rel-id")).toHaveCount(3);
  await expect(alert).not.toContainText("stalled");

  await alert.locator(".engine-alert-ack").click();
  await expect(alert).toContainText("run-draining");
  await expect(alert).toContainText("operator-stopped");
  await expect(alert).toContainText("draining");
  await expect(alert).not.toContainText("run-halted-newer");

  await alert.locator(".engine-alert-ack").click();
  await expect(alert).toContainText("run-drained-older");
  await expect(alert).toContainText("queue-drained");
  await expect(alert).toContainText("No quarantined stories");
  await expect(alert).not.toContainText("run-draining");
});

test("an alert modal survives reload and notice dismissal until its run is acknowledged", async ({
  page,
}) => {
  await page.clock.install();
  const drained = stoppedRun(
    "alpha",
    "run-durable",
    "finished",
    "queue-drained",
    "2026-08-30T11:00:00Z",
  );
  let posts = 0;
  let submitted: unknown = null;
  let releasePost!: () => void;
  const postGate = new Promise<void>((resolve) => {
    releasePost = resolve;
  });

  await page.route("**/api/repos/*/engine/ack", async (route) => {
    posts++;
    submitted = route.request().postDataJSON();
    await postGate;
    drained.acknowledged_at = "2026-08-30T11:05:00Z";
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({ result: "ok", run: drained }),
    });
  });
  await page.route("**/api/repos/*/engine", (route) => fulfillRuns(route, [drained]));

  await page.goto("/");
  await openProject(page, "Alpha Project");
  await expect(page.locator("#engine-alert-modal")).toContainText("run-durable");
  await page.reload();
  await expect(page.locator("#engine-alert-modal")).toContainText("run-durable");

  await page.clock.runFor(60_000);
  await page.evaluate(() =>
    (document.querySelector("#toast-dismiss-all") as HTMLButtonElement).click(),
  );
  await expect(page.locator("#engine-alert-modal")).toContainText("run-durable");
  expect(
    await page.locator("#engine-alert-modal").evaluate((node) => !!node.closest("#notice-dock")),
  ).toBe(false);

  await page.evaluate(() => {
    const button = document.querySelector(".engine-alert-ack") as HTMLButtonElement;
    button.click();
    button.click();
  });
  await expect.poll(() => posts).toBe(1);
  expect(submitted).toEqual({ run: "run-durable" });
  await expect(page.locator(".engine-alert-ack")).toBeDisabled();
  await expect(page.locator(".engine-alert-ack")).toHaveText("Acknowledging…");

  releasePost();
  await expect(page.locator("#engine-alert-modal")).not.toHaveClass(/open/);
});

test("a status reply from before acknowledgement cannot resurrect its alert", async ({
  page,
}) => {
  await installTestEventSource(page);
  const halted = stoppedRun(
    "alpha",
    "run-stale-before-ack",
    "halted",
    "breaker-tripped",
    "2026-08-30T11:30:00Z",
  );
  const preAck = JSON.parse(JSON.stringify(halted)) as EngineRun;
  let gets = 0;
  let project = "";
  let releaseOld!: () => void;
  let oldStarted!: () => void;
  let oldFinished!: () => void;
  const oldGate = new Promise<void>((resolve) => {
    releaseOld = resolve;
  });
  const started = new Promise<void>((resolve) => {
    oldStarted = resolve;
  });
  const finished = new Promise<void>((resolve) => {
    oldFinished = resolve;
  });

  await page.route("**/api/repos/*/engine/ack", async (route) => {
    halted.acknowledged_at = "2026-08-30T11:35:00Z";
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({ result: "ok", run: halted }),
    });
  });
  await page.route("**/api/repos/*/engine", async (route) => {
    gets++;
    const segments = new URL(route.request().url()).pathname.split("/");
    project = decodeURIComponent(segments[3]);
    if (gets === 2) {
      oldStarted();
      await oldGate;
      await fulfillRuns(route, [preAck]);
      oldFinished();
      return;
    }
    await fulfillRuns(route, [halted]);
  });

  await page.goto("/");
  await openProject(page, "Alpha Project");
  await page.evaluate((repo) => {
    const source = (window as unknown as {
      __testEventSource: { emit(name: string, data: string): void };
    }).__testEventSource;
    source.emit("repo-changed", JSON.stringify({ repo_id: repo }));
  }, project);
  await started;

  await page.locator(".engine-alert-ack").click();
  await expect(page.locator("#engine-alert-modal")).not.toHaveClass(/open/);
  releaseOld();
  await finished;
  await expect(page.locator("#engine-alert-modal")).not.toHaveClass(/open/);
});

test("an ambiguous acknowledgement retains the alert until status confirms it", async ({
  page,
}) => {
  const halted = stoppedRun(
    "alpha",
    "run-ambiguous-ack",
    "halted",
    "breaker-tripped",
    "2026-08-30T12:00:00Z",
  );
  let gets = 0;
  let releaseReconcile!: () => void;
  const reconcileGate = new Promise<void>((resolve) => {
    releaseReconcile = resolve;
  });

  await page.route("**/api/repos/*/engine/ack", (route) => route.abort("failed"));
  await page.route("**/api/repos/*/engine", async (route) => {
    gets++;
    if (gets > 1) await reconcileGate;
    await fulfillRuns(route, [halted]);
  });

  await page.goto("/");
  await openProject(page, "Alpha Project");
  await page.locator(".engine-alert-ack").click();
  await expect(page.locator("#toast-stack .toast.error")).toContainText(
    "storyhook could not confirm whether this reached the daemon",
  );
  await expect(page.locator("#engine-alert-modal")).toContainText("run-ambiguous-ack");

  halted.acknowledged_at = "2026-08-30T12:05:00Z";
  releaseReconcile();
  await expect(page.locator("#engine-alert-modal")).not.toHaveClass(/open/);
});

test("a failed refresh leaves the last-confirmed engine alert visibly stale", async ({
  page,
}) => {
  await page.clock.install();
  const halted = stoppedRun(
    "alpha",
    "run-stale",
    "halted",
    "breaker-tripped",
    "2026-08-30T13:00:00Z",
  );
  let gets = 0;
  await page.route("**/api/repos/*/engine", async (route) => {
    gets++;
    if (gets === 1) {
      await fulfillRuns(route, [halted]);
    } else {
      await route.abort("failed");
    }
  });

  await page.goto("/");
  await openProject(page, "Alpha Project");
  await expect(page.locator("#engine-alert-modal")).toContainText("run-stale");
  await page.clock.runFor(25_000);
  await expect.poll(() => gets).toBeGreaterThan(1);
  await expect(page.locator("#engine-alert-modal")).toContainText("run-stale");
  await expect(page.locator(".engine-alert-stale")).toContainText(
    "Status refresh failed; showing the last confirmed alert.",
  );
});

test("project launch is guarded once and becomes a live lane instrument", async ({
  page,
}) => {
  await page.clock.install();
  let current: EngineRun | null = null;
  let posts = 0;
  let submitted: unknown = null;
  let releasePost!: () => void;
  const postGate = new Promise<void>((resolve) => {
    releasePost = resolve;
  });

  await page.route("**/api/repos/*/engine", async (route) => {
    if (route.request().method() === "GET") {
      await fulfillRuns(route, current ? [current] : []);
      return;
    }
    posts++;
    submitted = route.request().postDataJSON();
    await postGate;
    current = run("alpha", "AA-12");
    await route.fulfill({
      status: 201,
      contentType: "application/json",
      body: JSON.stringify({ result: "ok", run: current }),
    });
  });

  await page.goto("/");
  await openProject(page, "Alpha Project");

  const lanes = page.locator("#engine-lanes");
  const start = page.locator(".engine-run-btn");
  await expect(lanes).toBeEnabled();
  await expect(lanes).toHaveValue("1");
  await lanes.fill("3");

  // HTMLElement.click() honors the native disabled state. Calling it twice
  // in one task is the strongest double-submit witness: no human timing and
  // no Playwright actionability wait can serialize the two activations.
  await page.evaluate(() => {
    const button = document.querySelector(".engine-run-btn") as HTMLButtonElement;
    button.click();
    button.click();
  });

  await expect.poll(() => posts).toBe(1);
  await expect(start).toBeDisabled();
  await expect(start).toHaveAccessibleName("Starting Full Auto…");
  expect(submitted).toEqual({ lanes: 3 });

  releasePost();
  await expect(page.locator(".engine-state")).toHaveText("running");
  await expect(page.locator(".engine-lane-count")).toHaveText("2 lanes");
  await expect(page.locator(".engine-lane").nth(0)).toContainText("AA-12");
  const elapsed = page.locator(".engine-lane-elapsed");
  await expect(elapsed).toContainText(/1m \d+s/);
  const beforeTick = await elapsed.textContent();
  await page.clock.runFor(1_000);
  await expect(elapsed).not.toHaveText(beforeTick || "");
  await expect(page.locator(".engine-lane").nth(1)).toContainText("idle");
});

test("running and paused controls guard and reconcile pause and resume", async ({ page }) => {
  let current = run("alpha", "AA-CONTROL");
  let pausePosts = 0;
  let resumePosts = 0;
  let pauseBody: unknown = null;
  let resumeBody: unknown = null;
  let releasePause!: () => void;
  let releaseResume!: () => void;
  const pauseGate = new Promise<void>((resolve) => {
    releasePause = resolve;
  });
  const resumeGate = new Promise<void>((resolve) => {
    releaseResume = resolve;
  });

  await page.route("**/api/repos/*/engine/pause", async (route) => {
    pausePosts++;
    pauseBody = route.request().postDataJSON();
    await pauseGate;
    current.state = "paused";
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({ result: "ok", run: current }),
    });
  });
  await page.route("**/api/repos/*/engine/resume", async (route) => {
    resumePosts++;
    resumeBody = route.request().postDataJSON();
    await resumeGate;
    current.state = "running";
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({ result: "ok", run: current }),
    });
  });
  await page.route("**/api/repos/*/engine", (route) => fulfillRuns(route, [current]));

  await page.goto("/");
  await openProject(page, "Alpha Project");
  const pause = page.locator(".engine-pause-btn");
  await expect(pause).toHaveText("Pause new claims");
  await expect(page.locator(".engine-resume-btn")).toHaveCount(0);
  await expect(page.locator(".engine-stop-btn")).toHaveText("Stop");

  await page.evaluate(() => {
    const button = document.querySelector(".engine-pause-btn") as HTMLButtonElement;
    button.click();
    button.click();
  });
  await expect.poll(() => pausePosts).toBe(1);
  expect(pauseBody).toEqual({ run: current.id });
  await expect(pause).toBeDisabled();
  await expect(pause).toHaveText("Pausing…");

  releasePause();
  const resume = page.locator(".engine-resume-btn");
  await expect(resume).toHaveText("Resume");
  await expect(page.locator(".engine-pause-btn")).toHaveCount(0);
  await expect(page.locator(".engine-stop-btn")).toHaveText("Stop");

  await page.evaluate(() => {
    const button = document.querySelector(".engine-resume-btn") as HTMLButtonElement;
    button.click();
    button.click();
  });
  await expect.poll(() => resumePosts).toBe(1);
  expect(resumeBody).toEqual({ run: current.id });
  await expect(resume).toBeDisabled();
  await expect(resume).toHaveText("Resuming…");

  releaseResume();
  await expect(page.locator(".engine-pause-btn")).toHaveText("Pause new claims");
});

test("Stop offers guarded Drain and Stop now with exact consequences", async ({ page }) => {
  let current = run("alpha", "AA-STOP");
  let stopPosts = 0;
  let acknowledgements = 0;
  const bodies: unknown[] = [];
  let releaseDrain!: () => void;
  let releaseNow!: () => void;
  const drainGate = new Promise<void>((resolve) => {
    releaseDrain = resolve;
  });
  const nowGate = new Promise<void>((resolve) => {
    releaseNow = resolve;
  });

  await page.route("**/api/repos/*/engine/stop", async (route) => {
    stopPosts++;
    const body = route.request().postDataJSON() as { now: boolean };
    bodies.push(body);
    if (body.now) {
      await nowGate;
      current.state = "finished";
      current.stop_reason = "operator-stopped-now";
      current.acknowledged_at = null;
    } else {
      await drainGate;
      current.state = "draining";
      current.stop_reason = "operator-stopped";
      current.acknowledged_at = null;
    }
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({ result: "ok", run: current }),
    });
  });
  await page.route("**/api/repos/*/engine/ack", async (route) => {
    acknowledgements++;
    expect(route.request().postDataJSON()).toEqual({ run: current.id });
    current.acknowledged_at = `2026-09-05T12:0${acknowledgements}:00Z`;
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({ result: "ok", run: current }),
    });
  });
  await page.route("**/api/repos/*/engine", (route) => fulfillRuns(route, [current]));

  await page.goto("/");
  await openProject(page, "Alpha Project");
  const stop = page.locator(".engine-stop-btn");
  await stop.click();
  const modal = page.locator("#engine-stop-modal");
  await expect(modal).toHaveClass(/open/);
  await expect(modal).toContainText(
    "Occupied lanes finish, no new stories are claimed, and Run Full Auto returns after the last lane lands.",
  );
  await expect(modal).toContainText(
    "Active claims are released and their windows are closed. Branches and worktrees are preserved.",
  );
  await expect(page.locator("#engine-stop-cancel")).toBeFocused();
  await page.locator("#engine-stop-cancel").click();
  await expect(modal).not.toHaveClass(/open/);
  await expect(stop).toBeFocused();

  await stop.click();
  await page.keyboard.press("Escape");
  await expect(modal).not.toHaveClass(/open/);
  await expect(stop).toBeFocused();
  await stop.click();
  await page.locator("#engine-stop-modal-backdrop").click({ position: { x: 1, y: 1 } });
  await expect(modal).not.toHaveClass(/open/);
  await expect(stop).toBeFocused();

  await stop.click();
  await expectCoarseEngineTargets(page, "#engine-stop-modal button");
  await page.evaluate(() => {
    const button = document.querySelector("#engine-stop-drain") as HTMLButtonElement;
    button.click();
    button.click();
  });
  await expect.poll(() => stopPosts).toBe(1);
  expect(bodies[0]).toEqual({ run: current.id, now: false });
  await expect(page.locator("#engine-stop-drain")).toBeDisabled();
  await expect(page.locator("#engine-stop-drain")).toHaveText("Draining…");
  await expect(page.locator("#engine-stop-now")).toBeDisabled();
  await expect(page.locator("#engine-stop-cancel")).toBeDisabled();
  await page.keyboard.press("Escape");
  await expect(modal).toHaveClass(/open/);

  releaseDrain();
  await expect(modal).not.toHaveClass(/open/);
  const alert = page.locator("#engine-alert-modal");
  await expect(alert).toHaveClass(/open/);
  await expect(alert).toContainText("operator-stopped");
  await expect(page.locator("#engine-alert-title")).toBeFocused();
  const drainingStop = page.locator(".engine-stop-btn");
  await expect(page.locator(".engine-pause-btn, .engine-resume-btn")).toHaveCount(0);
  await expect(drainingStop).toHaveText("Stop (draining)");
  await page.locator(".engine-alert-ack").click();
  await expect(alert).not.toHaveClass(/open/);
  await expect(drainingStop).toBeFocused();
  await expectCoarseEngineTargets(page, ".engine-live button");

  await drainingStop.click();
  await expect(page.locator("#engine-stop-drain")).toBeDisabled();
  await expect(page.locator("#engine-stop-drain")).toHaveText("Already draining");
  await expect(page.locator("#engine-stop-now")).toBeEnabled();
  await page.evaluate(() => {
    const button = document.querySelector("#engine-stop-now") as HTMLButtonElement;
    button.click();
    button.click();
  });
  await expect.poll(() => stopPosts).toBe(2);
  expect(bodies[1]).toEqual({ run: current.id, now: true });
  await expect(page.locator("#engine-stop-now")).toBeDisabled();
  await expect(page.locator("#engine-stop-now")).toHaveText("Stopping…");

  releaseNow();
  await expect(modal).not.toHaveClass(/open/);
  await expect(alert).toHaveClass(/open/);
  await expect(alert).toContainText("operator-stopped-now");
  await expect(page.locator("#engine-alert-title")).toBeFocused();
  await page.locator(".engine-alert-ack").click();
  await expect(alert).not.toHaveClass(/open/);
  expect(acknowledgements).toBe(2);
  await expect(page.locator(".engine-run-btn")).toBeFocused();
});

test("a timed-out lifecycle mutation is reported as ambiguous and reconciled", async ({
  page,
  browserName,
}) => {
  test.skip(
    browserName === "webkit",
    "WebKit never fires XMLHttpRequest.ontimeout on a request held by Playwright route interception -- measured, deterministic (SH-347)",
  );
  const timeoutMs = 300;
  let current = run("alpha", "AA-TIMEOUT");
  let gets = 0;
  let routeDone!: () => void;
  const done = new Promise<void>((resolve) => {
    routeDone = resolve;
  });

  await page.route("**/api/repos/*/engine/pause", async (route) => {
    current.state = "paused";
    await new Promise((resolve) => setTimeout(resolve, timeoutMs + 500));
    try {
      await route.fulfill({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify({ result: "ok", run: current }),
      });
    } finally {
      routeDone();
    }
  });
  await page.route("**/api/repos/*/engine", async (route) => {
    gets++;
    await fulfillRuns(route, [current]);
  });

  await page.goto(`/?mutationTimeoutMs=${timeoutMs}`);
  await openProject(page, "Alpha Project");
  const before = gets;
  await page.locator(".engine-pause-btn").click();

  const notice = page.locator("#toast-stack .toast.error");
  await expect(notice).toContainText("may or may not have gone through");
  await expect(notice).not.toContainText("request timed out");
  await expect.poll(() => gets).toBeGreaterThan(before);
  await expect(page.locator(".engine-resume-btn")).toHaveText("Resume");
  await done;
});

test("changing the lane count preserves the launch button's physical click", async ({
  page,
}) => {
  let submitted: unknown = null;
  let current: EngineRun | null = null;
  await page.route("**/api/repos/*/engine", async (route) => {
    if (route.request().method() === "GET") {
      await fulfillRuns(route, current ? [current] : []);
      return;
    }
    submitted = route.request().postDataJSON();
    current = run("alpha", "AA-PHYSICAL");
    await route.fulfill({
      status: 201,
      contentType: "application/json",
      body: JSON.stringify({ result: "ok", run: current }),
    });
  });

  await page.goto("/");
  await openProject(page, "Alpha Project");
  await page.locator("#engine-lanes").fill("2");

  // A trusted pointer gesture moves focus off the number input, firing its
  // `change` handler between press and click. HTMLElement.click() would skip
  // that boundary and could not witness WebKit's swallowed-click regression.
  await page.locator(".engine-run-btn").click();

  await expect.poll(() => submitted).toEqual({ lanes: 2 });
  await expect(page.locator(".engine-state")).toHaveText("running");
});

test("a definite refusal releases the start claim for another attempt", async ({
  page,
}) => {
  let posts = 0;
  await page.route("**/api/repos/*/engine", async (route) => {
    if (route.request().method() === "GET") {
      await fulfillRuns(route, []);
      return;
    }
    posts++;
    await route.fulfill({
      status: 409,
      contentType: "application/json",
      body: JSON.stringify({ error: "project already has a live engine run" }),
    });
  });

  await page.goto("/");
  await openProject(page, "Alpha Project");
  const button = page.locator(".engine-run-btn");
  await expect(button).toBeEnabled();
  await button.click();
  await expect(page.locator("#toast-stack .toast.error")).toContainText(
    "project already has a live engine run",
  );
  await expect(button).toBeEnabled();
  await button.click();
  await expect.poll(() => posts).toBe(2);
});

test("an epic replaces ordinary Dispatch with an epic-scoped Full Auto start", async ({
  page,
}) => {
  let submitted: Record<string, unknown> | null = null;
  let transformedEpicId = "";

  await page.route(/\/data$/, async (route) => {
    const response = await route.fetch();
    const data = await response.json();
    data.stories[0].story.story_type = "epic";
    transformedEpicId = data.stories[0].story.id;
    await route.fulfill({ response, json: data });
  });
  await page.route(/\/story\/[^/]+$/, async (route) => {
    if (route.request().method() !== "GET") {
      await route.continue();
      return;
    }
    const response = await route.fetch();
    const detail = await response.json();
    detail.story.story.story_type = "epic";
    await route.fulfill({ response, json: detail });
  });
  await page.route("**/api/repos/*/engine", async (route) => {
    if (route.request().method() === "GET") {
      await fulfillRuns(route, []);
      return;
    }
    const payload = route.request().postDataJSON() as Record<string, unknown>;
    submitted = payload;
    const epic = String(payload.epic);
    const started = run("alpha", epic);
    started.scope = { kind: "epic", story: epic };
    started.lane_count = 1;
    started.lanes = [started.lanes[0]];
    await route.fulfill({
      status: 201,
      contentType: "application/json",
      body: JSON.stringify({ result: "ok", run: started }),
    });
  });

  await page.goto("/");
  await openProject(page, "Alpha Project");
  await page.locator("#filter-toggle-btn").click();
  await expect(page.locator("#filter-toggle-btn")).toHaveAttribute("aria-expanded", "true");
  await page.locator("#toggle-epics").check();
  expect(transformedEpicId).not.toBe("");
  const epicCard = page.locator(`.card[data-id="${transformedEpicId}"]`);
  await epicCard.click();
  await expect(page.locator("#drawer")).toHaveClass(/open/);

  await expect(page.locator("#dispatch-btn")).toHaveCount(0);
  const button = page.locator("#engine-epic-run-btn");
  await expect(button).toHaveText("Run Full Auto on this epic");
  await button.click();
  await expect.poll(() => submitted).toEqual({ epic: transformedEpicId });
});

test("an unconfirmed start stays honest and reconciles the run with GET", async ({
  page,
}) => {
  let current: EngineRun | null = null;

  await page.route("**/api/repos/*/engine", async (route) => {
    if (route.request().method() === "GET") {
      await fulfillRuns(route, current ? [current] : []);
      return;
    }
    // Model the only fact status:0 permits the page to claim: the transport
    // delivered no reply, while the daemon may nevertheless have committed.
    current = run("alpha", "AA-ambiguous");
    await route.abort("failed");
  });

  await page.goto("/");
  await openProject(page, "Alpha Project");
  await expect(page.locator(".engine-run-btn")).toBeEnabled();
  await page.locator(".engine-run-btn").click();

  const notice = page.locator("#toast-stack .toast.error");
  await expect(notice).toContainText(
    "storyhook could not confirm whether this reached the daemon",
  );
  await expect(engineStoryLane(page, "AA-ambiguous")).toHaveText("AA-ambiguous");
});

test("a late project reply cannot overwrite the selected project's run", async ({
  page,
}) => {
  let first = true;
  let releaseFirst!: () => void;
  let firstSeen!: () => void;
  let firstFinished!: () => void;
  const firstGate = new Promise<void>((resolve) => {
    releaseFirst = resolve;
  });
  const seen = new Promise<void>((resolve) => {
    firstSeen = resolve;
  });
  const finished = new Promise<void>((resolve) => {
    firstFinished = resolve;
  });

  await page.route("**/api/repos/*/engine", async (route) => {
    if (first) {
      first = false;
      firstSeen();
      await firstGate;
      await fulfillRuns(route, [run("alpha", "ALPHA-LATE")]);
      firstFinished();
      return;
    }
    await fulfillRuns(route, [run("beta", "BETA-CURRENT")]);
  });

  await page.goto("/");
  await openProject(page, "Alpha Project");
  await seen;
  await page.locator("#projsel-btn").click();
  await page.locator("#projsel-menu .projsel-item", { hasText: "Beta Project" }).click();
  await expect(engineStoryLane(page, "BETA-CURRENT")).toHaveText("BETA-CURRENT");

  releaseFirst();
  await finished;
  await expect(engineStoryLane(page, "BETA-CURRENT")).toHaveText("BETA-CURRENT");
  await expect(engineStoryLane(page, "ALPHA-LATE")).toHaveCount(0);

  await page.locator("#projsel-btn").click();
  await page.locator("#projsel-menu .projsel-item", { hasText: "Gamma Archive" }).click();
  await expect(page.locator("#engine-control")).toBeHidden();
});

test("the safety poll reconciles engine state without an SSE event", async ({
  page,
}) => {
  await page.clock.install();
  let current: EngineRun | null = null;
  let gets = 0;
  await page.route("**/api/repos/*/engine", async (route) => {
    gets++;
    await fulfillRuns(route, current ? [current] : []);
  });

  await page.goto("/");
  await openProject(page, "Alpha Project");
  await expect(page.locator(".engine-run-btn")).toBeEnabled();
  const initialGets = gets;
  current = run("alpha", "AA-SAFETY");

  // The dashboard's named safety interval is 25 seconds. No EventSource
  // event is emitted in this test; advancing that interval is the witness.
  await page.clock.runFor(25_000);
  await expect.poll(() => gets).toBeGreaterThan(initialGets);
  await expect(engineStoryLane(page, "AA-SAFETY")).toHaveText("AA-SAFETY");
});

test("an alert repaint waits until a held acknowledgement can dispatch its click", async ({
  page,
}) => {
  await installTestEventSource(page);
  const halted = stoppedRun(
    "alpha",
    "run-held-ack",
    "halted",
    "breaker-tripped",
    "2026-08-30T14:00:00Z",
  );
  let gets = 0;
  let posts = 0;
  let project = "";
  await page.route("**/api/repos/*/engine/ack", async (route) => {
    posts++;
    halted.acknowledged_at = "2026-08-30T14:05:00Z";
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({ result: "ok", run: halted }),
    });
  });
  await page.route("**/api/repos/*/engine", async (route) => {
    gets++;
    const segments = new URL(route.request().url()).pathname.split("/");
    project = decodeURIComponent(segments[3]);
    halted.consecutive_hard_stops = gets;
    await fulfillRuns(route, [halted]);
  });

  await page.goto("/");
  await openProject(page, "Alpha Project");
  const acknowledge = page.locator(".engine-alert-ack");
  await expect(acknowledge).toBeEnabled();
  const box = await acknowledge.boundingBox();
  expect(box).not.toBeNull();
  await page.mouse.move(box!.x + box!.width / 2, box!.y + box!.height / 2);
  await page.mouse.down();
  await page.evaluate((repo) => {
    const source = (window as unknown as {
      __testEventSource: { emit(name: string, data: string): void };
    }).__testEventSource;
    source.emit("repo-changed", JSON.stringify({ repo_id: repo }));
  }, project);

  await expect
    .poll(() =>
      page.evaluate(() =>
        (window as unknown as { __storyhookPressGate: { deferred: string[] } })
          .__storyhookPressGate.deferred.includes("engine-alert"),
      ),
    )
    .toBe(true);
  await expect(acknowledge).toBeAttached();

  await page.mouse.up();
  await expect.poll(() => posts).toBe(1);
  await expect(page.locator("#engine-alert-modal")).not.toHaveClass(/open/);
});

test("an engine repaint waits until a held press can dispatch its click", async ({
  page,
}) => {
  await installTestEventSource(page);

  let gets = 0;
  let project = "";
  await page.route("**/api/repos/*/engine", async (route) => {
    gets++;
    const segments = new URL(route.request().url()).pathname.split("/");
    project = decodeURIComponent(segments[3]);
    await fulfillRuns(route, gets === 1 ? [] : [run(project, "AA-PUSH")]);
  });

  await page.goto("/");
  await openProject(page, "Alpha Project");
  const input = page.locator("#engine-lanes");
  await expect(input).toBeEnabled();
  await input.evaluate((node) => {
    (window as unknown as { __engineInputClicks: number }).__engineInputClicks = 0;
    node.addEventListener("click", () => {
      (window as unknown as { __engineInputClicks: number }).__engineInputClicks++;
    });
  });

  const box = await input.boundingBox();
  expect(box).not.toBeNull();
  await page.mouse.move(box!.x + box!.width / 2, box!.y + box!.height / 2);
  await page.mouse.down();
  await page.evaluate((repo) => {
    const source = (window as unknown as {
      __testEventSource: { emit(name: string, data: string): void };
    }).__testEventSource;
    source.emit("repo-changed", JSON.stringify({ repo_id: repo }));
  }, project);

  await expect
    .poll(() =>
      page.evaluate(() =>
        (window as unknown as { __storyhookPressGate: { deferred: string[] } })
          .__storyhookPressGate.deferred.includes("engine"),
      ),
    )
    .toBe(true);
  await expect(input).toBeAttached();

  await page.mouse.up();
  await expect
    .poll(() =>
      page.evaluate(() =>
        (window as unknown as { __engineInputClicks: number }).__engineInputClicks,
      ),
    )
    .toBe(1);
  await expect(engineStoryLane(page, "AA-PUSH")).toHaveText("AA-PUSH");
});

test("a live-control repaint waits until a held Pause press dispatches", async ({ page }) => {
  await installTestEventSource(page);
  const current = run("alpha", "AA-HELD-PAUSE");
  let project = "";
  let gets = 0;
  let pausePosts = 0;

  await page.route("**/api/repos/*/engine/pause", async (route) => {
    pausePosts++;
    current.state = "paused";
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify({ result: "ok", run: current }),
    });
  });
  await page.route("**/api/repos/*/engine", async (route) => {
    gets++;
    const segments = new URL(route.request().url()).pathname.split("/");
    project = decodeURIComponent(segments[3]);
    if (gets > 1) current.lanes[1].state = "dispatching";
    await fulfillRuns(route, [current]);
  });

  await page.goto("/");
  await openProject(page, "Alpha Project");
  const pause = page.locator(".engine-pause-btn");
  await expect(pause).toBeEnabled();
  const box = await pause.boundingBox();
  expect(box).not.toBeNull();
  await page.mouse.move(box!.x + box!.width / 2, box!.y + box!.height / 2);
  await page.mouse.down();
  await page.evaluate((repo) => {
    const source = (window as unknown as {
      __testEventSource: { emit(name: string, data: string): void };
    }).__testEventSource;
    source.emit("repo-changed", JSON.stringify({ repo_id: repo }));
  }, project);

  await expect
    .poll(() =>
      page.evaluate(() =>
        (window as unknown as { __storyhookPressGate: { deferred: string[] } })
          .__storyhookPressGate.deferred.includes("engine"),
      ),
    )
    .toBe(true);
  await expect(pause).toBeAttached();

  await page.mouse.up();
  await expect.poll(() => pausePosts).toBe(1);
  await expect(page.locator(".engine-resume-btn")).toHaveText("Resume");
});
