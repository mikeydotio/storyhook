import type { Locator, Page } from "@playwright/test";
import {
  expect,
  fullKeyboardAccess,
  openProject,
  seedToken,
  test,
} from "./support";

/**
 * SH-554: story detail is a peer of the board/list workspace, not an
 * overlay. The toolbar keeps the full viewport, the primary content gives
 * the right-hand panel room, and both halves remain interactive.
 */

const ALPHA_CARD_TITLE = "Wire up the auth flow";

test.beforeEach(async ({ page }) => {
  await page.setViewportSize({ width: 1280, height: 844 });
  await seedToken(page);
  await page.goto("/");
  await openProject(page, "Alpha Project");
});

/** Runs one user-facing open/close action synchronously and reports the CSS
 * transition properties it starts on the panel. Keeping this in the same
 * browser task prevents a short transition from finishing during a
 * Playwright round trip before the assertion can observe it. */
async function startedPanelTransitions(action: Locator): Promise<string[]> {
  return action.evaluate((node) => {
    const panel = document.querySelector<HTMLElement>("#drawer")!;
    panel.getBoundingClientRect();
    (node as HTMLElement).click();
    return panel
      .getAnimations()
      .map((animation) =>
        "transitionProperty" in animation
          ? (animation as CSSTransition).transitionProperty
          : "",
      )
      .filter(Boolean);
  });
}

test("opening detail compresses only the content workspace and keeps the toolbar live", async ({
  page,
}) => {
  const card = page.locator(".card", { hasText: ALPHA_CARD_TITLE });
  const closed = await workspaceGeometry(page);

  const openingTransitions = await startedPanelTransitions(card);
  expect(openingTransitions).toEqual(
    expect.arrayContaining(["transform", "width"]),
  );
  await expect(page.locator("#drawer")).toHaveClass(/open/);

  await expect
    .poll(async () => (await workspaceGeometry(page)).panelWidth)
    .toBe(480);
  const opened = await workspaceGeometry(page);

  expect(opened.topbarWidth).toBe(closed.topbarWidth);
  expect(opened.filterWidth).toBe(closed.filterWidth);
  expect(opened.workspaceWidth).toBe(closed.workspaceWidth);
  expect(opened.panelTop).toBe(opened.workspaceTop);
  expect(opened.contentRight).toBe(opened.panelLeft);
  expect(opened.panelRight).toBe(opened.workspaceRight);
  expect(opened.contentWidth).toBe(closed.contentWidth - opened.panelWidth);
  await expect(page.locator("#drawer-backdrop")).toHaveCount(0);
  await expect(page.locator("#app")).not.toHaveAttribute("inert", "");

  // A real click, not synthetic activation: peer content stays usable.
  await page.locator('#view-toggle button[data-view="list"]').click();
  await expect(page.locator("#list-view")).toBeVisible();
  await expect(page.locator("#drawer")).toHaveClass(/open/);

  const close = page.getByRole("button", { name: "Close story details" });
  await expect(close.locator("svg")).toHaveCount(1);
  const closingTransitions = await startedPanelTransitions(close);
  expect(closingTransitions).toEqual(
    expect.arrayContaining(["transform", "width"]),
  );
  await expect(page.locator("#drawer")).not.toHaveClass(/open/);
  await expect
    .poll(async () => (await workspaceGeometry(page)).panelWidth)
    .toBe(0);
  expect((await workspaceGeometry(page)).contentWidth).toBe(closed.contentWidth);
});

test("the peer panel takes focus without trapping it and returns focus on close", async ({
  page,
}) => {
  const card = page.locator(".card", { hasText: ALPHA_CARD_TITLE });
  await card.focus();
  await expect(card).toBeFocused();

  await page.keyboard.press("Enter");
  await expect(page.locator("#drawer")).toHaveClass(/open/);
  await expect(page.locator("#drawer")).toBeFocused();

  await page.evaluate(() => document.getElementById("settings-btn")?.focus());
  await expect(page.locator("#settings-btn")).toBeFocused();

  await page.getByRole("button", { name: "Close story details" }).click();
  await expect(page.locator("#drawer")).not.toHaveClass(/open/);
  await expect(card).toBeFocused();
});

test("retargeting detail from the board returns focus to the latest story", async ({
  page,
}) => {
  const first = page.locator(".card").filter({ hasText: ALPHA_CARD_TITLE });
  const second = page.locator(".card").filter({
    hasText: "Fix the flaky upload test",
  });

  await first.focus();
  await page.keyboard.press("Enter");
  await expect(page.locator("#drawer")).toHaveClass(/open/);

  await second.focus();
  await page.keyboard.press("Enter");
  await expect(page.locator("#drawer-id")).toHaveText(
    (await second.getAttribute("data-id"))!,
  );

  await page.getByRole("button", { name: "Close story details" }).click();
  await expect(second).toBeFocused();
});

test("a modal opened from detail covers both peers and restores the panel control", async ({
  page,
  browserName,
}) => {
  test.skip(
    browserName === "webkit" && !fullKeyboardAccess(),
    "WebKit doesn't focus a <button> on click unless AppleKeyboardUIMode>=2 (SH-335)",
  );
  await page.locator(".card", { hasText: ALPHA_CARD_TITLE }).click();
  const deleteButton = page.locator("#drawer-footer button", {
    hasText: "Delete",
  });
  await deleteButton.click();

  await expect(page.locator("#delete-modal")).toHaveClass(/open/);
  await expect(page.locator("#app")).toHaveAttribute("inert", "");
  expect(
    await page.locator("#drawer").evaluate((node) =>
      node.closest("[inert]")?.getAttribute("id"),
    ),
  ).toBe("app");

  await page.locator("#delete-modal-cancel").click();
  await expect(page.locator("#delete-modal")).not.toHaveClass(/open/);
  await expect(page.locator("#app")).not.toHaveAttribute("inert", "");
  await expect(deleteButton).toBeFocused();
});

interface WorkspaceGeometry {
  topbarWidth: number;
  filterWidth: number;
  workspaceWidth: number;
  workspaceTop: number;
  workspaceRight: number;
  contentWidth: number;
  contentRight: number;
  panelWidth: number;
  panelTop: number;
  panelLeft: number;
  panelRight: number;
}

/** Reads the shared edges whose equality is the peer-layout contract. */
async function workspaceGeometry(page: Page): Promise<WorkspaceGeometry> {
  return page.evaluate(() => {
    const rect = (selector: string) =>
      document.querySelector<HTMLElement>(selector)!.getBoundingClientRect();
    const topbar = rect(".topbar");
    const filter = rect("#filter-bar");
    const workspace = rect("#repo-workspace");
    const content = rect("#workspace-content");
    const panel = rect("#drawer");
    return {
      topbarWidth: topbar.width,
      filterWidth: filter.width,
      workspaceWidth: workspace.width,
      workspaceTop: workspace.top,
      workspaceRight: workspace.right,
      contentWidth: content.width,
      contentRight: content.right,
      panelWidth: panel.width,
      panelTop: panel.top,
      panelLeft: panel.left,
      panelRight: panel.right,
    };
  });
}
