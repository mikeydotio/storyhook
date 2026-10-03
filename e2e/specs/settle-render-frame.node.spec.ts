import vm from "node:vm";
import type { Locator } from "@playwright/test";
import { test, expect, awaitSettled } from "./support";

for (const mode of ["finished", "paused", "running"] as const) {
  test(`settling observes the rendered frame for ${mode} animations`, async () => {
    let frames = 0;
    let renderedSize = 13;
    const observations: number[] = [];
    // The engine state can reach finished before its last layout is applied
    // (SH-812, WebKit). This unit fixture controls that rendering boundary;
    // browser tests continue to exercise the real CSS and exact geometry.
    const root = {
      getAnimations(options: { subtree: boolean }) {
        expect(options).toEqual({ subtree: true });
        observations.push(frames);
        return [{
          playState: mode === "running" && frames >= 2 ? "finished" : mode,
          transitionProperty: "font-size",
          effect: { target: { tagName: "BUTTON" } },
        }];
      },
    };
    const context = vm.createContext({
      root,
      requestAnimationFrame(callback: FrameRequestCallback) {
        frames++;
        renderedSize = mode === "paused" ? 19 : 26;
        callback(frames);
      },
      document: {
        getAnimations() { throw new Error("An unrelated card is still animating"); },
      },
    });
    const locator = {
      toString: () => "controlled toolbar",
      evaluate: (callback: (node: Element) => unknown) =>
        vm.runInContext(`(${callback.toString()})(root)`, context),
    } as unknown as Locator;

    await awaitSettled(locator);

    expect(renderedSize).toBe(mode === "paused" ? 19 : 26);
    expect(observations).toEqual(mode === "running" ? [1, 2] : [1]);
  });
}
