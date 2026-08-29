import { test, expect } from "./support";
import type { Locator, Page } from "@playwright/test";
import {
  cleanUpCreatedStories,
  createStory,
  deleteStory,
  heldReadDeadlineMs,
  holdFetch,
  openProject,
  pressGateSwallows,
  projectSlug,
  requiredEnv,
  seedToken,
  settledBoundingBox,
} from "./support";

/**
 * SH-401 — the general form of SH-397, on the drawer instead of the board.
 *
 * SH-397 proved the mechanism: per the UI Events click-dispatch algorithm, if
 * the element under `mousedown` is disconnected from the document before
 * `mouseup`, **no `click` is dispatched anywhere** — not even at an ancestor —
 * because the two targets no longer share a common inclusive ancestor.
 * Playwright's hit-target check runs once, before the gesture, against the
 * node that existed then, so the click is *reported successful* while nothing
 * ran. Its fix closed the first board occurrence by pointing the pointer at a
 * node an unchanged-position reconcile does not destroy: `.card` itself, so
 * making its presentational descendants `pointer-events: none` was enough.
 * SH-422 later identified the remaining whole-card reposition case; this
 * SH-401 gate closes it too, with `card-reposition-click-race.spec.ts` as the
 * exact witness.
 *
 * The drawer has no such surviving node. `renderDrawer()` does `clear(body)`
 * and rebuilds **every** child from scratch, and it runs on any `/data` reply
 * whose diff reports a change while a drawer is open — a story edited in
 * another tab is enough. So every control in the drawer body — each
 * `.section-toggle`, the block form, the label and relation forms, the
 * archived/blocked banners' own buttons — is destroyed and rebuilt under an
 * in-flight press, and a `pointer-events` rule cannot help: the button *is*
 * the destroyed node.
 *
 * SH-401's third reported occurrence is exactly this shape —
 * `description-edit-mode.spec.ts`'s Comments `.section-toggle` stuck at
 * `aria-expanded="true"` after a click Playwright reported as successful.
 * These tests force that race deterministically through the real production
 * render path (`holdFetch` gates a genuine `/data` reply; no DOM is touched by
 * hand), rather than waiting out the rare organic occurrence.
 *
 * This is not a test-only defect. A user clicking any drawer control at the
 * instant a background refresh lands sees nothing happen, with no error and
 * nothing to retry against — the same silent swallow, on the same mechanism.
 */

test.beforeEach(async ({ page }) => {
  await seedToken(page);
  // `boardFetchTimeoutMs` past this suite's own maximum patience (SH-347):
  // the tests below hold `/data`, and must not race the page's own read
  // deadline for that hold.
  await page.goto(`/?boardFetchTimeoutMs=${heldReadDeadlineMs()}`);
});

cleanUpCreatedStories("Alpha Project");

/**
 * Opens `card`'s drawer and waits until nothing further will rebuild its body
 * on its own: the detail fetch has landed (`openDrawer()` renders once from
 * the cached summary and again when that reply arrives, SH-218) and the
 * drawer's 0.2s slide-in transition has finished.
 *
 * Both waits are load-bearing for a coordinate-driven press. Without the
 * first, the spec's *own* second render can destroy the target and the test
 * would report the bug it is looking for from the wrong cause. Without the
 * second, the box is read mid-slide and the press lands on the board behind
 * the drawer (measured: the control test below failed exactly this way before
 * the wait existed, with no reply interposed at all).
 */
async function openDrawerFor(page: Page, card: Locator, id: string): Promise<void> {
  const detail = page.waitForResponse(
    (r) =>
      r.request().method() === "GET" &&
      new URL(r.url()).pathname.endsWith(`/story/${id}`),
  );
  await card.click();
  await expect(page.locator("#drawer")).toHaveClass(/open/);
  await expect(page.locator("#drawer-id")).toHaveText(id);
  await detail;
}

/**
 * Arms a hold on the `/data` reply that will carry `id` at `priority`, then
 * makes that change through the daemon's own API — a mutation from outside
 * this tab, so the resulting re-render is a genuine SSE-driven one on the
 * production path, not a synthetic call into the renderer.
 *
 * The predicate is what makes this deterministic: a priority is a field
 * `diffSnapshots` compares, so only a reply that actually carries it produces
 * the `hasChanges` that makes `fetchData()` call `renderDrawer()` at all. A
 * bare "hold the next `/data`" would sometimes hold a pre-mutation snapshot,
 * whose delivery rebuilds nothing — and the test would pass having exercised
 * precisely nothing.
 */
async function holdARebuildingReply(
  page: Page,
  request: Parameters<typeof projectSlug>[0],
  id: string,
  priority: string,
) {
  const held = await holdFetch(
    page,
    (url) => url.pathname.endsWith("/data"),
    (body: { stories: { story: { id: string; priority: string } }[] }) =>
      body.stories.some((v) => v.story.id === id && v.story.priority === priority),
  );

  const slug = await projectSlug(request, "Alpha Project");
  const changed = await request.post(
    `/api/repos/${encodeURIComponent(slug)}/story/${encodeURIComponent(id)}/priority`,
    {
      headers: {
        "X-Storyhook": "1",
        "X-Storyhook-Token": requiredEnv("DASHBOARD_TOKEN"),
        "Content-Type": "application/json",
      },
      data: { priority },
    },
  );
  if (!changed.ok()) {
    throw new Error(
      `POST .../priority answered ${changed.status()}: ${await changed.text()} -- ` +
        "this spec depends on it landing, to drive the held /data reply",
    );
  }
  await held.taken;
  return held;
}

test("a /data reply landing between mousedown and mouseup does not swallow a drawer section toggle's click (SH-401)", async ({
  page,
  request,
}) => {
  await openProject(page, "Alpha Project");

  const title = "SH-401 forced race — drawer toggle during a rebuild";
  const id = await createStory(page, title);
  const card = page.locator('.column[data-state="todo"] .card', { hasText: title });
  await openDrawerFor(page, card, id);

  const toggle = page.locator(".section-toggle", { hasText: "Comments" });
  await expect(toggle).toHaveAttribute("aria-expanded", "true");
  const held = await holdARebuildingReply(page, request, id, "high");
  const box = await settledBoundingBox(page.locator("#drawer"), toggle);

  await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
  await page.mouse.down();
  // Releases the held reply -- `fetchData()`'s success handler runs
  // `renderAll()` and then `renderDrawer()` synchronously, and `renderDrawer()`
  // clears the whole drawer body, before `deliver()` resolves. The mouse is
  // still down; the pointer's original target is gone.
  await held.deliver();
  await page.mouse.up();

  await expect(page.locator(".section-toggle", { hasText: "Comments" })).toHaveAttribute(
    "aria-expanded",
    "false",
  );
  expect(await pressGateSwallows(page)).toEqual([]);

  await page.locator("#drawer-close").click();
  await expect(page.locator("#drawer")).not.toHaveClass(/open/);
  await deleteStory(page, title);
});

/**
 * Control for the test above: the identical mouse choreography (a manual
 * `move` / `down` / `up` rather than `locator.click()`), with no reply
 * interposed, so the pointer's target survives untouched. Proves the technique
 * itself toggles the section normally, so a pass above is never mistaken for
 * an artifact of how the click was driven rather than of the race removed.
 */
test("the same down/up choreography toggles the section when nothing re-renders in between", async ({
  page,
}) => {
  await openProject(page, "Alpha Project");

  const title = "SH-401 forced race — control, no interposed rebuild";
  const id = await createStory(page, title);
  const card = page.locator('.column[data-state="todo"] .card', { hasText: title });
  await openDrawerFor(page, card, id);

  const toggle = page.locator(".section-toggle", { hasText: "Comments" });
  await expect(toggle).toHaveAttribute("aria-expanded", "true");
  const box = await settledBoundingBox(page.locator("#drawer"), toggle);

  await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
  await page.mouse.down();
  await page.mouse.up();

  await expect(toggle).toHaveAttribute("aria-expanded", "false");

  await page.locator("#drawer-close").click();
  await expect(page.locator("#drawer")).not.toHaveClass(/open/);
  await deleteStory(page, title);
});

/**
 * SH-401's second witness: the drawer FOOTER, which the story does not name.
 * `renderDrawerFooter` does its own `clear(footer)` and rebuilds Reopen / Archive /
 * Dispatch / Delete from scratch, and `handleMutationSuccess` reaches it through
 * `renderDrawer` on every field edit — so a press on Delete is exposed to exactly
 * the same race as one on a section toggle, through a different teardown.
 */
test("a /data reply landing mid-press does not swallow a drawer footer button's click (SH-401)", async ({
  page,
  request,
}) => {
  await openProject(page, "Alpha Project");

  const title = "SH-401 forced race — footer button during a rebuild";
  const id = await createStory(page, title);
  const card = page.locator('.column[data-state="todo"] .card', { hasText: title });
  await openDrawerFor(page, card, id);

  const del = page.locator("#drawer-footer .btn-danger");
  await expect(del).toBeVisible();
  const held = await holdARebuildingReply(page, request, id, "high");
  const box = await settledBoundingBox(page.locator("#drawer"), del);

  await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
  await page.mouse.down();
  await held.deliver();
  await page.mouse.up();

  await expect(page.locator("#delete-modal")).toHaveClass(/open/);
  expect(await pressGateSwallows(page)).toEqual([]);

  await page.locator("#delete-modal-cancel").click();
  await expect(page.locator("#delete-modal")).not.toHaveClass(/open/);
  await page.locator("#drawer-close").click();
  await deleteStory(page, title);
});

/**
 * SH-401's third witness: the LIST view, which nothing had filed. `populateListRow`
 * clears and rebuilds every cell of a reused `<tr>` on each render, and the click
 * that opens the drawer is bound once on the `<tr>` itself — the same shape as the
 * board before SH-397, except that no `pointer-events` rule exists for list rows
 * anywhere in the sheet, so every cell is a live press target. Reached through the
 * gate on `renderView`, which is why gating that function rather than only the
 * drawer is what makes this pass.
 */
test("a /data reply landing mid-press does not swallow a list row's click (SH-401)", async ({
  page,
  request,
}) => {
  await openProject(page, "Alpha Project");

  const title = "SH-401 forced race — list row during a rebuild";
  const id = await createStory(page, title);

  await page.locator('#view-toggle button[data-view="list"]').click();
  const row = page.locator(`#list-body tr[data-id="${id}"]`);
  await expect(row).toBeVisible();
  const held = await holdARebuildingReply(page, request, id, "high");
  // The mutation/route handshake above can take arbitrarily long under
  // contention. Measure the live row only after that handshake, immediately
  // before the coordinate press, rather than aiming at a pre-network box.
  const box = await settledBoundingBox(page.locator("#list-view"), row);

  await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
  await page.mouse.down();
  await held.deliver();
  await page.mouse.up();

  await expect(page.locator("#drawer")).toHaveClass(/open/);
  await expect(page.locator("#drawer-id")).toHaveText(id);
  expect(await pressGateSwallows(page)).toEqual([]);

  await page.locator("#drawer-close").click();
  await page.locator('#view-toggle button[data-view="board"]').click();
  await deleteStory(page, title);
});

/**
 * The one ordering fact this whole design rests on, MEASURED rather than argued —
 * this suite's own precedent for exactly that is `interception-contract.spec.ts`,
 * which refuted a plausible WebKit claim that had already been written into a
 * quarantine reason.
 *
 * The claim: `pointerup` precedes `mouseup` precedes `click`, all within one input
 * task, and a callback scheduled from `pointerup` via two `requestAnimationFrame`s
 * lands after `click`. If that were false on either engine, the gate would flush
 * mid-gesture and the fix would be a coin flip that looks identical in review.
 */
test("a two-frame callback scheduled from pointerup lands after click (SH-401 ordering contract)", async ({
  page,
}) => {
  await openProject(page, "Alpha Project");

  const order = await page.evaluate(async () => {
    const seen: string[] = [];
    const target = document.getElementById("new-story-btn") as HTMLElement;
    const note = (name: string) => () => seen.push(name);
    const settled = new Promise<void>((resolve) => {
      target.addEventListener("pointerup", () => {
        seen.push("pointerup");
        requestAnimationFrame(() =>
          requestAnimationFrame(() => {
            seen.push("twoFrames");
            resolve();
          }),
        );
      });
      target.addEventListener("mouseup", note("mouseup"));
      target.addEventListener("click", note("click"));
    });
    const at = target.getBoundingClientRect();
    const opts = {
      bubbles: true,
      clientX: at.x + at.width / 2,
      clientY: at.y + at.height / 2,
      button: 0,
      isPrimary: true,
    };
    target.dispatchEvent(new PointerEvent("pointerup", opts));
    target.dispatchEvent(new MouseEvent("mouseup", opts));
    target.dispatchEvent(new MouseEvent("click", opts));
    await settled;
    return seen;
  });

  expect(order).toEqual(["pointerup", "mouseup", "click", "twoFrames"]);
});
