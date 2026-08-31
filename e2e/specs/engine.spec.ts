import type { Page, Route } from "@playwright/test";
import { test, expect } from "./support";
import { openProject, seedToken } from "./support";

type EngineLane = {
  index: number;
  state: string;
  story: string | null;
  dispatched_at: string | null;
  last_observed_at: string;
  outcome: string | null;
  outcome_detail: string | null;
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
    stop_reason: null,
    acknowledged_at: null,
    created_at: now.toISOString(),
    updated_at: now.toISOString(),
  };
}

function stoppedRun(
  project: string,
  id: string,
  state: "draining" | "halted" | "finished",
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

test("unacknowledged runs render newest-first with the last three linked quarantines", async ({
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
  halted.lanes = [
    quarantine(0, "AA-2", "stalled", "2026-08-30T09:01:00Z"),
    quarantine(1, "AA-1", "agent-blocked", "2026-08-30T09:02:00Z"),
    quarantine(2, "AA-2", "window-gone", "2026-08-30T09:03:00Z"),
    // A cleared quarantine has no live story column; the preserved detail is
    // still the story identity the alert must link.
    quarantine(3, null, "interrupted", "2026-08-30T09:04:00Z", "AA-1", "idle"),
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

  await page.route("**/api/repos/*/engine", (route) =>
    fulfillRuns(route, [older, draining, halted, acknowledged]),
  );
  await page.goto("/");
  await openProject(page, "Alpha Project");

  const banners = page.locator(".engine-banner");
  await expect(banners).toHaveCount(3);
  await expect(banners.nth(0)).toContainText("run-halted-newer");
  await expect(banners.nth(0)).toContainText("breaker-tripped");
  await expect(banners.nth(0)).toContainText("halted");
  await expect(banners.nth(0)).toContainText("4 consecutive hard stops");
  await expect(banners.nth(1)).toContainText("run-draining");
  await expect(banners.nth(1)).toContainText("operator-stopped");
  await expect(banners.nth(1)).toContainText("draining");
  await expect(banners.nth(2)).toContainText("run-drained-older");
  await expect(banners.nth(2)).toContainText("queue-drained");
  await expect(banners.nth(2)).toContainText("No quarantined stories");
  await expect(page.locator("#engine-banner-region")).not.toContainText(
    "run-already-acknowledged",
  );

  const quarantines = banners.nth(0).locator(".engine-quarantine-item");
  await expect(quarantines).toHaveCount(3);
  await expect(quarantines.nth(0)).toContainText("AA-1");
  await expect(quarantines.nth(0)).toContainText("interrupted");
  await expect(quarantines.nth(1)).toContainText("AA-2");
  await expect(quarantines.nth(1)).toContainText("window-gone");
  await expect(quarantines.nth(2)).toContainText("AA-1");
  await expect(quarantines.nth(2)).toContainText("agent-blocked");
  await expect(banners.nth(0).locator(".rel-id")).toHaveCount(3);
  await expect(banners.nth(0)).not.toContainText("stalled");

  await quarantines.nth(1).locator(".rel-id").click();
  await expect(page.locator("#drawer")).toHaveClass(/open/);
  await expect(page.locator("#drawer-id")).toHaveText("AA-2");
});

test("a banner survives reload and notice dismissal until its run is acknowledged", async ({
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
  await expect(page.locator(".engine-banner")).toContainText("run-durable");
  await page.reload();
  await expect(page.locator(".engine-banner")).toContainText("run-durable");

  await page.clock.runFor(60_000);
  await page.evaluate(() =>
    (document.querySelector("#toast-dismiss-all") as HTMLButtonElement).click(),
  );
  await expect(page.locator(".engine-banner")).toContainText("run-durable");
  expect(
    await page.locator(".engine-banner").evaluate((node) => !!node.closest("#notice-dock")),
  ).toBe(false);

  await page.evaluate(() => {
    const button = document.querySelector(".engine-banner-ack") as HTMLButtonElement;
    button.click();
    button.click();
  });
  await expect.poll(() => posts).toBe(1);
  expect(submitted).toEqual({ run: "run-durable" });
  await expect(page.locator(".engine-banner-ack")).toBeDisabled();
  await expect(page.locator(".engine-banner-ack")).toHaveText("Acknowledging…");

  releasePost();
  await expect(page.locator(".engine-banner")).toHaveCount(0);
});

test("a status reply from before acknowledgement cannot resurrect its banner", async ({
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

  await page.locator(".engine-banner-ack").click();
  await expect(page.locator(".engine-banner")).toHaveCount(0);
  releaseOld();
  await finished;
  await expect(page.locator(".engine-banner")).toHaveCount(0);
});

test("an ambiguous acknowledgement retains the banner until status confirms it", async ({
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
  await page.locator(".engine-banner-ack").click();
  await expect(page.locator("#toast-stack .toast.error")).toContainText(
    "storyhook could not confirm whether this reached the daemon",
  );
  await expect(page.locator(".engine-banner")).toContainText("run-ambiguous-ack");

  halted.acknowledged_at = "2026-08-30T12:05:00Z";
  releaseReconcile();
  await expect(page.locator(".engine-banner")).toHaveCount(0);
});

test("a failed refresh leaves the last-confirmed engine banner visibly stale", async ({
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
  await expect(page.locator(".engine-banner")).toContainText("run-stale");
  await page.clock.runFor(25_000);
  await expect.poll(() => gets).toBeGreaterThan(1);
  await expect(page.locator(".engine-banner")).toContainText("run-stale");
  await expect(page.locator(".engine-banner-stale")).toContainText(
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
  await expect(page.locator(".engine-lane-story")).toContainText("AA-ambiguous");
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
  await expect(page.locator(".engine-lane-story")).toContainText("BETA-CURRENT");

  releaseFirst();
  await finished;
  await expect(page.locator(".engine-lane-story")).toContainText("BETA-CURRENT");
  await expect(page.locator(".engine-lane-story")).not.toContainText("ALPHA-LATE");

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
  await expect(page.locator(".engine-lane-story")).toContainText("AA-SAFETY");
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
  const acknowledge = page.locator(".engine-banner-ack");
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
          .__storyhookPressGate.deferred.includes("engine-banner"),
      ),
    )
    .toBe(true);
  await expect(acknowledge).toBeAttached();

  await page.mouse.up();
  await expect.poll(() => posts).toBe(1);
  await expect(page.locator(".engine-banner")).toHaveCount(0);
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
  await expect(page.locator(".engine-lane-story")).toContainText("AA-PUSH");
});
