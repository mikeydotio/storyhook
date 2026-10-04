import type { Page } from "@playwright/test";
import { test, expect, onAFrozenClock } from "./support";

for (const failureAt of ["none", "pause", "body"] as const) {
  test(`clock acquisition tolerates transport delay and cleans up ${failureAt}`, async () => {
    let now = 10_000;
    let fixed = false;
    let paused = false;
    let entered = false;
    const failure = new Error(`controlled ${failureAt} failure`);
    const transit = () => { if (!fixed && !paused) now += 5_000; };
    const page = {
      evaluate: async () => { const result = now; transit(); return result; },
      clock: {
        setFixedTime: async (time: number) => { transit(); now = time; fixed = true; },
        pauseAt: async (time: number) => {
          transit();
          paused = true;
          if (time < now) throw new Error("Cannot fast-forward to the past");
          if (failureAt === "pause") throw failure;
          now = time;
        },
        setSystemTime: async (time: number) => { now = time; fixed = false; },
        runFor: async (ticks: number) => { if (!fixed) now += ticks; },
        resume: async () => { paused = false; },
      },
    } as unknown as Page;
    const result = onAFrozenClock(page, async () => {
      entered = true;
      expect(paused).toBe(true);
      expect(fixed).toBe(false);
      const before = now;
      await page.clock.runFor(3_000);
      expect(now - before).toBe(3_000);
      if (failureAt === "body") throw failure;
    });
    if (failureAt === "none") await result;
    else await expect(result).rejects.toBe(failure);
    expect(entered).toBe(failureAt !== "pause");
    expect(paused).toBe(false);
    expect(fixed).toBe(false);
    const before = now;
    transit();
    expect(now).toBeGreaterThan(before);
  });
}
