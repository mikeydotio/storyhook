import type { Page, Route } from "@playwright/test";
import { latch, onAFrozenClock, requiredEnv } from "./support";

/** Observe real dispatch results without racing a transient notice's lifetime. */
export async function withDispatchNoticeClock(page: Page, body: () => Promise<void>): Promise<void> {
  // Install before navigation; the real dispatch polling clock keeps running
  // until the daemon has actually completed the operation.
  await page.clock.install();
  const inspected = latch();
  const pending = new Set<Promise<void>>();
  const pattern = /\/story\/[^/]+\/dispatch\/[^/?]+$/;
  const forward = async (route: Route): Promise<void> => {
    const response = await route.fetch({
      headers: {
        ...route.request().headers(),
        "X-Storyhook-Token": requiredEnv("DASHBOARD_TOKEN"),
      },
    });
    if (!response.ok()) throw new Error(`Dispatch poll failed: HTTP ${response.status()}`);
    const state = (await response.json()).dispatch?.state;
    if (state === "running") {
      await route.fulfill({ response });
      return;
    }
    if (!["ok", "refused", "failed"].includes(state))
      throw new Error(`Dispatch poll has unknown state: ${JSON.stringify(state)}`);
    await onAFrozenClock(page, async () => {
      // Publish unchanged bytes only after expiry is under test control.
      await route.fulfill({ response });
      await inspected.held;
    });
  };
  const handler = async (route: Route): Promise<void> => {
    const task = forward(route);
    pending.add(task);
    try { await task; } finally { pending.delete(task); }
  };
  await page.route(pattern, handler);
  try {
    await body();
  } finally {
    inspected.release();
    await page.unroute(pattern, handler);
    await Promise.all(pending);
  }
}
