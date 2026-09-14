import { existsSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { test, expect, cleanUpCreatedStories, openProject, seedToken, requiredEnv, projectSlug } from "./support";

cleanUpCreatedStories("Alpha Project");

test.beforeEach(async ({ page }) => {
  await seedToken(page);
  await page.goto("/");
  await openProject(page, "Alpha Project");
});

async function create(page: import("@playwright/test").Page, title: string) {
  await page.locator("#new-story-btn").click();
  await page.locator("#create-title").fill(title);
  await page.locator("#create-submit").click();
  const card = page.locator(".card", { hasText: title });
  await expect(card).toBeVisible();
  return card;
}

test("Reset requires typed confirmation; Escape cancels without changing the story", async ({ page }) => {
  const card = await create(page, "SH-717 reset cancellation");
  let resets = 0;
  page.on("request", request => { if (request.method() === "POST" && request.url().endsWith("/reset")) resets++; });
  await card.click({ button: "right" });
  await expect(page.getByRole("menuitem", { name: /^Reset(?:…)?$/ })).toHaveCount(1);
  const action = page.getByRole("menuitem", { name: "Reset…", exact: true });
  await expect(action).toHaveClass(/danger/);
  await action.click();
  await expect(page.locator("#reset-modal")).toHaveClass(/open/);
  await expect(page.locator("#reset-modal-cancel")).toBeFocused();
  await expect(page.locator("#reset-modal-summary")).toContainText("unpushed");
  await page.locator("#reset-modal-submit").click();
  await expect(page.locator("#reset-modal-error")).toContainText("exactly");
  expect(resets).toBe(0);
  await page.keyboard.press("Escape");
  await expect(page.locator("#reset-modal")).not.toHaveClass(/open/);
  await expect(card).toBeFocused();
  expect(resets).toBe(0);
});

test("Reset supports keyboard menu navigation and list menus", async ({ page }) => {
  const title = "SH-717 keyboard reset";
  const card = await create(page, title);
  await card.getByRole("button", { name: /^Actions for/ }).click();
  await page.keyboard.press("Home");
  for (let index = 0; index < 6; index++) await page.keyboard.press("ArrowDown");
  await expect(page.getByRole("menuitem", { name: "Reset…", exact: true })).toBeFocused();
  await page.keyboard.press("Enter");
  await expect(page.locator("#reset-modal")).toHaveClass(/open/);
  await page.keyboard.press("Escape");
  await page.locator('#view-toggle button[data-view="list"]').click();
  const row = page.locator("#list-body tr", { hasText: title });
  await row.click({ button: "right" });
  await expect(page.getByRole("menuitem", { name: /^Reset(?:…)?$/ })).toHaveCount(1);
  await page.getByRole("menuitem", { name: "Reset…", exact: true }).click();
  await expect(page.locator("#reset-modal-summary")).toContainText(title);
  await page.locator("#reset-modal-cancel").click();
});

test("confirmed Reset uses the real endpoint and returns an active story to todo", async ({ page }) => {
  const title = "SH-717 reset active story";
  const card = await create(page, title);
  const id = (await card.getAttribute("data-id"))!;
  await card.click();
  await page.locator("#drawer-body select").first().selectOption("in-progress");
  await page.locator("#drawer-close").click();
  const active = page.locator('.column[data-state="in-progress"] .card', { hasText: title });
  await expect(active).toBeVisible();
  await active.click({ button: "right" });
  await page.getByRole("menuitem", { name: "Reset…", exact: true }).click();
  await page.locator("#reset-confirmation").fill(id);
  await page.locator("#reset-modal-submit").click();
  await expect(page.locator("#toast-stack .toast.success").filter({ hasText: `${id} reset` })).toBeVisible({ timeout: 20_000 });
  await expect(page.locator("#reset-modal")).not.toHaveClass(/open/);
  await expect(page.locator('.column[data-state="todo"] .card', { hasText: title })).toBeVisible();
});

test("a reset failure remains visible and can be retried", async ({ page }) => {
  const card = await create(page, "SH-717 reset failure");
  const id = (await card.getAttribute("data-id"))!;
  let attempts = 0;
  await page.route("**/story/*/reset", async route => {
    attempts++;
    await route.fulfill({ status: 500, contentType: "text/plain", body: "reset worker could not stop" });
  });
  await card.click({ button: "right" });
  await page.getByRole("menuitem", { name: "Reset…", exact: true }).click();
  await page.locator("#reset-confirmation").fill(id);
  await page.locator("#reset-modal-submit").click();
  await expect(page.locator("#reset-modal-error")).toContainText("reset worker could not stop");
  await expect(page.locator("#reset-modal-submit")).toBeEnabled();
  await page.locator("#reset-modal-submit").click();
  await expect.poll(() => attempts).toBe(2);
  await page.locator("#reset-modal-cancel").click();
});

test("Reset removes a real dispatched worktree with uncommitted content", async ({ page }) => {
  const title = "SH-717 real dispatched reset";
  const card = await create(page, title);
  const id = (await card.getAttribute("data-id"))!;
  await card.click({ button: "right" });
  await page.getByRole("menuitem", { name: "Dispatch", exact: true }).click();
  await page.locator("#dispatch-agent").selectOption("claude");
  await page.locator("#dispatch-auto").uncheck();
  await page.locator("#dispatch-modal-submit").click();
  await expect(page.locator("#toast-stack .toast.success")).toContainText(`${id} dispatched`, { timeout: 45_000 });
  const worktree = join(requiredEnv("DASHBOARD_ALPHA_CHECKOUT"), ".claude/worktrees", id);
  expect(existsSync(worktree)).toBe(true);
  writeFileSync(join(worktree, "uncommitted.txt"), "discarded by the confirmed reset");
  const active = page.locator('.column[data-state="in-progress"] .card', { hasText: title });
  await active.click({ button: "right" });
  await page.getByRole("menuitem", { name: "Reset…", exact: true }).click();
  await page.locator("#reset-confirmation").fill(id);
  await page.locator("#reset-modal-submit").click();
  await expect(page.locator("#toast-stack .toast.success").filter({ hasText: `${id} reset` })).toBeVisible({ timeout: 20_000 });
  expect(existsSync(worktree)).toBe(false);
  await expect(page.locator('.column[data-state="todo"] .card', { hasText: title })).toBeVisible();
});


test("an outstanding reset cannot be submitted twice or dismissed before its result", async ({ page }) => {
  const card = await create(page, "SH-717 pending reset");
  const id = (await card.getAttribute("data-id"))!;
  let release!: () => void;
  const held = new Promise<void>(resolve => { release = resolve; });
  let attempts = 0;
  await page.route("**/story/*/reset", async route => {
    attempts++;
    const response = await route.fetch();
    await held;
    await route.fulfill({ response });
  });
  await card.click({ button: "right" });
  await page.getByRole("menuitem", { name: "Reset…", exact: true }).click();
  await page.locator("#reset-confirmation").fill(id);
  await page.locator("#reset-modal-submit").click();
  try {
    await expect.poll(() => attempts).toBe(1);
    await expect(page.locator("#reset-modal-cancel")).toBeDisabled();
    await expect(page.locator("#reset-modal-submit")).toBeDisabled();
    await page.keyboard.press("Enter");
    await page.keyboard.press("Escape");
    await expect(page.locator("#reset-modal")).toHaveClass(/open/);
    await expect(page.locator("#toast-stack .toast.success").filter({ hasText: `${id} reset` })).toHaveCount(0);
    expect(attempts).toBe(1);
  } finally {
    release();
  }
  await expect(page.locator("#toast-stack .toast.success").filter({ hasText: `${id} reset` })).toBeVisible({ timeout: 20_000 });
});

test("a story closed by another client remains an actionable reset error", async ({ page }) => {
  const card = await create(page, "SH-718 concurrent closure during reset confirmation");
  const id = (await card.getAttribute("data-id"))!;
  await card.click({ button: "right" });
  await page.getByRole("menuitem", { name: "Reset…", exact: true }).click();
  await page.locator("#reset-confirmation").fill(id);
  const slug = await projectSlug(page.request, "Alpha Project");
  const closed = await page.request.post(`/api/repos/${slug}/story/${id}/move`, {
    headers: { "X-Storyhook": "1" }, data: { state: "done" },
  });
  expect(closed.ok()).toBeTruthy();
  const request = page.waitForRequest(request => request.method() === "POST" && request.url().endsWith(`/story/${id}/reset`));
  await page.locator("#reset-modal-submit").click();
  expect((await request).postDataJSON()).toEqual({ confirmation: id });
  await expect(page.locator("#reset-modal-error")).toContainText("closed");
  await expect(page.locator("#reset-modal")).toHaveClass(/open/);
  await expect(page.locator("#reset-modal-cancel")).toBeEnabled();
  await page.locator("#reset-modal-cancel").click();
  await expect(page.locator("#reset-modal")).not.toHaveClass(/open/);
});
