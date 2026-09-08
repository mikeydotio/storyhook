import type { APIRequestContext, Page } from "@playwright/test";
import {
  test,
  expect,
  cleanUpCreatedStories,
  openProject,
  projectSlug,
  requiredEnv,
  seedToken,
} from "./support";

cleanUpCreatedStories("Alpha Project");
cleanUpCreatedStories("Beta Project");

const PNG =
  "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aC1sAAAAASUVORK5CYII=";
const GIF = "R0lGODlhAQABAIAAAAAAAP///ywAAAAAAQABAAACAUwAOw==";
const WEBP = "UklGRiIAAABXRUJQVlA4IBYAAAAwAQCdASoBAAEADsD+JaQAA3AAAAAA";

type PastedImage = { name: string; type: string };

/** Dispatches the browser-standard clipboard payload without mocking the
 * dashboard handler. Synthetic paste has no native text insertion, so the
 * boolean reports whether application code cancelled that default. */
async function paste(
  page: Page,
  images: PastedImage[],
  text = "",
): Promise<boolean> {
  return page.locator("#create-description").evaluate(
    (node, input) => {
      const transfer = new DataTransfer();
      for (const image of input.images) {
        let encoded = input.png;
        if (image.type === "image/gif") {
          encoded = input.gif;
        } else if (image.type === "image/webp") {
          encoded = input.webp;
        } else if (image.type === "image/jpeg") {
          const canvas = document.createElement("canvas");
          canvas.width = 1;
          canvas.height = 1;
          const context = canvas.getContext("2d");
          if (!context) throw new Error("browser has no 2D canvas context");
          context.fillStyle = "#ffffff";
          context.fillRect(0, 0, 1, 1);
          const dataUrl = canvas.toDataURL(image.type);
          if (!dataUrl.startsWith(`data:${image.type};base64,`)) {
            throw new Error(`browser cannot encode ${image.type}`);
          }
          encoded = dataUrl.split(",")[1];
        }
        const raw = atob(encoded);
        const bytes = Uint8Array.from(raw, (character) => character.charCodeAt(0));
        transfer.items.add(new File([bytes], image.name, { type: image.type }));
      }
      if (input.text) transfer.setData("text/plain", input.text);
      const event = new ClipboardEvent("paste", {
        bubbles: true,
        cancelable: true,
        clipboardData: transfer,
      });
      return !node.dispatchEvent(event);
    },
    { images, text, png: PNG, gif: GIF, webp: WEBP },
  );
}

async function projectData(request: APIRequestContext, name = "Alpha Project"): Promise<{
  stories: Array<{ story: { id: string; title: string; attachments?: Array<{ name: string }> } }>;
  drafts: Array<{ story: { id: string; title: string; attachments?: Array<{ name: string }> } }>;
}> {
  const slug = await projectSlug(request, name);
  const response = await request.get(`/api/repos/${encodeURIComponent(slug)}/data`, {
    headers: { "X-Storyhook-Token": requiredEnv("DASHBOARD_TOKEN") },
  });
  expect(response.ok()).toBe(true);
  return response.json();
}

test.beforeEach(async ({ page }) => {
  await seedToken(page);
  await page.goto("/");
  await openProject(page, "Alpha Project");
  await page.locator("#new-story-btn").click();
  await expect(page.locator("#create-modal")).toHaveClass(/open/);
});

test("image representations win over text and upload in clipboard order", async ({
  page,
}) => {
  const title = "Pasted image order";
  await page.locator("#create-title").fill(title);
  expect(
    await paste(
      page,
      [
        { name: "first.png", type: "image/png" },
        { name: "second.jpg", type: "image/jpeg" },
        { name: "third.gif", type: "image/gif" },
        { name: "", type: "image/webp" },
        { name: '<img src=x onerror="window.__pastedNameRan=true">.png', type: "image/png" },
      ],
      "the clipboard also exposed text",
    ),
  ).toBe(true);

  const pending = page.locator('[data-create-attachment-state="pending"]');
  await expect(pending).toHaveCount(5);
  await expect(pending.locator(".create-attachment-name")).toHaveText([
    "first.png",
    "second.jpg",
    "third.gif",
    "pasted-image.webp",
    '<img src=x onerror="window.__pastedNameRan=true">.png',
  ]);
  await expect(pending.locator("img")).toHaveCount(5);
  expect(
    await page.evaluate(
      () => (window as Window & { __pastedNameRan?: boolean }).__pastedNameRan,
    ),
  ).toBeUndefined();
  await expect(page.locator("#create-description")).toHaveValue("");
  await expect
    .poll(() => pending.first().locator("img").evaluate((image) => image.naturalWidth))
    .toBeGreaterThan(0);
  await page.setViewportSize({ width: 375, height: 667 });
  await expect(page.locator("#create-submit")).toBeInViewport();
  await page.screenshot({ path: test.info().outputPath("paste-preview-narrow.png") });
  await pending
    .nth(4)
    .getByRole("button", {
      name: 'Remove <img src=x onerror="window.__pastedNameRan=true">.png',
    })
    .click();
  await expect(pending).toHaveCount(4);

  await page.locator("#create-submit").click();
  await expect(page.locator("#create-modal")).not.toHaveClass(/open/);
  const card = page.locator(".card", { hasText: title });
  await expect(card).toBeVisible();
  await card.click();
  await expect(page.locator(".attachment-thumbnail .attachment-name")).toHaveText([
    "first.png",
    "second.jpg",
    "third.gif",
    "pasted-image.webp",
  ]);
});

test("text-only paste keeps the textarea default and unsupported images fail locally", async ({
  page,
}) => {
  expect(await paste(page, [], "ordinary text")).toBe(false);
  expect(await paste(page, [{ name: "unsafe.svg", type: "image/svg+xml" }])).toBe(true);
  await expect(page.locator('[data-create-attachment-state="pending"]')).toHaveCount(0);
  await expect(page.locator("#create-attachment-status")).toContainText(
    "PNG, JPEG, GIF, or WebP",
  );
});

test("a failed later upload leaves one editable draft and retries only pending images", async ({
  page,
  request,
}) => {
  const title = "Recover a partial paste";
  await page.locator("#create-title").fill(title);
  await paste(page, [
    { name: "kept.png", type: "image/png" },
    { name: "retry.png", type: "image/png" },
  ]);

  let uploads = 0;
  await page.route("**/attachments", async (route) => {
    if (route.request().method() !== "POST") return route.continue();
    uploads += 1;
    if (uploads === 2) {
      await route.fulfill({ status: 422, body: "image rejected for the test" });
    } else {
      await route.continue();
    }
  });

  await page.locator("#create-submit").click();
  await expect(page.locator("#create-modal")).toHaveClass(/open/);
  await expect(page.locator("#create-modal-header")).toHaveText("Edit draft");
  await expect(page.locator("#create-error")).toContainText("image rejected for the test");
  await expect(page.locator('[data-create-attachment-state="persisted"]')).toHaveCount(1);
  await expect(page.locator('[data-create-attachment-state="pending"]')).toHaveCount(1);
  const afterFailure = await projectData(request);
  expect(afterFailure.stories.filter((view) => view.story.title === title)).toHaveLength(0);
  expect(afterFailure.drafts.filter((view) => view.story.title === title)).toHaveLength(1);

  await page.unroute("**/attachments");
  await page.locator("#create-submit").click();
  await expect(page.locator("#create-modal")).not.toHaveClass(/open/);
  const afterRetry = await projectData(request);
  const live = afterRetry.stories.filter((view) => view.story.title === title);
  expect(live).toHaveLength(1);
  expect(live[0].story.attachments?.map((attachment) => attachment.name)).toEqual([
    "kept.png",
    "retry.png",
  ]);
  expect(afterRetry.drafts.filter((view) => view.story.title === title)).toHaveLength(0);
});

test("a cross-project saved draft reopens with its image and publishes to its owner", async ({
  page,
  request,
}) => {
  const title = "Cross-project pasted image";
  const beta = await projectSlug(request, "Beta Project");
  await page.locator("#create-project").selectOption(beta);
  await expect(page.locator("#create-submit")).toBeEnabled();
  await page.locator("#create-title").fill(title);
  await paste(page, [{ name: "owned-by-beta.png", type: "image/png" }]);
  await page.locator("#create-save-draft").click();
  await expect(page.locator("#create-modal")).not.toHaveClass(/open/);

  await page.locator("#drafts-btn").click();
  await page.locator("#drafts-list .drafts-row", { hasText: title }).click();
  await expect(page.locator("#create-modal-header")).toHaveText("Edit draft");
  await expect(page.locator('[data-create-attachment-state="persisted"]')).toHaveCount(1);
  await expect(page.locator(".create-attachment-name")).toHaveText("owned-by-beta.png");
  await page.locator("#create-submit").click();
  await expect(page.locator("#create-modal")).not.toHaveClass(/open/);

  const alphaData = await projectData(request);
  expect(alphaData.stories.filter((view) => view.story.title === title)).toHaveLength(0);
  const betaData = await projectData(request, "Beta Project");
  const betaStory = betaData.stories.filter((view) => view.story.title === title);
  expect(betaStory).toHaveLength(1);
  expect(betaStory[0].story.attachments?.map((attachment) => attachment.name)).toEqual([
    "owned-by-beta.png",
  ]);
});

test("Save Draft persists images and an ambiguous upload is never replayed automatically", async ({
  page,
  request,
}) => {
  const title = "Ambiguous pasted image";
  await page.locator("#create-title").fill(title);
  await paste(page, [{ name: "uncertain.png", type: "image/png" }]);
  let uploads = 0;
  await page.route("**/attachments", async (route) => {
    if (route.request().method() !== "POST") return route.continue();
    uploads += 1;
    await route.abort("connectionfailed");
  });

  await page.locator("#create-save-draft").click();
  await expect(page.locator("#create-modal")).toHaveClass(/open/);
  await expect(page.locator("#create-error")).toContainText(
    "may already be attached; reload and check the draft before retrying",
  );
  await expect(page.locator('[data-create-attachment-state="pending"]')).toHaveCount(1);
  expect(uploads).toBe(1);
  await page.waitForTimeout(250);
  expect(uploads).toBe(1);
  const data = await projectData(request);
  expect(data.drafts.filter((view) => view.story.title === title)).toHaveLength(1);
});
