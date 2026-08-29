import { test, expect } from "./support";
import {
  cleanUpCreatedStories,
  createStory,
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
 * SH-422 -- the whole-card residue SH-397 could not close with pointer-events.
 *
 * `reconcileColumnCards()` walks the old DOM with a cursor. Given old order
 * [A, B] and desired order [B] after A leaves the column, B differs from the
 * cursor A, so `insertBefore(B, A)` disconnects and reinserts B. If that move
 * lands between a real pointer down and up on B, the browser dispatches no
 * click even though Playwright reports the gesture itself as successful.
 *
 * SH-401 landed after SH-422 was filed and closes the broader class: the
 * capture-phase press gate defers `renderView()` while a primary press is live,
 * so reconciliation cannot reach that `insertBefore()` until after the click.
 * This suite is the exact missing witness. It gates the real `/data` reply that
 * moves A, delivers it while B is pressed, and proves both that the old DOM is
 * still painted during the press and that B's own click opens B afterward.
 */

test.beforeEach(async ({ page }) => {
  await seedToken(page);
  await page.goto(`/?boardFetchTimeoutMs=${heldReadDeadlineMs()}`);
  await openProject(page, "Alpha Project");
});

cleanUpCreatedStories("Alpha Project");

async function seedOrderedPair(page: import("@playwright/test").Page, suffix: string) {
  const departingTitle = `SH-422 ${suffix} -- departing A`;
  const pressedTitle = `SH-422 ${suffix} -- pressed B`;
  const departingId = await createStory(page, departingTitle);
  const pressedId = await createStory(page, pressedTitle);
  const column = page.locator('.column[data-state="todo"] .column-cards');
  const departing = column.locator(`.card[data-id="${departingId}"]`);
  const pressed = column.locator(`.card[data-id="${pressedId}"]`);

  await expect
    .poll(() =>
      column.locator(".card").evaluateAll(
        (cards, ids) => {
          const positions = ids.map((id) =>
            cards.findIndex((card) => (card as HTMLElement).dataset.id === id),
          );
          return positions[0] !== -1 && positions[1] === positions[0] + 1;
        },
        [departingId, pressedId],
      ),
    )
    .toBe(true);

  return { column, departing, departingId, pressed, pressedId };
}

test("a /data reply that removes the card above does not swallow the staying card's click (SH-422)", async ({
  page,
  request,
}) => {
  const { column, departing, departingId, pressed, pressedId } = await seedOrderedPair(
    page,
    "forced reposition",
  );

  const held = await holdFetch(
    page,
    (url) => url.pathname.endsWith("/data"),
    (body: { stories: { story: { id: string; state: string } }[] }) =>
      body.stories.some((view) => view.story.id === departingId && view.story.state === "done"),
    { sealOnHold: true },
  );
  const slug = await projectSlug(request, "Alpha Project");
  const moved = await request.post(
    `/api/repos/${encodeURIComponent(slug)}/story/${encodeURIComponent(departingId)}/move`,
    {
      headers: {
        "X-Storyhook": "1",
        "X-Storyhook-Token": requiredEnv("DASHBOARD_TOKEN"),
        "Content-Type": "application/json",
      },
      data: { state: "done" },
    },
  );
  if (!moved.ok()) {
    throw new Error(
      `POST .../move answered ${moved.status()}: ${await moved.text()} -- ` +
        "this spec depends on A leaving todo in the held /data reply",
    );
  }
  await held.taken;

  const box = await settledBoundingBox(column, pressed);
  const pressedNode = await pressed.elementHandle();
  if (!pressedNode) throw new Error("the pressed B card has no element handle");

  await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
  await page.mouse.down();
  await held.deliver();

  // Paint, not data, waits: the exact original B node remains after A in the
  // todo column until the gesture can no longer produce a click. Without the
  // SH-401 renderView guard, A has already moved to done and the cursor walk
  // has disconnected/reinserted B before this observation.
  expect(
    await pressedNode.evaluate((node) => ({
      connected: node.isConnected,
      state: (node.closest(".column") as HTMLElement | null)?.dataset.state,
      previousId: (node.previousElementSibling as HTMLElement | null)?.dataset.id,
    })),
  ).toEqual({ connected: true, state: "todo", previousId: departingId });
  await expect(departing).toBeVisible();

  await page.mouse.up();

  await expect(page.locator("#drawer")).toHaveClass(/open/);
  await expect(page.locator("#drawer-id")).toHaveText(pressedId);
  await expect(
    page.locator(`.column[data-state="done"] .card[data-id="${departingId}"]`),
  ).toBeVisible();
  expect(await pressGateSwallows(page)).toEqual([]);
});

test("the same down/up choreography opens the staying card when nothing re-renders between them", async ({
  page,
}) => {
  const { column, pressed, pressedId } = await seedOrderedPair(page, "control");
  const box = await settledBoundingBox(column, pressed);

  await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
  await page.mouse.down();
  await page.mouse.up();

  await expect(page.locator("#drawer")).toHaveClass(/open/);
  await expect(page.locator("#drawer-id")).toHaveText(pressedId);
  expect(await pressGateSwallows(page)).toEqual([]);
});
