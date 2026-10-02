import { test, expect } from "./support";
import { withDrainedRoutes } from "../route-lifetime";
import { focusMenuItemByLabel, holdKey, openProject, projectSlug, seedToken } from "./support";

/**
 * SH-692: a `verifying` card belongs to the central verifier. Dropping it on
 * Done — the door PR #791's story went through eleven seconds after a hand
 * merge, with nothing recorded — opens a prompt that, unlike Blocked's
 * (SH-205), is not skippable: the move is sent only with a non-empty reason,
 * as `/move {state:"done", comment}`, and Cancel, backdrop and Escape leave
 * the card in Verifying with no request made. The drawer's state select
 * and the board/list Set Status menus (SH-782) reach the same prompt.
 *
 * The verifying card is injected through the `/data` route, as
 * `verification-status.spec.ts` does: a real story parked in `verifying`
 * would be picked up by the daemon's own verifier within the second. The
 * `/move` route is fulfilled here too, so the spec asserts the request the
 * page sends rather than a daemon reply about a story that does not exist.
 */

const TITLE = "SH-692 verifying card dropped on Done";
const ID = "SH-94692";

// Only completing rewrite handlers run here; keep their responses alive until
// the handlers finish, including when an assertion fails before normal cleanup.
test.use({
  page: async ({ page }, use) => {
    await withDrainedRoutes(page, () => use(page));
  },
});

type Page = import("@playwright/test").Page;
type View = "board" | "list";
type Selection = "pointer" | "keyboard";

async function injectVerifyingCard(page: Page, slug: string): Promise<void> {
  await page.route(
    (url) => url.pathname === `/api/repos/${encodeURIComponent(slug)}/data`,
    async (route) => {
      const response = await route.fetch();
      const data: { stories?: Array<Record<string, unknown>> } = await response.json();
      const template = (data.stories ?? [])[0];
      if (!template) throw new Error("override fixture has no story to clone");
      const clone = JSON.parse(JSON.stringify(template)) as {
        story: Record<string, unknown>;
        display_state?: string | null;
        is_ready?: boolean;
        is_blocked?: boolean;
        verification?: unknown;
      };
      clone.story.id = ID;
      clone.story.title = TITLE;
      clone.story.state = "verifying";
      clone.story.superstate = "OPEN";
      clone.display_state = null;
      clone.is_ready = false;
      clone.is_blocked = false;
      clone.verification = { status: "running", elapsed_seconds: 42 };
      (data.stories ??= []).push(clone);
      await route.fulfill({ response, json: data });
    },
  );
}

/** Records every `/move` the page sends for the injected card and answers
 * it as the daemon would answer a completed override. */
async function captureMoves(page: Page, slug: string): Promise<Array<Record<string, unknown>>> {
  const moves: Array<Record<string, unknown>> = [];
  await page.route(
    (url) => url.pathname === `/api/repos/${encodeURIComponent(slug)}/story/${ID}/move`,
    async (route) => {
      moves.push(route.request().postDataJSON() as Record<string, unknown>);
      await route.fulfill({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify({
          result: "ok",
          story: { story: { id: ID, title: TITLE, state: "done", superstate: "CLOSED", comments: [] } },
        }),
      });
    },
  );
  return moves;
}

function verifyingCard(page: Page) {
  return page.locator('.column[data-state="verifying"] .card', { hasText: TITLE });
}

/** Uses the real menu in either rendering, including its roving keyboard focus. */
async function selectDone(page: Page, view: View, selection: Selection): Promise<void> {
  const story = view === "board"
    ? verifyingCard(page)
    : page.locator("#list-body tr", { hasText: TITLE });
  await story.click({ button: "right" });
  if (selection === "pointer") {
    await page.getByRole("menuitem", { name: "Set Status", exact: true }).click();
    await page.locator(".ctxmenu-sub").getByRole("menuitem", { name: "done", exact: true }).click();
  } else {
    await focusMenuItemByLabel(page, "Set Status");
    await page.keyboard.press("ArrowRight");
    const submenu = page.locator(".ctxmenu-sub");
    const done = submenu.getByRole("menuitem", { name: "done", exact: true });
    const count = await submenu.getByRole("menuitem").count();
    for (let index = 0; index < count; index++) {
      if (await done.evaluate((node) => node === document.activeElement)) break;
      await page.keyboard.press("ArrowDown");
    }
    await expect(done).toBeFocused();
    await page.keyboard.press("Enter");
  }
  await expect(page.locator(".ctxmenu")).toHaveCount(0);
  await expect(page.locator("#verify-override-modal")).toHaveClass(/open/);
  await expect(page.locator("#verify-override-reason")).toBeFocused();
}

/** Assert the displayed persisted state, not just the presence of a list row. */
async function expectVerifying(page: Page, view: View): Promise<void> {
  if (view === "board") {
    await expect(verifyingCard(page)).toBeVisible();
  } else {
    await expect(page.locator("#list-body tr", { hasText: TITLE }).locator(".state-pill")).toHaveText("verifying");
  }
}

async function dropOnDone(page: Page): Promise<void> {
  // Done is the board's last column and can sit half off the viewport once
  // the drag has scrolled the verifying card into view; the column's centre
  // is then off-screen and a centred drop lands on nothing. Scroll it in
  // and drop on its header, which is the same drop target (`bindColumnDrop`
  // binds the whole column).
  const done = page.locator('.column[data-state="done"]');
  await done.scrollIntoViewIfNeeded();
  await verifyingCard(page).dragTo(done, { targetPosition: { x: 40, y: 24 } });
  await expect(page.locator("#verify-override-modal")).toHaveClass(/open/);
  await expect(page.locator("#verify-override-reason")).toBeFocused();
}

test.beforeEach(async ({ page }) => {
  await seedToken(page);
});

test("dropping a verifying card on Done requires a reason and sends it as the override", async ({
  page,
  request,
}) => {
  const slug = await projectSlug(request, "Alpha Project");
  await injectVerifyingCard(page, slug);
  const moves = await captureMoves(page, slug);
  await page.goto("/");
  await openProject(page, "Alpha Project");

  await dropOnDone(page);
  await page.locator("#verify-override-submit").click();
  await expect(page.locator("#verify-override-error")).toContainText(
    "A reason for overriding verification is required.",
  );
  expect(moves).toEqual([]);

  await page.locator("#verify-override-reason").fill("  merged by hand after a local gate run  ");
  await page.locator("#verify-override-submit").click();
  await expect(page.locator("#verify-override-modal")).not.toHaveClass(/open/);
  await expect.poll(() => moves.length).toBe(1);
  expect(moves[0]).toEqual({ state: "done", comment: "merged by hand after a local gate run" });
});

test("Cancel, backdrop and Escape leave the card in Verifying and send nothing", async ({
  page,
  request,
}) => {
  const slug = await projectSlug(request, "Alpha Project");
  await injectVerifyingCard(page, slug);
  const moves = await captureMoves(page, slug);
  await page.goto("/");
  await openProject(page, "Alpha Project");

  await dropOnDone(page);
  await page.locator("#verify-override-cancel").click();
  await expect(page.locator("#verify-override-modal")).not.toHaveClass(/open/);
  await expect(verifyingCard(page)).toBeVisible();

  await dropOnDone(page);
  await page.locator("#verify-override-backdrop").click({ position: { x: 5, y: 5 } });
  await expect(page.locator("#verify-override-modal")).not.toHaveClass(/open/);
  await expect(verifyingCard(page)).toBeVisible();

  await dropOnDone(page);
  await page.keyboard.press("Escape");
  await expect(page.locator("#verify-override-modal")).not.toHaveClass(/open/);
  await expect(verifyingCard(page)).toBeVisible();

  expect(moves).toEqual([]);
});

test("the drawer's state select reaches the same prompt", async ({ page, request }) => {
  const slug = await projectSlug(request, "Alpha Project");
  await injectVerifyingCard(page, slug);
  const moves = await captureMoves(page, slug);
  await page.goto("/");
  await openProject(page, "Alpha Project");

  await verifyingCard(page).click();
  await expect(page.locator("#drawer")).toHaveClass(/open/);
  const select = page.locator("#drawer-body select").first();
  await select.selectOption("done");
  await expect(page.locator("#verify-override-modal")).toHaveClass(/open/);
  await expect(select).toHaveValue("verifying");
  expect(moves).toEqual([]);

  await page.locator("#verify-override-reason").fill("verified locally; verifier queue is down");
  await page.locator("#verify-override-submit").click();
  await expect.poll(() => moves.length).toBe(1);
  expect(moves[0]).toEqual({ state: "done", comment: "verified locally; verifier queue is down" });
});

for (const view of ["board", "list"] as const) {
  for (const selection of ["pointer", "keyboard"] as const) {
    test(`SH-782 ${view} ${selection}: Set Status requires an override reason and submits once`, async ({ page, request }) => {
      const slug = await projectSlug(request, "Alpha Project");
      await injectVerifyingCard(page, slug);
      const moves = await captureMoves(page, slug);
      await page.goto("/");
      await openProject(page, "Alpha Project");
      await page.locator(`#view-toggle button[data-view="${view}"]`).click();

      await selectDone(page, view, selection);
      await expectVerifying(page, view);
      expect(moves).toEqual([]);

      const reason = page.locator("#verify-override-reason");
      for (const blank of ["", "   "]) {
        await reason.fill(blank);
        await page.locator("#verify-override-submit").click();
        await expect(page.locator("#verify-override-error")).toHaveText(
          "A reason for overriding verification is required.",
        );
        await expectVerifying(page, view);
        expect(moves).toEqual([]);
      }

      await reason.fill("  verified locally; complete by hand  ");
      await holdKey(page, "Enter", 4);
      await expect(page.locator("#verify-override-modal")).not.toHaveClass(/open/);
      await expect.poll(() => moves.length).toBe(1);
      expect(moves).toEqual([{ state: "done", comment: "verified locally; complete by hand" }]);
    });
  }

  test(`SH-782 ${view}: dismissing a menu override does not move and reopening clears the prompt`, async ({ page, request }) => {
    const slug = await projectSlug(request, "Alpha Project");
    await injectVerifyingCard(page, slug);
    const moves = await captureMoves(page, slug);
    await page.goto("/");
    await openProject(page, "Alpha Project");
    await page.locator(`#view-toggle button[data-view="${view}"]`).click();

    for (const dismiss of ["cancel", "backdrop", "escape"] as const) {
      await selectDone(page, view, "pointer");
      await expect(page.locator("#verify-override-reason")).toHaveValue("");
      await expect(page.locator("#verify-override-error")).toBeEmpty();
      await page.locator("#verify-override-submit").click();
      await expect(page.locator("#verify-override-error")).not.toBeEmpty();
      await page.locator("#verify-override-reason").fill("unsubmitted reason");
      if (dismiss === "cancel") await page.locator("#verify-override-cancel").click();
      else if (dismiss === "backdrop") {
        await page.locator("#verify-override-backdrop").click({ position: { x: 5, y: 5 } });
      } else await page.keyboard.press("Escape");
      await expect(page.locator("#verify-override-modal")).not.toHaveClass(/open/);
      await expectVerifying(page, view);
      expect(moves).toEqual([]);
    }
    await selectDone(page, view, "keyboard");
    await expect(page.locator("#verify-override-reason")).toHaveValue("");
    await expect(page.locator("#verify-override-error")).toBeEmpty();
    await page.keyboard.press("Escape");
    expect(moves).toEqual([]);
  });
}

test("SH-782 a refused menu override reports the error and restores Verifying", async ({ page, request }) => {
  const slug = await projectSlug(request, "Alpha Project");
  await injectVerifyingCard(page, slug);
  let release!: () => void;
  const responseReady = new Promise<void>((resolve) => { release = resolve; });
  const moves: Array<Record<string, unknown>> = [];
  await page.route(
    (url) => url.pathname === `/api/repos/${encodeURIComponent(slug)}/story/${ID}/move`,
    async (route) => {
      moves.push(route.request().postDataJSON() as Record<string, unknown>);
      await responseReady;
      await route.fulfill({ status: 409, json: { error: "override refused by fixture" } });
    },
  );
  await page.goto("/");
  await openProject(page, "Alpha Project");
  try {
    await selectDone(page, "board", "pointer");
    await page.locator("#verify-override-reason").fill("local verification complete");
    await page.locator("#verify-override-submit").click();
    await expect.poll(() => moves.length).toBe(1);
    await expect(page.locator('.column[data-state="done"] .card', { hasText: TITLE })).toBeVisible();
    release();
    await expect(page.locator("#toast-stack .toast.error")).toContainText("override refused by fixture");
    await expect(verifyingCard(page)).toBeVisible();
    expect(moves).toEqual([{ state: "done", comment: "local verification complete" }]);
  } finally {
    release();
  }
});
