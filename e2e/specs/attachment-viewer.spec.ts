import { execFileSync } from "node:child_process";
import { mkdirSync, writeFileSync } from "node:fs";
import { dirname } from "node:path";
import type { Page } from "@playwright/test";
import {
  test, expect, seedToken, openProject, createStory, cleanUpCreatedStories,
  requiredEnv, latch, fullKeyboardAccess, storyBinary,
} from "./support";

cleanUpCreatedStories("Alpha Project");

/** Mutates only this harness's isolated store, through the real CLI. */
function command(args: string[]): string {
  return execFileSync(storyBinary(), args, {
    cwd: requiredEnv("DASHBOARD_ALPHA_CHECKOUT"), encoding: "utf8",
    timeout: test.info().timeout,
  });
}

/** A decodable image, including hostile filenames, stored through production code. */
function addImage(id: string, name: string, bytes?: Buffer): number {
  const path = test.info().outputPath("image.png");
  mkdirSync(dirname(path), { recursive: true });
  writeFileSync(path, bytes ?? Buffer.from("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aC1sAAAAASUVORK5CYII=", "base64"));
  const result = JSON.parse(command(["attachment", "add", id, path, "--name", name, "--json"]));
  return result.story.story.attachments.at(-1).id;
}

/** Opens a newly created story using its actual board card. */
async function openStory(page: Page, id: string): Promise<void> {
  await page.locator(`.card[data-id="${id}"]`).click();
  await expect(page.locator("#drawer")).toHaveClass(/open/);
}

/** Confirms browser decoding rather than only an HTTP success or img element. */
async function decoded(page: Page): Promise<void> {
  await expect(page.locator("#attachment-image img")).toBeVisible();
  await expect.poll(() => page.locator("#attachment-image img").evaluate(
    (node: HTMLImageElement) => node.complete && node.naturalWidth > 0,
  )).toBe(true);
  await expect(page.locator("#attachment-status")).toHaveText("");
}

test.beforeEach(async ({ page }) => {
  await seedToken(page);
  await page.goto("/");
  await openProject(page, "Alpha Project");
});

test("empty drawer gains ordered thumbnails live; names are text and images use cookies", async ({ page }) => {
  const id = await createStory(page, "Attachment strip");
  await openStory(page, id);
  await expect(page.locator('[data-drawer-section="attachments"]')).toHaveCount(0);
  const name = '<img src=x onerror="alert(1)"> café.png';
  const imageResponse = page.waitForResponse(r => r.url().endsWith(`/story/${id}/attachments/1`));
  addImage(id, name);
  addImage(id, "second.png");
  const buttons = page.locator(".attachment-thumbnail");
  await expect(buttons).toHaveCount(2);
  await expect(buttons.locator(".attachment-name")).toHaveText([name, "second.png"]);
  await expect(buttons.first().locator("img")).toHaveCount(1);
  await buttons.first().click();
  await decoded(page);
  await expect(page.locator("#attachment-title")).toHaveText(name);
  const headers = await (await imageResponse).request().allHeaders();
  expect(headers["x-storyhook-token"]).toBeUndefined();
  expect(headers["x-storyhook"]).toBeUndefined();
  expect(headers.cookie).toContain(requiredEnv("DASHBOARD_COOKIE_NAME") + "=");
  await page.locator("#attachment-close").click();
});

test("keyboard, close, backdrop, and Escape restore focus without closing detail", async ({ page }) => {
  const id = await createStory(page, "Attachment modality");
  addImage(id, "focus.png");
  await openStory(page, id);
  const button = page.locator(".attachment-thumbnail");
  await button.focus();
  await page.keyboard.press("Enter");
  const close = page.locator("#attachment-close");
  await expect(close).toBeFocused();
  await expect(page.locator("#app")).toHaveAttribute("inert", "");
  if (test.info().project.name !== "webkit" || fullKeyboardAccess()) {
    await page.keyboard.press("Tab");
    await expect(close).toBeFocused();
    await page.keyboard.press("Shift+Tab");
    await expect(close).toBeFocused();
  }
  await page.keyboard.press("Escape");
  await expect(button).toBeFocused();
  await expect(page.locator("#drawer")).toHaveClass(/open/);
  await button.click();
  await close.click();
  await expect(button).toBeFocused();
  await button.click();
  await page.locator("#attachment-backdrop").click({ position: { x: 5, y: 5 } });
  await expect(button).toBeFocused();
  await expect(page.locator("#app")).not.toHaveAttribute("inert", "");
  await expect(page.locator("#attachment-image img")).toHaveCount(0);
});

test("live refresh preserves nodes and viewer; removing its attachment closes it", async ({ page }) => {
  const id = await createStory(page, "Live attachments");
  const first = addImage(id, "first.png");
  await openStory(page, id);
  const button = page.locator(".attachment-thumbnail").first();
  const original = await button.elementHandle();
  await button.click();
  await decoded(page);
  command(["set", id, "--title", "Live attachments updated"]);
  await expect(page.locator(`.card[data-id="${id}"]`)).toContainText("Live attachments updated");
  expect(await original!.evaluate(node => node.isConnected)).toBe(true);
  await expect(page.locator("#attachment-modal")).toHaveClass(/open/);
  addImage(id, "second.png");
  await expect(page.locator(".attachment-thumbnail")).toHaveCount(2);
  await page.locator("#attachment-close").click();
  await expect(button).toBeFocused();
  await button.click();
  command(["attachment", "remove", id, String(first)]);
  await expect(page.locator("#attachment-modal")).not.toHaveClass(/open/);
  await expect(page.locator(".attachment-name")).toHaveText(["second.png"]);
  await expect(page.locator("#drawer")).toBeFocused();
});

test("error can be retried and late image replies cannot replace a later selection", async ({ page }) => {
  const id = await createStory(page, "Attachment request lifecycle");
  addImage(id, "first.png");
  addImage(id, "second.png");
  const path = `**/story/${id}/attachments/1`;
  await page.route(path, route => route.fulfill({ status: 500, body: "image unavailable" }));
  await openStory(page, id);
  const buttons = page.locator(".attachment-thumbnail");
  await expect(buttons).toHaveCount(2);
  await buttons.first().click();
  await expect(page.locator("#attachment-status")).toContainText("Could not load first.png");
  await page.locator("#attachment-close").click();
  await page.unroute(path);
  await buttons.first().click();
  await decoded(page);
  await page.locator("#attachment-close").click();

  const held = latch();
  const received = latch();
  await page.route(`**/story/${id}/attachments/3`, async route => {
    received.release();
    await held.held;
    await route.continue();
  });
  try {
    addImage(id, "delayed.png");
    await expect(buttons).toHaveCount(3);
    await buttons.last().click();
    await received.held;
    await expect(page.locator("#attachment-status")).toHaveText("Loading image…");
    await page.locator("#attachment-close").click();
    await buttons.nth(1).click();
    await decoded(page);
    held.release();
    await page.unrouteAll({ behavior: "wait" });
    await expect(page.locator("#attachment-title")).toHaveText("second.png");
    await decoded(page);
  } finally {
    held.release();
    await page.unrouteAll({ behavior: "wait" });
    await page.keyboard.press("Escape");
  }
});

test("rapid reopen and narrow layout keep the image and close control reachable", async ({ page }) => {
  const id = await createStory(page, "Attachment geometry");
  const encoded = await page.evaluate(() => {
    const canvas = document.createElement("canvas");
    canvas.width = 2400;
    canvas.height = 1600;
    canvas.getContext("2d")!.fillRect(0, 0, canvas.width, canvas.height);
    return canvas.toDataURL("image/png").split(",")[1];
  });
  addImage(id, "a-very-long-filename-".repeat(20) + ".png", Buffer.from(encoded, "base64"));
  await openStory(page, id);
  await page.locator(".attachment-thumbnail").click();
  await page.evaluate(() => {
    document.getElementById("attachment-close")!.click();
    document.querySelector<HTMLButtonElement>(".attachment-thumbnail")!.click();
  });
  await decoded(page);
  await expect(page.locator("#attachment-backdrop")).toBeVisible();
  await page.setViewportSize({ width: 375, height: 667 });
  const modal = page.locator("#attachment-modal");
  await expect.poll(async () => {
    const box = await modal.boundingBox();
    return !!box && box.x >= 0 && box.y >= 0 && box.x + box.width <= 375 && box.y + box.height <= 667;
  }).toBe(true);
  await expect(page.locator("#attachment-close")).toBeInViewport();
  await page.screenshot({ path: test.info().outputPath("viewer-narrow.png") });
  await page.locator("#attachment-close").click();
});

test("story and project navigation clear the previous image identity", async ({ page }) => {
  const id = await createStory(page, "Attachment identity");
  addImage(id, "owned.png");
  await openStory(page, id);
  await page.locator(".attachment-thumbnail").click();
  await decoded(page);
  await page.keyboard.press("Escape");
  await page.locator(`.card[data-id="${requiredEnv("DASHBOARD_ALPHA_STORY_ID")}"]`).click();
  await expect(page.locator(".attachment-thumbnail")).toHaveCount(0);
  await expect(page.locator("#attachment-image img")).toHaveCount(0);
  await page.locator("#drawer-close").click();
  await openStory(page, id);
  await page.locator(".attachment-thumbnail").click();
  await decoded(page);
  await page.keyboard.press("Escape");
  await page.locator("#drawer-close").click();
  await page.locator("#home-btn").click();
  await openProject(page, "Beta Project");
  await expect(page.locator("#attachment-modal")).not.toHaveClass(/open/);
  await expect(page.locator("#attachment-image img")).toHaveCount(0);
});


test("deleting the owning story removes both viewer and thumbnail strip", async ({ page }) => {
  const id = await createStory(page, "Attachment owner deletion");
  addImage(id, "owned.png");
  await openStory(page, id);
  await page.locator(".attachment-thumbnail").click();
  await decoded(page);
  command(["delete", id, "--force"]);
  await expect(page.locator("#attachment-modal")).not.toHaveClass(/open/);
  await expect(page.locator(".attachment-thumbnail")).toHaveCount(0);
  await expect(page.locator("#attachment-image img")).toHaveCount(0);
});


test("Escape dismisses authentication above the viewer before the image", async ({ page }) => {
  const id = await createStory(page, "Attachment nested overlay");
  addImage(id, "nested.png");
  await openStory(page, id);
  await page.locator(".attachment-thumbnail").click();
  await decoded(page);
  await page.route("**/data", route => route.fulfill({ status: 401, body: "authentication required" }));
  command(["set", id, "--title", "Trigger authenticated board refresh"]);
  await expect(page.locator("#token-modal")).toHaveClass(/open/);
  await expect(page.locator("#attachment-modal")).toHaveAttribute("inert", "");
  await page.unroute("**/data");
  await page.keyboard.press("Escape");
  await expect(page.locator("#token-modal")).not.toHaveClass(/open/);
  await expect(page.locator("#attachment-modal")).toHaveClass(/open/);
  await expect(page.locator("#attachment-close")).toBeFocused();
  await page.keyboard.press("Escape");
  await expect(page.locator("#drawer")).toHaveClass(/open/);
  await expect(page.locator(".attachment-thumbnail")).toBeFocused();
});
