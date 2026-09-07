import { test, expect } from "./support";
import type { Locator } from "@playwright/test";
import {
  cleanUpCreatedStories,
  createStory,
  openProject,
  pressGateSwallows,
  seedToken,
  settledBoundingBox,
} from "./support";

cleanUpCreatedStories("Alpha Project");

test.beforeEach(async ({ page }) => {
  // A real scrollport, not a synthetic DOM replacement: Comments lies below
  // the fold even with this story's short description (SH-577).
  await page.setViewportSize({ width: 1280, height: 480 });
  await seedToken(page);
  await page.goto("/");
  await openProject(page, "Alpha Project");
  const title = `SH-577 coordinate preparation — ${test.info().title}`;
  const id = await createStory(page, title);
  const detail = page.waitForResponse((response) =>
    response.request().method() === "GET" &&
    new URL(response.url()).pathname.endsWith(`/story/${id}`),
  );
  await page.locator(".card", { hasText: title }).click();
  await detail;
  await expect(page.locator("#drawer-id")).toHaveText(id);
});

/** Names the actual receiver of the exact coordinates the helper returned. */
async function centreReceiver(
  target: Locator,
  box: { x: number; y: number; width: number; height: number },
): Promise<string> {
  return target.evaluate((node, rect) => {
    const hit = document.elementFromPoint(rect.x + rect.width / 2, rect.y + rect.height / 2);
    return hit && node.contains(hit) ? "target" : hit?.outerHTML ?? "outside viewport";
  }, box);
}

test("preparing an off-scrollport toggle preserves the edit until the real press", async ({ page }) => {
  const patches: unknown[] = [];
  page.on("request", (request) => {
    if (request.method() === "PATCH" && /\/story\/[^/]+$/.test(new URL(request.url()).pathname)) {
      patches.push(request.postDataJSON());
    }
  });
  await page.locator(".description-view").click();
  const editor = page.locator(".description-field");
  await editor.fill("SH-577 saved by the real press only");
  const toggle = page.locator(".section-toggle", { hasText: "Comments" });
  await expect(toggle).not.toBeInViewport();

  const box = await settledBoundingBox(page.locator("#drawer"), toggle);
  await expect(editor).toBeFocused();
  expect(patches).toEqual([]);
  expect(await centreReceiver(toggle, box)).toBe("target");
  await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
  await page.mouse.down();
  await page.mouse.up();

  await expect(toggle).toHaveAttribute("aria-expanded", "false");
  await expect(page.locator(".description-view")).toHaveText("SH-577 saved by the real press only");
  expect(patches).toEqual([{ description: "SH-577 saved by the real press only" }]);
  expect(await pressGateSwallows(page)).toEqual([]);
});

test("an already-visible toggle supports repeated real presses", async ({ page }) => {
  const toggle = page.locator(".section-toggle", { hasText: "Comments" });
  await toggle.scrollIntoViewIfNeeded();
  await expect(toggle).toBeInViewport({ ratio: 1 });
  for (const expanded of ["false", "true", "false"]) {
    const box = await settledBoundingBox(page.locator("#drawer"), toggle);
    expect(await centreReceiver(toggle, box)).toBe("target");
    await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
    await page.mouse.down();
    await page.mouse.up();
    await expect(toggle).toHaveAttribute("aria-expanded", expanded);
  }
  expect(await pressGateSwallows(page)).toEqual([]);
});

test("an obstructed centre is refused before a mouse press", async ({ page }) => {
  const toggle = page.locator(".section-toggle", { hasText: "Comments" });
  await toggle.scrollIntoViewIfNeeded();
  await page.locator("#drawer-footer .btn-danger").click();
  await expect(page.locator("#delete-modal")).toHaveClass(/open/);
  await expect(settledBoundingBox(page.locator("#drawer"), toggle)).rejects.toThrow(/centre.*hit target/);
  await expect(toggle).toHaveAttribute("aria-expanded", "true");
  await expect(page.locator("#delete-modal")).toHaveClass(/open/);
});
