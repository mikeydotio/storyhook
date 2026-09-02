import { test, expect } from "./support";
import type { Page } from "@playwright/test";
import {
  cleanUpCreatedStories,
  deleteStory,
  holdDetailFetch,
  openProject,
  pressGateSwallows,
  requiredEnv,
  seedToken,
  settledBoundingBox,
} from "./support";

/**
 * Exercises SH-217's read/edit split for the drawer's description field:
 * rendered markdown (`.description-view`) by default, the raw `<textarea>`
 * (`.description-field`) only while editing. The renderer's own grammar is
 * markdown-rendering.spec.ts's job (proved through comment bodies, which
 * have no focus/edit machinery in the way); this spec is the interaction
 * contract layered on top of it -- entering/leaving edit mode, the caret,
 * the commit path, and the keyboard route.
 *
 * Creates and deletes its own stories rather than touching the "Alpha
 * Project" fixture, whose exact two-story shape other specs
 * (filter-persistence.spec.ts, column-visibility.spec.ts) assert on
 * byte-for-byte per run-e2e.sh's own comment.
 */

cleanUpCreatedStories("Alpha Project");

test.beforeEach(async ({ page }) => {
  await seedToken(page);
  await page.goto("/");
  await openProject(page, "Alpha Project");
});

async function createStory(page: Page, title: string, description?: string) {
  await page.locator("#new-story-btn").click();
  await expect(page.locator("#create-modal")).toHaveClass(/open/);
  await page.locator("#create-title").fill(title);
  if (description) await page.locator("#create-description").fill(description);
  // Keep this fixture's priority explicit.
  await page.locator("#create-priority").selectOption("medium");
  await page.locator("#create-submit").click();
  await expect(page.locator("#create-modal")).not.toHaveClass(/open/);
  const card = page.locator('.column[data-state="todo"] .card', { hasText: title });
  await expect(card).toBeVisible();
  return card;
}

async function openStory(page: Page, card: ReturnType<Page["locator"]>) {
  const detailLoaded = page.waitForResponse(
    (resp) => /\/story\/[^/]+$/.test(new URL(resp.url()).pathname) && resp.request().method() === "GET",
  );
  await card.click();
  await expect(page.locator("#drawer")).toHaveClass(/open/);
  // The drawer renders once synchronously, then again when the detail GET
  // resolves (SH-218) -- wait for the second render before asserting.
  await detailLoaded;
}

test("an unfocused description shows rendered markdown, not raw text", async ({ page }) => {
  const title = "SH-217 description — rendered by default";
  const card = await createStory(page, title, "# Heading\n\n- one\n- two");
  await openStory(page, card);

  await expect(page.locator(".description-field")).toBeHidden();
  await expect(page.locator(".description-view")).toBeVisible();
  await expect(page.locator(".description-view h1")).toHaveText("Heading");
  await expect(page.locator(".description-view li")).toHaveCount(2);

  await page.locator("#drawer-close").click();
  await deleteStory(page, title);
});

test("clicking the rendered description swaps in the raw source with the caret at the end", async ({
  page,
}) => {
  const title = "SH-217 description — click to edit";
  const source = "## Section\n\nsome *text*";
  const card = await createStory(page, title, source);
  await openStory(page, card);

  await page.locator(".description-view").click();
  await expect(page.locator(".description-field")).toBeVisible();
  await expect(page.locator(".description-view")).toBeHidden();
  await expect(page.locator(".description-field")).toBeFocused();
  await expect(page.locator(".description-field")).toHaveValue(source);
  const caret = await page.locator(".description-field").evaluate(
    (el: HTMLTextAreaElement) => el.selectionStart,
  );
  expect(caret).toBe(source.length);

  await page.locator("#drawer-close").click();
  await deleteStory(page, title);
});

test("blurring the description saves it and returns to the rendered view", async ({ page }) => {
  const title = "SH-217 description — blur saves";
  const card = await createStory(page, title, "before");
  await openStory(page, card);

  const patches: unknown[] = [];
  let releasePatch!: () => void;
  let markPatchTaken!: () => void;
  let markPatchDelivered!: () => void;
  const patchHold = new Promise<void>((resolve) => { releasePatch = resolve; });
  const patchTaken = new Promise<void>((resolve) => { markPatchTaken = resolve; });
  const patchDelivered = new Promise<void>((resolve) => { markPatchDelivered = resolve; });
  await page.route(/\/story\/[^/]+$/, async (route) => {
    if (route.request().method() !== "PATCH") {
      await route.continue();
      return;
    }
    patches.push(route.request().postDataJSON());
    const response = await route.fetch({
      headers: {
        ...route.request().headers(),
        "X-Storyhook-Token": requiredEnv("DASHBOARD_TOKEN"),
      },
    });
    markPatchTaken();
    await patchHold;
    try {
      await route.fulfill({ response });
    } finally {
      markPatchDelivered();
    }
  });

  await page.locator(".description-view").click();
  await page.locator(".description-field").fill("**after**");
  // Optimistic: the view updates on blur, before the PATCH round trip.
  const patchLanded = page.waitForRequest(
    (req) => /\/story\/[^/]+$/.test(new URL(req.url()).pathname) && req.method() === "PATCH",
  );
  await page.locator(".drawer-title").click(); // blurs the description
  await expect(page.locator(".description-view")).toBeVisible();
  await expect(page.locator(".description-view strong")).toHaveText("after");
  await patchLanded;
  await expect.poll(() => patches.length).toBeGreaterThan(0);
  expect(patches).toEqual([{ description: "**after**" }]);

  await page.locator("#drawer-close").click();
  await deleteStory(page, title);
});

test("leaving an unchanged description saves nothing", async ({ page }) => {
  const title = "SH-217 description — no-op blur";
  const card = await createStory(page, title, "unchanged text");
  await openStory(page, card);

  let patchCount = 0;
  await page.route(/\/story\/[^/]+$/, async (route) => {
    if (route.request().method() === "PATCH") patchCount++;
    await route.continue();
  });

  await page.locator(".description-view").click();
  await page.locator(".drawer-title").click(); // blurs without editing
  await expect(page.locator(".description-view")).toBeVisible();
  expect(patchCount).toBe(0);

  await page.locator("#drawer-close").click();
  await deleteStory(page, title);
});

test("Escape reverts the edit and leaves the drawer open", async ({ page }) => {
  const title = "SH-217 description — Escape reverts";
  const original = "original text";
  const card = await createStory(page, title, original);
  await openStory(page, card);

  let patchCount = 0;
  await page.route(/\/story\/[^/]+$/, async (route) => {
    if (route.request().method() === "PATCH") patchCount++;
    await route.continue();
  });

  await page.locator(".description-view").click();
  await page.locator(".description-field").fill("this should not save");
  await page.locator(".description-field").press("Escape");

  await expect(page.locator("#drawer")).toHaveClass(/open/);
  await expect(page.locator(".description-view")).toBeVisible();
  await expect(page.locator(".description-view")).toContainText(original);
  expect(patchCount).toBe(0);

  await page.locator("#drawer-close").click();
  await deleteStory(page, title);
});

test("Tab reaches the rendered description and Enter opens the editor", async ({ page }) => {
  const title = "SH-217 description — keyboard entry";
  const card = await createStory(page, title, "keyboard text");
  await openStory(page, card);

  // The Type select is the field immediately before the description
  // section in the drawer body (buildFieldGrid, then buildDescriptionSection).
  await page.locator("#drawer-body select").nth(3).focus();
  await page.keyboard.press("Tab");
  await expect(page.locator(".description-view")).toBeFocused();
  await expect(page.locator(".description-field")).toBeHidden();

  await page.keyboard.press("Enter");
  await expect(page.locator(".description-field")).toBeFocused();
  await expect(page.locator(".description-view")).toBeHidden();

  await page.locator("#drawer-close").click();
  await deleteStory(page, title);
});

test("clicking a story reference in the rendered description opens that story without entering edit mode", async ({
  page,
}) => {
  const source = "SH-217 description — reference source";
  const target = "SH-217 description — reference target";
  const targetCard = await createStory(page, target);
  const targetId = (await targetCard.getAttribute("data-id"))!;
  const sourceCard = await createStory(page, source, `see ${targetId} for context`);
  await openStory(page, sourceCard);

  await page.locator(".description-view .rel-id").click();
  await expect(page.locator("#drawer-id")).toHaveText(targetId);
  await expect(page.locator(".description-field")).toBeHidden();

  await page.locator("#drawer-close").click();
  await deleteStory(page, source);
  await deleteStory(page, target);
});

test("a description edit survives a mid-edit detail-fetch re-render", async ({ page }) => {
  const title = "SH-217 description — SH-218 race";
  const card = await createStory(page, title, "original");
  const release = await holdDetailFetch(page);

  await card.click();
  await expect(page.locator("#drawer")).toHaveClass(/open/);

  // The drawer's first, synchronous render is from cached summary data,
  // before the held detail GET resolves -- edit mode is entered here,
  // against that first render.
  await page.locator(".description-view").click();
  await page.locator(".description-field").fill("mid-edit typed value");
  const caretPos = 6;
  await page.locator(".description-field").evaluate(
    (el: HTMLTextAreaElement, pos: number) => el.setSelectionRange(pos, pos),
    caretPos,
  );

  const patches: unknown[] = [];
  await page.route(/\/story\/[^/]+$/, async (route) => {
    if (route.request().method() === "PATCH") patches.push(route.request().postDataJSON());
    await route.continue();
  });

  release(); // let the detail GET through -- renderDrawer() fires again, mid-edit
  await page.waitForTimeout(500);

  // Still in edit mode, typed value and caret intact, no premature PATCH.
  await expect(page.locator(".description-field")).toBeVisible();
  await expect(page.locator(".description-field")).toBeFocused();
  await expect(page.locator(".description-field")).toHaveValue("mid-edit typed value");
  const restoredCaret = await page.locator(".description-field").evaluate(
    (el: HTMLTextAreaElement) => el.selectionStart,
  );
  expect(restoredCaret).toBe(caretPos);
  expect(patches.length).toBe(0);

  // Committing now produces exactly one PATCH, not a duplicate.
  await page.locator(".drawer-title").click();
  await expect.poll(() => patches.length).toBe(1);
  expect(patches).toEqual([{ description: "mid-edit typed value" }]);

  await page.locator("#drawer-close").click();
  await deleteStory(page, title);
});

test("clicking the Comments toggle straight from edit mode both saves and toggles the section", async ({
  page,
}) => {
  const title = "SH-217 description — click-away while editing";
  const card = await createStory(page, title, "before edit");
  await openStory(page, card);

  const patches: unknown[] = [];
  let releasePatch!: () => void;
  let markPatchTaken!: () => void;
  let markPatchDelivered!: () => void;
  const patchHold = new Promise<void>((resolve) => { releasePatch = resolve; });
  const patchTaken = new Promise<void>((resolve) => { markPatchTaken = resolve; });
  const patchDelivered = new Promise<void>((resolve) => { markPatchDelivered = resolve; });
  await page.route(/\/story\/[^/]+$/, async (route) => {
    if (route.request().method() !== "PATCH") {
      await route.continue();
      return;
    }
    patches.push(route.request().postDataJSON());
    const response = await route.fetch({
      headers: {
        ...route.request().headers(),
        "X-Storyhook-Token": requiredEnv("DASHBOARD_TOKEN"),
      },
    });
    markPatchTaken();
    await patchHold;
    try {
      await route.fulfill({ response });
    } finally {
      markPatchDelivered();
    }
  });

  await page.locator(".description-view").click();
  await page.locator(".description-field").fill("after edit");
  const commentsToggle = page.locator(".section-toggle", { hasText: "Comments" });
  await page.evaluate(() => {
    (window as typeof window & { __sh423PressTarget?: Element }).__sh423PressTarget =
      Array.from(document.querySelectorAll(".section-toggle")).find((node) =>
        node.textContent?.includes("Comments"),
      );
  });
  const box = await settledBoundingBox(page.locator("#drawer"), commentsToggle);

  await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
  await page.mouse.down();
  // The mousedown blurs the textarea and starts the real PATCH. Hold its real
  // response until the write has landed, then deliver it while the pointer is
  // still down: this deterministically exercises the organic WebKit failure
  // recorded on SH-423 instead of hoping the server wins a sub-click race.
  await patchTaken;
  releasePatch();
  await patchDelivered;
  await expect.poll(() => page.evaluate(() =>
    (window as typeof window & {
      __storyhookPressGate?: { deferred: string[] };
    }).__storyhookPressGate?.deferred ?? [],
  )).toContain("drawer");
  const targetSurvived = await page.evaluate(() => {
    const saved = (window as typeof window & { __sh423PressTarget?: Element }).__sh423PressTarget;
    const current = Array.from(document.querySelectorAll(".section-toggle")).find((node) =>
      node.textContent?.includes("Comments"),
    );
    return !!saved?.isConnected && saved === current;
  });
  expect(targetSurvived).toBe(true);
  await page.mouse.up();

  // The click reaches the toggle (proving the description's blur-driven
  // layout shift doesn't swallow it), the description commits, and the
  // Comments section's own expand/collapse state flips.
  await expect.poll(() => patches.length).toBeGreaterThan(0);
  expect(patches).toEqual([{ description: "after edit" }]);
  await expect(page.locator(".description-view")).toContainText("after edit");
  await expect(commentsToggle).toHaveAttribute("aria-expanded", "false");
  expect(await pressGateSwallows(page)).toEqual([]);
  await page.locator("#drawer-close").click();
  await deleteStory(page, title);
});

test("a drawer mutation retains unchanged sections and their handlers read current story data", async ({
  page,
}) => {
  const title = "SH-423 drawer sections — before title";
  const nextTitle = "SH-423 drawer sections — after title";
  const card = await createStory(page, title, "description stays put");
  await openStory(page, card);

  await page.evaluate(() => {
    const comments = Array.from(document.querySelectorAll(".section-toggle")).find((node) =>
      node.textContent?.includes("Comments"),
    );
    (window as typeof window & { __sh423Comments?: Element; __sh423Close?: Element })
      .__sh423Comments = comments;
    (window as typeof window & { __sh423Comments?: Element; __sh423Close?: Element })
      .__sh423Close = document.querySelector("#drawer-footer button:not(.btn-danger)");
  });

  const titleInput = page.locator(".drawer-title");
  await titleInput.fill(nextTitle);
  const patched = page.waitForResponse(
    (resp) => /\/story\/[^/]+$/.test(new URL(resp.url()).pathname) && resp.request().method() === "PATCH",
  );
  // Programmatic focus blurs the title without turning the description view
  // back into an editor, leaving the ensuing mutation as the only render.
  await page.locator(".description-view").focus();
  await patched;
  await expect(page.locator(".drawer-title")).toHaveValue(nextTitle);

  const retained = await page.evaluate(() => {
    const saved = window as typeof window & { __sh423Comments?: Element; __sh423Close?: Element };
    const currentComments = Array.from(document.querySelectorAll(".section-toggle")).find((node) =>
      node.textContent?.includes("Comments"),
    );
    return {
      comments: saved.__sh423Comments === currentComments,
      footer: saved.__sh423Close === document.querySelector("#drawer-footer button:not(.btn-danger)"),
    };
  });
  expect(retained).toEqual({ comments: true, footer: true });

  await page.locator("#drawer-footer button", { hasText: "Close" }).click();
  await expect(page.locator("#close-modal-summary")).toContainText(nextTitle);
  await page.locator("#close-modal-cancel").click();
  await page.locator("#drawer-close").click();
  await deleteStory(page, nextTitle);
});
