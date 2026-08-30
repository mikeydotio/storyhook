import type { Route } from "@playwright/test";
import { test, expect } from "./support";
import { openProject, seedToken } from "./support";

type EngineLane = {
  index: number;
  state: string;
  story: string | null;
  dispatched_at: string | null;
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
      { index: 0, state: "working", story, dispatched_at: dispatched },
      { index: 1, state: "idle", story: null, dispatched_at: null },
    ],
    consecutive_hard_stops: 0,
    stop_reason: null,
    acknowledged_at: null,
    created_at: now.toISOString(),
    updated_at: now.toISOString(),
  };
}

async function fulfillRuns(route: Route, runs: EngineRun[]): Promise<void> {
  await route.fulfill({
    status: 200,
    contentType: "application/json",
    body: JSON.stringify({ result: "ok", runs }),
  });
}

test.beforeEach(async ({ page }) => {
  await seedToken(page);
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

test("an engine repaint waits until a held press can dispatch its click", async ({
  page,
}) => {
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
