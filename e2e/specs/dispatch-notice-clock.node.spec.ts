import type { Page, Route } from "@playwright/test";
import { test, expect, latch } from "./support";
import { withDispatchNoticeClock } from "./dispatch-notice-clock";

for (const fails of [false, true]) {
  test(`terminal dispatch stays readable and clock resumes after ${fails ? "failure" : "success"}`, async () => {
    let handler: ((route: Route) => Promise<void>) | undefined;
    let paused = false;
    let installed = false;
    let forwarded = 0;
    const terminalDelivered = latch();
    const failure = new Error("wording mismatch");
    const page = {
      clock: {
        install: async () => { installed = true; },
        setFixedTime: async () => {},
        setSystemTime: async () => {},
        pauseAt: async () => { paused = true; },
        resume: async () => { paused = false; },
      },
      evaluate: async () => 1000,
      route: async (_pattern: unknown, callback: typeof handler) => { handler = callback; },
      unroute: async () => { handler = undefined; },
    } as unknown as Page;
    const route = (state: string): Route => {
      const response = { ok: () => true, json: async () => ({ dispatch: { state } }) };
      return {
        request: () => ({ method: () => "GET", headers: () => ({ accept: "application/json" }) }),
        fetch: async () => response,
        fulfill: async (options: { response: unknown }) => {
          expect(options.response).toBe(response);
          forwarded++;
          expect(paused).toBe(state !== "running");
          if (state !== "running") terminalDelivered.release();
        },
      } as unknown as Route;
    };

    const observation = withDispatchNoticeClock(page, async () => {
      expect(installed).toBe(true);
      expect(handler).toBeDefined();
      await handler!(route("running"));
      expect(paused).toBe(false);
      // The terminal handler must remain live until the reader finishes.
      const terminal = handler!(route("ok"));
      await terminalDelivered.held;
      expect(paused).toBe(true);
      // This models an arbitrarily delayed reader, not an absent-notice pass:
      // the owner must remain paused throughout all result assertions.
      await Promise.resolve();
      expect(paused).toBe(true);
      if (fails) throw failure;
      void terminal;
    });
    if (fails) await expect(observation).rejects.toBe(failure);
    else await observation;
    expect(forwarded).toBe(2);
    expect(paused).toBe(false);
    expect(handler).toBeUndefined();
  });
}
