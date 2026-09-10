import { test, expect } from "./support";
import {
  cleanUpCreatedStories,
  createStory,
  openProject,
  seedToken,
  settledBoundingBox,
} from "./support";

/**
 * SH-623 — the box a wait settles must be the box the test measures.
 *
 * `notice-dock-geometry.spec.ts` waited on `#drawer`'s own right edge being
 * inside the viewport and then hit-tested `#drawer-close`. On the release
 * tier's first real run, chromium reported the button's centre as `nothing`:
 * `elementFromPoint` had returned null for a point 113px outside a 1280px
 * viewport. The wait had released; the measured element was not settled.
 *
 * Re-measured here rather than reasoned about, the mechanism is sharper than
 * "the ancestor finished before its child": the closed drawer is `width: 0;
 * transform: translateX(100%)`, and **100% of a 0-wide box is 0px**. Its right
 * edge is therefore at the viewport edge BEFORE the transition has moved at
 * all, so the retired predicate was true at t=0 — and at that instant the
 * header lays its `nowrap` items out past a 0-wide box, putting the close
 * button ~100px off-screen. A proxy claim on an ancestor is a different claim
 * from the one the test makes, and can be true before the motion even starts.
 *
 * SH-420's posture: the reproduction is CONSTRUCTED rather than waited for.
 * The drawer's own CSS transitions are paused the moment they start and then
 * seeked across their whole duration, and the two claims are evaluated at
 * each sample. That is deterministic on every engine and costs no wall clock;
 * a test that instead re-raced the transition would prove nothing on the
 * engine that happened to win.
 *
 * What is pinned: (1) the retired ancestor predicate holds at sample 0 while
 * the close button's centre misses — the straddle; (2) the sampler is not
 * degenerate: at the end of the transition both claims hold; (3) the
 * instrument the fix adopted, `settledBoundingBox(#drawer, #drawer-close)`,
 * answers a box whose centre really hits the button once the drawer is let
 * finish. `awaitSettled` filters `running` and a paused animation is not
 * running — deliberately, see its doc — so this file's hold is not itself the
 * thing the instrument waits on; the centre-hit half is.
 */

cleanUpCreatedStories("Alpha Project");

/** One sample of the two claims, taken with the drawer's transitions held at
 * a chosen time. `ancestorInside` is the predicate the retired wait used,
 * verbatim; `closeCentreHits` is what the assertion it guarded needs. */
interface Sample {
  currentTime: number;
  drawerRight: number;
  innerWidth: number;
  closeCentreX: number;
  ancestorInside: boolean;
  closeCentreHits: boolean;
}

/** The slack the retired wait allowed the drawer's right edge. Quoted from
 * the predicate it belonged to (`right <= window.innerWidth + 0.5`), never
 * chosen here: the point is to evaluate exactly the claim that failed. */
const RETIRED_PREDICATE_SLACK_PX = 0.5;

/** How many points along the transition are sampled. Sample 0 is the one
 * the mechanism predicts; the rest show the claims' shape across the whole
 * motion in the failure message, so a reader sees where they diverge. */
const SAMPLES = 40;

test.beforeEach(async ({ page }) => {
  await seedToken(page);
  await page.goto("/");
  await openProject(page, "Alpha Project");
});

test("the drawer's settled right edge is not evidence its close button has arrived", async ({
  page,
}) => {
  const title = `SH-623 the measured box — ${test.info().title}`;
  await createStory(page, title);
  const card = page.locator(".card", { hasText: title });

  // Click, capture the transitions, pause and sample — in ONE renderer task,
  // so a 0.2s transition cannot finish during a Playwright round trip before
  // it is held (the `startedPanelTransitions` shape from
  // `detail-panel.spec.ts`). Forcing layout first is what makes the click's
  // style change start a transition rather than land as the initial style.
  const sampled = await card.evaluate(
    (node, args) => {
      const drawer = document.getElementById("drawer")!;
      const close = document.getElementById("drawer-close")!;
      drawer.getBoundingClientRect();
      (node as HTMLElement).click();
      const transitions = drawer.getAnimations() as CSSTransition[];
      const properties = transitions.map((t) => t.transitionProperty);
      for (const t of transitions) t.pause();
      const durations = transitions.map((t) => Number(t.effect!.getTiming().duration));
      const duration = Math.max(...durations);

      const samples: Sample[] = [];
      for (let i = 0; i <= args.samples; i++) {
        const currentTime = (duration * i) / args.samples;
        for (const t of transitions) t.currentTime = currentTime;
        const d = drawer.getBoundingClientRect();
        const c = close.getBoundingClientRect();
        const cx = c.x + c.width / 2;
        const cy = c.y + c.height / 2;
        const hit = document.elementFromPoint(cx, cy);
        samples.push({
          currentTime,
          drawerRight: d.right,
          innerWidth: window.innerWidth,
          closeCentreX: cx,
          ancestorInside: d.right <= window.innerWidth + args.slack,
          closeCentreHits: !!hit && close.contains(hit),
        });
      }
      for (const t of transitions) t.finish();
      return { properties, duration, samples };
    },
    { samples: SAMPLES, slack: RETIRED_PREDICATE_SLACK_PX },
  );

  // The premise, asserted rather than assumed: the drawer really does move
  // by BOTH width and transform. If either transition goes, the 100%-of-0px
  // mechanism goes with it and this test must say so rather than pass on a
  // surface it no longer describes.
  expect(sampled.properties).toEqual(expect.arrayContaining(["width", "transform"]));
  expect(sampled.duration).toBeGreaterThan(0);

  const table = sampled.samples
    .map(
      (s) =>
        `t=${s.currentTime.toFixed(1)}ms right=${s.drawerRight.toFixed(2)} ` +
        `closeCentreX=${s.closeCentreX.toFixed(2)} viewport=${s.innerWidth} ` +
        `ancestorInside=${s.ancestorInside} closeCentreHits=${s.closeCentreHits}`,
    )
    .join("\n");

  // (1) The straddle, at the instant the mechanism predicts it: the retired
  // wait would have released here, and the hit test it guarded would have
  // returned nothing.
  const first = sampled.samples[0];
  expect(
    first.ancestorInside,
    `at t=0 the closed drawer's right edge must already satisfy the retired ` +
      `predicate (translateX(100%) of a 0-wide box is 0px)\n${table}`,
  ).toBe(true);
  expect(
    first.closeCentreHits,
    `at t=0 the close button must NOT yet be reachable at its centre — if it ` +
      `is, the header no longer overflows a 0-wide drawer and this witness's ` +
      `premise is gone\n${table}`,
  ).toBe(false);
  expect(
    first.closeCentreX,
    `the close button's centre should sit outside the viewport at t=0\n${table}`,
  ).toBeGreaterThan(first.innerWidth);

  // (2) The sampler is not degenerate: let the transition finish and both
  // claims hold together.
  const last = sampled.samples[sampled.samples.length - 1];
  expect(last.ancestorInside, `settled drawer must be inside the viewport\n${table}`).toBe(true);
  expect(last.closeCentreHits, `settled close button must own its centre\n${table}`).toBe(true);

  // (3) The instrument the fix adopted answers the right box for the right
  // element, and that box's centre really hits the button.
  await expect(page.locator("#drawer")).toHaveClass(/open/);
  const closeButton = page.locator("#drawer-close");
  const box = await settledBoundingBox(page.locator("#drawer"), closeButton);
  const receiver = await closeButton.evaluate((node, rect) => {
    const hit = document.elementFromPoint(rect.x + rect.width / 2, rect.y + rect.height / 2);
    return hit && node.contains(hit) ? "target" : hit?.outerHTML ?? "outside viewport";
  }, box);
  expect(receiver).toBe("target");
});
