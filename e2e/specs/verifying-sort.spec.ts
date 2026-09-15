import type { Page } from "@playwright/test";
import { test, expect, onAFrozenClock, openProject, projectSlug, seedToken } from "./support";

/** Only API data is replaced: the dashboard owns sorting, menus, and refresh. */
type QueueFixture = {
  active: string | null;
  ordered: string[];
  visible: string[];
  snapshot: "present" | "missing";
};

const ACTIVE = "SH-91";
const FIRST = "SH-23";
const SECOND = "SH-7";
const HELD = "SH-40";
const STALLED = "SH-41";
const LANDING = "SH-42";
const FALLBACK = ["SH-08", "SH-8", "SH-10"];
const TODO = ["SH-101", "SH-102"];

/** Mutable transport fixture; later responses can change ownership alone. */
function queueFixture(): QueueFixture {
  return {
    active: ACTIVE,
    ordered: [FIRST, SECOND, HELD, STALLED, LANDING, ACTIVE],
    visible: [SECOND, ...FALLBACK.slice().reverse(), LANDING, ACTIVE, STALLED, FIRST, HELD],
    snapshot: "present",
  };
}

/** Clone a real view to retain unrelated wire fields and isolate this contract. */
async function injectQueue(page: Page, slug: string, fixture: QueueFixture): Promise<void> {
  await page.route(
    (url) => url.pathname === `/api/repos/${encodeURIComponent(slug)}/data`,
    async (route) => {
      const response = await route.fetch();
      const data = await response.json();
      const template = data.stories[0];
      if (!template) throw new Error("verifying sort fixture needs a real story view");
      data.stories = [...fixture.visible, ...TODO].map((id) => {
        const view = JSON.parse(JSON.stringify(template));
        view.story.id = id;
        view.story.title = `SH-731 sorting ${id}`;
        view.story.state = TODO.includes(id) ? "todo" : "verifying";
        view.story.superstate = "OPEN";
        view.story.priority = id === ACTIVE ? "low" : "high";
        view.display_state = null;
        view.is_ready = TODO.includes(id);
        view.is_blocked = false;
        delete view.verification;
        // Ownership must come from the snapshot, even when the chip reports
        // an older generation or retained incident rather than Running.
        if (id === ACTIVE) view.verification = {
          status: "superseding", generation: 2, superseded_generation: 1,
          wait_seconds: 4, active_elapsed_seconds: 10,
        };
        if (id === FIRST || id === SECOND) view.verification = {
          status: "queued", position: id === FIRST ? 1 : 2, wait_seconds: 4,
        };
        if (id === HELD) view.verification = { status: "held", blockers: ["SH-99"] };
        if (id === STALLED) view.verification = {
          status: "stalled", attempts: 3, first_failed_at: view.story.created_at,
          last_failed_at: view.story.created_at, detail: "test incident", halted: true,
        };
        if (id === LANDING) view.verification = { status: "landingpending" };
        return view;
      });
      data.next_ids = TODO.slice().reverse();
      if (fixture.snapshot === "missing") {
        delete data.verifier;
      } else {
        data.verifier = {
          ...data.verifier,
          verifying: fixture.ordered,
          active: fixture.active ? {
            story_id: fixture.active, attempt_id: "SH-731-fixture", generation: 1,
            started_at: template.story.created_at,
          } : null,
        };
      }
      await route.fulfill({ response, json: data });
    },
  );
}

/** Select the production column menu, without invoking internal comparators. */
async function selectNext(page: Page, slug: string, descending = false): Promise<void> {
  await page.locator(`.column[data-state="${slug}"] .column-sort-btn`).click();
  const menu = page.locator(`.ctxmenu[aria-label="Sort ${slug}"]`);
  await menu.getByRole("menuitemradio", { name: descending ? "Next ↓" : "Next ↑", exact: true }).click();
  await expect(menu).not.toBeVisible();
}

/** Read DOM order, including cards outside the visible column scroll area. */
async function expectOrder(page: Page, ids: string[], slug = "verifying"): Promise<void> {
  await expect.poll(() => page.locator(`.column[data-state="${slug}"] .card`).evaluateAll(
    (cards) => cards.map((card) => (card as HTMLElement).dataset.id),
  )).toEqual(ids);
}

test.beforeEach(async ({ page }) => {
  // Poll intervals must be created under this clock, before navigation.
  await page.clock.install();
  await seedToken(page);
  await page.goto("/");
});

test("Next puts the owner first and reverses every rank and fallback", async ({ page, request }) => {
  const fixture = queueFixture();
  await injectQueue(page, await projectSlug(request, "Alpha Project"), fixture);
  await openProject(page, "Alpha Project");
  const ascending = [ACTIVE, FIRST, SECOND, HELD, STALLED, LANDING, ...FALLBACK];
  await selectNext(page, "verifying");
  await expectOrder(page, ascending);
  await selectNext(page, "verifying", true);
  await expectOrder(page, ascending.slice().reverse());
  await selectNext(page, "todo");
  await expectOrder(page, TODO.slice().reverse(), "todo");
  await expectOrder(page, ascending.slice().reverse());
  await page.reload();
  await expectOrder(page, ascending.slice().reverse());
  await expectOrder(page, TODO.slice().reverse(), "todo");
});

test("safety refresh updates ownership and queue order without changing story fields", async ({ page, request }) => {
  const fixture = queueFixture();
  const slug = await projectSlug(request, "Alpha Project");
  await injectQueue(page, slug, fixture);
  await openProject(page, "Alpha Project");
  await selectNext(page, "verifying");
  const refresh = async () => {
    await onAFrozenClock(page, async () => {
      const response = page.waitForResponse((reply) =>
        new URL(reply.url()).pathname === `/api/repos/${encodeURIComponent(slug)}/data`);
      await page.clock.runFor(25_000);
      await response;
    });
  };
  fixture.active = SECOND;
  fixture.ordered = [LANDING, FIRST, ACTIVE, HELD, STALLED, SECOND];
  await refresh();
  await expectOrder(page, [SECOND, LANDING, FIRST, ACTIVE, HELD, STALLED, ...FALLBACK]);
  await selectNext(page, "verifying", true);
  fixture.visible = fixture.visible.filter((id) => id !== SECOND);
  fixture.ordered = fixture.ordered.filter((id) => id !== SECOND);
  fixture.active = FIRST;
  await refresh();
  await expectOrder(page, [FIRST, LANDING, ACTIVE, HELD, STALLED, ...FALLBACK].reverse());
});

for (const mode of ["no owner", "missing", "empty", "owner outside queue"] as const) {
  test(`Next handles ${mode} deterministically in both directions`, async ({ page, request }) => {
    const fixture = queueFixture();
    fixture.active = mode === "owner outside queue" ? ACTIVE : null;
    if (mode === "missing") fixture.snapshot = "missing";
    if (mode === "empty" || mode === "missing") fixture.ordered = [];
    if (mode === "owner outside queue") fixture.ordered = fixture.ordered.filter((id) => id !== ACTIVE);
    await injectQueue(page, await projectSlug(request, "Alpha Project"), fixture);
    await openProject(page, "Alpha Project");
    const ascending = mode === "missing" || mode === "empty"
      ? [SECOND, ...FALLBACK.slice(0, 2), FALLBACK[2], FIRST, HELD, STALLED, LANDING, ACTIVE]
      : mode === "no owner"
        ? [FIRST, SECOND, HELD, STALLED, LANDING, ACTIVE, ...FALLBACK]
        : [ACTIVE, FIRST, SECOND, HELD, STALLED, LANDING, ...FALLBACK];
    await selectNext(page, "verifying");
    await expectOrder(page, ascending);
    await selectNext(page, "verifying", true);
    await expectOrder(page, ascending.slice().reverse());
  });
}
