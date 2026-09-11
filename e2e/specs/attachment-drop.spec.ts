import type { JSHandle, Locator, Page } from "@playwright/test";
import {
  cleanUpCreatedStories,
  createStory,
  expect,
  latch,
  openProject,
  seedToken,
  test,
} from "./support";

cleanUpCreatedStories("Alpha Project");

const PIXEL = Array.from(Buffer.from(
  "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aC1sAAAAASUVORK5CYII=",
  "base64",
));

type DroppedFile = { name: string; bytes: number[]; type?: string };

async function transfer(page: Page, files: DroppedFile[]): Promise<JSHandle<DataTransfer>> {
  return page.evaluateHandle((entries) => {
    const value = new DataTransfer();
    for (const entry of entries) {
      value.items.add(new File(
        [new Uint8Array(entry.bytes)],
        entry.name,
        { type: entry.type ?? "application/octet-stream" },
      ));
    }
    return value;
  }, files);
}

async function dropFiles(
  page: Page,
  target: Locator,
  files: DroppedFile[],
  visualTarget = target,
): Promise<void> {
  const dataTransfer = await transfer(page, files);
  await target.dispatchEvent("dragenter", { dataTransfer });
  await target.dispatchEvent("dragover", { dataTransfer });
  await expect(visualTarget).toHaveClass(/attachment-drop-target/);
  await target.dispatchEvent("drop", { dataTransfer });
  await expect(visualTarget).not.toHaveClass(/attachment-drop-target/);
  await dataTransfer.dispose();
}

async function openStory(page: Page, id: string): Promise<void> {
  await page.locator(`.card[data-id="${id}"]`).click();
  await expect(page.locator("#drawer")).toHaveClass(/open/);
  await expect(page.locator("#drawer-id")).toHaveText(id);
}

test.beforeEach(async ({ page }) => {
  await seedToken(page);
  await page.goto("/");
  await openProject(page, "Alpha Project");
});

test("description and comment drops preserve text while ordered images appear", async ({ page }) => {
  const id = await createStory(page, `SH-392 field drops ${Date.now()}`);
  await openStory(page, id);

  const description = "a description, so Copy Description has something to copy";
  const descriptionWrap = page.locator(".description-section");
  const firstUpload = page.waitForRequest((request) =>
    request.method() === "POST" && request.url().endsWith(`/story/${id}/attachments`),
  );
  await dropFiles(page, page.locator(".description-view"), [
    { name: "first café.png", bytes: PIXEL, type: "image/png" },
  ], descriptionWrap);
  const request = await firstUpload;
  expect(request.headers()["content-type"]).toBe("application/octet-stream");
  expect(request.headers()["x-storyhook-attachment-name"]).toBe(encodeURIComponent("first café.png"));
  await expect(page.locator(".attachment-name")).toHaveText(["first café.png"]);
  await expect(page.locator(".description-view")).toContainText(description);

  const comment = page.locator(".comment-add textarea");
  await comment.fill("comment text that must remain a draft");
  await comment.evaluate((node: HTMLTextAreaElement) => node.setSelectionRange(8, 12));
  await dropFiles(page, comment, [
    { name: "second.png", bytes: PIXEL, type: "image/png" },
    { name: "third.png", bytes: PIXEL, type: "image/png" },
  ]);
  await expect(page.locator(".attachment-name")).toHaveText([
    "first café.png", "second.png", "third.png",
  ]);
  await expect(comment).toHaveValue("comment text that must remain a draft");
  await expect(comment).toBeFocused();
  expect(await comment.evaluate((node: HTMLTextAreaElement) => [node.selectionStart, node.selectionEnd]))
    .toEqual([8, 12]);
  await expect(page.locator(".comment")).toHaveCount(0);
  for (const image of await page.locator(".attachment-thumbnail img").all()) {
    await expect.poll(() => image.evaluate((node: HTMLImageElement) => node.complete && node.naturalWidth > 0))
      .toBe(true);
  }
});

test("a definite refusal names its file and the batch continues", async ({ page }) => {
  const id = await createStory(page, `SH-392 refusal ${Date.now()}`);
  await openStory(page, id);
  await dropFiles(page, page.locator(".comment-add textarea"), [
    { name: "not-an-image.txt", bytes: Array.from(Buffer.from("not an image")), type: "text/plain" },
    { name: "accepted.png", bytes: PIXEL, type: "image/png" },
  ]);
  await expect(page.locator(".toast.error").first()).toContainText("not-an-image.txt");
  await expect(page.locator(".attachment-name")).toHaveText(["accepted.png"]);
});

test("an ambiguous failure sends once and stops the remaining batch", async ({ page }) => {
  const id = await createStory(page, `SH-392 ambiguity ${Date.now()}`);
  await openStory(page, id);
  let uploads = 0;
  await page.route(`**/story/${id}/attachments`, async (route) => {
    uploads += 1;
    await route.abort("connectionfailed");
  });
  await dropFiles(page, page.locator(".description-view"), [
    { name: "uncertain.png", bytes: PIXEL, type: "image/png" },
    { name: "must-not-run.png", bytes: PIXEL, type: "image/png" },
  ], page.locator(".description-section"));
  await expect(page.locator(".toast.error").first()).toContainText("could not confirm");
  await expect(page.locator(".toast.error").first()).toContainText("uncertain.png");
  expect(uploads).toBe(1);
  await expect(page.locator(".attachment-thumbnail")).toHaveCount(0);
});

test("a held upload stays owned by its original project", async ({ page }) => {
  const id = await createStory(page, `SH-392 identity ${Date.now()}`);
  await openStory(page, id);
  const held = latch();
  const received = latch();
  await page.route(`**/story/${id}/attachments`, async (route) => {
    received.release();
    await held.held;
    await route.continue();
  });
  const dropping = dropFiles(page, page.locator(".description-view"), [
    { name: "owned-by-alpha.png", bytes: PIXEL, type: "image/png" },
  ], page.locator(".description-section"));
  await received.held;
  await page.locator("#drawer-close").click();
  await page.locator("#home-btn").click();
  await openProject(page, "Beta Project");
  held.release();
  await dropping;
  await expect(page.locator("#projsel-btn")).toContainText("BB · Beta Project");
  await expect(page.locator("#drawer")).not.toHaveClass(/open/);
  await expect(page.locator(".attachment-thumbnail")).toHaveCount(0);
});

test("closed stories refuse files and text drags never enter the upload path", async ({ page }) => {
  const id = await createStory(page, `SH-392 closed ${Date.now()}`);
  await openStory(page, id);
  await page.locator("#drawer-footer").getByRole("button", { name: "Drop", exact: true }).click();
  await page.locator("#close-reason").fill("exercise closed attachment behavior");
  await page.locator("#close-modal-submit").click();
  await expect(page.locator("#close-modal")).not.toHaveClass(/open/);

  let uploads = 0;
  page.on("request", (request) => {
    if (request.method() === "POST" && request.url().endsWith(`/story/${id}/attachments`)) uploads += 1;
  });
  const fileTransfer = await transfer(page, [{ name: "closed.png", bytes: PIXEL, type: "image/png" }]);
  const description = page.locator(".description-view");
  await description.dispatchEvent("dragover", { dataTransfer: fileTransfer });
  await expect(page.locator(".description-section")).toHaveClass(/attachment-drop-refused/);
  await description.dispatchEvent("drop", { dataTransfer: fileTransfer });
  await expect(page.locator(".toast.error").first()).toContainText("Reopen");
  await fileTransfer.dispose();

  const textTransfer = await page.evaluateHandle(() => {
    const value = new DataTransfer();
    value.setData("text/plain", "AA-999");
    return value;
  });
  await page.locator(".comment-add textarea").dispatchEvent("dragover", { dataTransfer: textTransfer });
  await page.locator(".comment-add textarea").dispatchEvent("drop", { dataTransfer: textTransfer });
  await textTransfer.dispose();
  expect(uploads).toBe(0);
});
